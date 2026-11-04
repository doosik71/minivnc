//! Small platform services: the local lock key state and the bell.

/// Local lock key state as `LED_*` bits, where the platform can tell.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn lock_keys() -> Option<u32> {
    use rfb_proto::consts::{LED_CAPS_LOCK, LED_NUM_LOCK, LED_SCROLL_LOCK};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL,
    };
    // SAFETY: GetKeyState only reads the thread's keyboard state.
    let on = |vk| unsafe { GetKeyState(i32::from(vk)) } & 1 != 0;
    Some(
        (if on(VK_CAPITAL) { LED_CAPS_LOCK } else { 0 })
            | (if on(VK_NUMLOCK) { LED_NUM_LOCK } else { 0 })
            | (if on(VK_SCROLL) { LED_SCROLL_LOCK } else { 0 }),
    )
}

/// winit does not report lock key state, so it is unknown elsewhere.
#[cfg(not(windows))]
pub fn lock_keys() -> Option<u32> {
    None
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn bell() {
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::UI::WindowsAndMessaging::MB_OK;
    // SAFETY: MessageBeep has no memory-safety preconditions.
    unsafe {
        MessageBeep(MB_OK);
    }
}

#[cfg(not(windows))]
pub fn bell() {
    tracing::debug!("bell");
}

/// Keyboard grab (Windows): a low-level keyboard hook, as the TurboVNC
/// Windows viewer's `LowLevelHook.cpp`, takes the keys Windows would act
/// on (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, the application and media keys)
/// while the viewer window has the focus, and posts them to the window
/// instead, which sends them to the server.
#[cfg(windows)]
#[allow(unsafe_code)]
mod grab {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
        LLKHF_EXTENDED, LLKHF_UP, PostMessageW, SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    /// The window keys go to while grabbing, 0 when not grabbing.
    static TARGET: AtomicIsize = AtomicIsize::new(0);
    static HOOKED: AtomicBool = AtomicBool::new(false);
    // Keys taken on press whose release must be taken too.
    static WIN_DOWN: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
    static ALT_TAB: AtomicBool = AtomicBool::new(false);
    static ALT_ESC: AtomicBool = AtomicBool::new(false);
    static CTRL_ESC: AtomicBool = AtomicBool::new(false);

    /// The key message's `lParam`. TurboVNC `MakeLParam` starts with
    /// `lParam &= scanCode << 16`, which loses the scan code and repeat
    /// count; here they are kept.
    fn make_lparam(msg: u32, k: &KBDLLHOOKSTRUCT) -> LPARAM {
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let mut l: u32 = 1 | (k.scanCode & 0xFF) << 16;
        if k.flags & LLKHF_EXTENDED != 0 {
            l |= 1 << 24;
        }
        if k.flags & LLKHF_ALTDOWN != 0 && (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) {
            l |= 1 << 29;
        }
        if k.flags & LLKHF_UP != 0 || up {
            l |= 1 << 30;
        }
        if up {
            l |= 1 << 31;
        }
        l as i32 as LPARAM
    }

    /// Takes the press after the modifier and the matching release.
    fn sequence(flag: &AtomicBool, modifier: bool, down: bool) -> bool {
        if modifier && down {
            flag.store(true, Ordering::Relaxed);
            true
        } else if flag.swap(false, Ordering::Relaxed) {
            !down
        } else {
            false
        }
    }

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let target = TARGET.load(Ordering::Relaxed);
        // SAFETY: GetForegroundWindow has no preconditions.
        let foreground = unsafe { GetForegroundWindow() } as isize;
        if code == HC_ACTION as i32 && target != 0 && foreground == target {
            // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
            let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let msg = wparam as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let alt = k.flags & LLKHF_ALTDOWN != 0;
            let vk = k.vkCode as u16;
            let take = match vk {
                VK_LWIN | VK_RWIN => {
                    // Only a release whose press was taken: switching
                    // virtual desktops may leave the press elsewhere.
                    let pressed = &WIN_DOWN[usize::from(vk == VK_RWIN)];
                    if down {
                        pressed.store(true, Ordering::Relaxed);
                        true
                    } else {
                        pressed.swap(false, Ordering::Relaxed)
                    }
                }
                VK_APPS | VK_BROWSER_HOME | VK_BROWSER_SEARCH | VK_LAUNCH_MAIL | VK_VOLUME_MUTE
                | VK_VOLUME_DOWN | VK_VOLUME_UP | VK_MEDIA_PLAY_PAUSE | VK_LAUNCH_APP2
                | VK_BROWSER_BACK | VK_BROWSER_FORWARD => true,
                VK_TAB => sequence(&ALT_TAB, alt, down),
                VK_ESCAPE => {
                    // SAFETY: GetAsyncKeyState has no preconditions.
                    let ctrl =
                        unsafe { GetAsyncKeyState(i32::from(VK_CONTROL)) } as u16 & 0x8000 != 0;
                    sequence(&ALT_ESC, alt, down) | sequence(&CTRL_ESC, ctrl, down)
                }
                _ => false,
            };
            if take {
                // SAFETY: posting to our own window.
                unsafe {
                    PostMessageW(target as HWND, msg, k.vkCode as WPARAM, make_lparam(msg, k));
                }
                return 1;
            }
        }
        // SAFETY: passing the event on, as hooks must.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    /// Starts or stops taking keys for `hwnd`. The hook is installed once,
    /// on the calling (event loop) thread, which must pump messages.
    pub fn set(hwnd: isize, on: bool) {
        TARGET.store(if on { hwnd } else { 0 }, Ordering::Relaxed);
        if on && !HOOKED.swap(true, Ordering::Relaxed) {
            // SAFETY: a valid hook procedure for this process's module.
            let installed = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(hook),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                )
            };
            if installed.is_null() {
                tracing::warn!("cannot grab the keyboard (SetWindowsHookEx failed)");
                HOOKED.store(false, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(windows)]
pub fn grab_keyboard(window: &winit::window::Window, on: bool) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
        grab::set(h.hwnd.get(), on);
    }
}

/// Grabbing the keyboard is only implemented for Windows.
#[cfg(not(windows))]
pub fn grab_keyboard(_: &winit::window::Window, _: bool) {}
