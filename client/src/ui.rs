use super::*;
use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, UpdateWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::BST_CHECKED;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    CBN_SELCHANGE, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, GetMessageW, GetWindowTextLengthW, GetWindowTextW, MSG, PostQuitMessage,
    RegisterClassW, SendMessageW, SetWindowTextW, ShowWindow, TranslateMessage, WM_COMMAND,
    WM_DESTROY, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
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
const WIFI_SCAN: u16 = 12;
const CBN_SELCHANGE_U16: u16 = CBN_SELCHANGE as u16;
const CBS_DROPDOWN: u32 = 0x0002;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const CBS_AUTOHSCROLL: u32 = 0x0040;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_PASSWORD: u32 = 0x0020;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_GROUPBOX: u32 = 0x0007;

struct UiState {
    window: HWND,
    ports: HWND,
    port_infos: Vec<serial::PortInfo>,
    summary: HWND,
    ssid: HWND,
    passphrase: HWND,
    brightness: HWND,
    color: HWND,
    led_enabled: HWND,
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

fn selected_port(state: &UiState) -> Result<serial::PortInfo, String> {
    let selected = unsafe { SendMessageW(state.ports, CB_GETCURSEL, 0, 0) };
    if selected < 0 {
        return Err("select a board or USB-UART candidate".into());
    }
    state
        .port_infos
        .get(selected as usize)
        .cloned()
        .ok_or_else(|| "invalid board selection".into())
}

fn open_board(state: &UiState, board: &serial::PortInfo) -> Result<serial::Port, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW,
    };
    if !board.recognized {
        let message = wide(&format!(
            "{}\n\nThis device has not been identified as a kvmctl board. Confirm this is your flashed board before sending data.",
            board.label
        ));
        let title = wide("Confirm USB-UART device");
        if unsafe {
            MessageBoxW(
                state.window,
                message.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        } != IDYES
        {
            return Err("device not confirmed; port was not opened".into());
        }
    }
    serial::Port::open(&board.name)
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

fn status_text(board: &str, status: &Status) -> String {
    format!(
        "{board}  |  Firmware: {}\r\nWi-Fi: {}    RGB: {}{} at {}% #{:02X}{:02X}{:02X}\r\nDevice identity: {}\r\nTLS pin: {}",
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
        let board = selected_port(state)?;
        set_text(state.summary, &format!("Reading {}…", board.label));
        let mut port = open_board(state, &board)?;
        let status = port.status()?;
        serial::remember_board(&board, &status.identity_fingerprint);
        set_led_fields(state, &status);
        Ok(status_text(&board.label, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("Error: {error}")),
    }
}

fn save_wifi(state: &mut UiState) {
    let result: Result<String, String> = (|| {
        let board = selected_port(state)?;
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
        let mut port = open_board(state, &board)?;
        port.mutate(payload.as_slice(), &id, WIFI_SET_COMMAND)?;
        let status = port.status()?;
        serial::remember_board(&board, &status.identity_fingerprint);
        set_led_fields(state, &status);
        Ok(format!(
            "Wi-Fi settings stored.\r\n{}",
            status_text(&board.label, &status)
        ))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("Wi-Fi setup failed: {error}")),
    }
}

fn save_led(state: &mut UiState) {
    let result: Result<String, String> = (|| {
        let board = selected_port(state)?;
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
        let mut port = open_board(state, &board)?;
        port.mutate(&payload, &id, LED_SET_COMMAND)?;
        let status = port.status()?;
        serial::remember_board(&board, &status.identity_fingerprint);
        set_led_fields(state, &status);
        Ok(status_text(&board.label, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("RGB update failed: {error}")),
    }
}

fn update_port_list(state: &mut UiState, ports: Vec<serial::PortInfo>) {
    let previous = selected_port(state).ok().and_then(|port| port.instance_id);
    unsafe { SendMessageW(state.ports, CB_RESETCONTENT, 0, 0) };
    state.port_infos = ports;
    for port in &state.port_infos {
        add_combo_item(state.ports, &port.label);
    }
    let selected = previous
        .and_then(|instance_id| {
            state
                .port_infos
                .iter()
                .position(|port| port.instance_id.as_deref() == Some(instance_id.as_str()))
        })
        .or_else(|| serial::automatic_selection(&state.port_infos));
    if let Some(index) = selected {
        unsafe { SendMessageW(state.ports, CB_SETCURSEL, index, 0) };
    }
    match state.port_infos.as_slice() {
        [] => set_text(
            state.summary,
            "No USB-UART candidates found. Connect board and rescan.",
        ),
        [port] => set_text(
            state.summary,
            &format!(
                "{}. Choose unknown devices explicitly; confirmation is required before communication.",
                port.label
            ),
        ),
        _ => set_text(
            state.summary,
            "Multiple USB-UART candidates found. Choose your board by name and USB location; unknown devices require confirmation.",
        ),
    }
}

fn refresh_wifi_list(state: &mut UiState) {
    let current = text(state.ssid);
    match wifi_networks::visible_ssids() {
        Ok(ssids) => {
            unsafe { SendMessageW(state.ssid, CB_RESETCONTENT, 0, 0) };
            for ssid in &ssids {
                add_combo_item(state.ssid, ssid);
            }
            set_text(state.ssid, &current);
            set_text(
                state.summary,
                &format!(
                    "Loaded {} visible SSID(s). You can also type a hidden SSID.",
                    ssids.len()
                ),
            );
        }
        Err(error) => set_text(
            state.summary,
            &format!("Wi-Fi list unavailable; type SSID manually ({error})."),
        ),
    }
}

fn rescan(state: &mut UiState) {
    match serial::enumerate() {
        Ok(ports) => update_port_list(state, ports),
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
        if !matches!(
            id,
            PORTS | WIFI_SAVE | LED_SAVE | REFRESH | RESCAN | WIFI_SCAN
        ) || (id == PORTS && notification != CBN_SELCHANGE_U16)
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
                (WIFI_SCAN, 0) => refresh_wifi_list(state),
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
        250,
        180,
    );
    const CB_SETDROPPEDWIDTH: u32 = 0x0160;
    unsafe { SendMessageW(ports, CB_SETDROPPEDWIDTH, 480, 0) };
    control(
        instance, window, "BUTTON", "Rescan", basic, RESCAN, 330, 14, 80, 26,
    );
    control(
        instance,
        window,
        "BUTTON",
        "Refresh status",
        basic,
        REFRESH,
        420,
        14,
        110,
        26,
    );
    let summary = control(
        instance,
        window,
        "STATIC",
        "Looking for compatible kvmctl boardsâ€¦",
        WS_BORDER,
        SUMMARY,
        18,
        50,
        580,
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
        "SSID (hidden OK)",
        0,
        0,
        20,
        190,
        104,
        22,
    );
    let ssid = control(
        instance,
        window,
        "COMBOBOX",
        "",
        CBS_DROPDOWN | CBS_AUTOHSCROLL | WS_VSCROLL | basic,
        SSID,
        134,
        186,
        290,
        150,
    );
    unsafe { SendMessageW(ssid, CB_SETDROPPEDWIDTH, 320, 0) };
    control(
        instance,
        window,
        "BUTTON",
        "Refresh list",
        basic,
        WIFI_SCAN,
        438,
        184,
        130,
        26,
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
        220,
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

    let mut state = UiState {
        window,
        ports,
        port_infos: Vec::new(),
        summary,
        ssid,
        passphrase,
        brightness,
        color,
        led_enabled,
    };
    match serial::enumerate() {
        Ok(ports) => update_port_list(&mut state, ports),
        Err(error) => set_text(state.summary, &format!("Board scan failed: {error}")),
    }
    if let Ok(ssids) = wifi_networks::visible_ssids() {
        for ssid in ssids {
            add_combo_item(state.ssid, &ssid);
        }
    }
    state
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
