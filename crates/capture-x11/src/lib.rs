//! X11 screen capture and input injection for the MiniVNC server.
//!
//! The capture and XTest code is Linux-only; the keysym logic in [`keymap`]
//! is platform independent so it can be tested everywhere.

pub mod keymap;
#[cfg(any(target_os = "linux", test))]
mod qnum_table;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub use x11::{X11Input, X11Source, open, set_otp};

#[cfg(test)]
mod tests {
    use crate::qnum_table::{QNUM_TO_XORGEVDEV, QNUM_TO_XORGKBD};

    #[test]
    fn korean_keys_map_to_evdev_keycodes() {
        // The keycodes the TurboVNC Hangul/Hanja viewer patch aims for.
        assert_eq!(QNUM_TO_XORGEVDEV[0x72], 130);
        assert_eq!(QNUM_TO_XORGEVDEV[0x71], 131);
        // Escape and Right Ctrl (E0 1D) in both keycode sets.
        assert_eq!((QNUM_TO_XORGEVDEV[0x01], QNUM_TO_XORGKBD[0x01]), (9, 9));
        assert_eq!(QNUM_TO_XORGEVDEV[0x9d], 105);
    }
}
