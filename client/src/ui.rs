use super::osd;
use super::tray_icon::{TrayIcon, TrayState};
use super::*;
use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, UpdateWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::BST_CHECKED;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
    CB_SETCURSEL, CBN_SELCHANGE, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetWindowTextLengthW, GetWindowTextW, KillTimer, LB_ADDSTRING, LB_GETCURSEL,
    LB_RESETCONTENT, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SendMessageW, SetForegroundWindow, SetTimer,
    SetWindowTextW, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
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
const MOTION_RATE: u16 = 13;
const MOTION_RATE_SAVE: u16 = 14;
const ESCAPE_BINDINGS: u16 = 15;
const ESCAPE_BINDING: u16 = 16;
const ESCAPE_ADD: u16 = 17;
const ESCAPE_REMOVE: u16 = 18;
const WIFI_SCAN_TIMER: usize = 1;
const SESSION_TIMER: usize = 2;
const WIFI_SCAN_INTERVAL_MS: u32 = 350;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const TARGET_MENU_BASE: u16 = 1000;
const DISCONNECT_MENU: u16 = 2000;
const SETUP_MENU: u16 = 2001;
const QUIT_MENU: u16 = 2002;
const CBN_SELCHANGE_U16: u16 = CBN_SELCHANGE as u16;
const CBS_DROPDOWN: u32 = 0x0002;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const CBS_AUTOHSCROLL: u32 = 0x0040;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_PASSWORD: u32 = 0x0020;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_GROUPBOX: u32 = 0x0007;
const LBS_NOTIFY: u32 = 0x0001;

struct TraySession {
    target: String,
    control: transport::SessionControl,
    events: Receiver<TrayMessage>,
    worker: std::thread::JoinHandle<()>,
}

enum TrayMessage {
    Event(transport::SessionEvent),
    Finished(Result<(), String>),
}

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
    wifi_scan_button: HWND,
    motion_rate: HWND,
    escape_bindings: HWND,
    escape_binding: HWND,
    wifi_scan_rx: Option<Receiver<Result<Vec<String>, String>>>,
    wifi_scan_frame: u8,
    wifi_scan_summary_stale: bool,
    tray: TrayIcon,
    tray_session: Option<TraySession>,
    tray_state: TrayState,
    selected_target: Option<targets::Target>,
    pending_target: Option<targets::Target>,
}

thread_local! {
    static STATE: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn copy_wide(destination: &mut [u16], value: &str) {
    let encoded = value
        .encode_utf16()
        .take(destination.len().saturating_sub(1));
    for (slot, unit) in destination.iter_mut().zip(encoded) {
        *slot = unit;
    }
}

fn tray_data(tray: &TrayIcon, state: TrayState, target: Option<&str>) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = tray.window;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = TRAY_MESSAGE;
    data.hIcon = tray.icon(state);
    let status = match state {
        TrayState::NoTarget => "No active target; input local",
        TrayState::Starting => "Starting; input local",
        TrayState::Connecting => "Connecting; input local",
        TrayState::ConnectedLocal => "Connected/local",
        TrayState::RemoteControl => "Remote control",
        TrayState::ConnectionDegraded => "Connection degraded; input state unchanged",
        TrayState::OwnershipPending => "Takeover pending; input local",
        TrayState::Recovering => "Recovering; input local",
        TrayState::Unavailable => "Unavailable; input local",
        TrayState::OwnershipLost => "Ownership lost; input local",
    };
    let tip = target.map_or_else(
        || format!("kvmctl — {status}"),
        |target| format!("kvmctl — {target} — {status}"),
    );
    copy_wide(&mut data.szTip, &tip);
    data
}

fn update_tray(tray: &TrayIcon, state: TrayState, target: Option<&str>) {
    let data = tray_data(tray, state, target);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn add_tray(tray: &TrayIcon, state: TrayState, target: Option<&str>) {
    let data = tray_data(tray, state, target);
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
}

fn remove_tray(tray: &TrayIcon) {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = tray.window;
    data.uID = 1;
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

fn tray_notice(tray: &TrayIcon, title: &str, message: &str) {
    let _ = tray;
    let _ = osd::show(title, message);
}

fn target_menu_text(target: &targets::Target) -> String {
    let short = target.endpoint_fingerprint[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}  [{short}]", target.name)
}

fn show_tray_menu(window: HWND, has_disconnect: bool) -> Option<targets::Target> {
    let Ok(saved) = targets::load() else {
        return None;
    };
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return None;
    }
    if saved.is_empty() {
        let text = wide("No saved targets");
        unsafe { AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text.as_ptr()) };
    } else {
        for (index, target) in saved
            .iter()
            .take(usize::from(DISCONNECT_MENU - TARGET_MENU_BASE))
            .enumerate()
        {
            let text = wide(&target_menu_text(target));
            unsafe {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    (TARGET_MENU_BASE as usize) + index,
                    text.as_ptr(),
                )
            };
        }
    }
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
    if has_disconnect {
        let text = wide("Disconnect");
        unsafe { AppendMenuW(menu, MF_STRING, DISCONNECT_MENU as usize, text.as_ptr()) };
    }
    let text = wide("Open setup window");
    unsafe { AppendMenuW(menu, MF_STRING, SETUP_MENU as usize, text.as_ptr()) };
    let text = wide("Quit");
    unsafe { AppendMenuW(menu, MF_STRING, QUIT_MENU as usize, text.as_ptr()) };
    let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    let command = unsafe {
        GetCursorPos(&mut point);
        SetForegroundWindow(window);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            0,
            window,
            std::ptr::null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        command as u16
    };
    if (TARGET_MENU_BASE..DISCONNECT_MENU).contains(&command) {
        return saved
            .into_iter()
            .nth(usize::from(command - TARGET_MENU_BASE));
    }
    if command != 0 {
        unsafe { SendMessageW(window, WM_COMMAND, WPARAM::from(command), 0) };
    }
    None
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

fn add_list_item(list: HWND, value: &str) {
    let value = wide(value);
    unsafe { SendMessageW(list, LB_ADDSTRING, 0, value.as_ptr() as LPARAM) };
}

fn refresh_input_settings(state: &UiState) -> Result<(), String> {
    set_text(
        state.motion_rate,
        &settings::load_motion_rate_hz()?.to_string(),
    );
    unsafe { SendMessageW(state.escape_bindings, LB_RESETCONTENT, 0, 0) };
    for binding in settings::load_escape_bindings()? {
        add_list_item(state.escape_bindings, &binding.display());
    }
    Ok(())
}

fn save_motion_rate(state: &UiState) {
    let result = (|| {
        let rate = text(state.motion_rate)
            .trim()
            .parse::<u32>()
            .map_err(|_| "motion rate must be an integer".to_owned())?;
        settings::save_motion_rate_hz(rate)?;
        refresh_input_settings(state)?;
        Ok::<_, String>(format!("Motion rate saved: {rate} Hz"))
    })();
    set_text(
        state.summary,
        &result.unwrap_or_else(|error| format!("Input settings failed: {error}")),
    );
}

fn add_escape_binding(state: &UiState) {
    let result = (|| {
        let binding = settings::EscapeBinding::parse(&text(state.escape_binding))?;
        let mut bindings = settings::load_escape_bindings()?;
        if !bindings.contains(&binding) {
            bindings.push(binding);
            settings::save_escape_bindings(&bindings)?;
        }
        refresh_input_settings(state)?;
        set_text(state.escape_binding, "");
        Ok::<_, String>("Escape binding saved".to_owned())
    })();
    set_text(
        state.summary,
        &result.unwrap_or_else(|error| format!("Input settings failed: {error}")),
    );
}

fn remove_escape_binding(state: &UiState) {
    let result = (|| {
        let selected = unsafe { SendMessageW(state.escape_bindings, LB_GETCURSEL, 0, 0) };
        if selected < 0 {
            return Err("select an escape binding first".to_owned());
        }
        let mut bindings = settings::load_escape_bindings()?;
        if bindings.len() == 1 {
            return Err("at least one escape binding is required".to_owned());
        }
        bindings.remove(selected as usize);
        settings::save_escape_bindings(&bindings)?;
        refresh_input_settings(state)?;
        Ok::<_, String>("Escape binding removed".to_owned())
    })();
    set_text(
        state.summary,
        &result.unwrap_or_else(|error| format!("Input settings failed: {error}")),
    );
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

fn remember_board(state: &mut UiState, board: &serial::PortInfo, fingerprint: &[u8; 32]) {
    serial::remember_board(board, fingerprint);
    if let Ok(ports) = serial::enumerate() {
        update_port_list(state, ports);
    }
}

fn open_board(state: &mut UiState, board: &serial::PortInfo) -> Result<serial::Port, String> {
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
    let mut port = serial::Port::open(&board.name)?;
    if !board.recognized {
        let status = port.status()?;
        remember_board(state, board, &status.identity_fingerprint);
    }
    Ok(port)
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
        let board = selected_port(state)?;
        let status = port.status()?;
        remember_board(state, &board, &status.identity_fingerprint);
        set_led_fields(state, &status);
        Ok(status_text(&board.label, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("Error: {error}")),
    }
    mark_wifi_scan_summary_stale(state);
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
        let board = selected_port(state)?;
        port.mutate(payload.as_slice(), &id, WIFI_SET_COMMAND)?;
        let status = port.status()?;
        remember_board(state, &board, &status.identity_fingerprint);
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
    mark_wifi_scan_summary_stale(state);
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
        let board = selected_port(state)?;
        port.mutate(&payload, &id, LED_SET_COMMAND)?;
        let status = port.status()?;
        remember_board(state, &board, &status.identity_fingerprint);
        set_led_fields(state, &status);
        Ok(status_text(&board.label, &status))
    })();
    match result {
        Ok(value) => set_text(state.summary, &value),
        Err(error) => set_text(state.summary, &format!("RGB update failed: {error}")),
    }
    mark_wifi_scan_summary_stale(state);
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

fn finish_wifi_scan(state: &mut UiState, result: Result<Vec<String>, String>) {
    let update_summary = !state.wifi_scan_summary_stale;
    state.wifi_scan_rx = None;
    state.wifi_scan_summary_stale = false;
    unsafe { KillTimer(state.window, WIFI_SCAN_TIMER) };
    set_text(state.wifi_scan_button, "Refresh list");

    let current = text(state.ssid);
    match result {
        Ok(ssids) => {
            unsafe { SendMessageW(state.ssid, CB_RESETCONTENT, 0, 0) };
            for ssid in &ssids {
                add_combo_item(state.ssid, ssid);
            }
            set_text(state.ssid, &current);
            if update_summary {
                set_text(
                    state.summary,
                    &format!(
                        "Loaded {} visible SSID(s). You can also type a hidden SSID.",
                        ssids.len()
                    ),
                );
            }
        }
        Err(error) if update_summary => set_text(
            state.summary,
            &format!("Wi-Fi list unavailable; type SSID manually ({error})."),
        ),
        Err(_) => {}
    }
}

fn mark_wifi_scan_summary_stale(state: &mut UiState) {
    if state.wifi_scan_rx.is_some() {
        state.wifi_scan_summary_stale = true;
    }
}

fn start_wifi_scan(state: &mut UiState) {
    if state.wifi_scan_rx.is_some() {
        return;
    }
    if unsafe { SetTimer(state.window, WIFI_SCAN_TIMER, WIFI_SCAN_INTERVAL_MS, None) } == 0 {
        set_text(state.summary, "Could not start Wi-Fi refresh timer.");
        return;
    }

    let (sender, receiver) = mpsc::channel();
    state.wifi_scan_rx = Some(receiver);
    state.wifi_scan_frame = 1;
    state.wifi_scan_summary_stale = false;
    set_text(state.wifi_scan_button, "Refreshing.");
    set_text(state.summary, "Scanning host Wi-Fi networks…");
    unsafe { UpdateWindow(state.window) };

    if let Err(error) = std::thread::Builder::new()
        .name("wifi-scan".into())
        .spawn(move || {
            let _ = sender.send(wifi_networks::visible_ssids());
        })
    {
        finish_wifi_scan(state, Err(format!("cannot start Wi-Fi scan: {error}")));
    }
}

fn poll_wifi_scan(state: &mut UiState) {
    let result = match state.wifi_scan_rx.as_ref().map(Receiver::try_recv) {
        Some(Ok(result)) => Some(result),
        Some(Err(TryRecvError::Disconnected)) => {
            Some(Err("Wi-Fi scan stopped unexpectedly".into()))
        }
        Some(Err(TryRecvError::Empty)) => None,
        None => return,
    };
    if let Some(result) = result {
        finish_wifi_scan(state, result);
    } else {
        set_text(
            state.wifi_scan_button,
            &format!("Refreshing{}", ".".repeat(state.wifi_scan_frame as usize)),
        );
        state.wifi_scan_frame = state.wifi_scan_frame % 3 + 1;
    }
}

fn rescan(state: &mut UiState) {
    match serial::enumerate() {
        Ok(ports) => update_port_list(state, ports),
        Err(error) => set_text(state.summary, &format!("Port scan failed: {error}")),
    }
}

fn selected_target_name(state: &UiState) -> Option<&str> {
    state
        .selected_target
        .as_ref()
        .map(|target| target.name.as_str())
}

fn set_tray_state(state: &mut UiState, tray_state: TrayState) {
    state.tray_state = tray_state;
    update_tray(&state.tray, tray_state, selected_target_name(state));
}

fn start_tray_session(state: &mut UiState, selection: targets::Target) {
    if state.tray_session.is_some() {
        state.pending_target = Some(selection);
        disconnect_tray_session(state, true);
        return;
    }
    set_tray_state(state, TrayState::Starting);
    let target = match targets::load().and_then(|saved| selection.resolve_selection(saved)) {
        Ok(target) => target,
        Err(error) => {
            tray_notice(&state.tray, "kvmctl unavailable", &error);
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
    };
    state.selected_target = Some(target.clone());
    set_tray_state(state, TrayState::Starting);
    let credential = match credentials::ClientCredential::load_existing(false) {
        Ok(credential) if credential.fingerprint == target.client_fingerprint => credential,
        Ok(_) => {
            tray_notice(
                &state.tray,
                "kvmctl unavailable",
                "Saved target belongs to another local enrollment.",
            );
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
        Err(error) => {
            tray_notice(&state.tray, "kvmctl unavailable", &error);
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
    };
    let service = match target_service(&target, None) {
        Ok(service) => service,
        Err(error) => {
            tray_notice(&state.tray, "kvmctl unavailable", &error);
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
    };
    let heartbeat_mode = match discovery::compatibility_mode(&service) {
        Ok(mode) => mode,
        Err(error) => {
            tray_notice(&state.tray, "Firmware upgrade required", &error);
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
    };
    let status = saved_status(&target);
    let control = match transport::SessionControl::new_live(transport::SessionMode::Tray) {
        Ok(control) => control,
        Err(error) => {
            tray_notice(&state.tray, "kvmctl unavailable", &error.to_string());
            set_tray_state(state, TrayState::Unavailable);
            return;
        }
    };
    let worker_control = control.clone();
    let (sender, events) = mpsc::channel();
    let worker = match std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let event_sender = sender.clone();
            let result = transport::run_session_with_heartbeat_ack(
                service,
                status,
                credential,
                worker_control,
                true,
                heartbeat_mode == discovery::HeartbeatMode::Acknowledged,
                move |event| {
                    let _ = event_sender.send(TrayMessage::Event(event));
                },
            )
            .map(|_| ())
            .map_err(|error| error.to_string());
            let _ = sender.send(TrayMessage::Finished(result));
        }) {
        Ok(worker) => worker,
        Err(error) => {
            set_tray_state(state, TrayState::Unavailable);
            tray_notice(
                &state.tray,
                "kvmctl unavailable",
                &format!("Cannot start session worker: {error}"),
            );
            return;
        }
    };
    state.tray_session = Some(TraySession {
        target: target.name.clone(),
        control,
        events,
        worker,
    });
    set_tray_state(state, TrayState::Connecting);
    tray_notice(
        &state.tray,
        "kvmctl connecting",
        &format!("{} — input remains local.", target.name),
    );
    unsafe { SetTimer(state.window, SESSION_TIMER, 100, None) };
}

fn disconnect_tray_session(state: &mut UiState, preserve_pending_target: bool) {
    if !preserve_pending_target {
        state.pending_target = None;
    }
    if let Some(session) = &state.tray_session {
        session.control.cancel();
        tray_notice(
            &state.tray,
            "kvmctl disconnecting",
            "Releasing lease and closing session.",
        );
    }
    if state.tray_session.is_none() {
        state.selected_target = None;
        set_tray_state(state, TrayState::NoTarget);
    }
}

fn takeover_prompt(
    window: HWND,
    control: transport::SessionControl,
    target: String,
    request_id: [u8; 16],
    requester: [u8; 32],
) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
    };
    let message = wide(&format!(
        "Input is local on {}. Allow enrolled controller {} to take ownership?",
        target,
        requester[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    let title = wide("kvmctl takeover request");
    let answer = unsafe {
        MessageBoxW(
            window,
            message.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        )
    };
    control.respond_to_takeover(request_id, answer == IDYES);
}

fn apply_session_event(
    state: &mut UiState,
    event: transport::SessionEvent,
) -> Option<(transport::SessionControl, String, [u8; 16], [u8; 32])> {
    let target = selected_target_name(state).map(str::to_owned);
    let mut prompt = None;
    match event {
        transport::SessionEvent::Connecting => set_tray_state(state, TrayState::Connecting),
        transport::SessionEvent::ConnectedLocal => {
            set_tray_state(state, TrayState::ConnectedLocal);
            tray_notice(
                &state.tray,
                "kvmctl connected/local",
                &format!(
                    "{} — input remains local.",
                    target.as_deref().unwrap_or("target")
                ),
            );
        }
        transport::SessionEvent::InputLocalConfirmed => {
            if !matches!(state.tray_state, TrayState::ConnectionDegraded) {
                set_tray_state(state, TrayState::ConnectedLocal);
            }
            tray_notice(
                &state.tray,
                "kvmctl input local",
                &format!(
                    "{} — remote input released.",
                    target.as_deref().unwrap_or("target")
                ),
            );
        }
        transport::SessionEvent::InputRemoteConfirmed => {
            set_tray_state(state, TrayState::RemoteControl);
            tray_notice(
                &state.tray,
                "kvmctl input remote",
                &format!(
                    "{} — keyboard and mouse routed remotely.",
                    target.as_deref().unwrap_or("target")
                ),
            );
        }
        transport::SessionEvent::ConnectionDegraded { .. } => {
            set_tray_state(state, TrayState::ConnectionDegraded);
        }
        transport::SessionEvent::ConnectionRecovered { .. } => {
            let remote = state
                .tray_session
                .as_ref()
                .is_some_and(|session| session.control.input_is_remote());
            set_tray_state(
                state,
                if remote {
                    TrayState::RemoteControl
                } else {
                    TrayState::ConnectedLocal
                },
            );
        }
        transport::SessionEvent::TakeoverPending {
            request_id,
            requester,
            ..
        } => {
            if let Some((control, target_name)) = state
                .tray_session
                .as_ref()
                .map(|session| (session.control.clone(), session.target.clone()))
            {
                if matches!(
                    state.tray_state,
                    TrayState::ConnectedLocal | TrayState::ConnectionDegraded
                ) {
                    set_tray_state(state, TrayState::OwnershipPending);
                    prompt = Some((control, target_name, request_id, requester));
                } else {
                    control.respond_to_takeover(request_id, false);
                }
            }
        }
        transport::SessionEvent::Recovering { seconds_remaining } => {
            set_tray_state(state, TrayState::Recovering);
            tray_notice(
                &state.tray,
                "kvmctl recovering",
                &format!(
                    "{} — input remains local; {seconds_remaining}s left.",
                    target.as_deref().unwrap_or("target")
                ),
            );
        }
        transport::SessionEvent::Unavailable { reason } => {
            set_tray_state(state, TrayState::Unavailable);
            tray_notice(&state.tray, "kvmctl unavailable", &reason);
        }
        transport::SessionEvent::OwnershipLost { reason } => {
            set_tray_state(state, TrayState::OwnershipLost);
            tray_notice(&state.tray, "kvmctl ownership lost", &reason);
        }
        transport::SessionEvent::Notice(message) => {
            if (message.starts_with("Takeover accepted")
                || message.starts_with("Takeover rejected"))
                && !matches!(state.tray_state, TrayState::ConnectionDegraded)
            {
                set_tray_state(state, TrayState::ConnectedLocal);
            }
            tray_notice(&state.tray, "kvmctl", &message);
        }
        transport::SessionEvent::Disconnected => {
            if state.selected_target.is_none() {
                set_tray_state(state, TrayState::NoTarget);
            }
        }
    }
    prompt
}

fn poll_session(
    state: &mut UiState,
) -> Option<(transport::SessionControl, String, [u8; 16], [u8; 32])> {
    let mut finished = None;
    let mut events = Vec::new();
    let mut prompt = None;
    if let Some(session) = state.tray_session.as_ref() {
        loop {
            match session.events.try_recv() {
                Ok(TrayMessage::Event(event)) => events.push(event),
                Ok(TrayMessage::Finished(result)) => {
                    finished = Some(result);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("Session worker stopped unexpectedly".into()));
                    break;
                }
            }
        }
    }
    for event in events {
        let next_prompt = apply_session_event(state, event);
        prompt = prompt.or(next_prompt);
    }
    if let Some(result) = finished {
        if let Some(session) = state.tray_session.take() {
            let _ = session.worker.join();
        }
        unsafe { KillTimer(state.window, SESSION_TIMER) };
        let pending_target = state.pending_target.take();
        state.selected_target = None;
        set_tray_state(state, TrayState::NoTarget);
        match (result, pending_target) {
            (Err(error), _) => {
                set_tray_state(state, TrayState::Unavailable);
                tray_notice(&state.tray, "kvmctl unavailable", &error);
            }
            (Ok(()), Some(target)) => start_tray_session(state, target),
            (Ok(()), None) => {}
        }
    }
    prompt
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if TASKBAR_CREATED
        .get()
        .is_some_and(|registered| *registered == message)
    {
        STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                let target = selected_target_name(state).map(str::to_owned);
                add_tray(&state.tray, state.tray_state, target.as_deref());
            }
        });
        return 0;
    }
    if message == WM_CLOSE {
        unsafe { ShowWindow(window, 0) };
        return 0;
    }
    if message == WM_DESTROY {
        let state = STATE.with(|cell| cell.borrow_mut().take());
        if let Some(state) = state {
            unsafe { KillTimer(state.window, WIFI_SCAN_TIMER) };
            unsafe { KillTimer(state.window, SESSION_TIMER) };
            remove_tray(&state.tray);
            if let Some(session) = state.tray_session {
                session.control.cancel();
                if session.worker.join().is_err() {
                    log::error!("tray session worker panicked during shutdown");
                }
            }
        }
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    if message == WM_TIMER && wparam == WIFI_SCAN_TIMER {
        STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                poll_wifi_scan(state);
            }
        });
        return 0;
    }
    if message == WM_TIMER && wparam == SESSION_TIMER {
        let prompt = STATE.with(|cell| cell.borrow_mut().as_mut().and_then(poll_session));
        if let Some((control, target, request_id, requester)) = prompt {
            takeover_prompt(window, control, target, request_id, requester);
        }
        return 0;
    }
    if message == TRAY_MESSAGE {
        if lparam as u32 == WM_RBUTTONUP || lparam as u32 == WM_LBUTTONUP {
            let has_disconnect = STATE.with(|cell| {
                cell.borrow().as_ref().is_some_and(|state| {
                    state.tray_session.is_some() || state.selected_target.is_some()
                })
            });
            if let Some(target) = show_tray_menu(window, has_disconnect) {
                STATE.with(|cell| {
                    if let Some(state) = cell.borrow_mut().as_mut() {
                        start_tray_session(state, target);
                    }
                });
            }
        }
        return 0;
    }
    if message == WM_COMMAND {
        let id = (wparam & 0xffff) as u16;
        let notification = ((wparam >> 16) & 0xffff) as u16;
        if id == DISCONNECT_MENU {
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    disconnect_tray_session(state, false);
                }
            });
            return 0;
        }
        if id == SETUP_MENU {
            unsafe {
                ShowWindow(window, 5);
                UpdateWindow(window);
            }
            return 0;
        }
        if id == QUIT_MENU {
            unsafe { DestroyWindow(window) };
            return 0;
        }
        if !matches!(
            id,
            PORTS
                | WIFI_SAVE
                | LED_SAVE
                | REFRESH
                | RESCAN
                | WIFI_SCAN
                | MOTION_RATE_SAVE
                | ESCAPE_ADD
                | ESCAPE_REMOVE
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
                (WIFI_SCAN, 0) => start_wifi_scan(state),
                (LED_SAVE, 0) => save_led(state),
                (REFRESH, 0) | (PORTS, CBN_SELCHANGE_U16) => refresh(state),
                (RESCAN, 0) => rescan(state),
                (MOTION_RATE_SAVE, 0) => save_motion_rate(state),
                (ESCAPE_ADD, 0) => add_escape_binding(state),
                (ESCAPE_REMOVE, 0) => remove_escape_binding(state),
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

fn create_controls(instance: HINSTANCE, window: HWND) -> Result<UiState, String> {
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
    let wifi_scan_button = control(
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
        "Brightness",
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
    control(instance, window, "STATIC", "%", 0, 0, 297, 322, 15, 22);
    control(instance, window, "STATIC", "RRGGBB", 0, 0, 315, 322, 55, 22);
    let color = control(
        instance,
        window,
        "EDIT",
        "FFFFFF",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        COLOR,
        380,
        318,
        90,
        25,
    );
    control(
        instance, window, "BUTTON", "Save RGB", basic, LED_SAVE, 480, 316, 90, 29,
    );

    control(
        instance,
        window,
        "BUTTON",
        "Input settings",
        BS_GROUPBOX,
        0,
        18,
        410,
        560,
        145,
    );
    control(
        instance,
        window,
        "STATIC",
        "Motion rate",
        0,
        0,
        30,
        438,
        95,
        22,
    );
    let motion_rate = control(
        instance,
        window,
        "EDIT",
        "125",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        MOTION_RATE,
        125,
        434,
        65,
        25,
    );
    control(instance, window, "STATIC", "Hz", 0, 0, 195, 438, 25, 22);
    control(
        instance,
        window,
        "BUTTON",
        "Save rate",
        basic,
        MOTION_RATE_SAVE,
        230,
        432,
        90,
        29,
    );
    control(
        instance,
        window,
        "STATIC",
        "Escape combinations",
        0,
        0,
        30,
        472,
        125,
        22,
    );
    let escape_binding = control(
        instance,
        window,
        "EDIT",
        "CTRL+SHIFT+F10",
        WS_BORDER | ES_AUTOHSCROLL | basic,
        ESCAPE_BINDING,
        155,
        468,
        180,
        25,
    );
    control(
        instance, window, "BUTTON", "Add", basic, ESCAPE_ADD, 345, 466, 70, 29,
    );
    let escape_bindings = control(
        instance,
        window,
        "LISTBOX",
        "",
        WS_BORDER | WS_VSCROLL | LBS_NOTIFY | basic,
        ESCAPE_BINDINGS,
        30,
        500,
        305,
        42,
    );
    control(
        instance,
        window,
        "BUTTON",
        "Remove",
        basic,
        ESCAPE_REMOVE,
        345,
        500,
        70,
        29,
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
        wifi_scan_button,
        motion_rate,
        escape_bindings,
        escape_binding,
        wifi_scan_rx: None,
        wifi_scan_frame: 1,
        wifi_scan_summary_stale: false,
        tray: TrayIcon::new(window)?,
        tray_session: None,
        tray_state: TrayState::NoTarget,
        selected_target: None,
        pending_target: None,
    };
    if let Err(error) = refresh_input_settings(&state) {
        set_text(
            state.summary,
            &format!("Input settings unavailable: {error}"),
        );
    }
    match serial::enumerate() {
        Ok(ports) => update_port_list(&mut state, ports),
        Err(error) => set_text(state.summary, &format!("Board scan failed: {error}")),
    }
    Ok(state)
}

pub fn run() -> Result<(), String> {
    transport::init_logging();
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    if taskbar_created == 0 {
        return Err("cannot register TaskbarCreated message".into());
    }
    let _ = TASKBAR_CREATED.set(taskbar_created);
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
            610,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err("cannot create control window".into());
    }
    osd::initialize()?;
    let state = create_controls(instance, window)?;
    let first_target = targets::load()
        .ok()
        .and_then(|targets| targets.into_iter().next());
    add_tray(
        &state.tray,
        state.tray_state,
        first_target.as_ref().map(|target| target.name.as_str()),
    );
    STATE.with(|cell| *cell.borrow_mut() = Some(state));
    unsafe {
        // Tray is resident; setup window opens from its context menu.
        ShowWindow(window, 0);
        UpdateWindow(window);
    }
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            start_wifi_scan(state);
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
