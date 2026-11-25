//! M1.4 scripted event wire format and single-lease model.
//!
//! This module deliberately owns no transport or HID handles.  It is the
//! fail-closed boundary shared by transport mode and the opt-in HID bench.

pub const VERSION_MAJOR: u64 = 1;
pub const VERSION_MINOR: u64 = 0;
pub const LEASE_REQUEST: u64 = 0x20;
pub const TAKEOVER_REQUEST: u64 = 0x21;
pub const TAKEOVER_RESPONSE: u64 = 0x22;
pub const LEASE_STATE: u64 = 0x23;
pub const HEARTBEAT: u64 = 0x24;
pub const TAKEOVER_NOTICE: u64 = 0x25;
pub const ERROR: u64 = 0x26;
pub const LEASE_RELEASE: u64 = 0x27;
pub const HID_BENCH_ARM: u64 = 0x28;
pub const KEYBOARD_STATE: u64 = 0x30;
pub const MOUSE_BUTTONS: u64 = 0x31;
pub const WHEEL: u64 = 0x32;
pub const MOTION: u64 = 0x40;
pub const HEARTBEAT_VECTOR: &[u8] = &[
    0xa7, 0x00, 0x01, 0x01, 0x00, 0x02, 0x18, 0x24, 0x03, 0x50, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
    0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x04, 0x07, 0x05, 0x00, 0x06, 0xa0,
];
pub const MAX_CONTROL: usize = 512;
pub const MAX_MOTION: usize = 64;
const TAKEOVER_WINDOW_MS: u64 = 30_000;
const HEARTBEAT_EXPIRY_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    LeaseRequest {
        heartbeat_ack_supported: bool,
    },
    TakeoverRequest {
        request_id: [u8; 16],
    },
    TakeoverResponse {
        request_id: [u8; 16],
        accepted: bool,
    },
    LeaseState {
        owner: Option<[u8; 32]>,
        state: u8,
        request_sequence: Option<u64>,
    },
    Heartbeat,
    TakeoverNotice {
        request_id: [u8; 16],
        requester: [u8; 32],
        response_window_ms: u64,
    },
    Error {
        code: String,
    },
    LeaseRelease,
    HidBenchArm,
    KeyboardState {
        modifiers: u8,
        keys: Vec<(u16, u16)>,
    },
    MouseButtons {
        mask: u8,
    },
    Wheel {
        vertical: i16,
        horizontal: i16,
    },
    Motion {
        dx: i16,
        dy: i16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub message_type: u64,
    pub session_id: [u8; 16],
    pub lease_epoch: u32,
    pub sequence: u64,
    pub event: Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    NonCanonical,
    WrongType,
    UnknownKey,
    WrongFieldCount,
    InvalidValue,
    Oversized,
    TrailingBytes,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn head(&mut self, major: u8) -> Result<u64, DecodeError> {
        let first = *self.bytes.get(self.pos).ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        if first >> 5 != major {
            return Err(DecodeError::WrongType);
        }
        let ai = first & 0x1f;
        let width = match ai {
            0..=23 => 0,
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            _ => return Err(DecodeError::InvalidValue),
        };
        if width == 0 {
            return Ok(ai as u64);
        }
        let end = self.pos.checked_add(width).ok_or(DecodeError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated)?;
        self.pos = end;
        let value = bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | *byte as u64);
        if (width == 1 && value < 24)
            || (width == 2 && value <= 0xff)
            || (width == 4 && value <= 0xffff)
            || (width == 8 && value <= 0xffff_ffff)
        {
            return Err(DecodeError::NonCanonical);
        }
        Ok(value)
    }

    fn uint(&mut self) -> Result<u64, DecodeError> {
        self.head(0)
    }
    fn map(&mut self, count: u64) -> Result<(), DecodeError> {
        if self.head(5)? == count {
            Ok(())
        } else {
            Err(DecodeError::WrongFieldCount)
        }
    }
    fn bytes(&mut self, expected: usize) -> Result<&'a [u8], DecodeError> {
        let length = usize::try_from(self.head(2)?).map_err(|_| DecodeError::Oversized)?;
        if length != expected {
            return Err(DecodeError::InvalidValue);
        }
        let end = self.pos.checked_add(length).ok_or(DecodeError::Truncated)?;
        let result = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(result)
    }
    fn array(&mut self, count: u64) -> Result<(), DecodeError> {
        if self.head(4)? == count {
            Ok(())
        } else {
            Err(DecodeError::WrongFieldCount)
        }
    }
    fn text(&mut self, maximum: usize) -> Result<&'a str, DecodeError> {
        let length = usize::try_from(self.head(3)?).map_err(|_| DecodeError::Oversized)?;
        if length > maximum {
            return Err(DecodeError::Oversized);
        }
        let end = self.pos.checked_add(length).ok_or(DecodeError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated)?;
        self.pos = end;
        std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidValue)
    }
    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn put_head(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=0xff => out.extend_from_slice(&[prefix | 24, value as u8]),
        0x100..=0xffff => {
            out.push(prefix | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(prefix | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(prefix | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}
fn uint(out: &mut Vec<u8>, value: u64) {
    put_head(out, 0, value);
}
fn signed(out: &mut Vec<u8>, value: i16) {
    if value >= 0 {
        uint(out, value as u64);
    } else {
        put_head(out, 1, (-1 - value as i32) as u64);
    }
}
fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    put_head(out, 2, value.len() as u64);
    out.extend_from_slice(value);
}
fn field(out: &mut Vec<u8>, key: u64) {
    uint(out, key);
}

pub fn encode(envelope: &Envelope) -> Vec<u8> {
    let mut out = Vec::with_capacity(if matches!(&envelope.event, Event::Motion { .. }) {
        32
    } else {
        96
    });
    put_head(&mut out, 5, 7);
    field(&mut out, 0);
    uint(&mut out, VERSION_MAJOR);
    field(&mut out, 1);
    uint(&mut out, VERSION_MINOR);
    field(&mut out, 2);
    uint(&mut out, envelope.message_type);
    field(&mut out, 3);
    bytes(&mut out, &envelope.session_id);
    field(&mut out, 4);
    uint(&mut out, envelope.lease_epoch as u64);
    field(&mut out, 5);
    uint(&mut out, envelope.sequence);
    field(&mut out, 6);
    match &envelope.event {
        Event::LeaseRequest {
            heartbeat_ack_supported,
        } => {
            put_head(&mut out, 5, if *heartbeat_ack_supported { 1 } else { 0 });
            if *heartbeat_ack_supported {
                field(&mut out, 0);
                out.push(0xf5);
            }
        }
        Event::Heartbeat | Event::LeaseRelease | Event::HidBenchArm => put_head(&mut out, 5, 0),
        Event::TakeoverRequest { request_id } => {
            put_head(&mut out, 5, 1);
            field(&mut out, 0);
            bytes(&mut out, request_id);
        }
        Event::TakeoverResponse {
            request_id,
            accepted,
        } => {
            put_head(&mut out, 5, 2);
            field(&mut out, 0);
            bytes(&mut out, request_id);
            field(&mut out, 1);
            out.push(if *accepted { 0xf5 } else { 0xf4 });
        }
        Event::LeaseState {
            owner,
            state,
            request_sequence,
        } => {
            put_head(&mut out, 5, if request_sequence.is_some() { 3 } else { 2 });
            field(&mut out, 0);
            match owner {
                Some(owner) => bytes(&mut out, owner),
                None => out.push(0xf6),
            }
            field(&mut out, 1);
            uint(&mut out, *state as u64);
            if let Some(request_sequence) = request_sequence {
                field(&mut out, 2);
                uint(&mut out, *request_sequence);
            }
        }
        Event::KeyboardState { modifiers, keys } => {
            put_head(&mut out, 5, 2);
            field(&mut out, 0);
            uint(&mut out, *modifiers as u64);
            field(&mut out, 1);
            put_head(&mut out, 4, keys.len() as u64);
            for (page, usage) in keys {
                put_head(&mut out, 4, 2);
                uint(&mut out, *page as u64);
                uint(&mut out, *usage as u64);
            }
        }
        Event::MouseButtons { mask } => {
            put_head(&mut out, 5, 1);
            field(&mut out, 0);
            uint(&mut out, *mask as u64);
        }
        Event::Wheel {
            vertical,
            horizontal,
        } => {
            put_head(&mut out, 5, 2);
            field(&mut out, 0);
            signed(&mut out, *vertical);
            field(&mut out, 1);
            signed(&mut out, *horizontal);
        }
        Event::Motion { dx, dy } => {
            put_head(&mut out, 5, 2);
            field(&mut out, 0);
            signed(&mut out, *dx);
            field(&mut out, 1);
            signed(&mut out, *dy);
        }
        Event::TakeoverNotice {
            request_id,
            requester,
            response_window_ms,
        } => {
            put_head(&mut out, 5, 3);
            field(&mut out, 0);
            bytes(&mut out, request_id);
            field(&mut out, 1);
            bytes(&mut out, requester);
            field(&mut out, 2);
            uint(&mut out, *response_window_ms);
        }
        Event::Error { code } => {
            put_head(&mut out, 5, 1);
            field(&mut out, 0);
            put_head(&mut out, 3, code.len() as u64);
            out.extend_from_slice(code.as_bytes());
        }
    }
    out
}

fn signed_value(reader: &mut Reader<'_>) -> Result<i16, DecodeError> {
    let first = *reader.bytes.get(reader.pos).ok_or(DecodeError::Truncated)?;
    match first >> 5 {
        0 => {
            let value = reader.uint()?;
            i16::try_from(value).map_err(|_| DecodeError::InvalidValue)
        }
        1 => {
            let value = reader.head(1)?;
            let value = i32::try_from(value).map_err(|_| DecodeError::InvalidValue)?;
            i16::try_from(-1 - value).map_err(|_| DecodeError::InvalidValue)
        }
        _ => Err(DecodeError::WrongType),
    }
}

fn payload(reader: &mut Reader<'_>, message_type: u64) -> Result<Event, DecodeError> {
    match message_type {
        LEASE_REQUEST => {
            let field_count = reader.head(5)?;
            let heartbeat_ack_supported = match field_count {
                0 => false,
                1 => {
                    if reader.uint()? != 0 {
                        return Err(DecodeError::UnknownKey);
                    }
                    let value = *reader.bytes.get(reader.pos).ok_or(DecodeError::Truncated)?;
                    reader.pos += 1;
                    if value != 0xf5 {
                        return Err(DecodeError::InvalidValue);
                    }
                    true
                }
                _ => return Err(DecodeError::WrongFieldCount),
            };
            Ok(Event::LeaseRequest {
                heartbeat_ack_supported,
            })
        }
        HEARTBEAT | LEASE_RELEASE | HID_BENCH_ARM => {
            reader.map(0)?;
            Ok(match message_type {
                HEARTBEAT => Event::Heartbeat,
                LEASE_RELEASE => Event::LeaseRelease,
                _ => Event::HidBenchArm,
            })
        }
        TAKEOVER_REQUEST => {
            reader.map(1)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let id = reader
                .bytes(16)?
                .try_into()
                .map_err(|_| DecodeError::InvalidValue)?;
            Ok(Event::TakeoverRequest { request_id: id })
        }
        TAKEOVER_RESPONSE => {
            reader.map(2)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let id = reader
                .bytes(16)?
                .try_into()
                .map_err(|_| DecodeError::InvalidValue)?;
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let value = *reader.bytes.get(reader.pos).ok_or(DecodeError::Truncated)?;
            reader.pos += 1;
            let accepted = match value {
                0xf4 => false,
                0xf5 => true,
                _ => return Err(DecodeError::InvalidValue),
            };
            Ok(Event::TakeoverResponse {
                request_id: id,
                accepted,
            })
        }
        LEASE_STATE => {
            let field_count = reader.head(5)?;
            if field_count != 2 && field_count != 3 {
                return Err(DecodeError::WrongFieldCount);
            }
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let owner = match reader
                .bytes
                .get(reader.pos)
                .copied()
                .ok_or(DecodeError::Truncated)?
            {
                0xf6 => {
                    reader.pos += 1;
                    None
                }
                _ => Some(
                    reader
                        .bytes(32)?
                        .try_into()
                        .map_err(|_| DecodeError::InvalidValue)?,
                ),
            };
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let state = u8::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?;
            if state > 2 {
                return Err(DecodeError::InvalidValue);
            }
            let request_sequence = if field_count == 3 {
                if reader.uint()? != 2 {
                    return Err(DecodeError::UnknownKey);
                }
                Some(reader.uint()?)
            } else {
                None
            };
            Ok(Event::LeaseState {
                owner,
                state,
                request_sequence,
            })
        }
        TAKEOVER_NOTICE => {
            reader.map(3)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let request_id = reader
                .bytes(16)?
                .try_into()
                .map_err(|_| DecodeError::InvalidValue)?;
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let requester = reader
                .bytes(32)?
                .try_into()
                .map_err(|_| DecodeError::InvalidValue)?;
            if reader.uint()? != 2 {
                return Err(DecodeError::UnknownKey);
            }
            let response_window_ms = reader.uint()?;
            if response_window_ms > 30_000 {
                return Err(DecodeError::InvalidValue);
            }
            Ok(Event::TakeoverNotice {
                request_id,
                requester,
                response_window_ms,
            })
        }
        ERROR => {
            reader.map(1)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let code = reader.text(32)?.to_owned();
            if !matches!(
                code.as_str(),
                "unsupported_version"
                    | "unauthorized"
                    | "pin_mismatch"
                    | "replay"
                    | "busy"
                    | "lease_expired"
                    | "stale_epoch"
                    | "invalid_capability"
                    | "rate_limited"
                    | "resource_exhausted"
                    | "malformed_event"
            ) {
                return Err(DecodeError::InvalidValue);
            }
            Ok(Event::Error { code })
        }
        KEYBOARD_STATE => {
            reader.map(2)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let modifiers = u8::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?;
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let count = reader.head(4)?;
            if count > 32 {
                return Err(DecodeError::Oversized);
            }
            let mut keys = Vec::with_capacity(count as usize);
            let mut previous = None;
            for _ in 0..count {
                reader.array(2)?;
                let key = (
                    u16::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?,
                    u16::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?,
                );
                if previous.is_some_and(|old| key <= old) {
                    return Err(DecodeError::InvalidValue);
                }
                previous = Some(key);
                keys.push(key);
            }
            Ok(Event::KeyboardState { modifiers, keys })
        }
        MOUSE_BUTTONS => {
            reader.map(1)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let mask = u8::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?;
            if mask & 0xe0 != 0 {
                return Err(DecodeError::InvalidValue);
            }
            Ok(Event::MouseButtons { mask })
        }
        WHEEL => {
            reader.map(2)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let vertical = signed_value(reader)?;
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let horizontal = signed_value(reader)?;
            Ok(Event::Wheel {
                vertical,
                horizontal,
            })
        }
        MOTION => {
            reader.map(2)?;
            if reader.uint()? != 0 {
                return Err(DecodeError::UnknownKey);
            }
            let dx = signed_value(reader)?;
            if reader.uint()? != 1 {
                return Err(DecodeError::UnknownKey);
            }
            let dy = signed_value(reader)?;
            Ok(Event::Motion { dx, dy })
        }
        _ => Err(DecodeError::InvalidValue),
    }
}

pub fn decode(data: &[u8]) -> Result<Envelope, DecodeError> {
    if data.len() > MAX_CONTROL {
        return Err(DecodeError::Oversized);
    }
    let mut reader = Reader::new(data);
    reader.map(7)?;
    if reader.uint()? != 0
        || reader.uint()? != VERSION_MAJOR
        || reader.uint()? != 1
        || reader.uint()? != VERSION_MINOR
    {
        return Err(DecodeError::InvalidValue);
    }
    if reader.uint()? != 2 {
        return Err(DecodeError::UnknownKey);
    }
    let message_type = reader.uint()?;
    if reader.uint()? != 3 {
        return Err(DecodeError::UnknownKey);
    }
    let session_id: [u8; 16] = reader
        .bytes(16)?
        .try_into()
        .map_err(|_| DecodeError::InvalidValue)?;
    if session_id.iter().all(|byte| *byte == 0) {
        return Err(DecodeError::InvalidValue);
    }
    if reader.uint()? != 4 {
        return Err(DecodeError::UnknownKey);
    }
    let lease_epoch = u32::try_from(reader.uint()?).map_err(|_| DecodeError::InvalidValue)?;
    if reader.uint()? != 5 {
        return Err(DecodeError::UnknownKey);
    }
    let sequence = reader.uint()?;
    if reader.uint()? != 6 {
        return Err(DecodeError::UnknownKey);
    }
    let event = payload(&mut reader, message_type)?;
    if !reader.done() {
        return Err(DecodeError::TrailingBytes);
    }
    if message_type == MOTION && data.len() > MAX_MOTION {
        return Err(DecodeError::Oversized);
    }
    Ok(Envelope {
        message_type,
        session_id,
        lease_epoch,
        sequence,
        event,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldState {
    pub modifiers: u8,
    pub keys: Vec<(u16, u16)>,
    pub buttons: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTakeover {
    pub requester: [u8; 32],
    pub request_id: [u8; 16],
    pub requested_at_ms: u64,
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseState {
    pub epoch: u32,
    pub owner: Option<[u8; 32]>,
    pub pending: Option<PendingTakeover>,
    pub held: HeldState,
    last_heartbeat_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseResult {
    Granted,
    Released,
    Rejected,
    Busy,
    Stale,
    AcceptedPending,
    Expired,
}

impl Default for LeaseState {
    fn default() -> Self {
        Self {
            epoch: 0,
            owner: None,
            pending: None,
            held: HeldState::default(),
            last_heartbeat_ms: None,
        }
    }
}

impl LeaseState {
    fn bump(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
    }
    fn clear_held(&mut self) {
        self.held = HeldState::default();
    }
    fn grant(&mut self, owner: [u8; 32], now_ms: u64) {
        self.bump();
        self.owner = Some(owner);
        self.pending = None;
        self.last_heartbeat_ms = Some(now_ms);
        self.clear_held();
    }
    fn invalidate(&mut self) {
        self.bump();
        self.owner = None;
        self.last_heartbeat_ms = None;
        self.clear_held();
    }
    pub fn request(&mut self, client: [u8; 32], now_ms: u64) -> LeaseResult {
        if self.owner.is_none() {
            self.grant(client, now_ms);
            LeaseResult::Granted
        } else if self.owner == Some(client) {
            self.last_heartbeat_ms = Some(now_ms);
            LeaseResult::Granted
        } else {
            LeaseResult::Rejected
        }
    }
    pub fn takeover(
        &mut self,
        owner: [u8; 32],
        requester: [u8; 32],
        request_id: [u8; 16],
        now_ms: u64,
    ) -> LeaseResult {
        if self.owner != Some(owner) {
            return LeaseResult::Rejected;
        }
        if self.pending.is_some() {
            return LeaseResult::Busy;
        }
        self.pending = Some(PendingTakeover {
            requester,
            request_id,
            requested_at_ms: now_ms,
            accepted: false,
        });
        LeaseResult::AcceptedPending
    }
    pub fn response(
        &mut self,
        owner: [u8; 32],
        request_id: [u8; 16],
        accepted: bool,
    ) -> LeaseResult {
        let Some(pending) = self.pending.as_mut() else {
            return LeaseResult::Rejected;
        };
        if self.owner != Some(owner) || pending.request_id != request_id {
            return LeaseResult::Rejected;
        }
        pending.accepted = accepted;
        if accepted {
            LeaseResult::AcceptedPending
        } else {
            self.pending = None;
            LeaseResult::Rejected
        }
    }
    pub fn heartbeat(&mut self, owner: [u8; 32], epoch: u32, now_ms: u64) -> LeaseResult {
        if self.owner == Some(owner) && self.epoch == epoch {
            self.last_heartbeat_ms = Some(now_ms);
            LeaseResult::Granted
        } else {
            LeaseResult::Stale
        }
    }
    pub fn release(&mut self, owner: [u8; 32], epoch: u32, _now_ms: u64) -> LeaseResult {
        if self.owner != Some(owner) || self.epoch != epoch {
            return LeaseResult::Stale;
        }
        self.invalidate();
        LeaseResult::Released
    }

    pub fn grant_pending(&mut self, requester_authorized: bool, now_ms: u64) -> LeaseResult {
        if !requester_authorized {
            self.pending = None;
            return LeaseResult::Rejected;
        }
        let Some(pending) = self.pending.take() else {
            return LeaseResult::Rejected;
        };
        self.grant(pending.requester, now_ms);
        LeaseResult::Granted
    }
    pub fn revoke(&mut self, client: [u8; 32]) {
        if self.owner == Some(client) {
            self.invalidate();
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.requester == client)
        {
            self.pending = None;
        }
    }
    pub fn restart(&mut self) {
        if self.owner.is_some() || self.pending.is_some() || self.held != HeldState::default() {
            self.bump();
        }
        self.owner = None;
        self.pending = None;
        self.last_heartbeat_ms = None;
        self.clear_held();
    }
    pub fn tick(&mut self, now_ms: u64) -> LeaseResult {
        if self.owner.is_none() {
            return LeaseResult::Rejected;
        }
        if now_ms.saturating_sub(self.last_heartbeat_ms.unwrap_or(0)) >= HEARTBEAT_EXPIRY_MS {
            self.invalidate();
            return if self.pending.is_some() {
                LeaseResult::Granted
            } else {
                LeaseResult::Expired
            };
        }
        if let Some(pending) = self.pending.as_ref() {
            if now_ms.saturating_sub(pending.requested_at_ms) >= TAKEOVER_WINDOW_MS {
                self.invalidate();
                return LeaseResult::Granted;
            }
        }
        LeaseResult::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(event: Event, epoch: u32, sequence: u64) -> Envelope {
        Envelope {
            message_type: match event {
                Event::LeaseRequest { .. } => LEASE_REQUEST,
                Event::TakeoverRequest { .. } => TAKEOVER_REQUEST,
                Event::TakeoverResponse { .. } => TAKEOVER_RESPONSE,
                Event::LeaseState { .. } => LEASE_STATE,
                Event::Heartbeat => HEARTBEAT,
                Event::TakeoverNotice { .. } => TAKEOVER_NOTICE,
                Event::Error { .. } => ERROR,
                Event::LeaseRelease => LEASE_RELEASE,
                Event::HidBenchArm => HID_BENCH_ARM,
                Event::KeyboardState { .. } => KEYBOARD_STATE,
                Event::MouseButtons { .. } => MOUSE_BUTTONS,
                Event::Wheel { .. } => WHEEL,
                Event::Motion { .. } => MOTION,
            },
            session_id: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            lease_epoch: epoch,
            sequence,
            event,
        }
    }
    #[test]
    fn shared_heartbeat_vector_round_trips() {
        assert_eq!(encode(&env(Event::Heartbeat, 7, 0)), HEARTBEAT_VECTOR);
        assert_eq!(decode(HEARTBEAT_VECTOR).unwrap().event, Event::Heartbeat);
    }

    #[test]
    fn lease_request_negotiates_heartbeat_ack_without_changing_legacy_wire() {
        let legacy = Event::LeaseRequest {
            heartbeat_ack_supported: false,
        };
        assert_eq!(
            decode(&encode(&env(legacy.clone(), 0, 0))).unwrap().event,
            legacy
        );
        let capable = Event::LeaseRequest {
            heartbeat_ack_supported: true,
        };
        assert_eq!(
            decode(&encode(&env(capable.clone(), 0, 0))).unwrap().event,
            capable
        );
        assert_eq!(encode(&env(legacy, 0, 0)).last(), Some(&0xa0));
    }
    #[test]
    fn strict_rejections_cover_canonical_and_unknown_data() {
        let mut bytes = HEARTBEAT_VECTOR.to_vec();
        bytes[2] = 0x18;
        bytes.insert(3, 0x01);
        assert_eq!(decode(&bytes), Err(DecodeError::NonCanonical));
        let mut bytes = HEARTBEAT_VECTOR.to_vec();
        bytes[0] = 0xa8;
        assert_eq!(decode(&bytes), Err(DecodeError::WrongFieldCount));
        let mut bytes = encode(&env(Event::MouseButtons { mask: 1 }, 1, 0));
        *bytes.last_mut().unwrap() = 0x18;
        bytes.push(1);
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn lease_state_can_echo_heartbeat_request_sequence() {
        let event = Event::LeaseState {
            owner: Some([8; 32]),
            state: 1,
            request_sequence: Some(42),
        };
        let decoded = decode(&encode(&env(event.clone(), 7, 3))).unwrap();
        assert_eq!(decoded.event, event);
    }
    #[test]
    fn lease_is_exclusive_and_takeover_bounded() {
        let a = [1; 32];
        let b = [2; 32];
        let mut state = LeaseState::default();
        assert_eq!(state.request(a, 0), LeaseResult::Granted);
        assert_eq!(state.request(b, 1), LeaseResult::Rejected);
        let id = [9; 16];
        assert_eq!(state.takeover(a, b, id, 2), LeaseResult::AcceptedPending);
        assert_eq!(state.takeover(a, [3; 32], [8; 16], 3), LeaseResult::Busy);
        assert_eq!(state.response(a, id, false), LeaseResult::Rejected);
        assert_eq!(state.owner, Some(a));
        assert_eq!(state.takeover(a, b, id, 4), LeaseResult::AcceptedPending);
        assert_eq!(state.tick(10_004), LeaseResult::Granted);
        assert_eq!(state.grant_pending(true, 10_004), LeaseResult::Granted);
        assert_eq!(state.owner, Some(b));
        assert_eq!(state.held, HeldState::default());
    }
    #[test]
    fn heartbeat_expiry_cleans_held_state_and_pending_can_take_over() {
        let a = [1; 32];
        let b = [2; 32];
        let mut state = LeaseState::default();
        state.request(a, 0);
        assert_eq!(state.heartbeat(a, state.epoch, 1), LeaseResult::Granted);
        state.held.buttons = 1;
        state.takeover(a, b, [1; 16], 1);
        assert_eq!(state.tick(10_001), LeaseResult::Granted);
        assert_eq!(state.grant_pending(true, 10_001), LeaseResult::Granted);
        assert_eq!(state.owner, Some(b));
        assert_eq!(state.held, HeldState::default());
    }

    #[test]
    fn release_revoke_restart_and_event_shapes_cleanup() {
        let owner = [1; 32];
        let requester = [2; 32];
        let mut state = LeaseState::default();
        assert_eq!(state.request(owner, 0), LeaseResult::Granted);
        assert_eq!(state.release(owner, state.epoch, 1), LeaseResult::Released);
        assert!(state.owner.is_none());
        assert_eq!(state.request(owner, 2), LeaseResult::Granted);
        state.takeover(owner, requester, [3; 16], 3);
        state.revoke(requester);
        assert!(state.pending.is_none());
        state.held.buttons = 1;
        state.restart();
        assert!(state.owner.is_none());
        assert_eq!(state.held, HeldState::default());

        for event in [
            Event::KeyboardState {
                modifiers: 4,
                keys: vec![(1, 2), (7, 4)],
            },
            Event::MouseButtons { mask: 0x1f },
            Event::Wheel {
                vertical: -2,
                horizontal: 3,
            },
            Event::Motion { dx: -12, dy: 34 },
            Event::TakeoverRequest {
                request_id: [4; 16],
            },
            Event::TakeoverResponse {
                request_id: [4; 16],
                accepted: true,
            },
            Event::LeaseState {
                owner: None,
                state: 0,
                request_sequence: None,
            },
            Event::TakeoverNotice {
                request_id: [5; 16],
                requester: [6; 32],
                response_window_ms: 30_000,
            },
            Event::Error {
                code: "busy".into(),
            },
        ] {
            let message_type = match &event {
                Event::KeyboardState { .. } => KEYBOARD_STATE,
                Event::MouseButtons { .. } => MOUSE_BUTTONS,
                Event::Wheel { .. } => WHEEL,
                Event::Motion { .. } => MOTION,
                Event::TakeoverRequest { .. } => TAKEOVER_REQUEST,
                Event::TakeoverResponse { .. } => TAKEOVER_RESPONSE,
                Event::LeaseState { .. } => LEASE_STATE,
                Event::TakeoverNotice { .. } => TAKEOVER_NOTICE,
                Event::Error { .. } => ERROR,
                _ => unreachable!(),
            };
            let encoded = encode(&env(event, 1, 0));
            assert_eq!(decode(&encoded).unwrap().message_type, message_type);
        }
    }

    #[test]
    fn scripted_two_enrolled_identity_takeover_scenario() {
        let enrolled_a = [0xa1; 32];
        let enrolled_b = [0xb2; 32];
        let mut state = LeaseState::default();

        assert_eq!(state.request(enrolled_a, 0), LeaseResult::Granted);
        let epoch_a = state.epoch;
        state.held.keys.push((7, 4));
        state.held.buttons = 1;

        assert_eq!(state.request(enrolled_b, 1), LeaseResult::Rejected);
        let rejected = [9; 16];
        assert_eq!(
            state.takeover(enrolled_a, enrolled_b, rejected, 2),
            LeaseResult::AcceptedPending
        );
        assert_eq!(
            state.takeover(enrolled_a, [0xc3; 32], [8; 16], 3),
            LeaseResult::Busy
        );
        assert_eq!(
            state.response(enrolled_a, rejected, false),
            LeaseResult::Rejected
        );
        assert_eq!(state.owner, Some(enrolled_a));
        assert_eq!(state.held.buttons, 1);

        let early = [10; 16];
        assert_eq!(
            state.takeover(enrolled_a, enrolled_b, early, 4),
            LeaseResult::AcceptedPending
        );
        assert_eq!(state.release(enrolled_a, epoch_a, 5), LeaseResult::Released);
        assert_eq!(state.grant_pending(true, 5), LeaseResult::Granted);
        assert_eq!(state.owner, Some(enrolled_b));
        assert_eq!(state.held, HeldState::default());

        let epoch_b = state.epoch;
        state.held.keys.push((1, 2));
        let expiry_takeover = [11; 16];
        assert_eq!(
            state.takeover(enrolled_b, enrolled_a, expiry_takeover, 6),
            LeaseResult::AcceptedPending
        );
        assert_eq!(state.tick(10_006), LeaseResult::Granted);
        assert_eq!(state.grant_pending(true, 10_006), LeaseResult::Granted);
        assert_eq!(state.owner, Some(enrolled_a));
        assert_eq!(state.held, HeldState::default());

        let old_epoch = state.epoch;
        state.held.buttons = 1;
        assert_eq!(state.tick(20_007), LeaseResult::Expired);
        assert!(state.owner.is_none());
        assert_eq!(state.held, HeldState::default());
        assert_eq!(
            state.heartbeat(enrolled_a, old_epoch, 6_008),
            LeaseResult::Stale
        );

        assert_eq!(state.request(enrolled_a, 7_000), LeaseResult::Granted);
        let timeout_request = [12; 16];
        assert_eq!(
            state.takeover(enrolled_a, enrolled_b, timeout_request, 7_001),
            LeaseResult::AcceptedPending
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 8_999),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 11_998),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 14_997),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 16_996),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 19_995),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 22_994),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 25_993),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 28_992),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 31_991),
            LeaseResult::Granted
        );
        assert_eq!(
            state.heartbeat(enrolled_a, state.epoch, 34_990),
            LeaseResult::Granted
        );
        assert_eq!(state.tick(37_001), LeaseResult::Granted);
        assert_eq!(state.grant_pending(true, 37_001), LeaseResult::Granted);
        assert_eq!(state.owner, Some(enrolled_b));
        assert_eq!(state.held, HeldState::default());

        let revoked_epoch = state.epoch;
        state.held.keys.push((7, 4));
        state.revoke(enrolled_b);
        assert!(state.owner.is_none());
        assert_eq!(state.held, HeldState::default());
        assert_eq!(
            state.heartbeat(enrolled_b, revoked_epoch, 38_000),
            LeaseResult::Stale
        );
        assert_eq!(state.request(enrolled_b, 38_001), LeaseResult::Granted);
        state.held.buttons = 1;
        state.restart();
        assert!(state.owner.is_none());
        assert_eq!(state.held, HeldState::default());
        assert_eq!(
            state.heartbeat(enrolled_b, state.epoch.wrapping_sub(1), 38_002),
            LeaseResult::Stale
        );
        assert!(epoch_b < state.epoch);
    }

    #[test]
    fn pending_requester_race_cannot_grant_after_release_or_expiry() {
        let a = [1; 32];
        let b = [2; 32];
        let mut state = LeaseState::default();
        assert_eq!(state.request(a, 0), LeaseResult::Granted);
        let epoch = state.epoch;
        assert_eq!(
            state.takeover(a, b, [3; 16], 1),
            LeaseResult::AcceptedPending
        );
        assert_eq!(state.release(a, epoch, 2), LeaseResult::Released);
        assert_eq!(state.grant_pending(false, 2), LeaseResult::Rejected);
        assert!(state.owner.is_none() && state.pending.is_none());

        assert_eq!(state.request(a, 3), LeaseResult::Granted);
        assert_eq!(
            state.takeover(a, b, [4; 16], 4),
            LeaseResult::AcceptedPending
        );
        assert_eq!(state.tick(10_004), LeaseResult::Granted);
        assert_eq!(state.grant_pending(false, 10_004), LeaseResult::Rejected);
        assert!(state.owner.is_none() && state.pending.is_none());
    }
}
