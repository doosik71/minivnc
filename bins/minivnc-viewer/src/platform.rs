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
