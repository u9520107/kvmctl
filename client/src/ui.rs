use super::*;
use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, UpdateWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::BST_CHECKED;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCURSEL, CB_GETLBTEXT, CB_GETLBTEXTLEN,
    CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowTextLengthW,
    GetWindowTextW, MSG, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowTextW, ShowWindow,
    TranslateMessage, WM_COMMAND, WM_DESTROY, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

const PORTS: u16 = 1;
const SUMMARY: u16 = 2;
const SSID: u16 = 3;
const PASSPHRASE: u16 = 4;
const BRIGHTNESS: u16 = 5;
const COLOR: u16 = 6;
const LED_ENABLED: u16 = 7;
const WIFI_SAVE: u16 = 8;
const LED_SAVE: u16 = 9;
const REFRESH: u16 = 10;
const RESCAN: u16 = 11;
const CBN_SELCHANGE_U16: u16 = CBN_SELCHANGE as u16;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_PASSWORD: u32 = 0x0020;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_GROUPBOX: u32 = 0x0007;

struct UiState {
    window: HWND,
    ports: HWND,
    summary: HWND,
    ssid: HWND,
    passphrase: HWND,
    brightness: HWND,
    color: HWND,
    led_enabled: HWND,
    port_names: Vec<String>,
}

thread_local! {
    static STATE: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn control(
    instance: HINSTANCE,
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    id: u16,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> HWND {
    let class = wide(class);
    let text = wide(text);
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            width,
            height,
            parent,
            id as usize as _,
            instance,
            std::ptr::null(),
        )
    }
}

fn set_text(window: HWND, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(window, value.as_ptr()) };
}

fn text(window: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let read =
        unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..read])
}

fn add_combo_item(combo: HWND, value: &str) {
    let value = wide(value);
    unsafe { SendMessageW(combo, CB_ADDSTRING, 0, value.as_ptr() as LPARAM) };
}

fn selected_port(state: &UiState) -> Result<String, String> {
    let selected = unsafe { SendMessageW(state.ports, CB_GETCURSEL, 0, 0) };
    if selected < 0 {
        return Err("select a USB-UART port".into());
    }
    let length = unsafe { SendMessageW(state.ports, CB_GETLBTEXTLEN, selected as usize, 0) };
    if length < 0 || length > 32 {
        return Err("invalid port selection".into());
    }
    let mut buffer = vec![0u16; length as usize + 1];
    unsafe {
        SendMessageW(
            state.ports,
            CB_GETLBTEXT,
            selected as usize,
            buffer.as_mut_ptr() as LPARAM,
        );
    }
    String::from_utf16(&buffer[..length as usize]).map_err(|_| "invalid COM port name".into())
}

fn set_led_fields(state: &UiState, status: &Status) {
    unsafe {
        SendMessageW(
            state.led_enabled,
            BM_SETCHECK,
            if status.led_enabled {
                BST_CHECKED as usize
            } else {
                0
            },
            0,
        );
    }
    set_text(state.brightness, &status.led_brightness.to_string());
    set_text(
        state.color,
        &format!(
            "{:02X}{:02X}{:02X}",
            status.led_rgb[0], status.led_rgb[1], status.led_rgb[2]
        ),
    );
}

fn status_text(port: &str, status: &Status) -> String {
    format!(
        "{port}  |  Firmware: {}\r\nWi-Fi: {}    RGB: {}{} at {}% #{:02X}{:02X}{:02X}\r\nDevice identity: {}\r\nTLS pin: {}",
        status.firmware_version,
        wifi_name(status.wifi_state),
        if status.led_supported {
            ""
        } else {
            "unsupported; "
        },
        if status.led_enabled { "on" } else { "off" },
        status.led_brightness,
        status.led_rgb[0],
        status.led_rgb[1],
        status.led_rgb[2],
        hex(&status.identity_fingerprint),
        hex(&status.tls_spki_hash),
    )
}

fn refresh(state: &mut UiState) {
    let result: Result<String, String> = (|| {
        let port_name = selected_port(state)?;
        set_text(state.summary, &format!("Reading {port_name}…"));
        let mut port = serial::Port::open(&port_name)?;
        let status = port.status()?;
        set_led_fields(state, &status);
        Ok(status_text(&port_name, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("Error: {error}")),
    }
}

fn save_wifi(state: &mut UiState) {
    let result: Result<String, String> = (|| {
        let port_name = selected_port(state)?;
        let ssid = text(state.ssid);
        let mut passphrase = Secret::new(text(state.passphrase).into_bytes());
        set_text(state.passphrase, "");
        let id = random_id()?;
        let payload = Secret::new(encode_wifi_request(
            &id,
            ssid.as_bytes(),
            passphrase.as_slice(),
        )?);
        passphrase.0.fill(0);
        set_text(state.summary, "Sending Wi-Fi settings over USB…");
        unsafe { UpdateWindow(state.window) };
        let mut port = serial::Port::open(&port_name)?;
        port.mutate(payload.as_slice(), &id, WIFI_SET_COMMAND)?;
        let status = port.status()?;
        set_led_fields(state, &status);
        Ok(format!(
            "Wi-Fi settings stored.\r\n{}",
            status_text(&port_name, &status)
        ))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("Wi-Fi setup failed: {error}")),
    }
}

fn save_led(state: &mut UiState) {
    let result: Result<String, String> = (|| {
        let port_name = selected_port(state)?;
        let enabled =
            unsafe { SendMessageW(state.led_enabled, BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
        let brightness = text(state.brightness)
            .parse::<u8>()
            .map_err(|_| "brightness must be 0–100")?;
        let rgb = parse_rgb(&text(state.color))?;
        let id = random_id()?;
        let payload = encode_led_request(&id, enabled, brightness, rgb)?;
        set_text(state.summary, "Saving RGB preferences over USB…");
        unsafe { UpdateWindow(state.window) };
        let mut port = serial::Port::open(&port_name)?;
        port.mutate(&payload, &id, LED_SET_COMMAND)?;
        let status = port.status()?;
        set_led_fields(state, &status);
        Ok(status_text(&port_name, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("RGB update failed: {error}")),
    }
}

fn rescan(state: &mut UiState) {
    match serial::enumerate() {
        Ok(names) => {
            state.port_names = names;
            unsafe {
                SendMessageW(state.ports, CB_RESETCONTENT, 0, 0);
            }
            for name in &state.port_names {
                add_combo_item(state.ports, name);
            }
            if !state.port_names.is_empty() {
                unsafe { SendMessageW(state.ports, CB_SETCURSEL, 0, 0) };
                refresh(state);
            } else {
                set_text(
                    state.summary,
                    "No serial ports found. Connect the board and rescan.",
                );
            }
        }
        Err(error) => set_text(state.summary, &format!("Port scan failed: {error}")),
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    if message == WM_COMMAND {
        let id = (wparam & 0xffff) as u16;
        let notification = ((wparam >> 16) & 0xffff) as u16;
        if !matches!(id, PORTS | WIFI_SAVE | LED_SAVE | REFRESH | RESCAN)
            || (id == PORTS && notification != CBN_SELCHANGE_U16)
        {
            return unsafe { DefWindowProcW(window, message, wparam, lparam) };
        }
        let handled = STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            let Some(state) = state.as_mut() else {
                return false;
            };
            match (id, notification) {
                (WIFI_SAVE, 0) => save_wifi(state),
                (LED_SAVE, 0) => save_led(state),
                (REFRESH, 0) | (PORTS, CBN_SELCHANGE_U16) => refresh(state),
                (RESCAN, 0) => rescan(state),
                _ => return false,
            }
            true
        });
        if handled {
            return 0;
        }
    }
    let _ = lparam;
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn create_controls(instance: HINSTANCE, window: HWND) -> UiState {
    let basic = WS_TABSTOP;
    control(instance, window, "STATIC", "Board", 0, 0, 18, 18, 48, 22);
    let ports = control(
        instance,
        window,
        "COMBOBOX",
        "",
        CBS_DROPDOWNLIST | WS_VSCROLL | basic,
        PORTS,
        68,
        14,
        150,
        180,
    );
    control(
        instance, window, "BUTTON", "Rescan", basic, RESCAN, 228, 14, 80, 26,
    );
    control(
        instance,
        window,
        "BUTTON",
        "Refresh status",
        basic,
        REFRESH,
        318,
        14,
        110,
        26,
    );
    let summary = control(
        instance,
        window,
        "STATIC",
        "Choose a board and refresh status.",
        WS_BORDER,
        SUMMARY,
        18,
        50,
        560,
        112,
    );

    control(
        instance,
        window,
        "BUTTON",
        "Wi-Fi setup",
        BS_GROUPBOX,
        0,
        18,
        170,
        560,
        112,
    );
    control(
        instance,
        window,
        "STATIC",
        "Network name",
        0,
        0,
        20,
        190,
        94,
        22,
    );
    let ssid = control(
        instance,
        window,
        "EDIT",
        "",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        SSID,
        124,
        186,
        300,
        25,
    );
    control(
        instance,
        window,
        "STATIC",
        "Passphrase",
        0,
        0,
        20,
        224,
        94,
        22,
    );
    let passphrase = control(
        instance,
        window,
        "EDIT",
        "",
        WS_BORDER | ES_AUTOHSCROLL | ES_PASSWORD | basic,
        PASSPHRASE,
        124,
        220,
        300,
        25,
    );
    control(
        instance,
        window,
        "BUTTON",
        "Save and connect",
        basic,
        WIFI_SAVE,
        438,
        203,
        130,
        30,
    );

    control(
        instance,
        window,
        "BUTTON",
        "RGB lighting",
        BS_GROUPBOX,
        0,
        18,
        292,
        560,
        106,
    );
    let led_enabled = control(
        instance,
        window,
        "BUTTON",
        "Enabled",
        BS_AUTOCHECKBOX | basic,
        LED_ENABLED,
        30,
        320,
        105,
        24,
    );
    control(
        instance,
        window,
        "STATIC",
        "Brightness %",
        0,
        0,
        150,
        322,
        85,
        22,
    );
    let brightness = control(
        instance,
        window,
        "EDIT",
        "25",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        BRIGHTNESS,
        240,
        318,
        55,
        25,
    );
    control(
        instance,
        window,
        "STATIC",
        "Color #RRGGBB",
        0,
        0,
        315,
        322,
        90,
        22,
    );
    let color = control(
        instance,
        window,
        "EDIT",
        "FFFFFF",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        COLOR,
        407,
        318,
        80,
        25,
    );
    control(
        instance, window, "BUTTON", "Save RGB", basic, LED_SAVE, 497, 316, 70, 29,
    );

    let port_names = serial::enumerate().unwrap_or_default();
    for name in &port_names {
        add_combo_item(ports, name);
    }
    if !port_names.is_empty() {
        unsafe { SendMessageW(ports, CB_SETCURSEL, 0, 0) };
    }
    UiState {
        window,
        ports,
        summary,
        ssid,
        passphrase,
        brightness,
        color,
        led_enabled,
        port_names,
    }
}

pub fn run() -> Result<(), String> {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    if instance.is_null() {
        return Err("cannot load Windows application instance".into());
    }
    let class_name = wide("KvmCtlControlWindow");
    let title = wide("kvmctl — Control Setup");
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: (COLOR_WINDOW + 1) as _,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err("cannot register control window".into());
    }
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            620,
            465,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err("cannot create control window".into());
    }
    let state = create_controls(instance, window);
    STATE.with(|cell| *cell.borrow_mut() = Some(state));
    unsafe {
        ShowWindow(window, 5);
        UpdateWindow(window);
    }
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            refresh(state);
        }
    });
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}
