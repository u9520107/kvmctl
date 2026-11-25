#![cfg_attr(not(windows), allow(dead_code))]

use std::io::{self, Write};

#[cfg(windows)]
mod credentials;
#[cfg(windows)]
mod discovery;
#[cfg(windows)]
mod flash;
#[cfg(windows)]
mod hid_protocol;
#[cfg(windows)]
mod protocol;
#[cfg(windows)]
mod raw_input;
#[cfg(windows)]
mod signaling;
#[cfg(windows)]
mod targets;
#[cfg(windows)]
mod transport;

const MAX_FRAME: usize = 4096;
const WIFI_SET_COMMAND: u64 = 0x60;
const STATUS_COMMAND: u64 = 0x61;
const ENROLL_COMMAND: u64 = 0x62;
const LIST_CLIENTS_COMMAND: u64 = 0x63;
const REVOKE_CLIENT_COMMAND: u64 = 0x64;
const REVOKE_ALL_COMMAND: u64 = 0x65;
const LED_SET_COMMAND: u64 = 0x66;
const TRANSPORT_OFFER_COMMAND: u64 = 0x67;
const TRANSPORT_CLOSE_COMMAND: u64 = 0x68;
const TRANSPORT_STATUS_COMMAND: u64 = 0x69;

#[derive(Debug)]
struct Status {
    identity_fingerprint: [u8; 32],
    tls_spki_hash: [u8; 32],
    wifi_state: u64,
    firmware_version: String,
    led_supported: bool,
    led_enabled: bool,
    led_brightness: u64,
    led_rgb: [u8; 3],
}

#[derive(Clone, Copy)]
struct TransportMetrics {
    state: u8,
    internal_free_start: u32,
    internal_free_min: u32,
    internal_free_end: u32,
    psram_free_start: u32,
    psram_free_min: u32,
    psram_free_end: u32,
    channel_opens: u32,
    application_bytes_received: u32,
    peer_task_cpu_us: u32,
    tls_diagnostic: [u8; 96],
    tls_diagnostic_len: usize,
    tls_rate_limited: u32,
    board_timestamp_ms: u64,
    lease_epoch: u32,
    last_event_sequence: u64,
    response_queue_depth: u32,
    scripted_events: u32,
    scripted_rejections: u32,
    scripted_drops: u32,
    scripted_reorders: u32,
    normalized_keys: u32,
    normalized_buttons: u32,
    hid_reports: Option<u32>,
    total_soc_cpu_us: u32,
    event_processing_us: u32,
    response_queue_residence_us: u32,
}

impl Default for TransportMetrics {
    fn default() -> Self {
        Self {
            state: 0,
            internal_free_start: 0,
            internal_free_min: 0,
            internal_free_end: 0,
            psram_free_start: 0,
            psram_free_min: 0,
            psram_free_end: 0,
            channel_opens: 0,
            application_bytes_received: 0,
            peer_task_cpu_us: 0,
            tls_diagnostic: [0; 96],
            tls_diagnostic_len: 0,
            tls_rate_limited: 0,
            board_timestamp_ms: 0,
            lease_epoch: 0,
            last_event_sequence: 0,
            response_queue_depth: 0,
            scripted_events: 0,
            scripted_rejections: 0,
            scripted_drops: 0,
            scripted_reorders: 0,
            normalized_keys: 0,
            normalized_buttons: 0,
            hid_reports: None,
            total_soc_cpu_us: 0,
            event_processing_us: 0,
            response_queue_residence_us: 0,
        }
    }
}

struct Secret(Vec<u8>);

impl Secret {
    fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn encode_status_request(request_id: &[u8; 16]) -> Vec<u8> {
    let mut out = vec![0xa5, 0x00, 0x01, 0x01, 0x00, 0x02, 0x18, 0x61, 0x03, 0x50];
    out.extend_from_slice(request_id);
    out.extend_from_slice(&[0x04, 0xa0]);
    out
}

fn put_head(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=255 => out.extend_from_slice(&[prefix | 24, value as u8]),
        256..=65_535 => {
            out.push(prefix | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            out.push(prefix | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(prefix | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn put_uint(out: &mut Vec<u8>, value: u64) {
    put_head(out, 0, value);
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_head(out, 2, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_text(out: &mut Vec<u8>, text: &[u8]) {
    put_head(out, 3, text.len() as u64);
    out.extend_from_slice(text);
}

fn encode_request(command: u64, request_id: &[u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 32);
    out.push(0xa5);
    put_uint(&mut out, 0);
    put_uint(&mut out, 1);
    put_uint(&mut out, 1);
    put_uint(&mut out, 0);
    put_uint(&mut out, 2);
    put_uint(&mut out, command);
    put_uint(&mut out, 3);
    put_bytes(&mut out, request_id);
    put_uint(&mut out, 4);
    out.extend_from_slice(payload);
    out
}

fn encode_wifi_request(
    request_id: &[u8; 16],
    ssid: &[u8],
    passphrase: &[u8],
) -> Result<Vec<u8>, String> {
    if ssid.is_empty() || ssid.len() > 32 || std::str::from_utf8(ssid).is_err() {
        return Err("SSID must be 1–32 UTF-8 bytes".into());
    }
    if !valid_passphrase(passphrase) {
        return Err("passphrase must be 8–63 printable ASCII bytes or 64 hexadecimal bytes".into());
    }
    let mut payload = vec![0xa2, 0x00];
    put_text(&mut payload, ssid);
    put_uint(&mut payload, 1);
    put_bytes(&mut payload, passphrase);
    Ok(encode_request(WIFI_SET_COMMAND, request_id, &payload))
}

fn valid_passphrase(passphrase: &[u8]) -> bool {
    (8..=63).contains(&passphrase.len()) && passphrase.iter().all(|b| (0x20..=0x7e).contains(b))
        || passphrase.len() == 64 && passphrase.iter().all(u8::is_ascii_hexdigit)
}

fn encode_led_request(
    request_id: &[u8; 16],
    enabled: bool,
    brightness: u8,
    rgb: [u8; 3],
) -> Result<Vec<u8>, String> {
    if brightness > 100 {
        return Err("brightness must be 0–100".into());
    }
    let mut payload = vec![0xa3, 0x00, if enabled { 0xf5 } else { 0xf4 }, 0x01];
    put_uint(&mut payload, u64::from(brightness));
    payload.extend_from_slice(&[0x02, 0x43, rgb[0], rgb[1], rgb[2]]);
    Ok(encode_request(LED_SET_COMMAND, request_id, &payload))
}

fn encode_enroll_request(request_id: &[u8; 16], spki: &[u8]) -> Result<Vec<u8>, String> {
    if spki.len() != 91 {
        return Err("client public key must be P-256 SPKI DER".into());
    }
    let mut payload = vec![0xa1, 0x00];
    put_bytes(&mut payload, spki);
    Ok(encode_request(ENROLL_COMMAND, request_id, &payload))
}

fn encode_list_clients_request(request_id: &[u8; 16]) -> Vec<u8> {
    encode_request(LIST_CLIENTS_COMMAND, request_id, &[0xa0])
}

fn encode_revoke_client_request(request_id: &[u8; 16], fingerprint: &[u8; 32]) -> Vec<u8> {
    let mut payload = vec![0xa1, 0x00];
    put_bytes(&mut payload, fingerprint);
    encode_request(REVOKE_CLIENT_COMMAND, request_id, &payload)
}

fn encode_revoke_all_request(request_id: &[u8; 16]) -> Vec<u8> {
    encode_request(REVOKE_ALL_COMMAND, request_id, &[0xa0])
}

fn encode_transport_offer_request(request_id: &[u8; 16], sdp: &str) -> Result<Vec<u8>, String> {
    if sdp.is_empty() || sdp.len() > 3500 || !sdp.is_ascii() {
        return Err("SDP is empty, oversized, or non-ASCII".into());
    }
    let mut payload = vec![0xa1, 0x00];
    put_text(&mut payload, sdp.as_bytes());
    let request = encode_request(TRANSPORT_OFFER_COMMAND, request_id, &payload);
    if request.len() > MAX_FRAME {
        return Err("SDP exceeds USB signaling frame limit".into());
    }
    Ok(request)
}

fn decode_transport_offer_response(data: &[u8], request_id: &[u8; 16]) -> Result<String, String> {
    let mut r = Reader::new(data);
    let (major, map_len) = r.head()?;
    if major != 5 || !(map_len == 5 || map_len == 6) {
        return Err("invalid transport offer response".into());
    }
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(TRANSPORT_OFFER_COMMAND)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let code = r.uint()?;
    if code != 0 {
        if map_len != 5 || !r.done() {
            return Err("invalid transport offer error response".into());
        }
        if code == 1 {
            return Err("device rejected malformed SDP or certificate fingerprint identity".into());
        }
        if code == 3 {
            return Err("device is busy with another transport session".into());
        }
        return Err(format!("device rejected SDP offer with code {code}"));
    }
    if map_len != 6 {
        return Err("successful transport offer omitted answer".into());
    }
    r.expect_uint(5)?;
    let answer = r.long_text(3500)?;
    if !r.done() {
        return Err("trailing CBOR data".into());
    }
    Ok(answer)
}

fn decode_transport_status_response(
    data: &[u8],
    request_id: &[u8; 16],
) -> Result<TransportMetrics, String> {
    let mut r = Reader::new(data);
    r.expect_map(6)?;
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(TRANSPORT_STATUS_COMMAND)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let code = r.uint()?;
    if code != 0 {
        return Err(format!(
            "device rejected transport status request with code {code}"
        ));
    }
    r.expect_uint(5)?;
    let (result_major, result_fields) = r.head()?;
    if result_major != 5 || !matches!(result_fields, 12 | 22 | 23 | 25 | 26) {
        return Err("invalid transport telemetry field count".into());
    }
    let mut values = [0; 10];
    for (key, value) in values.iter_mut().enumerate() {
        r.expect_uint(key as u64)?;
        *value = r.uint()?;
    }
    r.expect_uint(10)?;
    let diagnostic = r.text()?;
    r.expect_uint(11)?;
    let tls_rate_limited = r.uint()?;
    let mut telemetry = [0u64; 10];
    if result_fields >= 22 {
        for (index, value) in telemetry.iter_mut().enumerate() {
            r.expect_uint((index + 12) as u64)?;
            *value = r.uint()?;
        }
    }
    let hid_reports = if matches!(result_fields, 23 | 26) {
        r.expect_uint(22)?;
        Some(u32::try_from(r.uint()?).map_err(|_| "invalid HID report count")?)
    } else {
        None
    };
    let (total_soc_cpu_us, event_processing_us, response_queue_residence_us) = if result_fields
        == 25
        || result_fields == 26
    {
        r.expect_uint(23)?;
        let cpu = u32::try_from(r.uint()?).map_err(|_| "invalid SoC CPU time")?;
        r.expect_uint(24)?;
        let processing = u32::try_from(r.uint()?).map_err(|_| "invalid event processing time")?;
        r.expect_uint(25)?;
        let residence = u32::try_from(r.uint()?).map_err(|_| "invalid response queue residence")?;
        (cpu, processing, residence)
    } else {
        (0, 0, 0)
    };
    if values[0] > 4
        || values[1..].iter().any(|value| *value > u32::MAX as u64)
        || diagnostic.len() > 96
        || tls_rate_limited > u32::MAX as u64
        || telemetry[1] > u32::MAX as u64
        || telemetry[3..].iter().any(|value| *value > u32::MAX as u64)
        || !r.done()
    {
        return Err("invalid transport status response".into());
    }
    let mut tls_diagnostic = [0; 96];
    tls_diagnostic[..diagnostic.len()].copy_from_slice(diagnostic.as_bytes());
    Ok(TransportMetrics {
        state: values[0] as u8,
        internal_free_start: values[1] as u32,
        internal_free_min: values[2] as u32,
        internal_free_end: values[3] as u32,
        psram_free_start: values[4] as u32,
        psram_free_min: values[5] as u32,
        psram_free_end: values[6] as u32,
        channel_opens: values[7] as u32,
        application_bytes_received: values[8] as u32,
        peer_task_cpu_us: values[9] as u32,
        tls_diagnostic,
        tls_diagnostic_len: diagnostic.len(),
        tls_rate_limited: tls_rate_limited as u32,
        board_timestamp_ms: telemetry[0],
        lease_epoch: telemetry[1] as u32,
        last_event_sequence: telemetry[2],
        response_queue_depth: telemetry[3] as u32,
        scripted_events: telemetry[4] as u32,
        scripted_rejections: telemetry[5] as u32,
        scripted_drops: telemetry[6] as u32,
        scripted_reorders: telemetry[7] as u32,
        normalized_keys: telemetry[8] as u32,
        normalized_buttons: telemetry[9] as u32,
        hid_reports,
        total_soc_cpu_us,
        event_processing_us,
        response_queue_residence_us,
    })
}

fn parse_rgb(color: &str) -> Result<[u8; 3], String> {
    let color = color.strip_prefix('#').unwrap_or(color);
    if color.len() != 6 || !color.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("color must be six hexadecimal digits (RRGGBB)".into());
    }
    Ok([
        u8::from_str_radix(&color[0..2], 16).unwrap(),
        u8::from_str_radix(&color[2..4], 16).unwrap(),
        u8::from_str_radix(&color[4..6], 16).unwrap(),
    ])
}

#[derive(Debug)]
struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn head(&mut self) -> Result<(u8, u64), String> {
        let initial = *self.data.get(self.offset).ok_or("truncated CBOR")?;
        self.offset += 1;
        let major = initial >> 5;
        let ai = initial & 31;
        let (value, min) = match ai {
            0..=23 => (u64::from(ai), 0),
            24 => (u64::from(self.take_u8()?), 24),
            25 => (u64::from(self.take_u16()?), 256),
            26 => (u64::from(self.take_u32()?), 65_536),
            27 => (self.take_u64()?, 4_294_967_296),
            _ => return Err("indefinite/reserved CBOR item".into()),
        };
        if ai >= 24 && value < min {
            return Err("non-shortest CBOR integer".into());
        }
        Ok((major, value))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(len).ok_or("length overflow")?;
        let value = self.data.get(self.offset..end).ok_or("truncated CBOR")?;
        self.offset = end;
        Ok(value)
    }

    fn take_u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn take_u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn take_u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn expect_uint(&mut self, expected: u64) -> Result<(), String> {
        let (major, value) = self.head()?;
        if major != 0 || value != expected {
            return Err(format!("expected unsigned integer {expected}"));
        }
        Ok(())
    }

    fn uint(&mut self) -> Result<u64, String> {
        let (major, value) = self.head()?;
        if major != 0 {
            return Err("expected unsigned integer".into());
        }
        Ok(value)
    }

    fn boolean(&mut self) -> Result<bool, String> {
        let (major, value) = self.head()?;
        if major != 7 || (value != 20 && value != 21) {
            return Err("expected boolean".into());
        }
        Ok(value == 21)
    }

    fn expect_bytes(&mut self, len: usize) -> Result<&'a [u8], String> {
        let (major, actual) = self.head()?;
        if major != 2 || actual != len as u64 {
            return Err(format!("expected {len}-byte string"));
        }
        self.take(len)
    }

    fn bytes(&mut self, max_len: usize) -> Result<&'a [u8], String> {
        let (major, len) = self.head()?;
        if major != 2 || len > max_len as u64 {
            return Err("invalid byte string".into());
        }
        self.take(len as usize)
    }

    fn text(&mut self) -> Result<String, String> {
        let (major, len) = self.head()?;
        if major != 3 || len > 128 {
            return Err("expected short UTF-8 text".into());
        }
        std::str::from_utf8(self.take(len as usize)?)
            .map(str::to_owned)
            .map_err(|_| "invalid UTF-8 text".into())
    }

    fn long_text(&mut self, max_len: usize) -> Result<String, String> {
        let (major, len) = self.head()?;
        if major != 3 || len > max_len as u64 {
            return Err("invalid UTF-8 text length".into());
        }
        std::str::from_utf8(self.take(len as usize)?)
            .map(str::to_owned)
            .map_err(|_| "invalid UTF-8 text".into())
    }

    fn expect_map(&mut self, len: u64) -> Result<(), String> {
        let (major, actual) = self.head()?;
        if major != 5 || actual != len {
            return Err(format!("expected map of {len} entries"));
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.offset == self.data.len()
    }
}

fn decode_status_response(data: &[u8], request_id: &[u8; 16]) -> Result<Status, String> {
    if data.is_empty() || data.len() > MAX_FRAME {
        return Err("invalid response frame size".into());
    }
    let mut r = Reader::new(data);
    r.expect_map(6)?;
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(STATUS_COMMAND)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let status_code = r.uint()?;
    if status_code != 0 {
        return Err(format!(
            "device rejected status request with code {status_code}"
        ));
    }
    r.expect_uint(5)?;
    r.expect_map(6)?;
    r.expect_uint(0)?;
    let identity_fingerprint: [u8; 32] = r.expect_bytes(32)?.try_into().unwrap();
    r.expect_uint(1)?;
    let tls_spki_hash: [u8; 32] = r.expect_bytes(32)?.try_into().unwrap();
    r.expect_uint(2)?;
    let wifi_state = r.uint()?;
    if wifi_state > 3 {
        return Err("unknown Wi-Fi state".into());
    }
    r.expect_uint(3)?;
    let firmware_version = r.text()?;
    r.expect_uint(4)?;
    let identity_der = r.bytes(128)?;
    if identity_der.is_empty() {
        return Err("empty endpoint public key".into());
    }
    r.expect_uint(5)?;
    r.expect_map(4)?;
    r.expect_uint(0)?;
    let led_supported = r.boolean()?;
    r.expect_uint(1)?;
    let led_enabled = r.boolean()?;
    r.expect_uint(2)?;
    let led_brightness = r.uint()?;
    if led_brightness > 100 {
        return Err("invalid LED brightness".into());
    }
    r.expect_uint(3)?;
    let led_rgb: [u8; 3] = r.expect_bytes(3)?.try_into().unwrap();
    if !r.done() {
        return Err("trailing CBOR data".into());
    }
    if sha256(identity_der)? != identity_fingerprint {
        return Err("endpoint identity fingerprint does not match public key".into());
    }
    Ok(Status {
        identity_fingerprint,
        tls_spki_hash,
        wifi_state,
        firmware_version,
        led_supported,
        led_enabled,
        led_brightness,
        led_rgb,
    })
}

fn decode_mutation_response(
    data: &[u8],
    request_id: &[u8; 16],
    command: u64,
) -> Result<(), String> {
    let mut r = Reader::new(data);
    let (major, len) = r.head()?;
    if major != 5 || !(len == 5 || len == 6) {
        return Err("invalid command response".into());
    }
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(command)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let code = r.uint()?;
    if code == 0 {
        if len != 6 {
            return Err("successful response omitted result".into());
        }
        r.expect_uint(5)?;
        r.expect_map(0)?;
        if !r.done() {
            return Err("trailing CBOR data".into());
        }
        Ok(())
    } else {
        if len != 5 || !r.done() {
            return Err("invalid error response".into());
        }
        Err(match code {
            1 => "device rejected invalid settings".into(),
            7 => "device does not support this setting".into(),
            _ => format!("device rejected command with code {code}"),
        })
    }
}

fn decode_enroll_response(data: &[u8], request_id: &[u8; 16]) -> Result<[u8; 32], String> {
    let mut r = Reader::new(data);
    let (major, map_len) = r.head()?;
    if major != 5 || !(map_len == 5 || map_len == 6) {
        return Err("invalid client enrollment response".into());
    }
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(ENROLL_COMMAND)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let code = r.uint()?;
    if code != 0 {
        if map_len != 5 || !r.done() {
            return Err("invalid enrollment error response".into());
        }
        return Err(format!("device rejected enrollment with code {code}"));
    }
    if map_len != 6 {
        return Err("enrollment response omitted client fingerprint".into());
    }
    r.expect_uint(5)?;
    let fingerprint = r.expect_bytes(32)?.try_into().unwrap();
    if !r.done() {
        return Err("trailing CBOR data".into());
    }
    Ok(fingerprint)
}

fn decode_client_list_response(
    data: &[u8],
    request_id: &[u8; 16],
) -> Result<Vec<[u8; 32]>, String> {
    let mut r = Reader::new(data);
    let (major, map_len) = r.head()?;
    if major != 5 || !(map_len == 5 || map_len == 6) {
        return Err("invalid enrolled client list response".into());
    }
    r.expect_uint(0)?;
    r.expect_uint(1)?;
    r.expect_uint(1)?;
    r.expect_uint(0)?;
    r.expect_uint(2)?;
    r.expect_uint(LIST_CLIENTS_COMMAND)?;
    r.expect_uint(3)?;
    if r.expect_bytes(16)? != request_id {
        return Err("response request ID mismatch".into());
    }
    r.expect_uint(4)?;
    let code = r.uint()?;
    if code != 0 {
        if map_len != 5 || !r.done() {
            return Err("invalid client list error response".into());
        }
        return Err(format!("device rejected client list with code {code}"));
    }
    if map_len != 6 {
        return Err("client list response omitted entries".into());
    }
    r.expect_uint(5)?;
    let (major, count) = r.head()?;
    if major != 4 || count > 8 {
        return Err("invalid enrolled client list".into());
    }
    let mut clients = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let fingerprint: [u8; 32] = r.expect_bytes(32)?.try_into().unwrap();
        if clients.contains(&fingerprint) {
            return Err("duplicate enrolled client fingerprint".into());
        }
        clients.push(fingerprint);
    }
    if !r.done() {
        return Err("trailing CBOR data".into());
    }
    Ok(clients)
}

#[cfg(windows)]
fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    use windows_sys::Win32::Security::Cryptography::BCRYPT_SHA256_ALGORITHM;
    bcrypt_hash::<32>(BCRYPT_SHA256_ALGORITHM, data)
}

#[cfg(windows)]
fn md5(data: &[u8]) -> Result<[u8; 16], String> {
    use windows_sys::Win32::Security::Cryptography::BCRYPT_MD5_ALGORITHM;
    bcrypt_hash::<16>(BCRYPT_MD5_ALGORITHM, data)
}

#[cfg(windows)]
fn bcrypt_hash<const N: usize>(
    algorithm_id: windows_sys::core::PCWSTR,
    data: &[u8],
) -> Result<[u8; N], String> {
    use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCryptOpenAlgorithmProvider};
    let mut algorithm = std::ptr::null_mut();
    let open =
        unsafe { BCryptOpenAlgorithmProvider(&mut algorithm, algorithm_id, std::ptr::null(), 0) };
    if open < 0 {
        return Err(format!(
            "BCryptOpenAlgorithmProvider failed: 0x{:08x}",
            open as u32
        ));
    }
    let mut digest = [0u8; N];
    let len = u32::try_from(data.len()).map_err(|_| "input too large")?;
    let digest_len = u32::try_from(N).map_err(|_| "output too large")?;
    let result = unsafe {
        BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            data.as_ptr(),
            len,
            digest.as_mut_ptr(),
            digest_len,
        )
    };
    unsafe {
        windows_sys::Win32::Security::Cryptography::BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    if result < 0 {
        return Err(format!("BCryptHash failed: 0x{:08x}", result as u32));
    }
    Ok(digest)
}

#[cfg(not(windows))]
fn sha256(_: &[u8]) -> Result<[u8; 32], String> {
    Err("SHA-256 is provided by Windows CNG".into())
}

#[cfg(windows)]
fn random_id() -> Result<[u8; 16], String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let mut id = [0u8; 16];
    let result = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            id.as_mut_ptr(),
            16,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if result < 0 {
        return Err(format!("BCryptGenRandom failed: 0x{:08x}", result as u32));
    }
    Ok(id)
}

#[cfg(windows)]
mod serial {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use windows_sys::Win32::Devices::Communication::{
        COMMTIMEOUTS, DCB, GetCommState, NOPARITY, ONESTOPBIT, PURGE_RXCLEAR, PURGE_TXCLEAR,
        PurgeComm, SetCommState, SetCommTimeouts,
    };
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA, SPDRP_FRIENDLYNAME,
        SPDRP_HARDWAREID, SPDRP_LOCATION_INFORMATION, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
        SetupDiGetDeviceRegistryPropertyW,
    };
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Registry::{REG_MULTI_SZ, REG_SZ};

    const SUPPORTED_UART_ID: &str = "VID_1A86&PID_55D3";

    #[derive(Clone)]
    pub struct PortInfo {
        pub name: String,
        pub label: String,
        pub instance_id: Option<String>,
        pub recognized: bool,
    }

    pub fn automatic_selection(ports: &[PortInfo]) -> Option<usize> {
        (ports.len() == 1 && ports[0].recognized).then_some(0)
    }

    pub(crate) struct Port(HANDLE);

    // Port ownership moves to one signaling thread; the handle is never shared.
    unsafe impl Send for Port {}

    impl Drop for Port {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }

    fn win_error(context: &str) -> String {
        format!("{context} (Windows error {})", unsafe { GetLastError() })
    }

    fn device_property(
        devices: HDEVINFO,
        info: &SP_DEVINFO_DATA,
        property: u32,
    ) -> Option<(u32, Vec<u16>)> {
        let mut data = vec![0u16; 1024];
        let mut value_type = 0u32;
        let mut required = 0u32;
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices,
                info,
                property,
                &mut value_type,
                data.as_mut_ptr().cast(),
                (data.len() * std::mem::size_of::<u16>()) as u32,
                &mut required,
            )
        };
        if ok == 0 || required % 2 != 0 || required as usize > data.len() * 2 {
            return None;
        }
        data.truncate((required / 2) as usize);
        Some((value_type, data))
    }

    fn property_text(devices: HDEVINFO, info: &SP_DEVINFO_DATA, property: u32) -> Option<String> {
        let (value_type, data) = device_property(devices, info, property)?;
        (value_type == REG_SZ).then(|| {
            String::from_utf16_lossy(&data)
                .trim_end_matches('\0')
                .to_owned()
        })
    }

    fn port_name(friendly_name: &str) -> Option<String> {
        let start = friendly_name.rfind("(COM")? + 1;
        let end = friendly_name[start..].find(')')? + start;
        let name = &friendly_name[start..end];
        (name.len() > 3 && name.starts_with("COM") && name[3..].bytes().all(|b| b.is_ascii_digit()))
            .then(|| name.to_owned())
    }

    fn instance_id(devices: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
        let mut data = [0u16; 512];
        let mut required = 0u32;
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(
                devices,
                info,
                data.as_mut_ptr(),
                data.len() as u32,
                &mut required,
            )
        };
        if ok == 0 || required == 0 || required as usize > data.len() {
            return None;
        }
        let length = data
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(required as usize);
        Some(String::from_utf16_lossy(&data[..length]))
    }

    fn board_map_path() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("kvmctl").join("boards.tsv"))
    }

    struct KnownBoard {
        fingerprint: String,
        last_ipv4: Option<std::net::Ipv4Addr>,
    }

    fn load_board_identities() -> BTreeMap<String, KnownBoard> {
        let Some(path) = board_map_path() else {
            return BTreeMap::new();
        };
        let Ok(contents) = fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        contents
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let instance_id = fields.next()?;
                let fingerprint = fields.next()?;
                let last_ipv4 = fields.next().and_then(|address| address.parse().ok());
                if fields.next().is_some() {
                    return None;
                }
                (fingerprint.len() == 64
                    && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| {
                    (
                        instance_id.to_owned(),
                        KnownBoard {
                            fingerprint: fingerprint.to_owned(),
                            last_ipv4,
                        },
                    )
                })
            })
            .collect()
    }

    fn save_board(
        info: &PortInfo,
        fingerprint: &[u8; 32],
        last_ipv4: Option<std::net::Ipv4Addr>,
    ) -> Result<(), String> {
        let (Some(path), Some(instance_id)) = (board_map_path(), info.instance_id.as_ref()) else {
            return Err(
                "cannot save board map: LOCALAPPDATA or device instance ID unavailable".into(),
            );
        };
        if instance_id.chars().any(char::is_control) {
            return Err(
                "cannot save board map: device instance ID contains control characters".into(),
            );
        }
        let mut identities = load_board_identities();
        let fingerprint = super::hex(fingerprint);
        let board = identities
            .entry(instance_id.clone())
            .or_insert_with(|| KnownBoard {
                fingerprint: fingerprint.clone(),
                last_ipv4: None,
            });
        if !board.fingerprint.eq_ignore_ascii_case(&fingerprint) {
            board.last_ipv4 = None;
        }
        board.fingerprint = fingerprint;
        if last_ipv4.is_some() {
            board.last_ipv4 = last_ipv4;
        }
        let contents = identities
            .iter()
            .map(|(id, board)| match board.last_ipv4 {
                Some(address) => format!("{id}\t{}\t{address}\n", board.fingerprint),
                None => format!("{id}\t{}\n", board.fingerprint),
            })
            .collect::<String>();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create board-map directory: {error}"))?;
        }
        fs::write(path, contents).map_err(|error| format!("cannot write board map: {error}"))
    }

    pub fn remember_board(info: &PortInfo, fingerprint: &[u8; 32]) {
        let _ = save_board(info, fingerprint, None);
    }

    pub fn remember_board_ip(
        info: &PortInfo,
        fingerprint: &[u8; 32],
        address: std::net::Ipv4Addr,
    ) -> Result<(), String> {
        save_board(info, fingerprint, Some(address))
    }

    pub fn last_known_ipv4(info: &PortInfo, fingerprint: &[u8; 32]) -> Option<std::net::Ipv4Addr> {
        let instance_id = info.instance_id.as_ref()?;
        if instance_id.chars().any(char::is_control) {
            return None;
        }
        let identities = load_board_identities();
        let board = identities.get(instance_id)?;
        board
            .fingerprint
            .eq_ignore_ascii_case(&super::hex(fingerprint))
            .then_some(board.last_ipv4)
            .flatten()
    }

    pub fn enumerate() -> Result<Vec<PortInfo>, String> {
        let devices = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_ALLCLASSES | DIGCF_PRESENT,
            )
        };
        if devices == -1 {
            return Err(format!(
                "cannot enumerate Windows devices (error {})",
                unsafe { GetLastError() }
            ));
        }

        let known_identities = load_board_identities();
        let mut ports = Vec::new();
        for index in 0u32.. {
            let mut info: SP_DEVINFO_DATA = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if unsafe { SetupDiEnumDeviceInfo(devices, index, &mut info) } == 0 {
                break;
            }
            let Some((REG_MULTI_SZ, hardware_ids)) =
                device_property(devices, &info, SPDRP_HARDWAREID)
            else {
                continue;
            };
            if !String::from_utf16_lossy(&hardware_ids)
                .to_ascii_uppercase()
                .contains(SUPPORTED_UART_ID)
            {
                continue;
            }
            let Some(friendly_name) = property_text(devices, &info, SPDRP_FRIENDLYNAME) else {
                continue;
            };
            let Some(name) = port_name(&friendly_name) else {
                continue;
            };
            let instance_id = instance_id(devices, &info);
            let known_identity = instance_id
                .as_ref()
                .and_then(|id| known_identities.get(id))
                .filter(|board| board.fingerprint.len() >= 8);
            let location = property_text(devices, &info, SPDRP_LOCATION_INFORMATION)
                .filter(|value| !value.is_empty());
            let board_name = known_identity
                .map(|board| format!("kvmctl {} — {friendly_name}", &board.fingerprint[..8]))
                .unwrap_or_else(|| format!("Unknown USB-UART device — {friendly_name}"));
            let label = location
                .map(|location| format!("{board_name} — {location}"))
                .unwrap_or(board_name);
            ports.push(PortInfo {
                name,
                label,
                instance_id,
                recognized: known_identity.is_some(),
            });
        }
        unsafe { SetupDiDestroyDeviceInfoList(devices) };
        ports.sort_by_key(|port| port.name[3..].parse::<u32>().unwrap_or(u32::MAX));
        Ok(ports)
    }

    impl Port {
        pub fn open(name: &str) -> Result<Self, String> {
            if !name.starts_with("COM") || !name[3..].bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid COM port".into());
            }
            let path: Vec<u16> = OsStr::new(&format!(r"\\.\{name}"))
                .encode_wide()
                .chain(Some(0))
                .collect();
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(win_error("cannot open serial port"));
            }
            let port = Self(handle);
            let mut dcb: DCB = unsafe { std::mem::zeroed() };
            dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
            if unsafe { GetCommState(handle, &mut dcb) } == 0 {
                return Err(win_error("cannot read serial settings"));
            }
            dcb.BaudRate = 115200;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            // Binary, no parity/flow control, DTR and RTS disabled (avoids reset pulses).
            dcb._bitfield = 1;
            if unsafe { SetCommState(handle, &dcb) } == 0 {
                return Err(win_error("cannot apply serial settings"));
            }
            let timeouts = COMMTIMEOUTS {
                ReadIntervalTimeout: 10,
                ReadTotalTimeoutMultiplier: 0,
                ReadTotalTimeoutConstant: 2000,
                WriteTotalTimeoutMultiplier: 10,
                WriteTotalTimeoutConstant: 2000,
            };
            if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
                return Err(win_error("cannot set serial timeouts"));
            }
            unsafe {
                PurgeComm(handle, PURGE_RXCLEAR | PURGE_TXCLEAR);
            }
            Ok(port)
        }

        fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), String> {
            while !bytes.is_empty() {
                let mut written = 0;
                let ok = unsafe {
                    WriteFile(
                        self.0,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || written == 0 {
                    return Err(win_error("serial write failed"));
                }
                bytes = &bytes[written as usize..];
            }
            Ok(())
        }

        fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), String> {
            let mut offset = 0;
            while offset < bytes.len() {
                let mut read = 0;
                let ok = unsafe {
                    ReadFile(
                        self.0,
                        bytes[offset..].as_mut_ptr(),
                        (bytes.len() - offset) as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(win_error("serial read failed"));
                }
                if read == 0 {
                    return Err("device did not return a complete status response".into());
                }
                offset += read as usize;
            }
            Ok(())
        }

        pub fn status(&mut self) -> Result<Status, String> {
            let mut last_error = String::new();
            // UART0 may still be flushing reset logs; retry only this read-only query.
            for attempt in 0..3 {
                let id = random_id()?;
                match self
                    .exchange(&encode_status_request(&id))
                    .and_then(|response| decode_status_response(&response, &id))
                {
                    Ok(status) => return Ok(status),
                    Err(error)
                        if matches!(
                            error.as_str(),
                            "invalid response frame length"
                                | "device did not return a complete status response"
                                | "response request ID mismatch"
                        ) =>
                    {
                        last_error = error;
                        if attempt < 2 {
                            unsafe {
                                PurgeComm(self.0, PURGE_RXCLEAR | PURGE_TXCLEAR);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(format!(
                "device status unavailable after serial startup retries: {last_error}"
            ))
        }

        pub fn mutate(
            &mut self,
            payload: &[u8],
            request_id: &[u8; 16],
            command: u64,
        ) -> Result<(), String> {
            let response = self.exchange(payload)?;
            decode_mutation_response(&response, request_id, command)
        }

        pub fn enroll_client(&mut self, spki: &[u8]) -> Result<[u8; 32], String> {
            let id = random_id()?;
            let request = encode_enroll_request(&id, spki)?;
            decode_enroll_response(&self.exchange(&request)?, &id)
        }

        pub fn list_clients(&mut self) -> Result<Vec<[u8; 32]>, String> {
            let id = random_id()?;
            decode_client_list_response(&self.exchange(&encode_list_clients_request(&id))?, &id)
        }

        pub fn reconcile_clients(&mut self) -> Result<Vec<[u8; 32]>, String> {
            let mut last_error = String::new();
            for attempt in 0..3 {
                if attempt != 0 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                unsafe {
                    PurgeComm(self.0, PURGE_RXCLEAR);
                }
                match self.list_clients() {
                    Ok(clients) => return Ok(clients),
                    Err(error) => last_error = error,
                }
            }
            Err(format!(
                "client list unavailable after enrollment reconciliation retries: {last_error}"
            ))
        }

        pub fn revoke_client(&mut self, fingerprint: &[u8; 32]) -> Result<(), String> {
            let id = random_id()?;
            let response = self.exchange(&encode_revoke_client_request(&id, fingerprint))?;
            decode_mutation_response(&response, &id, REVOKE_CLIENT_COMMAND)
        }

        pub fn revoke_all(&mut self) -> Result<(), String> {
            let id = random_id()?;
            let response = self.exchange(&encode_revoke_all_request(&id))?;
            decode_mutation_response(&response, &id, REVOKE_ALL_COMMAND)
        }

        pub(crate) fn transport_offer(&mut self, sdp: &str) -> Result<String, String> {
            let id = random_id()?;
            let payload = encode_transport_offer_request(&id, sdp)?;
            let response = self.exchange(&payload)?;
            decode_transport_offer_response(&response, &id)
        }

        pub(crate) fn transport_close(&mut self) -> Result<(), String> {
            let id = random_id()?;
            let request = encode_request(TRANSPORT_CLOSE_COMMAND, &id, &[0xa0]);
            let response = self.exchange(&request)?;
            decode_mutation_response(&response, &id, TRANSPORT_CLOSE_COMMAND)
        }

        pub(crate) fn transport_status(&mut self) -> Result<TransportMetrics, String> {
            let id = random_id()?;
            let response =
                self.exchange(&encode_request(TRANSPORT_STATUS_COMMAND, &id, &[0xa0]))?;
            decode_transport_status_response(&response, &id)
        }

        fn exchange(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
            if payload.len() > MAX_FRAME {
                return Err("request too large".into());
            }
            self.write_all(&(payload.len() as u32).to_be_bytes())?;
            self.write_all(&payload)?;
            let mut prefix = [0u8; 4];
            self.read_exact(&mut prefix)?;
            let length = u32::from_be_bytes(prefix) as usize;
            if length == 0 || length > MAX_FRAME {
                return Err("invalid response frame length".into());
            }
            let mut response = vec![0u8; length];
            self.read_exact(&mut response)?;
            Ok(response)
        }
    }
}

#[cfg(windows)]
mod ui;

#[cfg(windows)]
mod wifi_networks {
    use std::collections::BTreeSet;
    use std::ffi::c_void;
    use std::ptr;
    use std::slice;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::NetworkManagement::WiFi::{
        L2_NOTIFICATION_DATA, WLAN_AVAILABLE_NETWORK_LIST, WLAN_INTERFACE_INFO_LIST,
        WLAN_NOTIFICATION_SOURCE_ACM, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
        WlanGetAvailableNetworkList, WlanOpenHandle, WlanRegisterNotification, WlanScan,
        wlan_notification_acm_scan_complete, wlan_notification_acm_scan_fail,
    };
    use windows_sys::core::GUID;

    #[derive(Clone, Copy)]
    struct ScanEvent {
        interface: GUID,
        code: u32,
    }

    fn same_guid(left: &GUID, right: &GUID) -> bool {
        left.data1 == right.data1
            && left.data2 == right.data2
            && left.data3 == right.data3
            && left.data4 == right.data4
    }

    unsafe extern "system" fn scan_notification(
        data: *mut L2_NOTIFICATION_DATA,
        context: *mut c_void,
    ) {
        if data.is_null() || context.is_null() {
            return;
        }
        let data = unsafe { &*data };
        if data.NotificationSource != WLAN_NOTIFICATION_SOURCE_ACM
            || (data.NotificationCode != wlan_notification_acm_scan_complete as u32
                && data.NotificationCode != wlan_notification_acm_scan_fail as u32)
        {
            return;
        }
        let sender = unsafe { &*context.cast::<Sender<ScanEvent>>() };
        let _ = sender.send(ScanEvent {
            interface: data.InterfaceGuid,
            code: data.NotificationCode,
        });
    }

    struct Notifications(HANDLE);

    impl Drop for Notifications {
        fn drop(&mut self) {
            unsafe {
                WlanRegisterNotification(
                    self.0,
                    0,
                    0,
                    None,
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                );
            }
        }
    }

    fn wait_for_scan(receiver: &Receiver<ScanEvent>, interface: &GUID) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = receiver.recv_timeout(remaining).map_err(|_| {
                "Windows Wi-Fi scan did not finish within 4 seconds; refresh again".to_owned()
            })?;
            if !same_guid(&event.interface, interface) {
                continue;
            }
            return if event.code == wlan_notification_acm_scan_complete as u32 {
                Ok(())
            } else {
                Err("Windows Wi-Fi scan failed; refresh again".into())
            };
        }
    }

    struct Client(HANDLE);

    impl Drop for Client {
        fn drop(&mut self) {
            unsafe { WlanCloseHandle(self.0, ptr::null()) };
        }
    }

    struct WlanMemory(*mut c_void);

    impl Drop for WlanMemory {
        fn drop(&mut self) {
            unsafe { WlanFreeMemory(self.0) };
        }
    }

    pub fn visible_ssids() -> Result<Vec<String>, String> {
        let mut negotiated_version = 0;
        let mut handle = ptr::null_mut();
        let result =
            unsafe { WlanOpenHandle(2, ptr::null(), &mut negotiated_version, &mut handle) };
        if result != 0 {
            return Err(format!("cannot open Windows Wi-Fi API (error {result})"));
        }
        let client = Client(handle);

        let mut raw_interfaces: *mut WLAN_INTERFACE_INFO_LIST = ptr::null_mut();
        let result = unsafe { WlanEnumInterfaces(client.0, ptr::null(), &mut raw_interfaces) };
        if result != 0 {
            return Err(format!(
                "cannot enumerate Wi-Fi interfaces (error {result})"
            ));
        }
        if raw_interfaces.is_null() {
            return Ok(Vec::new());
        }
        let _interface_memory = WlanMemory(raw_interfaces.cast());
        let interface_list = unsafe { &*raw_interfaces };
        let interfaces = unsafe {
            slice::from_raw_parts(
                interface_list.InterfaceInfo.as_ptr(),
                interface_list.dwNumberOfItems as usize,
            )
        };

        let (sender, receiver) = mpsc::channel();
        let result = unsafe {
            WlanRegisterNotification(
                client.0,
                WLAN_NOTIFICATION_SOURCE_ACM,
                0,
                Some(scan_notification),
                (&sender as *const Sender<ScanEvent>).cast(),
                ptr::null(),
                ptr::null_mut(),
            )
        };
        if result != 0 {
            return Err(format!(
                "cannot register Wi-Fi scan notifications (error {result})"
            ));
        }
        let _notifications = Notifications(client.0);

        let mut ssids = BTreeSet::new();
        for interface in interfaces {
            while receiver.try_recv().is_ok() {}
            let result = unsafe {
                WlanScan(
                    client.0,
                    &interface.InterfaceGuid,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                )
            };
            if result != 0 {
                return Err(format!("cannot scan Wi-Fi networks (error {result})"));
            }
            wait_for_scan(&receiver, &interface.InterfaceGuid)?;

            let mut raw_networks: *mut WLAN_AVAILABLE_NETWORK_LIST = ptr::null_mut();
            let result = unsafe {
                WlanGetAvailableNetworkList(
                    client.0,
                    &interface.InterfaceGuid,
                    0,
                    ptr::null(),
                    &mut raw_networks,
                )
            };
            if result != 0 || raw_networks.is_null() {
                continue;
            }
            let _network_memory = WlanMemory(raw_networks.cast());
            let network_list = unsafe { &*raw_networks };
            let networks = unsafe {
                slice::from_raw_parts(
                    network_list.Network.as_ptr(),
                    network_list.dwNumberOfItems as usize,
                )
            };
            for network in networks {
                let ssid = &network.dot11Ssid;
                let length = ssid.uSSIDLength as usize;
                if length == 0 || length > ssid.ucSSID.len() {
                    continue;
                }
                if let Ok(value) = String::from_utf8(ssid.ucSSID[..length].to_vec()) {
                    if !value.chars().any(char::is_control) {
                        ssids.insert(value);
                    }
                }
            }
        }
        Ok(ssids.into_iter().collect())
    }
}

#[cfg(windows)]
fn read_hidden_line(prompt: &str) -> Result<String, String> {
    use std::io::{BufRead, stdin};
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0;
    let console = unsafe { GetConsoleMode(handle, &mut mode) } != 0;
    if console && unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) } == 0 {
        return Err("cannot disable console echo for passphrase input".into());
    }
    let mut line = String::new();
    let read = stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string());
    if console {
        unsafe {
            SetConsoleMode(handle, mode);
        }
    }
    println!();
    read?;
    while line.ends_with('\r') || line.ends_with('\n') {
        line.pop();
    }
    Ok(line)
}

#[cfg(windows)]
fn is_com_port_name(value: &str) -> bool {
    value.len() > 3
        && value.starts_with("COM")
        && value[3..].bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(windows)]
fn select_port(
    ports: &[serial::PortInfo],
    requested: Option<&str>,
) -> Result<serial::PortInfo, String> {
    if let Some(port) = requested {
        if !is_com_port_name(port) {
            return Err("invalid COM port".into());
        }
        return ports
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(port))
            .cloned()
            .ok_or_else(|| "port is not a detected USB-UART candidate".into());
    }
    if ports.is_empty() {
        return Err("no USB-UART candidates found".into());
    }
    if let Some(index) = serial::automatic_selection(ports) {
        return Ok(ports[index].clone());
    }
    for (index, port) in ports.iter().enumerate() {
        println!("{}: {}", index + 1, port.label);
    }
    print!("Select device (unknown devices require confirmation): ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let index = line
        .trim()
        .parse::<usize>()
        .map_err(|_| "enter a listed port number")?;
    ports
        .get(index.checked_sub(1).ok_or("enter a listed port number")?)
        .ok_or("port number out of range".into())
        .cloned()
}

#[cfg(windows)]
fn service_at_ipv4(address: std::net::Ipv4Addr, fingerprint: &[u8; 32]) -> discovery::Service {
    discovery::Service {
        instance: format!("direct IPv4 {address}"),
        hostname: discovery::endpoint_hostname(fingerprint),
        port: 443,
        ipv4: Some(address),
        properties: std::collections::BTreeMap::new(),
    }
}

#[cfg(windows)]
fn saved_status(target: &targets::Target) -> Status {
    Status {
        identity_fingerprint: target.endpoint_fingerprint,
        tls_spki_hash: target.tls_spki_hash,
        wifi_state: 2,
        firmware_version: "saved target".into(),
        led_supported: false,
        led_enabled: false,
        led_brightness: 0,
        led_rgb: [0; 3],
    }
}

#[cfg(windows)]
fn target_service(
    target: &targets::Target,
    manual_address: Option<std::net::Ipv4Addr>,
) -> Result<discovery::Service, String> {
    if let Some(address) = manual_address {
        println!(
            "Using manual IPv4 {address} with pinned certificate name {}.",
            discovery::endpoint_hostname(&target.endpoint_fingerprint)
        );
        return Ok(service_at_ipv4(address, &target.endpoint_fingerprint));
    }
    let services = discovery::browse().map_err(|error| {
        format!("DNS-SD browse failed: {error}; pass an IPv4 address to use manual-IP fallback")
    })?;
    let mut matches = services
        .into_iter()
        .filter(|service| discovery::matches_endpoint(service, &target.endpoint_fingerprint));
    if let Some(service) = matches.next() {
        if matches.next().is_some() {
            return Err("multiple DNS-SD services claim this saved target identity".into());
        }
        return Ok(service);
    }
    if let Some(address) = target.last_ipv4 {
        println!("DNS-SD found no match; using saved last-known IPv4 {address}.");
        return Ok(service_at_ipv4(address, &target.endpoint_fingerprint));
    }
    Err(format!(
        "no DNS-SD service matches saved target identity {}; pass an IPv4 address for manual-IP fallback",
        hex(&target.endpoint_fingerprint)
    ))
}

#[cfg(windows)]
fn run_target_command(args: &[String]) -> Result<(), String> {
    let command = args.first().map(String::as_str).unwrap_or_default();
    if command == "targets" || (command == "target" && args.get(1).is_some_and(|v| v == "list")) {
        let saved = targets::load()?;
        if saved.is_empty() {
            println!("No saved targets.");
        } else {
            for target in saved {
                println!(
                    "{} identity={} client={} address={}",
                    target.name,
                    hex(&target.endpoint_fingerprint),
                    hex(&target.client_fingerprint),
                    target
                        .last_ipv4
                        .map_or_else(|| "discovery".into(), |address| address.to_string())
                );
            }
        }
        return Ok(());
    }
    if command == "target" && args.get(1).is_some_and(|v| v == "remove") {
        let name = args.get(2).ok_or("usage: kvmctl target remove <name>")?;
        if args.len() != 3 {
            return Err("usage: kvmctl target remove <name>".into());
        }
        targets::remove(name)?;
        println!("Removed saved target `{name}`.");
        return Ok(());
    }
    if command == "session" {
        if !(2..=3).contains(&args.len()) {
            return Err("usage: kvmctl session <target> [IPv4]".into());
        }
        let target = targets::find(&args[1])?;
        let credential = credentials::ClientCredential::load_existing(false).map_err(|error| {
            format!("{error}; enroll this Windows user over USB before opening a session")
        })?;
        if credential.fingerprint != target.client_fingerprint {
            return Err(
                "saved target belongs to a different local enrollment; re-save it after USB enrollment"
                    .into(),
            );
        }
        let manual_address = args
            .get(2)
            .map(|value| {
                value
                    .parse::<std::net::Ipv4Addr>()
                    .map_err(|_| "invalid IPv4 address".to_owned())
            })
            .transpose()?;
        let service = target_service(&target, manual_address)?;
        println!(
            "Opening authenticated foreground session for `{}`. Input remains local; physical-input capture is off.",
            target.name
        );
        let status = saved_status(&target);
        let target_for_resolve = target.clone();
        let address = transport::run_foreground_session(&service, status, credential, move || {
            target_service(&target_for_resolve, manual_address)
        })
        .map_err(|error| error.to_string())?;
        if let std::net::SocketAddr::V4(address) = address {
            targets::update_address(&target.name, *address.ip())?;
        }
        println!("Session closed cleanly.");
        return Ok(());
    }
    if command == "target" && args.get(1).is_some_and(|v| v == "save") {
        let tail = args.get(2..).unwrap_or(&[]);
        let (requested_port, fields) = if tail.first().is_some_and(|value| is_com_port_name(value))
        {
            (tail.first().map(String::as_str), &tail[1..])
        } else {
            (None, tail)
        };
        if !(1..=2).contains(&fields.len()) {
            return Err("usage: kvmctl target save [COMx] <name> [IPv4]".into());
        }
        let address = fields
            .get(1)
            .map(|value| {
                value
                    .parse::<std::net::Ipv4Addr>()
                    .map_err(|_| "invalid IPv4 address".to_owned())
            })
            .transpose()?;
        let ports = serial::enumerate()?;
        let selected = select_port(&ports, requested_port)?;
        if !selected.recognized {
            println!(
                "{} has not been identified as a kvmctl board.",
                selected.label
            );
            print!("Confirm this is your flashed board before sending data (yes/no): ");
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(|error| error.to_string())?;
            if !answer.trim().eq_ignore_ascii_case("yes") {
                return Err("device not confirmed; port was not opened".into());
            }
        }
        let mut port = serial::Port::open(&selected.name)?;
        let status = port.status()?;
        serial::remember_board(&selected, &status.identity_fingerprint);
        let credential = credentials::ClientCredential::load_existing(false).map_err(|error| {
            format!(
                "{error}; run `cargo run --locked -p kvmctl -- enroll {}` first",
                selected.name
            )
        })?;
        if !port.list_clients()?.contains(&credential.fingerprint) {
            return Err(format!(
                "this Windows user is not enrolled on the board; run `cargo run --locked -p kvmctl -- enroll {}` first",
                selected.name
            ));
        }
        let target = targets::Target::new(
            fields[0].clone(),
            status.identity_fingerprint,
            status.tls_spki_hash,
            credential.fingerprint,
            address,
        )?;
        targets::save(&target)?;
        println!(
            "Saved target `{}` for endpoint identity {}.",
            target.name,
            hex(&target.endpoint_fingerprint)
        );
        return Ok(());
    }
    Err("usage: kvmctl targets | target list | target save [COMx] <name> [IPv4] | target remove <name> | session <target> [IPv4]".into())
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return ui::run();
    }
    let command = args.first().map(String::as_str).unwrap_or("status");
    if command == "--help" || command == "-h" {
        println!(
            "kvmctl [ports | status [COMx] | telemetry [COMx] | wifi [COMx] | enroll [COMx] [takeover] | clients [COMx] | revoke [COMx] <fingerprint|all> | discover [COMx] | targets | target list | target save [COMx] <name> [IPv4] | target remove <name> | session <target> [IPv4] | signaling-test [COMx] [scenario [IPv4] | IPv4] | led [COMx] on|off <brightness> <RRGGBB> | transport-test [COMx] | flash [COMx] <firmware.bin> <backup.bin>]; signaling-test takeover-request uses takeover identity; KVMCTL_LOG=debug enables diagnostics; log file defaults to %LOCALAPPDATA%\\kvmctl\\kvmctl.log"
        );
        return Ok(());
    }
    if command == "targets" || command == "session" || command == "target" {
        return run_target_command(&args);
    }
    let ports = serial::enumerate()?;
    if command == "ports" {
        for port in ports {
            println!("{}", port.label);
        }
        return Ok(());
    }
    let (requested_port, params) = match command {
        "led" if args.get(1).is_some_and(|arg| is_com_port_name(arg)) => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
        "led" => (None, args.get(1..).unwrap_or(&[])),
        "flash" if args.get(1).is_some_and(|arg| is_com_port_name(arg)) => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
        "revoke" if args.get(1).is_some_and(|arg| is_com_port_name(arg)) => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
        "signaling-test" if args.get(1).is_some_and(|arg| is_com_port_name(arg)) => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
        "signaling-test" => (None, args.get(1..).unwrap_or(&[])),
        "flash" => (None, args.get(1..).unwrap_or(&[])),
        "revoke" => (None, args.get(1..).unwrap_or(&[])),
        "enroll" if args.get(1).is_some_and(|arg| arg == "takeover") => {
            (None, args.get(1..).unwrap_or(&[]))
        }
        _ => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
    };
    if !matches!(
        command,
        "status"
            | "telemetry"
            | "wifi"
            | "enroll"
            | "recover"
            | "clients"
            | "revoke"
            | "discover"
            | "signaling-test"
            | "led"
            | "transport-test"
            | "flash"
    ) {
        return Err("unknown command; use --help".into());
    }
    if command == "flash" && params.len() != 2 {
        return Err("usage: kvmctl flash [COMx] <firmware.bin> <backup.bin>".into());
    }
    if matches!(
        command,
        "status" | "telemetry" | "wifi" | "recover" | "clients" | "discover" | "transport-test"
    ) && !params.is_empty()
    {
        return Err("this command takes only an optional COM port".into());
    }
    if command == "enroll" && !params.is_empty() && params != ["takeover"] {
        return Err("usage: kvmctl enroll [COMx] [takeover]".into());
    }
    if command == "signaling-test"
        && (params.len() > 2
            || params.first().is_some_and(|value| {
                !(params.len() == 1 && value.parse::<std::net::Ipv4Addr>().is_ok())
                    && !matches!(
                        value.as_str(),
                        "success"
                            | "missing"
                            | "invalid"
                            | "replay"
                            | "resources"
                            | "revoke-active"
                            | "scripted"
                            | "takeover"
                            | "takeover-zero"
                            | "takeover-request"
                            | "hid-bench"
                            | "hid-sustain"
                            | "cleanup-disconnect"
                            | "cleanup-expiry"
                            | "cleanup-revoke"
                            | "cleanup-restart"
                            | "cleanup-transfer-failure"
                    )
            })
            || params
                .get(1)
                .is_some_and(|address| address.parse::<std::net::Ipv4Addr>().is_err()))
    {
        return Err("usage: kvmctl signaling-test [COMx] [scenario [IPv4] | IPv4]".into());
    }
    if command == "revoke" && params.len() != 1 {
        return Err("usage: kvmctl revoke [COMx] <fingerprint|all>".into());
    }
    if command == "discover" {
        let endpoint_fingerprint = if let Some(requested_port) = requested_port {
            let selected = select_port(&ports, Some(requested_port))?;
            if !selected.recognized {
                println!(
                    "{} has not been identified as a kvmctl board.",
                    selected.label
                );
                print!("Confirm this is your flashed board before reading status (yes/no): ");
                io::stdout().flush().map_err(|error| error.to_string())?;
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .map_err(|error| error.to_string())?;
                if !answer.trim().eq_ignore_ascii_case("yes") {
                    return Err("device not confirmed; port was not opened".into());
                }
            }
            let mut port = serial::Port::open(&selected.name)?;
            let status = port.status()?;
            serial::remember_board(&selected, &status.identity_fingerprint);
            Some(status.identity_fingerprint)
        } else {
            None
        };
        let services = discovery::browse()?;
        if services.is_empty() {
            println!(
                "Windows DNS-SD found no _kvmctl._tcp.local services within 5 seconds. Check that the PC and board share a LAN that passes multicast DNS (UDP 5353); USB administration remains available."
            );
            return Ok(());
        }
        for service in services {
            let identity = endpoint_fingerprint
                .as_ref()
                .map(|fingerprint| {
                    if discovery::matches_endpoint(&service, fingerprint) {
                        "matches USB identity"
                    } else {
                        "does not match USB identity"
                    }
                })
                .unwrap_or("unverified; TXT data is advisory");
            let address = service
                .ipv4
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "host lookup".into());
            println!(
                "service={:?} host={} address={} port={} fp={} path={} identity={}",
                service.instance,
                service.hostname,
                address,
                service.port,
                service
                    .properties
                    .get("fp")
                    .map(String::as_str)
                    .unwrap_or("missing"),
                service
                    .properties
                    .get("path")
                    .map(String::as_str)
                    .unwrap_or("missing"),
                identity,
            );
        }
        return Ok(());
    }
    let selected = select_port(&ports, requested_port)?;
    if !selected.recognized {
        println!(
            "{} has not been identified as a kvmctl board.",
            selected.label
        );
        print!("Confirm this is your flashed board before sending data (yes/no): ");
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        if !answer.trim().eq_ignore_ascii_case("yes") {
            return Err("device not confirmed; port was not opened".into());
        }
    }
    if command == "flash" {
        return flash::run(
            &selected.name,
            std::path::Path::new(&params[0]),
            std::path::Path::new(&params[1]),
        );
    }
    let mut port = serial::Port::open(&selected.name)?;
    if command == "signaling-test" {
        let status = port.status()?;
        serial::remember_board(&selected, &status.identity_fingerprint);
        if status.wifi_state != 2 {
            return Err(format!(
                "Wi-Fi is {}; connect the board before running signaling-test",
                wifi_name(status.wifi_state)
            ));
        }
        let enrolled = port.list_clients()?;
        let direct_only = params.len() == 1 && params[0].parse::<std::net::Ipv4Addr>().is_ok();
        let scenario = if direct_only {
            "success"
        } else {
            params.first().map(String::as_str).unwrap_or("success")
        };
        let credential = if scenario == "takeover-request" {
            None
        } else {
            let credential =
                credentials::ClientCredential::load_existing(false).map_err(|error| {
                    format!(
                        "{error}; run `cargo run --offline -p kvmctl -- enroll {}` first",
                        selected.name
                    )
                })?;
            if !enrolled.contains(&credential.fingerprint) {
                return Err(format!(
                    "this Windows client is not enrolled; run `cargo run --offline -p kvmctl -- enroll {}` first",
                    selected.name
                ));
            }
            Some(credential)
        };
        let takeover_peer = if matches!(scenario, "takeover" | "takeover-zero" | "takeover-request")
        {
            let credential =
                credentials::ClientCredential::load_existing(true).map_err(|error| {
                    format!(
                        "{error}; run `cargo run --offline -p kvmctl -- enroll {} takeover` first",
                        selected.name
                    )
                })?;
            if !enrolled.contains(&credential.fingerprint) {
                return Err(format!(
                    "takeover peer is not enrolled; run `cargo run --offline -p kvmctl -- enroll {} takeover` first",
                    selected.name
                ));
            }
            Some(credential)
        } else {
            None
        };
        let manual_address = if direct_only {
            params.first().map(String::as_str)
        } else {
            params.get(1).map(String::as_str)
        };
        let service = if let Some(address) = manual_address {
            let ip = address
                .parse::<std::net::Ipv4Addr>()
                .map_err(|_| "invalid IPv4 address".to_owned())?;
            let hostname = discovery::endpoint_hostname(&status.identity_fingerprint);
            println!("Using manual IPv4 {ip} with pinned certificate name {hostname}.");
            service_at_ipv4(ip, &status.identity_fingerprint)
        } else {
            let (services, browse_error) = match discovery::browse() {
                Ok(services) => (services, None),
                Err(error) => {
                    eprintln!("DNS-SD browse failed: {error}");
                    (Vec::new(), Some(error))
                }
            };
            let mut matches = services.into_iter().filter(|service| {
                discovery::matches_endpoint(service, &status.identity_fingerprint)
            });
            if let Some(service) = matches.next() {
                if matches.next().is_some() {
                    return Err("multiple DNS-SD services claim this USB identity; refusing ambiguous discovery".into());
                }
                service
            } else if let Some(ip) =
                serial::last_known_ipv4(&selected, &status.identity_fingerprint)
            {
                print!("DNS-SD did not find this board. Try last-known IPv4 {ip}? (yes/no): ");
                io::stdout().flush().map_err(|error| error.to_string())?;
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .map_err(|error| error.to_string())?;
                if !answer.trim().eq_ignore_ascii_case("yes") {
                    return Err("last-known IPv4 fallback declined".into());
                }
                service_at_ipv4(ip, &status.identity_fingerprint)
            } else {
                let detail = browse_error
                    .map(|error| format!(" ({error})"))
                    .unwrap_or_default();
                return Err(format!(
                    "no DNS-SD service matches COM identity {}{detail}; run `cargo run --offline -p kvmctl -- discover {}` for diagnostics or pass an IPv4 address",
                    hex(&status.identity_fingerprint),
                    selected.name
                ));
            }
        };
        let hid_mode = match scenario {
            "hid-bench" => "scripted HID",
            "hid-sustain" => "sustained HID",
            "takeover" => "physical HID takeover",
            "cleanup-disconnect"
            | "cleanup-expiry"
            | "cleanup-revoke"
            | "cleanup-restart"
            | "cleanup-transfer-failure" => "physical HID cleanup",
            _ => "zero HID",
        };
        println!(
            "Running signaling scenario `{scenario}` in {hid_mode} mode. Physical-input capture is off."
        );
        if scenario == "takeover-request" {
            let requester = takeover_peer
                .as_ref()
                .ok_or_else(|| "takeover requester credential is unavailable".to_owned())?;
            return transport::run_takeover_request(&service, status, requester)
                .map_err(|error| error.to_string());
        }
        let credential =
            credential.ok_or_else(|| "primary credential is unavailable".to_owned())?;
        let endpoint_fingerprint = status.identity_fingerprint;
        let address = transport::run_signaling_test(
            port,
            &service,
            status,
            credential,
            takeover_peer,
            scenario,
        )
        .map_err(|error| error.to_string())?;
        if let std::net::SocketAddr::V4(address) = address {
            let address = *address.ip();
            serial::remember_board_ip(&selected, &endpoint_fingerprint, address).map_err(
                |error| format!("signaling succeeded, but board-map persistence failed: {error}"),
            )?;
            println!("Remembered last-known IPv4 {address}.");
        }
        return Ok(());
    }
    if command == "recover" {
        let before = port.status()?;
        serial::remember_board(&selected, &before.identity_fingerprint);
        let enrolled = port.list_clients()?;
        println!(
            "Direct USB recovery target: identity={} TLS pin={} enrolled_clients={}",
            hex(&before.identity_fingerprint),
            hex(&before.tls_spki_hash),
            enrolled.len()
        );
        print!("Authorize replacement of this user's local key and USB enrollment (yes/no)? ");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        if !answer.trim().eq_ignore_ascii_case("yes") {
            return Err("USB recovery cancelled; board and local key were not changed".into());
        }
        let credential = credentials::ClientCredential::generate()?;
        let enrolled_fingerprint = match port.enroll_client(&credential.spki) {
            Ok(fingerprint) => fingerprint,
            Err(enrollment_error) => {
                let clients = port.reconcile_clients().map_err(|reconcile_error| {
                    format!(
                        "enrollment response failed ({enrollment_error}); reconciliation failed ({reconcile_error}); enrollment outcome is unknown; do not retry until `clients` is checked"
                    )
                })?;
                if !clients.contains(&credential.fingerprint) {
                    return Err(enrollment_error);
                }
                credential.fingerprint
            }
        };
        if enrolled_fingerprint != credential.fingerprint {
            let _ = port.revoke_client(&enrolled_fingerprint);
            return Err("device returned a different recovery client fingerprint".into());
        }
        if let Err(error) = credential.install_replacement(false) {
            let rollback = port.revoke_client(&enrolled_fingerprint);
            return Err(match rollback {
                Ok(()) => {
                    format!("USB recovery key save failed; new enrollment rolled back: {error}")
                }
                Err(rollback_error) => format!(
                    "USB recovery key save failed ({error}); new enrollment rollback failed ({rollback_error})"
                ),
            });
        }
        match port.status() {
            Ok(after) => {
                serial::remember_board(&selected, &after.identity_fingerprint);
                println!(
                    "USB recovery complete: new client={} identity={} TLS pin={}",
                    hex(&enrolled_fingerprint),
                    hex(&after.identity_fingerprint),
                    hex(&after.tls_spki_hash)
                );
            }
            Err(error) => {
                eprintln!(
                    "USB recovery committed: new client={}; post-recovery status unavailable: {error}",
                    hex(&enrolled_fingerprint)
                );
            }
        }
        return Ok(());
    }
    if command == "transport-test" {
        let status = port.status()?;
        serial::remember_board(&selected, &status.identity_fingerprint);
        if status.wifi_state != 2 {
            return Err(format!(
                "Wi-Fi is {}; connect the board before running transport-test",
                wifi_name(status.wifi_state)
            ));
        }
        println!("Transport test only; HID output and physical-input capture remain disabled.");
        return transport::run_test(port).map_err(|error| error.to_string());
    }
    match command {
        "status" => {
            let status = port.status()?;
            serial::remember_board(&selected, &status.identity_fingerprint);
            println!("Board: {}", selected.label);
            println!("Port: {}", selected.name);
            println!("Firmware: {}", status.firmware_version);
            println!(
                "Wi-Fi: {}",
                match status.wifi_state {
                    0 => "unconfigured",
                    1 => "connecting",
                    2 => "connected",
                    _ => "failed",
                }
            );
            println!("Device identity: {}", hex(&status.identity_fingerprint));
            println!("TLS SPKI SHA-256: {}", hex(&status.tls_spki_hash));
            println!(
                "RGB LED: {}{} at {}% #{:02X}{:02X}{:02X}",
                if status.led_supported {
                    "supported, "
                } else {
                    "unsupported, "
                },
                if status.led_enabled { "on" } else { "off" },
                status.led_brightness,
                status.led_rgb[0],
                status.led_rgb[1],
                status.led_rgb[2]
            );
            Ok(())
        }
        "telemetry" => {
            let metrics = port.transport_status()?;
            println!("state={}", metrics.state);
            println!("lease_epoch={}", metrics.lease_epoch);
            println!("last_event_sequence={}", metrics.last_event_sequence);
            println!("response_queue_depth={}", metrics.response_queue_depth);
            println!("scripted_events={}", metrics.scripted_events);
            println!("scripted_rejections={}", metrics.scripted_rejections);
            println!("scripted_drops={}", metrics.scripted_drops);
            println!("scripted_reorders={}", metrics.scripted_reorders);
            println!("normalized_keys={}", metrics.normalized_keys);
            println!("normalized_buttons={}", metrics.normalized_buttons);
            println!(
                "hid_reports={}",
                metrics
                    .hid_reports
                    .map_or_else(|| "n/a".into(), |value| value.to_string())
            );
            println!(
                "tls_diagnostic={}",
                String::from_utf8_lossy(&metrics.tls_diagnostic[..metrics.tls_diagnostic_len])
            );
            Ok(())
        }
        "wifi" => {
            let ssid = read_hidden_line("SSID: ")?;
            let mut passphrase =
                Secret::new(read_hidden_line("Wi-Fi passphrase (hidden): ")?.into_bytes());
            let id = random_id()?;
            let payload = Secret::new(encode_wifi_request(
                &id,
                ssid.as_bytes(),
                passphrase.as_slice(),
            )?);
            passphrase.0.fill(0);
            port.mutate(payload.as_slice(), &id, WIFI_SET_COMMAND)?;
            let status = port.status()?;
            serial::remember_board(&selected, &status.identity_fingerprint);
            println!(
                "Wi-Fi settings stored; status: {}",
                wifi_name(status.wifi_state)
            );
            Ok(())
        }
        "enroll" => {
            let credential = credentials::ClientCredential::load_or_create(!params.is_empty())?;
            let existing = port.list_clients()?;
            if existing.contains(&credential.fingerprint) {
                println!("This Windows user is already enrolled on the selected board.");
                return Ok(());
            }
            let enrolled = port.enroll_client(&credential.spki)?;
            if enrolled != credential.fingerprint {
                return Err("device returned a different client fingerprint".into());
            }
            println!("Enrolled client {}.", hex(&enrolled));
            Ok(())
        }
        "clients" => {
            let clients = port.list_clients()?;
            if clients.is_empty() {
                println!("No clients enrolled.");
            } else {
                for fingerprint in clients {
                    println!("{}", hex(&fingerprint));
                }
            }
            Ok(())
        }
        "revoke" if params[0].eq_ignore_ascii_case("all") => {
            port.revoke_all()?;
            println!("All enrolled clients revoked.");
            Ok(())
        }
        "revoke" => {
            let fingerprint = parse_fingerprint(&params[0])?;
            port.revoke_client(&fingerprint)?;
            println!("Revoked client {}.", hex(&fingerprint));
            Ok(())
        }
        "led" => {
            if params.len() != 3 {
                return Err("usage: kvmctl led [COMx] on|off <brightness 0..100> <RRGGBB>".into());
            }
            let enabled = match params[0].as_str() {
                "on" => true,
                "off" => false,
                _ => return Err("LED state must be on or off".into()),
            };
            let brightness = params[1]
                .parse::<u8>()
                .map_err(|_| "brightness must be 0–100")?;
            let rgb = parse_rgb(&params[2])?;
            let id = random_id()?;
            let payload = encode_led_request(&id, enabled, brightness, rgb)?;
            port.mutate(&payload, &id, LED_SET_COMMAND)?;
            let status = port.status()?;
            serial::remember_board(&selected, &status.identity_fingerprint);
            println!(
                "RGB LED {} at {brightness}% #{:02X}{:02X}{:02X}.",
                if enabled { "on" } else { "off" },
                rgb[0],
                rgb[1],
                rgb[2]
            );
            Ok(())
        }
        _ => unreachable!(),
    }
}

#[cfg(windows)]
fn wifi_name(state: u64) -> &'static str {
    match state {
        0 => "unconfigured",
        1 => "connecting",
        2 => "connected",
        _ => "failed",
    }
}

#[cfg(windows)]
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

#[cfg(windows)]
fn parse_fingerprint(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("client fingerprint must be 64 hexadecimal digits".into());
    }
    let mut fingerprint = [0u8; 32];
    for (index, byte) in fingerprint.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid client fingerprint")?;
    }
    Ok(fingerprint)
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("kvmctl currently supports Windows only".into())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("kvmctl: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn automatic_selection_requires_one_recognized_board() {
        let port = |recognized| serial::PortInfo {
            name: "COM6".into(),
            label: "USB-UART device".into(),
            instance_id: Some("test-device".into()),
            recognized,
        };
        assert_eq!(serial::automatic_selection(&[]), None);
        assert_eq!(serial::automatic_selection(&[port(false)]), None);
        assert_eq!(serial::automatic_selection(&[port(true)]), Some(0));
        assert_eq!(
            serial::automatic_selection(&[port(true), port(false)]),
            None
        );
        assert_eq!(serial::automatic_selection(&[port(true), port(true)]), None);
    }

    #[cfg(windows)]
    #[test]
    fn com_port_detection_requires_digits_after_com() {
        assert!(is_com_port_name("COM1"));
        assert!(is_com_port_name("COM10"));
        assert!(!is_com_port_name("COM"));
        assert!(!is_com_port_name("COMPUTER"));
        assert!(!is_com_port_name("COM1X"));
        assert!(!is_com_port_name("com1"));
    }

    fn bytes(contents: &str) -> Vec<u8> {
        contents
            .split(|c: char| !c.is_ascii_hexdigit() && c != 'x')
            .filter(|token| !token.is_empty() && *token != "x")
            .map(|token| u8::from_str_radix(token.strip_prefix("0x").unwrap_or(token), 16).unwrap())
            .collect()
    }

    #[test]
    fn status_request_matches_wire_vector() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let vector = bytes(include_str!(
            "../../protocol/vectors/usb_status_request.inc"
        ));
        assert_eq!(encode_status_request(&id), vector);
    }

    #[test]
    fn wifi_and_led_requests_match_wire_vectors() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let wifi = bytes(include_str!(
            "../../protocol/vectors/usb_wifi_set_request.inc"
        ));
        assert_eq!(
            encode_wifi_request(&id, b"test", b"password").unwrap(),
            wifi
        );
        let led = bytes(include_str!(
            "../../protocol/vectors/usb_led_set_request.inc"
        ));
        assert_eq!(
            encode_led_request(&id, true, 50, [255, 0, 128]).unwrap(),
            led
        );
        assert!(encode_wifi_request(&id, b"", b"password").is_err());
        assert!(encode_wifi_request(&id, b"test", b"short").is_err());
        assert!(encode_led_request(&id, false, 101, [0; 3]).is_err());
    }

    #[test]
    fn client_admin_requests_and_responses_are_bound_to_command_and_request() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let spki = [0x5a; 91];
        let enroll = encode_enroll_request(&id, &spki).unwrap();
        assert!(
            enroll.ends_with(
                &[0xa1, 0x00, 0x58, 0x5b]
                    .into_iter()
                    .chain(spki)
                    .collect::<Vec<_>>()
            )
        );
        assert!(encode_enroll_request(&id, &[0; 90]).is_err());
        assert_eq!(encode_list_clients_request(&id).last(), Some(&0xa0));
        assert!(encode_revoke_all_request(&id).ends_with(&[0x04, 0xa0]));

        let fingerprint = [0xab; 32];
        let mut response = vec![0xa6];
        for (key, value) in [(0, 1), (1, 0), (2, ENROLL_COMMAND)] {
            put_uint(&mut response, key);
            put_uint(&mut response, value);
        }
        put_uint(&mut response, 3);
        put_bytes(&mut response, &id);
        put_uint(&mut response, 4);
        put_uint(&mut response, 0);
        put_uint(&mut response, 5);
        put_bytes(&mut response, &fingerprint);
        assert_eq!(decode_enroll_response(&response, &id).unwrap(), fingerprint);

        let mut listed = vec![0xa6];
        for (key, value) in [(0, 1), (1, 0), (2, LIST_CLIENTS_COMMAND)] {
            put_uint(&mut listed, key);
            put_uint(&mut listed, value);
        }
        put_uint(&mut listed, 3);
        put_bytes(&mut listed, &id);
        put_uint(&mut listed, 4);
        put_uint(&mut listed, 0);
        put_uint(&mut listed, 5);
        put_head(&mut listed, 4, 1);
        put_bytes(&mut listed, &fingerprint);
        assert_eq!(
            decode_client_list_response(&listed, &id).unwrap(),
            [fingerprint]
        );
        assert!(decode_client_list_response(&listed, &[1; 16]).is_err());
    }

    #[test]
    fn mutation_response_checks_identity_and_command() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let response = [
            0xa6, 0x00, 0x01, 0x01, 0x00, 0x02, 0x18, 0x66, 0x03, 0x50, 0, 1, 2, 3, 4, 5, 6, 7, 8,
            9, 10, 11, 12, 13, 14, 15, 0x04, 0x00, 0x05, 0xa0,
        ];
        assert!(decode_mutation_response(&response, &id, LED_SET_COMMAND).is_ok());
        assert!(decode_mutation_response(&response, &id, WIFI_SET_COMMAND).is_err());
    }

    #[test]
    fn transport_offer_decodes_success_and_error_response_shapes() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let mut rejected = vec![0xa5];
        for (key, value) in [(0, 1), (1, 0), (2, TRANSPORT_OFFER_COMMAND)] {
            put_uint(&mut rejected, key);
            put_uint(&mut rejected, value);
        }
        put_uint(&mut rejected, 3);
        put_bytes(&mut rejected, &id);
        put_uint(&mut rejected, 4);
        put_uint(&mut rejected, 3);
        assert_eq!(
            decode_transport_offer_response(&rejected, &id).unwrap_err(),
            "device is busy with another transport session"
        );

        let mut accepted = vec![0xa6];
        for (key, value) in [(0, 1), (1, 0), (2, TRANSPORT_OFFER_COMMAND)] {
            put_uint(&mut accepted, key);
            put_uint(&mut accepted, value);
        }
        put_uint(&mut accepted, 3);
        put_bytes(&mut accepted, &id);
        put_uint(&mut accepted, 4);
        put_uint(&mut accepted, 0);
        put_uint(&mut accepted, 5);
        put_text(&mut accepted, b"v=0\r\n");
        assert_eq!(
            decode_transport_offer_response(&accepted, &id).unwrap(),
            "v=0\r\n"
        );
    }

    #[test]
    fn decodes_transport_identity_failure_status_and_metrics() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let mut response = vec![0xa6];
        for (key, value) in [(0, 1), (1, 0), (2, TRANSPORT_STATUS_COMMAND)] {
            put_uint(&mut response, key);
            put_uint(&mut response, value);
        }
        put_uint(&mut response, 3);
        put_bytes(&mut response, &id);
        put_uint(&mut response, 4);
        put_uint(&mut response, 0);
        put_uint(&mut response, 5);
        let result_header = response.len();
        put_head(&mut response, 5, 22);
        for (key, value) in [
            (0, 3),
            (1, 50_000),
            (2, 40_000),
            (3, 48_000),
            (4, 1_000_000),
            (5, 900_000),
            (6, 999_000),
            (7, 0),
            (8, 0),
            (9, 321),
        ] {
            put_uint(&mut response, key);
            put_uint(&mut response, value);
        }
        put_uint(&mut response, 10);
        put_text(&mut response, b"ESP-TLS: handshake_failure");
        put_uint(&mut response, 11);
        put_uint(&mut response, 17);
        for (key, value) in [
            (12, 123_456_u64),
            (13, 9),
            (14, 8),
            (15, 2),
            (16, 7),
            (17, 1),
            (18, 2),
            (19, 3),
            (20, 1),
            (21, 0),
        ] {
            put_uint(&mut response, key);
            put_uint(&mut response, value);
        }
        let metrics = decode_transport_status_response(&response, &id).unwrap();
        assert_eq!(metrics.state, 3);
        assert_eq!(metrics.internal_free_min, 40_000);
        assert_eq!(metrics.psram_free_min, 900_000);
        assert_eq!(metrics.channel_opens, 0);
        assert_eq!(metrics.application_bytes_received, 0);
        assert_eq!(metrics.peer_task_cpu_us, 321);
        assert_eq!(metrics.tls_rate_limited, 17);
        assert_eq!(metrics.board_timestamp_ms, 123_456);
        assert_eq!(metrics.lease_epoch, 9);
        assert_eq!(metrics.last_event_sequence, 8);
        assert_eq!(metrics.response_queue_depth, 2);
        assert_eq!(metrics.scripted_drops, 2);
        assert_eq!(metrics.normalized_keys, 1);
        assert_eq!(metrics.hid_reports, None);
        assert_eq!(
            &metrics.tls_diagnostic[..metrics.tls_diagnostic_len],
            b"ESP-TLS: handshake_failure"
        );
        let mut non_hid = response.clone();
        non_hid[result_header] = 0xb8;
        non_hid.insert(result_header + 1, 25);
        for (key, value) in [(23, 456_u64), (24, 789_u64), (25, 123_u64)] {
            put_uint(&mut non_hid, key);
            put_uint(&mut non_hid, value);
        }
        let metrics = decode_transport_status_response(&non_hid, &id).unwrap();
        assert_eq!(metrics.hid_reports, None);
        assert_eq!(metrics.total_soc_cpu_us, 456);
        assert_eq!(metrics.event_processing_us, 789);
        assert_eq!(metrics.response_queue_residence_us, 123);
        response[result_header] = 0xb7;
        put_uint(&mut response, 22);
        put_uint(&mut response, 6);
        assert_eq!(
            decode_transport_status_response(&response, &id)
                .unwrap()
                .hid_reports,
            Some(6)
        );
        response[result_header] = 0xb8;
        response.insert(result_header + 1, 26);
        for (key, value) in [(23, 456_u64), (24, 789_u64), (25, 123_u64)] {
            put_uint(&mut response, key);
            put_uint(&mut response, value);
        }
        let metrics = decode_transport_status_response(&response, &id).unwrap();
        assert_eq!(metrics.total_soc_cpu_us, 456);
        assert_eq!(metrics.event_processing_us, 789);
        assert_eq!(metrics.response_queue_residence_us, 123);
    }

    #[test]
    fn decodes_status_vector_and_checks_identity_hash() {
        let id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let response = bytes(include_str!(
            "../../protocol/vectors/usb_status_response.inc"
        ));
        let status = decode_status_response(&response, &id).unwrap();
        assert_eq!(status.identity_fingerprint[0], 0xca);
        assert_eq!(status.tls_spki_hash, [0; 32]);
        assert_eq!(status.wifi_state, 0);
        assert_eq!(status.firmware_version, "test");
        assert!(status.led_supported);
        assert!(!status.led_enabled);
        assert_eq!(status.led_brightness, 25);
        assert_eq!(status.led_rgb, [255; 3]);
    }

    #[test]
    fn rejects_non_shortest_and_trailing_cbor() {
        let mut reader = Reader::new(&[0x18, 0x01]);
        assert!(reader.expect_uint(1).is_err());
        let mut reader = Reader::new(&[0x00, 0x00]);
        assert!(reader.expect_uint(0).is_ok());
        assert!(!reader.done());
    }

    #[test]
    fn parses_rgb_values() {
        assert_eq!(parse_rgb("#FF2200").unwrap(), [255, 34, 0]);
        assert_eq!(parse_rgb("00aaff").unwrap(), [0, 170, 255]);
        assert!(parse_rgb("orange").is_err());
    }
}
