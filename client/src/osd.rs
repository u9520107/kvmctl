use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, DEFAULT_GUI_FONT, DT_CALCRECT, DT_CENTER, DT_NOPREFIX, DT_WORDBREAK,
    DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, HWND_TOPMOST, KillTimer,
    LWA_ALPHA, PostQuitMessage, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SPI_GETWORKAREA, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    SystemParametersInfoW, WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS_NAME: &str = "kvmctl-osd";
const TIMER: usize = 1;
const WIDTH: i32 = 520;
const HEIGHT: i32 = 80;
const PADDING_X: i32 = 20;
const PADDING_Y: i32 = 10;
const DEFAULT_DURATION_MS: u32 = 5_000;
const DEFAULT_OPACITY: f32 = 0.85;

struct State {
    window: HWND,
    text: Vec<u16>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        window: std::ptr::null_mut(),
        text: Vec::new(),
    }) };
}

pub(crate) fn initialize() -> Result<(), String> {
    let class_name = wide(CLASS_NAME);
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSW {
        hInstance: instance,
        lpfnWndProc: Some(window_proc),
        lpszClassName: class_name.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { RegisterClassW(&class) };
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            class_name.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err("cannot create OSD window".into());
    }
    STATE.with(|state| state.borrow_mut().window = window);
    Ok(())
}

pub(crate) fn show(title: &str, message: &str) -> Result<(), String> {
    let text = if title == "kvmctl" {
        message.to_string()
    } else if message.is_empty() {
        title.to_string()
    } else {
        format!("{title}: {message}")
    };
    let window = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.text = wide(&text);
        state.window
    });
    if window.is_null() {
        return Err("OSD is not initialized".into());
    }
    let alpha = (DEFAULT_OPACITY * 255.0).round() as u8;
    if unsafe { SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA) } == 0 {
        return Err("cannot set OSD opacity".into());
    }
    let mut area = RECT {
        right: unsafe { GetSystemMetrics(SM_CXSCREEN) },
        bottom: unsafe { GetSystemMetrics(SM_CYSCREEN) },
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut area as *mut RECT).cast(), 0);
        let x = area.left + (area.right - area.left - WIDTH) / 2;
        let y = area.bottom - HEIGHT - 32;
        let _ = SetWindowPos(
            window,
            HWND_TOPMOST,
            x,
            y,
            WIDTH,
            HEIGHT,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = SetTimer(window, TIMER, DEFAULT_DURATION_MS, None);
        let _ = InvalidateRect(window, std::ptr::null(), 1);
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => unsafe { paint(window) },
        WM_TIMER if wparam == TIMER => {
            unsafe {
                let _ = KillTimer(window, TIMER);
                let _ = ShowWindow(window, SW_HIDE);
            }
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

unsafe fn paint(window: HWND) -> LRESULT {
    let mut paint = unsafe { std::mem::zeroed() };
    let dc = unsafe { BeginPaint(window, &mut paint) };
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        let _ = GetClientRect(window, &mut rect);
        let _ = FillRect(dc, &rect, GetStockObject(BLACK_BRUSH) as _);
        let old_font = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        let _ = SetBkMode(dc, TRANSPARENT as i32);
        let _ = SetTextColor(dc, 0x00ff_ffff);
        STATE.with(|state| {
            let text = &state.borrow().text;
            let text_len = text.len().saturating_sub(1) as i32;
            let mut text_rect = RECT {
                left: rect.left + PADDING_X,
                top: rect.top + PADDING_Y,
                right: rect.right - PADDING_X,
                bottom: rect.bottom - PADDING_Y,
            };
            let text_height = DrawTextW(
                dc,
                text.as_ptr(),
                text_len,
                &mut text_rect,
                DT_CENTER | DT_WORDBREAK | DT_NOPREFIX | DT_CALCRECT,
            );
            text_rect.top = rect.top + ((rect.bottom - rect.top - text_height) / 2).max(PADDING_Y);
            text_rect.bottom = text_rect.top + text_height;
            let _ = DrawTextW(
                dc,
                text.as_ptr(),
                text_len,
                &mut text_rect,
                DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
            );
        });
        let _ = SelectObject(dc, old_font);
        let _ = EndPaint(window, &paint);
    }
    0
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
