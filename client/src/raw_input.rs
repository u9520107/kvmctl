use anyhow::{Context, Result, bail, ensure};
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Parent, CR_SUCCESS, HDEVINFO, MAX_DEVICE_ID_LEN,
    SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA, SetupDiCreateDeviceInfoList,
    SetupDiDestroyDeviceInfoList, SetupDiGetDeviceInterfaceDetailW, SetupDiOpenDeviceInterfaceW,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_WINDOW_HANDLE, GetLastError, HANDLE, HINSTANCE, HWND,
    INVALID_HANDLE_VALUE, POINT,
};
use windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, HRAWINPUT, MOUSE_MOVE_ABSOLUTE,
    RAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RID_HEADER, RID_INPUT,
    RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICENAME, RIM_TYPEKEYBOARD,
    RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GIDC_REMOVAL, GetCursorPos,
    GetForegroundWindow, GetMessageW, MSG, PostMessageW, RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1,
    RI_MOUSE_HWHEEL, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_WHEEL,
    RegisterClassW, SetForegroundWindow, UnregisterClassW, WHEEL_DELTA, WM_APP, WM_CLOSE, WM_INPUT,
    WM_INPUT_DEVICE_CHANGE, WNDCLASSW, WS_CAPTION, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    WindowFromPoint,
};

const STOP: u32 = WM_APP + 1;
const WINDOW_CLASS: *const u16 = windows_sys::core::w!("KvmCtlHidBenchWindow");

pub(crate) struct Observer {
    window: usize,
    complete: Receiver<()>,
    worker: Option<JoinHandle<Result<Report>>>,
}

impl Observer {
    pub(crate) fn start(vid: u16, pid: u16, serial: &str) -> Result<Self> {
        let identity = format!("USB\\VID_{vid:04X}&PID_{pid:04X}\\{serial}");
        let (ready_sender, ready) = mpsc::channel();
        let (complete_sender, complete) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("kvmctl-raw-input".into())
            .spawn(move || observe(&identity, ready_sender, complete_sender))?;
        let window = match ready.recv() {
            Ok(window) => window,
            Err(_) => {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("Raw Input observer panicked"))??;
                bail!("Raw Input observer stopped before startup");
            }
        };
        Ok(Self {
            window,
            complete,
            worker: Some(worker),
        })
    }

    pub(crate) fn verify(mut self) -> Result<()> {
        let _ = self.complete.recv_timeout(Duration::from_secs(5));
        let report = self.stop()?.context("Raw Input observer already stopped")?;
        ensure!(
            report.complete(),
            "incomplete board Raw Input script: {report:?}"
        );
        eprintln!(
            "M1.4 RAW INPUT shift=make-break key_a=make-break left_button=down-up vertical={} horizontal={} relative=({},{}) release=true",
            report.vertical, report.horizontal, report.motion.0, report.motion.1,
        );
        Ok(())
    }

    pub(crate) fn check_focus(&self) -> Result<()> {
        check_focus(self.window as HWND)
    }

    fn stop(&mut self) -> Result<Option<Report>> {
        let Some(worker) = self.worker.take() else {
            return Ok(None);
        };
        if !worker.is_finished() && unsafe { PostMessageW(self.window as HWND, STOP, 0, 0) } == 0 {
            // A failed worker may have destroyed the window just before this message.
            let error = io::Error::last_os_error();
            if !worker.is_finished()
                && error.raw_os_error() != Some(ERROR_INVALID_WINDOW_HANDLE as i32)
            {
                return Err(error).context("stop Raw Input observer");
            }
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Raw Input observer panicked"))?
            .map(Some)
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct Window {
    handle: HWND,
    instance: HINSTANCE,
}

impl Window {
    fn create() -> Result<Self> {
        let cursor = cursor_position()?;
        let instance = unsafe { GetModuleHandleW(null()) };
        ensure!(
            !instance.is_null(),
            "cannot load HID bench application instance"
        );
        let class = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: instance,
            hbrBackground: (COLOR_WINDOW + 1) as _,
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        ensure!(
            unsafe { RegisterClassW(&class) } != 0,
            "register Raw Input window class: {}",
            io::Error::last_os_error(),
        );
        let window = Self {
            instance,
            handle: unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST,
                    WINDOW_CLASS,
                    windows_sys::core::w!("kvmctl HID bench - keep keyboard and mouse idle"),
                    WS_POPUP | WS_CAPTION | WS_VISIBLE,
                    cursor.x - 210,
                    cursor.y - 80,
                    420,
                    160,
                    null_mut(),
                    null_mut(),
                    instance,
                    null(),
                )
            },
        };
        ensure!(
            !window.handle.is_null(),
            "create Raw Input window: {}",
            io::Error::last_os_error()
        );
        Ok(window)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let _ = register(null_mut(), RIDEV_REMOVE);
            unsafe { DestroyWindow(self.handle) };
        }
        unsafe { UnregisterClassW(WINDOW_CLASS, self.instance) };
    }
}

fn register(window: HWND, flags: u32) -> Result<()> {
    let devices = [2, 6].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: flags,
        hwndTarget: window,
    });
    ensure!(
        unsafe { RegisterRawInputDevices(devices.as_ptr(), 2, size_of::<RAWINPUTDEVICE>() as u32) }
            != 0,
        "register Raw Input keyboard and mouse: {}",
        io::Error::last_os_error(),
    );
    Ok(())
}

fn observe(identity: &str, ready: Sender<usize>, complete: Sender<()>) -> Result<Report> {
    let window = Window::create()?;
    register(window.handle, RIDEV_INPUTSINK | RIDEV_DEVNOTIFY)?;
    let (keyboard, mouse) = board_collections(identity)?;
    unsafe { SetForegroundWindow(window.handle) };
    check_focus(window.handle)?;
    eprintln!("M1.4 RAW INPUT device={identity} keyboard=matched mouse=matched");
    ready
        .send(window.handle as usize)
        .context("Raw Input startup receiver closed")?;
    drop(ready);
    let mut report = Report::default();
    let mut complete = Some(complete);
    let mut message = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
            -1 => return Err(io::Error::last_os_error()).context("read Raw Input message"),
            0 => break,
            _ => {}
        }
        match message.message {
            STOP => break,
            WM_CLOSE => bail!("HID bench window was closed"),
            WM_INPUT_DEVICE_CHANGE if message.wParam == GIDC_REMOVAL as usize => {
                let device = message.lParam as HANDLE;
                ensure!(
                    device != keyboard && device != mouse,
                    "bench USB HID collection disconnected"
                );
            }
            WM_INPUT => {
                let result = collect(
                    message.lParam as HRAWINPUT,
                    keyboard,
                    mouse,
                    window.handle,
                    &mut report,
                );
                // Required for foreground WM_INPUT, including ignored packets.
                unsafe {
                    DefWindowProcW(
                        message.hwnd,
                        message.message,
                        message.wParam,
                        message.lParam,
                    )
                };
                result?;
                if report.complete()
                    && let Some(sender) = complete.take()
                {
                    let _ = sender.send(());
                }
            }
            _ => {
                unsafe { DispatchMessageW(&message) };
            }
        }
    }
    Ok(report)
}

fn cursor_position() -> Result<POINT> {
    let mut cursor = POINT::default();
    ensure!(
        unsafe { GetCursorPos(&mut cursor) } != 0,
        "read pointer position: {}",
        io::Error::last_os_error()
    );
    Ok(cursor)
}

fn check_focus(window: HWND) -> Result<()> {
    ensure!(
        unsafe { GetForegroundWindow() } == window,
        "the HID bench window does not have foreground focus"
    );
    let cursor = cursor_position()?;
    ensure!(
        unsafe { WindowFromPoint(cursor) } == window,
        "the pointer is outside the HID bench window"
    );
    Ok(())
}

struct DeviceInfo(HDEVINFO);

impl Drop for DeviceInfo {
    fn drop(&mut self) {
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn board_collections(identity: &str) -> Result<(HANDLE, HANDLE)> {
    let info = unsafe { SetupDiCreateDeviceInfoList(null(), null_mut()) };
    ensure!(
        info != INVALID_HANDLE_VALUE as HDEVINFO,
        "create USB device info set: {}",
        io::Error::last_os_error()
    );
    let info = DeviceInfo(info);
    let entry_size = size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0;
    ensure!(
        unsafe { GetRawInputDeviceList(null_mut(), &mut count, entry_size) } == 0,
        "get Raw Input device count: {}",
        io::Error::last_os_error(),
    );
    ensure!(count > 0, "Windows reports no Raw Input devices");
    let mut entries = vec![RAWINPUTDEVICELIST::default(); count as usize];
    let read = unsafe { GetRawInputDeviceList(entries.as_mut_ptr(), &mut count, entry_size) };
    ensure!(
        read <= entries.len() as u32,
        "read Raw Input device list: {}",
        io::Error::last_os_error()
    );
    let mut keyboard = None;
    let mut mouse = None;
    let mut parent = None;
    for entry in &entries[..read as usize] {
        let selected = match entry.dwType {
            RIM_TYPEKEYBOARD => &mut keyboard,
            RIM_TYPEMOUSE => &mut mouse,
            _ => continue,
        };
        if let Some(board) = usb_parent(info.0, entry.hDevice, identity) {
            ensure!(
                parent.is_none_or(|parent| parent == board),
                "ambiguous bench USB device identity {identity}"
            );
            ensure!(
                selected.is_none(),
                "duplicate Raw Input collection for {identity}"
            );
            parent = Some(board);
            *selected = Some(entry.hDevice);
        }
    }
    keyboard
        .zip(mouse)
        .with_context(|| format!("missing native USB keyboard or mouse collection for {identity}"))
}

fn usb_parent(info: HDEVINFO, raw_device: HANDLE, identity: &str) -> Option<u32> {
    let mut count = 0;
    if unsafe { GetRawInputDeviceInfoW(raw_device, RIDI_DEVICENAME, null_mut(), &mut count) } != 0
        || count == 0
    {
        return None;
    }
    let mut name = vec![0u16; count as usize];
    let read = unsafe {
        GetRawInputDeviceInfoW(
            raw_device,
            RIDI_DEVICENAME,
            name.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if read == u32::MAX || !name.contains(&0) {
        return None;
    }
    let mut interface = SP_DEVICE_INTERFACE_DATA {
        cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    if unsafe { SetupDiOpenDeviceInterfaceW(info, name.as_ptr(), 0, &mut interface) } == 0 {
        return None;
    }
    let mut device = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    // A null detail buffer still returns DeviceInfoData with ERROR_INSUFFICIENT_BUFFER.
    let result = unsafe {
        SetupDiGetDeviceInterfaceDetailW(info, &interface, null_mut(), 0, null_mut(), &mut device)
    };
    if result == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut node = device.DevInst;
    for _ in 0..16 {
        let mut id = [0u16; MAX_DEVICE_ID_LEN as usize];
        if unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) } != CR_SUCCESS {
            break;
        }
        let len = id.iter().position(|&ch| ch == 0)?;
        if String::from_utf16_lossy(&id[..len]).eq_ignore_ascii_case(identity) {
            return Some(node);
        }
        let mut parent = 0;
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            break;
        }
        node = parent;
    }
    None
}

fn collect(
    input: HRAWINPUT,
    keyboard: HANDLE,
    mouse: HANDLE,
    window: HWND,
    report: &mut Report,
) -> Result<()> {
    let header_size = size_of::<RAWINPUTHEADER>() as u32;
    let mut bytes = header_size;
    let mut header = RAWINPUTHEADER::default();
    if unsafe {
        GetRawInputData(
            input,
            RID_HEADER,
            (&mut header as *mut RAWINPUTHEADER).cast(),
            &mut bytes,
            header_size,
        )
    } != header_size
    {
        // The sender cannot be identified. Do not read a body.
        return Ok(());
    }
    if header.hDevice != keyboard && header.hDevice != mouse {
        return Ok(());
    }
    check_focus(window)?;
    let mut raw = RAWINPUT::default();
    bytes = size_of::<RAWINPUT>() as u32;
    let read = unsafe {
        GetRawInputData(
            input,
            RID_INPUT,
            (&mut raw as *mut RAWINPUT).cast(),
            &mut bytes,
            header_size,
        )
    };
    ensure!(
        read == header.dwSize && read <= size_of::<RAWINPUT>() as u32,
        "read board Raw Input packet"
    );
    ensure!(
        raw.header.hDevice == header.hDevice && raw.header.dwType == header.dwType,
        "board Raw Input header changed"
    );
    match header.dwType {
        RIM_TYPEKEYBOARD if header.hDevice == keyboard => {
            let key = unsafe { raw.data.keyboard };
            ensure!(
                key.Flags as u32 & (RI_KEY_E0 | RI_KEY_E1) == 0,
                "unexpected extended board key"
            );
            let pressed = key.Flags as u32 & RI_KEY_BREAK == 0;
            match key.MakeCode {
                0x2a => transition(&mut report.shift, pressed, "left Shift"),
                0x1e => transition(&mut report.key_a, pressed, "A"),
                _ => bail!("unexpected board key scan code {:#x}", key.MakeCode),
            }
        }
        RIM_TYPEMOUSE if header.hDevice == mouse => {
            let mouse = unsafe { raw.data.mouse };
            ensure!(
                mouse.usFlags & MOUSE_MOVE_ABSOLUTE == 0,
                "board mouse reported absolute motion"
            );
            let buttons = unsafe { mouse.Anonymous.Anonymous };
            let flags = buttons.usButtonFlags as u32;
            ensure!(
                flags
                    & !(RI_MOUSE_LEFT_BUTTON_DOWN
                        | RI_MOUSE_LEFT_BUTTON_UP
                        | RI_MOUSE_WHEEL
                        | RI_MOUSE_HWHEEL)
                    == 0,
                "unexpected board mouse flags {flags:#x}",
            );
            if flags & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
                transition(&mut report.left_button, true, "left button")?;
            }
            let delta = buttons.usButtonData as i16 as i32;
            if flags & RI_MOUSE_WHEEL != 0 {
                ensure!(
                    report.vertical == 0 && delta == WHEEL_DELTA as i32,
                    "unexpected board wheel delta {delta}"
                );
                report.vertical = delta;
            }
            if flags & RI_MOUSE_HWHEEL != 0 {
                ensure!(
                    report.horizontal == 0 && delta == -(WHEEL_DELTA as i32),
                    "unexpected board pan delta {delta}"
                );
                report.horizontal = delta;
            }
            let motion = (mouse.lLastX, mouse.lLastY);
            if motion != (0, 0) {
                ensure!(
                    report.motion == (0, 0) && motion == (12, -4),
                    "unexpected board relative motion {motion:?}"
                );
                report.motion = motion;
            }
            if flags & RI_MOUSE_LEFT_BUTTON_UP != 0 {
                transition(&mut report.left_button, false, "left button")?;
            }
            Ok(())
        }
        _ => bail!("unexpected board Raw Input collection type"),
    }
}

#[derive(Debug, Default)]
struct Report {
    shift: u8,
    key_a: u8,
    left_button: u8,
    vertical: i32,
    horizontal: i32,
    motion: (i32, i32),
}

impl Report {
    fn complete(&self) -> bool {
        self.shift == 2
            && self.key_a == 2
            && self.left_button == 2
            && self.vertical == WHEEL_DELTA as i32
            && self.horizontal == -(WHEEL_DELTA as i32)
            && self.motion == (12, -4)
    }
}

fn transition(stage: &mut u8, pressed: bool, label: &str) -> Result<()> {
    ensure!(
        matches!((*stage, pressed), (0, true) | (1, false)),
        "unexpected board {label} transition (stage={stage}, pressed={pressed})",
    );
    *stage += 1;
    Ok(())
}
