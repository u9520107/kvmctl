use crate::settings::EscapeBinding;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, HC_ACTION, HWND_MESSAGE, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    PostThreadMessageW, SM_CXSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, SetWindowsHookExW, ShowWindow,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_INPUT, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, WS_BORDER, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const STOP: u32 = WM_APP + 20;
const READY: u32 = WM_APP + 21;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_UP: u32 = 0x80;
const WHEEL_DELTA: i32 = 120;
const EVENT_CAPACITY: usize = 256;
const CAPTURE_SURFACE_WIDTH: i32 = 300;
const CAPTURE_SURFACE_HEIGHT: i32 = 32;

// Current backend routes focused-session input through the client. Raw Input
// observers in other processes may still see the same physical reports.
pub(crate) const fn capture_supported() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    Key { page: u16, usage: u16, down: bool },
    MouseButton { mask: u8, down: bool },
    Motion { dx: i32, dy: i32 },
    Wheel { vertical: i16, horizontal: i16 },
    Escape,
    RemoteReady(u64),
}

enum Command {
    Ready(bool),
    Remote(u64),
    Local(mpsc::Sender<()>),
}

#[derive(Default)]
struct EventQueue {
    events: VecDeque<InputEvent>,
    closed: bool,
    escapes: u8,
}

impl EventQueue {
    fn push(&mut self, event: InputEvent) -> bool {
        if event == InputEvent::Escape {
            self.escapes = self.escapes.saturating_add(1);
            return true;
        }
        if let (
            Some(InputEvent::Motion { dx, dy }),
            InputEvent::Motion {
                dx: next_x,
                dy: next_y,
            },
        ) = (self.events.back_mut(), event)
        {
            if let (Some(next_x), Some(next_y)) = (dx.checked_add(next_x), dy.checked_add(next_y)) {
                *dx = next_x;
                *dy = next_y;
                return true;
            }
            return false;
        }
        if self.events.len() == EVENT_CAPACITY {
            return false;
        }
        self.events.push_back(event);
        true
    }
}

pub(crate) struct CapturedEvents(Arc<Mutex<EventQueue>>);

impl CapturedEvents {
    pub(crate) fn take_escape(&self) -> bool {
        let mut queue = self.0.lock().unwrap();
        if queue.escapes == 0 || queue.closed {
            return false;
        }
        queue.escapes -= 1;
        true
    }

    pub(crate) fn try_recv(&self) -> Result<InputEvent, TryRecvError> {
        let mut queue = self.0.lock().unwrap();
        if queue.closed {
            Err(TryRecvError::Disconnected)
        } else {
            queue.events.pop_front().ok_or(TryRecvError::Empty)
        }
    }
}

struct CaptureWriter(Arc<Mutex<EventQueue>>);

impl Drop for CaptureWriter {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.0.lock() {
            queue.closed = true;
        }
    }
}

pub(crate) struct Capture {
    thread_id: u32,
    command: SyncSender<Command>,
    faulted: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), String>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Local,
    Remote,
    Shortcut,
}

struct HookState {
    sender: Arc<Mutex<EventQueue>>,
    bindings: Vec<EscapeBinding>,
    capture: bool,
    ready: bool,
    remote: bool,
    remote_pending: Option<u64>,
    held_modifiers: u8,
    initial_keys: BTreeSet<u32>,
    keys: BTreeMap<(u32, u32, bool), Owner>,
    mouse_buttons: u8,
    remote_buttons: u8,
    wheel_remainders: [i32; 2],
    faulted: Arc<AtomicBool>,
}

static HOOK_STATE: OnceLock<Mutex<Option<HookState>>> = OnceLock::new();

impl HookState {
    fn modifiers(&self) -> u8 {
        // Left modifiers use the low nibble and right modifiers use the high nibble.
        (self.held_modifiers & 0x0f) | (self.held_modifiers >> 4)
    }

    fn local(&mut self) {
        self.remote = false;
        self.remote_pending = None;
        self.wheel_remainders = [0; 2];
        // Retain remote ownership until physical release even after HID all-up.
    }

    fn complete_handoff(&mut self) {
        if self.ready
            && self.capture
            && !self.faulted.load(Ordering::Acquire)
            && self.initial_keys.is_empty()
            && self.keys.is_empty()
            && self.mouse_buttons == 0
        {
            if let Some(attempt) = self.remote_pending.take() {
                let mut queue = self.sender.lock().unwrap();
                if queue.escapes == 0 {
                    self.remote = true;
                    if !queue.push(InputEvent::RemoteReady(attempt)) {
                        drop(queue);
                        self.local();
                        self.faulted.store(true, Ordering::Release);
                    }
                }
            }
        }
    }

    fn wake_handoff(&self) {
        if self.remote_pending.is_some() {
            let thread = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            unsafe { PostThreadMessageW(thread, READY, 0, 0) };
        }
    }
}

impl Capture {
    #[cfg(test)]
    pub(crate) fn exited_for_test() -> (Self, CapturedEvents) {
        let (command, _) = mpsc::sync_channel(1);
        let events = EventQueue {
            closed: true,
            ..Default::default()
        };
        (
            Self {
                thread_id: 0,
                command,
                worker: None,
                faulted: Arc::new(AtomicBool::new(false)),
                stopping: Arc::new(AtomicBool::new(true)),
            },
            CapturedEvents(Arc::new(Mutex::new(events))),
        )
    }

    pub(crate) fn start(bindings: Vec<EscapeBinding>) -> Result<(Self, CapturedEvents), String> {
        if bindings.is_empty() {
            return Err("no usable escape binding is configured".into());
        }
        let events = Arc::new(Mutex::new(EventQueue::default()));
        let receiver = CapturedEvents(events.clone());
        let (commands, command_receiver) = mpsc::sync_channel(16);
        let (ready_sender, ready_receiver) = mpsc::channel();
        let faulted = Arc::new(AtomicBool::new(false));
        let thread_faulted = faulted.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = stopping.clone();
        let worker = thread::Builder::new()
            .name("kvmctl-input-capture".into())
            .spawn(move || {
                capture_thread(
                    bindings,
                    CaptureWriter(events),
                    command_receiver,
                    ready_sender,
                    thread_faulted,
                    thread_stopping,
                )
            })
            .map_err(|error| format!("cannot start input capture: {error}"))?;
        let thread_id = ready_receiver
            .recv()
            .map_err(|_| "input capture stopped before hook installation".to_owned())??;
        Ok((
            Self {
                thread_id,
                command: commands,
                faulted,
                stopping,
                worker: Some(worker),
            },
            receiver,
        ))
    }

    fn send_command(&self, command: Command) -> Result<(), String> {
        self.command.try_send(command).map_err(|error| {
            self.faulted.store(true, Ordering::Release);
            format!("cannot command input capture: {error}")
        })?;
        if unsafe { PostThreadMessageW(self.thread_id, READY, 0, 0) } == 0 {
            self.faulted.store(true, Ordering::Release);
            return Err(format!(
                "wake input capture thread: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub(crate) fn set_ready(&self, ready: bool) -> Result<(), String> {
        self.send_command(Command::Ready(ready))
    }

    pub(crate) fn request_remote(&self, attempt: u64) -> Result<(), String> {
        if !capture_supported() {
            return Err("Windows focused-session capture is unavailable".into());
        }
        self.send_command(Command::Remote(attempt))
    }

    pub(crate) fn set_remote(&self, remote: bool) -> Result<(), String> {
        if remote {
            return Err("remote handoff requires a correlated arm attempt".into());
        }
        let (complete, completed) = mpsc::channel();
        self.send_command(Command::Local(complete))?;
        completed
            .recv_timeout(std::time::Duration::from_millis(250))
            .map_err(|error| {
                self.faulted.store(true, Ordering::Release);
                format!("local input handoff did not complete: {error}")
            })
    }

    pub(crate) fn is_faulted(&self) -> bool {
        self.faulted.load(Ordering::Acquire)
            || self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub(crate) fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stopping.store(true, Ordering::Release);
            unsafe { PostThreadMessageW(self.thread_id, STOP, 0, 0) };
            let _ = worker.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_thread(
    bindings: Vec<EscapeBinding>,
    sender: CaptureWriter,
    commands: Receiver<Command>,
    ready: mpsc::Sender<Result<u32, String>>,
    faulted: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
) -> Result<(), String> {
    let state = HOOK_STATE.get_or_init(|| Mutex::new(None));
    *state.lock().unwrap() = Some(HookState {
        sender: sender.0.clone(),
        bindings,
        capture: capture_supported(),
        ready: false,
        remote: false,
        remote_pending: None,
        held_modifiers: 0,
        initial_keys: BTreeSet::new(),
        keys: BTreeMap::new(),
        mouse_buttons: 0,
        remote_buttons: 0,
        wheel_remainders: [0; 2],
        faulted,
    });
    let instance: HINSTANCE = unsafe { GetModuleHandleW(std::ptr::null()) };
    let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0) };
    if keyboard.is_null() {
        *state.lock().unwrap() = None;
        let error = io::Error::last_os_error();
        let _ = ready.send(Err(format!("install global keyboard hook: {error}")));
        return Ok(());
    }
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0) };
    if mouse.is_null() {
        unsafe { UnhookWindowsHookEx(keyboard) };
        *state.lock().unwrap() = None;
        let error = io::Error::last_os_error();
        let _ = ready.send(Err(format!("install global mouse hook: {error}")));
        return Ok(());
    }
    let window = match raw_mouse_window(instance) {
        Ok(window) => window,
        Err(error) => {
            unsafe {
                UnhookWindowsHookEx(keyboard);
                UnhookWindowsHookEx(mouse);
            }
            *state.lock().unwrap() = None;
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    let surface = match capture_surface_window() {
        Ok(surface) => surface,
        Err(error) => {
            unsafe {
                UnhookWindowsHookEx(keyboard);
                UnhookWindowsHookEx(mouse);
                DestroyWindow(window);
            }
            *state.lock().unwrap() = None;
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    let thread_id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    let _ = with_state(|state| {
        for key in 1..=254 {
            if matches!(key, 0x10..=0x12) || !async_key_down(key) {
                continue;
            }
            let button = match key {
                1 => 1,
                2 => 2,
                4 => 4,
                5 => 8,
                6 => 16,
                _ => 0,
            };
            if button != 0 {
                state.mouse_buttons |= button;
            } else if !state
                .keys
                .keys()
                .any(|&(virtual_key, ..)| virtual_key == key)
            {
                state.initial_keys.insert(key);
                state.held_modifiers |= sided_modifier_mask(key);
            }
        }
    });
    let _ = ready.send(Ok(thread_id));
    let mut message = MSG::default();
    while !stopping.load(Ordering::Acquire) {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Ready(ready) => {
                    let _ = with_state(|state| {
                        state.ready = ready;
                        if !ready {
                            state.local();
                        }
                    });
                }
                Command::Remote(attempt) => {
                    let _ = with_state(|state| state.remote_pending = Some(attempt));
                }
                Command::Local(complete) => {
                    let _ = with_state(|state| {
                        state.local();
                        state.sender.lock().unwrap().events.clear();
                    });
                    let _ = complete.send(());
                }
            };
        }
        // Run outside the low-level callback so the last locally owned
        // release has already been returned to Windows.
        let _ = with_state(HookState::complete_handoff);
        let remote = with_state(|state| state.remote).unwrap_or(false);
        unsafe {
            ShowWindow(surface, if remote { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result <= 0 || message.message == STOP {
            break;
        }
        if message.message == WM_INPUT && message.hwnd == window {
            let _ = with_state(|state| {
                if state.ready && state.remote && !state.faulted.load(Ordering::Acquire) {
                    read_raw_motion(state, message.lParam as HRAWINPUT);
                }
            });
            // Foreground WM_INPUT needs default processing even when capture is inactive.
            unsafe { DefWindowProcW(window, WM_INPUT, message.wParam, message.lParam) };
        } else {
            unsafe { DispatchMessageW(&message) };
        }
    }
    let _ = register_raw_mouse(null_mut(), RIDEV_REMOVE);
    unsafe {
        UnhookWindowsHookEx(keyboard);
        UnhookWindowsHookEx(mouse);
        DestroyWindow(window);
        DestroyWindow(surface);
    }
    *state.lock().unwrap() = None;
    Ok(())
}

fn capture_surface_window() -> Result<HWND, String> {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let window = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            windows_sys::core::w!("STATIC"),
            windows_sys::core::w!(
                "kvmctl: remote input active | configured escape shortcut to release"
            ),
            WS_POPUP | WS_BORDER,
            width.saturating_sub(CAPTURE_SURFACE_WIDTH + 20),
            20,
            CAPTURE_SURFACE_WIDTH,
            CAPTURE_SURFACE_HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(format!(
            "create capture status window: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(window)
}

fn async_key_down(virtual_key: u32) -> bool {
    unsafe { GetAsyncKeyState(virtual_key as i32) < 0 }
}

fn raw_mouse_window(instance: HINSTANCE) -> Result<HWND, String> {
    let window = unsafe {
        CreateWindowExW(
            0,
            windows_sys::core::w!("STATIC"),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        )
    };
    if window.is_null() {
        return Err(format!(
            "create Raw Input window: {}",
            io::Error::last_os_error()
        ));
    }
    if let Err(error) = register_raw_mouse(window, RIDEV_INPUTSINK) {
        unsafe { DestroyWindow(window) };
        return Err(format!("register Raw Input mouse: {error}"));
    }
    Ok(window)
}

fn register_raw_mouse(window: HWND, flags: u32) -> io::Result<()> {
    let device = RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: 2,
        dwFlags: flags,
        hwndTarget: window,
    };
    if unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_raw_motion(state: &mut HookState, input: HRAWINPUT) {
    let mut raw = RAWINPUT::default();
    let mut bytes = size_of::<RAWINPUT>() as u32;
    let read = unsafe {
        GetRawInputData(
            input,
            RID_INPUT,
            (&mut raw as *mut RAWINPUT).cast(),
            &mut bytes,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read == size_of::<RAWINPUT>() as u32 && raw.header.dwType == RIM_TYPEMOUSE {
        forward_raw_motion(state, unsafe { raw.data.mouse });
    }
}

fn forward_raw_motion(state: &mut HookState, mouse: RAWMOUSE) {
    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
        // This backend cannot translate absolute input. Return local if a
        // source appears after activation instead of dropping all its motion.
        state.local();
        state.faulted.store(true, Ordering::Release);
    } else if mouse.lLastX != 0 || mouse.lLastY != 0 {
        let _ = send(
            state,
            InputEvent::Motion {
                dx: mouse.lLastX,
                dy: mouse.lLastY,
            },
        );
    }
}

fn with_state<R>(f: impl FnOnce(&mut HookState) -> R) -> Option<R> {
    HOOK_STATE.get().and_then(|state| {
        state
            .lock()
            .ok()
            .and_then(|mut state| state.as_mut().map(f))
    })
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let extended = info.flags & LLKHF_EXTENDED != 0;
    let down = !matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP) && info.flags & LLKHF_UP == 0;
    let handled =
        with_state(|state| handle_keyboard(state, info.vkCode, info.scanCode, extended, down));
    if handled == Some(true) {
        1
    } else {
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
}

fn handle_keyboard(
    state: &mut HookState,
    virtual_key: u32,
    scan_code: u32,
    extended: bool,
    down: bool,
) -> bool {
    if state.faulted.load(Ordering::Acquire) {
        state.local();
    }
    let modifier = sided_modifier_mask(virtual_key);
    let key = (virtual_key, scan_code, extended);
    if down {
        state.held_modifiers |= modifier;
    } else {
        state.held_modifiers &= !modifier;
    }

    let owner = if state.initial_keys.contains(&virtual_key) {
        if !down {
            state.initial_keys.remove(&virtual_key);
        }
        Owner::Local
    } else if !down {
        state.keys.remove(&key).unwrap_or(Owner::Local)
    } else if let Some(&owner) = state.keys.get(&key) {
        return owner != Owner::Local;
    } else {
        let modifiers = state.modifiers();
        let binding = state.bindings.iter().any(|binding| {
            binding.virtual_key == virtual_key && modifiers & binding.modifiers == binding.modifiers
        });
        let owner = if binding {
            state.remote_pending = None;
            let _ = send(state, InputEvent::Escape);
            if state.capture {
                Owner::Shortcut
            } else {
                Owner::Local
            }
        } else if state.ready && state.remote {
            Owner::Remote
        } else {
            Owner::Local
        };
        state.keys.insert(key, owner);
        owner
    };

    if owner == Owner::Remote && state.remote {
        if let Some((page, usage)) = keyboard_usage(scan_code, extended, virtual_key) {
            let _ = send(state, InputEvent::Key { page, usage, down });
        }
    }
    if !down {
        state.wake_handoff();
    }
    owner != Owner::Local
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let handled = with_state(|state| handle_mouse(state, wparam as u32, info));
    if handled == Some(true) {
        1
    } else {
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
}

fn handle_mouse(state: &mut HookState, message: u32, info: &MSLLHOOKSTRUCT) -> bool {
    if state.faulted.load(Ordering::Acquire) {
        state.local();
    }
    let mask = match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => 1,
        WM_RBUTTONDOWN | WM_RBUTTONUP => 2,
        WM_MBUTTONDOWN | WM_MBUTTONUP => 4,
        WM_XBUTTONDOWN | WM_XBUTTONUP if (info.mouseData >> 16) & 0xffff == 1 => 8,
        WM_XBUTTONDOWN | WM_XBUTTONUP => 16,
        _ => 0,
    };
    if mask != 0 {
        let down = matches!(
            message,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        );
        if down && state.mouse_buttons & mask == 0 && state.ready && state.remote {
            state.remote_buttons |= mask;
        }
        let remote = state.remote_buttons & mask != 0;
        if down {
            state.mouse_buttons |= mask;
        } else {
            state.mouse_buttons &= !mask;
            state.remote_buttons &= !mask;
            state.wake_handoff();
        }
        if remote && state.remote {
            let _ = send(state, InputEvent::MouseButton { mask, down });
        }
        return remote;
    }
    if !state.ready || !state.remote {
        return false;
    }
    if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        let delta = ((info.mouseData >> 16) as u16) as i16;
        let horizontal = message == WM_MOUSEHWHEEL;
        let remainder = &mut state.wheel_remainders[usize::from(horizontal)];
        let detents = wheel_detents(remainder, delta);
        let (vertical, horizontal) = if horizontal {
            (0, detents)
        } else {
            (detents, 0)
        };
        for (vertical, horizontal) in hid_delta_chunks(vertical, horizontal) {
            if !send(
                state,
                InputEvent::Wheel {
                    vertical,
                    horizontal,
                },
            ) {
                break;
            }
        }
    }
    // Raw Input supplies movement. Suppress its legacy mouse message locally.
    true
}

fn send(state: &mut HookState, event: InputEvent) -> bool {
    let accepted = state.sender.lock().unwrap().push(event);
    if !accepted {
        state.local();
        state.faulted.store(true, Ordering::Release);
    }
    accepted
}

fn sided_modifier_mask(virtual_key: u32) -> u8 {
    use crate::settings::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};

    match virtual_key {
        0xa0 => MOD_SHIFT,
        0xa1 => MOD_SHIFT << 4,
        0xa2 => MOD_CTRL,
        0xa3 => MOD_CTRL << 4,
        0xa4 => MOD_ALT,
        0xa5 => MOD_ALT << 4,
        0x5b => MOD_WIN,
        0x5c => MOD_WIN << 4,
        _ => 0,
    }
}

fn keyboard_usage(scan_code: u32, extended: bool, virtual_key: u32) -> Option<(u16, u16)> {
    // Windows Scan 1 make codes for the HID Keyboard/Keypad page.
    // https://learn.microsoft.com/windows/win32/inputdev/about-keyboard-input#scan-codes
    let scan_code = if extended && scan_code <= 0xff {
        scan_code | 0xe000
    } else {
        scan_code
    };
    let usage = match scan_code {
        0x1e => 0x04, // A
        0x30 => 0x05,
        0x2e => 0x06,
        0x20 => 0x07,
        0x12 => 0x08,
        0x21 => 0x09,
        0x22 => 0x0a,
        0x23 => 0x0b,
        0x17 => 0x0c,
        0x24 => 0x0d,
        0x25 => 0x0e,
        0x26 => 0x0f,
        0x32 => 0x10,
        0x31 => 0x11,
        0x18 => 0x12,
        0x19 => 0x13,
        0x10 => 0x14,
        0x13 => 0x15,
        0x1f => 0x16,
        0x14 => 0x17,
        0x16 => 0x18,
        0x2f => 0x19,
        0x11 => 0x1a,
        0x2d => 0x1b,
        0x15 => 0x1c,
        0x2c => 0x1d, // Z
        0x02..=0x0b => 0x1e + (scan_code as u16 - 0x02),
        0x1c => 0x28,
        0x01 => 0x29,
        0x0e => 0x2a,
        0x0f => 0x2b,
        0x39 => 0x2c,
        0x0c => 0x2d,
        0x0d => 0x2e,
        0x1a => 0x2f,
        0x1b => 0x30,
        0x2b => 0x31,
        0x27 => 0x33,
        0x28 => 0x34,
        0x29 => 0x35,
        0x33 => 0x36,
        0x34 => 0x37,
        0x35 => 0x38,
        0x3a => 0x39,
        0x3b..=0x44 => 0x3a + (scan_code as u16 - 0x3b),
        0x57 => 0x44,
        0x58 => 0x45,
        0xe037 | 0x54 => 0x46, // Print Screen or Alt+Print Screen
        0x46 => 0x47,
        // Windows can report the same scan code for Pause and Num Lock.
        0x45 | 0xe045 if virtual_key == 0x13 => 0x48,
        0xe11d45 | 0xe046 => 0x48,
        0xe052 => 0x49,
        0xe047 => 0x4a,
        0xe049 => 0x4b,
        0xe053 => 0x4c,
        0xe04f => 0x4d,
        0xe051 => 0x4e,
        0xe04d => 0x4f,
        0xe04b => 0x50,
        0xe050 => 0x51,
        0xe048 => 0x52,
        0x45 | 0xe045 => 0x53,
        0xe035 => 0x54,
        0x37 => 0x55,
        0x4a => 0x56,
        0x4e => 0x57,
        0xe01c => 0x58,
        0x4f..=0x51 => 0x59 + (scan_code as u16 - 0x4f),
        0x4b..=0x4d => 0x5c + (scan_code as u16 - 0x4b),
        0x47..=0x49 => 0x5f + (scan_code as u16 - 0x47),
        0x52 => 0x62,
        0x53 => 0x63,
        0x56 => 0x64, // ISO extra key
        0xe05d => 0x65,
        0x59 => 0x67,
        0x64..=0x6e => 0x68 + (scan_code as u16 - 0x64),
        0x76 => 0x73,
        0x1d => 0xe0,
        0x2a => 0xe1,
        0x38 => 0xe2,
        0xe05b => 0xe3,
        0xe01d => 0xe4,
        0x36 => 0xe5,
        0xe038 => 0xe6,
        0xe05c => 0xe7,
        _ => return None,
    };
    Some((7, usage))
}

fn wheel_detents(remainder: &mut i32, delta: i16) -> i32 {
    let total = *remainder + i32::from(delta);
    *remainder = total % WHEEL_DELTA;
    total / WHEEL_DELTA
}

fn hid_delta_chunks(mut dx: i32, mut dy: i32) -> impl Iterator<Item = (i16, i16)> {
    std::iter::from_fn(move || {
        if dx == 0 && dy == 0 {
            return None;
        }
        let chunk_dx = dx.clamp(-127, 127) as i16;
        let chunk_dy = dy.clamp(-127, 127) as i16;
        dx -= i32::from(chunk_dx);
        dy -= i32::from(chunk_dy);
        Some((chunk_dx, chunk_dy))
    })
}

#[cfg(test)]
pub(crate) struct TestCapture(HookState, CapturedEvents);

#[cfg(test)]
impl TestCapture {
    pub(crate) fn new() -> Self {
        let (state, events) = capture_state(true);
        Self(state, events)
    }

    pub(crate) fn request_remote(&mut self, attempt: u64) {
        self.0.remote_pending = Some(attempt);
    }

    pub(crate) fn key(
        &mut self,
        virtual_key: u32,
        scan_code: u32,
        extended: bool,
        down: bool,
    ) -> bool {
        handle_keyboard(&mut self.0, virtual_key, scan_code, extended, down)
    }

    pub(crate) fn complete_handoff(&mut self) -> Option<u64> {
        self.0.complete_handoff();
        match self.next_event() {
            Some(InputEvent::RemoteReady(attempt)) => Some(attempt),
            None => None,
            other => panic!("expected handoff completion, got {other:?}"),
        }
    }

    pub(crate) fn next_event(&self) -> Option<InputEvent> {
        self.1.try_recv().ok()
    }

    pub(crate) fn take_escape(&self) -> bool {
        self.1.take_escape()
    }

    pub(crate) fn local(&mut self) {
        self.0.local();
        self.0.sender.lock().unwrap().events.clear();
    }

    pub(crate) fn is_remote(&self) -> bool {
        self.0.remote
    }
}

#[cfg(test)]
fn capture_state(capture: bool) -> (HookState, CapturedEvents) {
    let queue = Arc::new(Mutex::new(EventQueue::default()));
    (
        HookState {
            sender: queue.clone(),
            bindings: vec![EscapeBinding::parse("CTRL+SHIFT+F10").unwrap()],
            capture,
            ready: true,
            remote: false,
            remote_pending: None,
            held_modifiers: 0,
            initial_keys: BTreeSet::new(),
            keys: BTreeMap::new(),
            mouse_buttons: 0,
            remote_buttons: 0,
            wheel_remainders: [0; 2],
            faulted: Arc::new(AtomicBool::new(false)),
        },
        CapturedEvents(queue),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_physical_windows_keys_to_keyboard_page() {
        assert_eq!(keyboard_usage(0x15, false, b'Z' as u32), Some((7, 0x1c)));
        assert_eq!(keyboard_usage(0x2c, false, b'Y' as u32), Some((7, 0x1d)));
        assert_eq!(keyboard_usage(0x1c, false, 0x0d), Some((7, 0x28)));
        assert_eq!(keyboard_usage(0x1c, true, 0x0d), Some((7, 0x58)));
        assert_eq!(keyboard_usage(0x4f, false, 0x23), Some((7, 0x59)));
        assert_eq!(keyboard_usage(0x4f, true, 0x23), Some((7, 0x4d)));
        assert_eq!(keyboard_usage(0x45, false, 0x13), Some((7, 0x48)));
        assert_eq!(keyboard_usage(0x45, true, 0x90), Some((7, 0x53)));
        assert_eq!(keyboard_usage(0, false, 0), None);
    }

    #[test]
    fn waits_for_all_local_releases_and_preserves_remote_ownership_after_all_up() {
        let (mut state, events) = capture_state(true);
        let mouse = MSLLHOOKSTRUCT::default();
        state.initial_keys.insert(b'B' as u32);
        assert!(!handle_keyboard(&mut state, b'A' as u32, 0x1e, false, true));
        for (vk, scan, extended) in [(0xa2, 0x1d, false), (0xa3, 0x1d, true), (0xa0, 0x2a, false)] {
            assert!(!handle_keyboard(&mut state, vk, scan, extended, true));
        }
        assert!(!handle_mouse(&mut state, WM_RBUTTONDOWN, &mouse));
        state.remote_pending = Some(42);
        state.complete_handoff();
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        for (vk, scan, extended) in [
            (0xa2, 0x1d, false),
            (b'A' as u32, 0x1e, false),
            (0xa3, 0x1d, true),
            (b'B' as u32, 0x30, false),
            (0xa0, 0x2a, false),
        ] {
            assert!(!handle_keyboard(&mut state, vk, scan, extended, false));
            state.complete_handoff();
            assert!(!state.remote);
        }
        assert!(!handle_mouse(&mut state, WM_RBUTTONUP, &mouse));
        state.complete_handoff();
        assert_eq!(events.try_recv(), Ok(InputEvent::RemoteReady(42)));

        assert!(handle_keyboard(&mut state, b'A' as u32, 0x1e, false, true));
        assert!(handle_mouse(&mut state, WM_LBUTTONDOWN, &mouse));
        assert_eq!(
            events.try_recv(),
            Ok(InputEvent::Key {
                page: 7,
                usage: 4,
                down: true
            })
        );
        assert_eq!(
            events.try_recv(),
            Ok(InputEvent::MouseButton {
                mask: 1,
                down: true
            })
        );
        state.local();
        assert!(handle_keyboard(&mut state, b'A' as u32, 0x1e, false, true));
        assert!(handle_keyboard(&mut state, b'A' as u32, 0x1e, false, false));
        assert!(handle_mouse(&mut state, WM_LBUTTONUP, &mouse));
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        assert!(!handle_keyboard(&mut state, b'A' as u32, 0x1e, false, true));
    }

    #[test]
    fn preserves_each_supported_mouse_buttons_destination() {
        let (mut state, events) = capture_state(true);
        for (mask, down, up, data) in [
            (1, WM_LBUTTONDOWN, WM_LBUTTONUP, 0),
            (2, WM_RBUTTONDOWN, WM_RBUTTONUP, 0),
            (4, WM_MBUTTONDOWN, WM_MBUTTONUP, 0),
            (8, WM_XBUTTONDOWN, WM_XBUTTONUP, 1 << 16),
            (16, WM_XBUTTONDOWN, WM_XBUTTONUP, 2 << 16),
        ] {
            let mouse = MSLLHOOKSTRUCT {
                mouseData: data,
                ..Default::default()
            };
            // Windows already received the down before capture started.
            state.mouse_buttons |= mask;
            state.remote_pending = Some(42);
            state.complete_handoff();
            assert!(!state.remote);
            assert!(!handle_mouse(&mut state, up, &mouse));
            state.complete_handoff();
            assert_eq!(events.try_recv(), Ok(InputEvent::RemoteReady(42)));

            assert!(handle_mouse(&mut state, down, &mouse));
            assert_eq!(
                events.try_recv(),
                Ok(InputEvent::MouseButton { mask, down: true })
            );
            state.local();
            assert!(handle_mouse(&mut state, up, &mouse));
            assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        }
    }

    #[test]
    fn current_backend_consumes_toggle_and_confirms_remote() {
        let (mut state, events) = capture_state(capture_supported());
        assert!(!handle_keyboard(&mut state, 0xa3, 0x1d, true, true));
        assert!(!handle_keyboard(&mut state, 0xa1, 0x36, false, true));
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
        assert!(!events.take_escape());
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, false));
        assert!(!handle_keyboard(&mut state, 0xa3, 0x1d, true, false));
        assert!(!handle_keyboard(&mut state, 0xa1, 0x36, false, false));
        state.remote_pending = Some(42);
        state.complete_handoff();
        assert!(state.remote);
        assert_eq!(events.try_recv(), Ok(InputEvent::RemoteReady(42)));

        assert!(handle_keyboard(&mut state, 0xa3, 0x1d, true, true));
        assert!(handle_keyboard(&mut state, 0xa1, 0x36, false, true));
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
        assert!(!events.take_escape());
    }

    #[test]
    fn shortcut_usable_before_lease_and_during_recovery() {
        let (mut state, events) = capture_state(false);
        state.ready = false;
        assert!(!handle_keyboard(&mut state, 0xa2, 0x1d, false, true));
        assert!(!handle_keyboard(&mut state, 0xa0, 0x2a, false, true));
        assert!(!handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
        assert!(!handle_keyboard(&mut state, 0x79, 0x44, false, false));
        assert!(!handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
    }

    #[test]
    fn pending_handoff_cancels_before_the_shortcut_is_released() {
        let (mut state, events) = capture_state(true);
        assert!(!handle_keyboard(&mut state, 0xa2, 0x1d, false, true));
        assert!(!handle_keyboard(&mut state, 0xa0, 0x2a, false, true));
        state.remote_pending = Some(7);
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, false));
        assert!(!handle_keyboard(&mut state, 0xa2, 0x1d, false, false));
        assert!(!handle_keyboard(&mut state, 0xa0, 0x2a, false, false));
        state.complete_handoff();
        assert!(!state.remote);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn accumulates_signed_wheel_deltas() {
        let mut remainder = 0;
        assert_eq!(wheel_detents(&mut remainder, 120), 1);
        assert_eq!(wheel_detents(&mut remainder, 90), 0);
        assert_eq!(wheel_detents(&mut remainder, -30), 0);
        assert_eq!(wheel_detents(&mut remainder, 240), 2);
        assert_eq!(remainder, 60);
        assert_eq!(wheel_detents(&mut remainder, -180), -1);
        assert_eq!(remainder, 0);
    }

    #[test]
    fn coalesces_motion_at_125_and_2000_hz_with_priority_escape() {
        for count in [125, 2000] {
            let (mut state, events) = capture_state(true);
            let motion = RAWMOUSE {
                lLastX: 5,
                lLastY: -2,
                ..Default::default()
            };
            for _ in 0..count {
                forward_raw_motion(&mut state, motion);
            }
            assert!(send(
                &mut state,
                InputEvent::MouseButton {
                    mask: 1,
                    down: true
                }
            ));
            forward_raw_motion(&mut state, motion);
            assert!(send(
                &mut state,
                InputEvent::Wheel {
                    vertical: 1,
                    horizontal: 0
                }
            ));
            assert!(send(&mut state, InputEvent::Escape));
            assert!(events.take_escape());
            assert!(!state.faulted.load(Ordering::Acquire));
            assert_eq!(
                events.try_recv(),
                Ok(InputEvent::Motion {
                    dx: 5 * count,
                    dy: -2 * count
                })
            );
            assert_eq!(
                events.try_recv(),
                Ok(InputEvent::MouseButton {
                    mask: 1,
                    down: true
                })
            );
            assert_eq!(events.try_recv(), Ok(InputEvent::Motion { dx: 5, dy: -2 }));
            assert_eq!(
                events.try_recv(),
                Ok(InputEvent::Wheel {
                    vertical: 1,
                    horizontal: 0
                })
            );
        }
    }

    #[test]
    fn absolute_motion_falls_back_local_with_owned_releases_still_swallowed() {
        let (mut state, events) = capture_state(true);
        let mouse = MSLLHOOKSTRUCT::default();
        state.remote = true;
        assert!(handle_mouse(&mut state, WM_LBUTTONDOWN, &mouse));
        forward_raw_motion(
            &mut state,
            RAWMOUSE {
                usFlags: MOUSE_MOVE_ABSOLUTE,
                ..Default::default()
            },
        );
        assert!(state.faulted.load(Ordering::Acquire));
        assert!(!state.remote);
        assert!(handle_mouse(&mut state, WM_LBUTTONUP, &mouse));
        assert_eq!(
            events.try_recv(),
            Ok(InputEvent::MouseButton {
                mask: 1,
                down: true
            })
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn distinguishes_worker_exit_from_idle_even_with_queued_drag() {
        let (mut state, events) = capture_state(true);
        let sender = CaptureWriter(state.sender.clone());
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        state.remote = true;
        assert!(handle_mouse(
            &mut state,
            WM_LBUTTONDOWN,
            &MSLLHOOKSTRUCT::default()
        ));
        drop(sender);
        assert_eq!(events.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn overflow_fault_keeps_escape_available() {
        let (mut state, events) = capture_state(true);
        state.remote = true;
        for _ in 0..EVENT_CAPACITY {
            assert!(send(
                &mut state,
                InputEvent::Wheel {
                    vertical: 1,
                    horizontal: 0
                }
            ));
        }
        assert!(!send(
            &mut state,
            InputEvent::Wheel {
                vertical: -1,
                horizontal: 0
            }
        ));
        assert!(state.faulted.load(Ordering::Acquire));
        assert!(!state.remote);
        assert!(!handle_keyboard(&mut state, 0xa2, 0x1d, false, true));
        assert!(!handle_keyboard(&mut state, 0xa0, 0x2a, false, true));
        assert!(handle_keyboard(&mut state, 0x79, 0x44, false, true));
        assert!(events.take_escape());
    }

    #[test]
    fn splits_deltas_into_hid_sized_chunks() {
        let chunks: Vec<_> = hid_delta_chunks(300, -130).collect();
        assert!(
            chunks
                .iter()
                .all(|(dx, dy)| (-127..=127).contains(dx) && (-127..=127).contains(dy))
        );
        assert_eq!(
            chunks.iter().map(|(dx, _)| i32::from(*dx)).sum::<i32>(),
            300
        );
        assert_eq!(
            chunks.iter().map(|(_, dy)| i32::from(*dy)).sum::<i32>(),
            -130
        );
    }
}
