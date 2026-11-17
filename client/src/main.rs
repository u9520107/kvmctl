#![cfg_attr(not(windows), allow(dead_code))]

use std::io::{self, Write};

const MAX_FRAME: usize = 4096;
const WIFI_SET_COMMAND: u64 = 0x60;
const STATUS_COMMAND: u64 = 0x61;
const LED_SET_COMMAND: u64 = 0x66;

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

#[cfg(windows)]
fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_SHA256_ALGORITHM, BCryptHash, BCryptOpenAlgorithmProvider,
    };
    let mut algorithm = std::ptr::null_mut();
    let open = unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), 0)
    };
    if open < 0 {
        return Err(format!(
            "BCryptOpenAlgorithmProvider failed: 0x{:08x}",
            open as u32
        ));
    }
    let mut digest = [0u8; 32];
    let len = u32::try_from(data.len()).map_err(|_| "input too large")?;
    let result = unsafe {
        BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            data.as_ptr(),
            len,
            digest.as_mut_ptr(),
            32,
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

    pub struct Port(HANDLE);

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

    fn load_board_identities() -> BTreeMap<String, String> {
        let Some(path) = board_map_path() else {
            return BTreeMap::new();
        };
        let Ok(contents) = fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        contents
            .lines()
            .filter_map(|line| {
                let (instance_id, fingerprint) = line.split_once('\t')?;
                (fingerprint.len() == 64
                    && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| (instance_id.to_owned(), fingerprint.to_owned()))
            })
            .collect()
    }

    pub fn remember_board(info: &PortInfo, fingerprint: &[u8; 32]) {
        let (Some(path), Some(instance_id)) = (board_map_path(), info.instance_id.as_ref()) else {
            return;
        };
        if instance_id.chars().any(char::is_control) {
            return;
        }
        let mut identities = load_board_identities();
        identities.insert(instance_id.clone(), super::hex(fingerprint));
        let contents = identities
            .iter()
            .map(|(id, fingerprint)| format!("{id}\t{fingerprint}\n"))
            .collect::<String>();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, contents);
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
                .filter(|fingerprint| fingerprint.len() >= 8);
            let location = property_text(devices, &info, SPDRP_LOCATION_INFORMATION)
                .filter(|value| !value.is_empty());
            let board_name = known_identity
                .map(|fingerprint| format!("kvmctl {} — {friendly_name}", &fingerprint[..8]))
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
            let id = random_id()?;
            let payload = encode_status_request(&id);
            let response = self.exchange(&payload)?;
            decode_status_response(&response, &id)
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
fn select_port(
    ports: &[serial::PortInfo],
    requested: Option<&str>,
) -> Result<serial::PortInfo, String> {
    if let Some(port) = requested {
        if !port.starts_with("COM")
            || !port[3..].bytes().all(|b| b.is_ascii_digit())
            || port.len() == 3
        {
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
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return ui::run();
    }
    let command = args.first().map(String::as_str).unwrap_or("status");
    if command == "--help" || command == "-h" {
        println!(
            "kvmctl [status [COMx] | wifi [COMx] | led [COMx] on|off <brightness> <RRGGBB> | ports]; no arguments opens the control window"
        );
        return Ok(());
    }
    let ports = serial::enumerate()?;
    if command == "ports" {
        for port in ports {
            println!("{}", port.label);
        }
        return Ok(());
    }
    let (requested_port, params) = match command {
        "led" if args.get(1).is_some_and(|arg| arg.starts_with("COM")) => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
        "led" => (None, args.get(1..).unwrap_or(&[])),
        _ => (
            args.get(1).map(String::as_str),
            args.get(2..).unwrap_or(&[]),
        ),
    };
    if !matches!(command, "status" | "wifi" | "led") {
        return Err("unknown command; use --help".into());
    }
    if command == "status" && !params.is_empty() || command == "wifi" && !params.is_empty() {
        return Err("usage: kvmctl [status [COMx] | wifi [COMx] | led [COMx] on|off <brightness> <RRGGBB> | ports]".into());
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
    let mut port = serial::Port::open(&selected.name)?;
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
