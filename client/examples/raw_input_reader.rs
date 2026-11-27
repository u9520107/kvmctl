//! Independent Windows Raw Input observer for physical routing checks.

#[cfg(not(windows))]
fn main() {
    eprintln!("Raw Input requires Windows.");
}

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    windows::run()
}

#[cfg(windows)]
mod windows {
    use std::cell::Cell;
    use std::io;
    use std::mem::size_of;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWKEYBOARD,
        RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
        RegisterRawInputDevices,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG,
        PostQuitMessage, RIM_INPUT, RegisterClassW, TranslateMessage, WM_DESTROY, WM_INPUT,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    thread_local! {
        static HELD_SHORTCUT_KEYS: Cell<u8> = const { Cell::new(0) };
    }

    fn shortcut_key(key: RAWKEYBOARD) -> (&'static str, u8) {
        const RI_KEY_E0: u16 = 0x02;
        match key.VKey {
            0x11 if key.Flags & RI_KEY_E0 != 0 => ("Right Ctrl", 2),
            0x11 | 0xa2 => ("Left Ctrl", 1),
            0xa3 => ("Right Ctrl", 2),
            0x10 if key.MakeCode == 0x36 => ("Right Shift", 8),
            0x10 | 0xa0 => ("Left Shift", 4),
            0xa1 => ("Right Shift", 8),
            0x79 => ("F10", 16),
            _ => ("Key", 0),
        }
    }

    fn update_shortcut(held: &Cell<u8>, key_bit: u8, released: bool) -> bool {
        let previous = held.get();
        held.set(if released {
            previous & !key_bit
        } else {
            previous | key_bit
        });
        key_bit == 16 && !released && previous & 16 == 0 && previous & 3 != 0 && previous & 12 != 0
    }

    pub(super) fn run() -> io::Result<()> {
        let class_name: Vec<_> = "KvmCtlRawInputReader\0".encode_utf16().collect();
        let title: Vec<_> = "Raw Input reader (see terminal)\0".encode_utf16().collect();
        let instance = unsafe { GetModuleHandleW(null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                420,
                160,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        let devices = [2, 6].map(|usage| RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: usage,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        });
        if unsafe {
            RegisterRawInputDevices(
                devices.as_ptr(),
                devices.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        println!(
            "Watching keyboard and mouse WM_INPUT. Focus this window or another app. Close this window to exit."
        );
        let mut message = MSG::default();
        loop {
            match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
                -1 => return Err(io::Error::last_os_error()),
                0 => return Ok(()),
                _ => unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                },
            }
        }
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_INPUT => observe(wparam, lparam as HRAWINPUT),
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                return 0;
            }
            _ => {}
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    fn observe(wparam: WPARAM, input: HRAWINPUT) {
        let mut raw = RAWINPUT::default();
        let mut bytes = size_of::<RAWINPUT>() as u32;
        let header_size = size_of::<RAWINPUTHEADER>();
        let read = unsafe {
            GetRawInputData(
                input,
                RID_INPUT,
                (&mut raw as *mut RAWINPUT).cast(),
                &mut bytes,
                header_size as u32,
            )
        } as usize;
        if !(header_size..=size_of::<RAWINPUT>()).contains(&read) {
            return;
        }
        let delivery = if wparam & 0xff == RIM_INPUT as usize {
            "foreground"
        } else {
            "background"
        };
        let device = raw.header.hDevice;
        match raw.header.dwType {
            RIM_TYPEKEYBOARD if read >= header_size + size_of::<RAWKEYBOARD>() => {
                let key = unsafe { raw.data.keyboard };
                const RI_KEY_BREAK: u16 = 0x01;
                let released = key.Flags & RI_KEY_BREAK != 0;
                let action = if released { "up" } else { "down" };
                let (name, bit) = shortcut_key(key);
                println!(
                    "{delivery} {device:p} {name} {action} (vk={:02x} scan={:02x} flags={:04x})",
                    key.VKey, key.MakeCode, key.Flags
                );
                if HELD_SHORTCUT_KEYS.with(|held| update_shortcut(held, bit, released)) {
                    println!("{delivery} default escape chord observed locally (Ctrl+Shift+F10)");
                }
            }
            RIM_TYPEMOUSE if read >= header_size + size_of::<RAWMOUSE>() => {
                let mouse = unsafe { raw.data.mouse };
                let buttons = unsafe { mouse.Anonymous.ulButtons };
                println!(
                    "{delivery} {device:p} mouse x={} y={} flags={:04x} buttons={buttons:08x}",
                    mouse.lLastX, mouse.lLastY, mouse.usFlags
                );
            }
            _ => {}
        }
    }

    #[test]
    fn default_chord_requires_both_modifiers_and_ignores_f10_repeat() {
        let held = Cell::new(0);
        assert!(!update_shortcut(&held, 16, false));
        assert!(!update_shortcut(&held, 16, true));
        assert!(!update_shortcut(&held, 2, false));
        assert!(!update_shortcut(&held, 4, false));
        assert!(update_shortcut(&held, 16, false));
        assert!(!update_shortcut(&held, 16, false));
        assert!(!update_shortcut(&held, 16, true));
        assert!(update_shortcut(&held, 16, false));
    }
}
