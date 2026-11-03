//! Keyboard input to X keysyms (the TurboVNC viewer's keysym path).

use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey};

use crate::ucs_table::UCS_TO_KEYSYM;

/// `UnicodeToKeysym.ucs2keysym`: Latin-1 maps 1:1, then the legacy keysym
/// table, then the direct Unicode keysym range.
pub fn ucs_to_keysym(c: char) -> u32 {
    let ucs = u32::from(c);
    if (0x20..=0x7e).contains(&ucs) || (0xa0..=0xff).contains(&ucs) {
        return ucs;
    }
    match UCS_TO_KEYSYM.binary_search_by_key(&ucs, |&(u, _)| u) {
        Ok(i) => UCS_TO_KEYSYM[i].1,
        Err(_) => ucs | 0x0100_0000,
    }
}

/// Keypad keysyms for keys that winit reports as characters or as their
/// NumLock-off functions.
fn keypad(key: &Key) -> Option<u32> {
    Some(match key {
        Key::Character(s) => match s.as_str() {
            d @ ("0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                0xffb0 + u32::from(d.as_bytes()[0] - b'0')
            }
            "." | "," => 0xffae, // KP_Decimal
            "+" => 0xffab,
            "-" => 0xffad,
            "*" => 0xffaa,
            "/" => 0xffaf,
            "=" => 0xffbd,
            _ => return None,
        },
        Key::Named(n) => match n {
            NamedKey::Enter => 0xff8d,
            NamedKey::Home => 0xff95,
            NamedKey::ArrowLeft => 0xff96,
            NamedKey::ArrowUp => 0xff97,
            NamedKey::ArrowRight => 0xff98,
            NamedKey::ArrowDown => 0xff99,
            NamedKey::PageUp => 0xff9a,
            NamedKey::PageDown => 0xff9b,
            NamedKey::End => 0xff9c,
            NamedKey::Clear => 0xff9d, // KP_Begin (5 with NumLock off)
            NamedKey::Insert => 0xff9e,
            NamedKey::Delete => 0xff9f,
            _ => return None,
        },
        _ => return None,
    })
}

fn named(key: NamedKey, right: bool) -> Option<u32> {
    let side = |left: u32, r: u32| if right { r } else { left };
    Some(match key {
        NamedKey::Shift => side(0xffe1, 0xffe2),
        NamedKey::Control => side(0xffe3, 0xffe4),
        NamedKey::Alt => side(0xffe9, 0xffea),
        NamedKey::Super => side(0xffeb, 0xffec),
        NamedKey::Meta => side(0xffe7, 0xffe8),
        NamedKey::AltGraph => 0xfe03, // ISO_Level3_Shift
        NamedKey::CapsLock => 0xffe5,
        NamedKey::NumLock => 0xff7f,
        NamedKey::ScrollLock => 0xff14,
        NamedKey::Enter => 0xff0d,
        NamedKey::Tab => 0xff09,
        NamedKey::Space => 0x20,
        NamedKey::ArrowLeft => 0xff51,
        NamedKey::ArrowUp => 0xff52,
        NamedKey::ArrowRight => 0xff53,
        NamedKey::ArrowDown => 0xff54,
        NamedKey::Home => 0xff50,
        NamedKey::End => 0xff57,
        NamedKey::PageUp => 0xff55,
        NamedKey::PageDown => 0xff56,
        NamedKey::Backspace => 0xff08,
        NamedKey::Clear => 0xff0b,
        NamedKey::Delete => 0xffff,
        NamedKey::Insert => 0xff63,
        NamedKey::Escape => 0xff1b,
        NamedKey::Pause => 0xff13,
        NamedKey::PrintScreen => 0xff61,
        NamedKey::ContextMenu => 0xff67,
        NamedKey::Help => 0xff6a,
        NamedKey::HangulMode => 0xff31,
        NamedKey::HanjaMode => 0xff34,
        NamedKey::Convert => 0xff23,    // Henkan
        NamedKey::NonConvert => 0xff22, // Muhenkan
        NamedKey::F1 => 0xffbe,
        NamedKey::F2 => 0xffbf,
        NamedKey::F3 => 0xffc0,
        NamedKey::F4 => 0xffc1,
        NamedKey::F5 => 0xffc2,
        NamedKey::F6 => 0xffc3,
        NamedKey::F7 => 0xffc4,
        NamedKey::F8 => 0xffc5,
        NamedKey::F9 => 0xffc6,
        NamedKey::F10 => 0xffc7,
        NamedKey::F11 => 0xffc8,
        NamedKey::F12 => 0xffc9,
        NamedKey::F13 => 0xffca,
        NamedKey::F14 => 0xffcb,
        NamedKey::F15 => 0xffcc,
        NamedKey::F16 => 0xffcd,
        NamedKey::F17 => 0xffce,
        NamedKey::F18 => 0xffcf,
        NamedKey::F19 => 0xffd0,
        NamedKey::F20 => 0xffd1,
        NamedKey::F21 => 0xffd2,
        NamedKey::F22 => 0xffd3,
        NamedKey::F23 => 0xffd4,
        NamedKey::F24 => 0xffd5,
        _ => return None,
    })
}

/// The keysym to send for a key press, or `None` to ignore the key.
/// `logical` already has Shift and the layout applied (but not Ctrl).
pub fn keysym(logical: &Key, physical: PhysicalKey, location: KeyLocation) -> Option<u32> {
    if location == KeyLocation::Numpad
        && let Some(k) = keypad(logical)
    {
        return Some(k);
    }
    match logical {
        Key::Named(n) => named(*n, location == KeyLocation::Right),
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(ucs_to_keysym(c)),
                _ => None,
            }
        }
        // Without an IME, Korean keyboards may report the Hangul and
        // Hanja keys only by position.
        _ => match physical {
            PhysicalKey::Code(KeyCode::Lang1) => Some(0xff31),
            PhysicalKey::Code(KeyCode::Lang2) => Some(0xff34),
            _ => None,
        },
    }
}

/// The XT scan code ("qnum", 0x80 marks E0-prefixed keys) for the QEMU
/// Extended Key Event. As in the TurboVNC viewer patch for Korean
/// keyboards, the Hangul and Hanja keys are sent as 0x72 and 0x71
/// wherever they sit (Windows reports Right Alt and Right Ctrl).
pub fn qnum(logical: &Key, physical: PhysicalKey) -> Option<u32> {
    match logical {
        Key::Named(NamedKey::HangulMode) => return Some(0x72),
        Key::Named(NamedKey::HanjaMode) => return Some(0x71),
        _ => {}
    }
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    use KeyCode::*;
    Some(match code {
        Escape => 0x01,
        Digit1 => 0x02,
        Digit2 => 0x03,
        Digit3 => 0x04,
        Digit4 => 0x05,
        Digit5 => 0x06,
        Digit6 => 0x07,
        Digit7 => 0x08,
        Digit8 => 0x09,
        Digit9 => 0x0a,
        Digit0 => 0x0b,
        Minus => 0x0c,
        Equal => 0x0d,
        Backspace => 0x0e,
        Tab => 0x0f,
        KeyQ => 0x10,
        KeyW => 0x11,
        KeyE => 0x12,
        KeyR => 0x13,
        KeyT => 0x14,
        KeyY => 0x15,
        KeyU => 0x16,
        KeyI => 0x17,
        KeyO => 0x18,
        KeyP => 0x19,
        BracketLeft => 0x1a,
        BracketRight => 0x1b,
        Enter => 0x1c,
        ControlLeft => 0x1d,
        KeyA => 0x1e,
        KeyS => 0x1f,
        KeyD => 0x20,
        KeyF => 0x21,
        KeyG => 0x22,
        KeyH => 0x23,
        KeyJ => 0x24,
        KeyK => 0x25,
        KeyL => 0x26,
        Semicolon => 0x27,
        Quote => 0x28,
        Backquote => 0x29,
        ShiftLeft => 0x2a,
        Backslash => 0x2b,
        KeyZ => 0x2c,
        KeyX => 0x2d,
        KeyC => 0x2e,
        KeyV => 0x2f,
        KeyB => 0x30,
        KeyN => 0x31,
        KeyM => 0x32,
        Comma => 0x33,
        Period => 0x34,
        Slash => 0x35,
        ShiftRight => 0x36,
        NumpadMultiply => 0x37,
        AltLeft => 0x38,
        Space => 0x39,
        CapsLock => 0x3a,
        F1 => 0x3b,
        F2 => 0x3c,
        F3 => 0x3d,
        F4 => 0x3e,
        F5 => 0x3f,
        F6 => 0x40,
        F7 => 0x41,
        F8 => 0x42,
        F9 => 0x43,
        F10 => 0x44,
        NumLock => 0x45,
        ScrollLock => 0x46,
        Numpad7 => 0x47,
        Numpad8 => 0x48,
        Numpad9 => 0x49,
        NumpadSubtract => 0x4a,
        Numpad4 => 0x4b,
        Numpad5 => 0x4c,
        Numpad6 => 0x4d,
        NumpadAdd => 0x4e,
        Numpad1 => 0x4f,
        Numpad2 => 0x50,
        Numpad3 => 0x51,
        Numpad0 => 0x52,
        NumpadDecimal => 0x53,
        IntlBackslash => 0x56,
        F11 => 0x57,
        F12 => 0x58,
        NumpadEqual => 0x59,
        Lang2 => 0x71,
        Lang1 => 0x72,
        IntlRo => 0x73,
        Convert => 0x79,
        NonConvert => 0x7b,
        IntlYen => 0x7d,
        NumpadComma => 0x7e,
        MediaTrackPrevious => 0x90,
        MediaTrackNext => 0x99,
        NumpadEnter => 0x9c,
        ControlRight => 0x9d,
        AudioVolumeMute => 0xa0,
        MediaPlayPause => 0xa2,
        MediaStop => 0xa4,
        AudioVolumeDown => 0xae,
        AudioVolumeUp => 0xb0,
        BrowserHome => 0xb2,
        NumpadDivide => 0xb5,
        PrintScreen => 0xb7,
        AltRight => 0xb8,
        Pause => 0xc6,
        Home => 0xc7,
        ArrowUp => 0xc8,
        PageUp => 0xc9,
        ArrowLeft => 0xcb,
        ArrowRight => 0xcd,
        End => 0xcf,
        ArrowDown => 0xd0,
        PageDown => 0xd1,
        Insert => 0xd2,
        Delete => 0xd3,
        SuperLeft => 0xdb,
        SuperRight => 0xdc,
        ContextMenu => 0xdd,
        Power => 0xde,
        Sleep => 0xdf,
        WakeUp => 0xe3,
        BrowserForward => 0xe9,
        BrowserBack => 0xea,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_codes() {
        let code = |c| PhysicalKey::Code(c);
        let a = Key::Character("a".into());
        assert_eq!(qnum(&a, code(KeyCode::KeyA)), Some(0x1e));
        assert_eq!(qnum(&a, code(KeyCode::AltRight)), Some(0xb8));
        // Korean keyboards on Windows: Right Alt/Ctrl acting as Hangul/Hanja.
        let hangul = Key::Named(NamedKey::HangulMode);
        assert_eq!(qnum(&hangul, code(KeyCode::AltRight)), Some(0x72));
        let hanja = Key::Named(NamedKey::HanjaMode);
        assert_eq!(qnum(&hanja, code(KeyCode::ControlRight)), Some(0x71));
        assert_eq!(
            qnum(
                &a,
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified)
            ),
            None
        );
    }

    #[test]
    fn unicode_to_keysym() {
        assert_eq!(ucs_to_keysym('a'), 0x61);
        assert_eq!(ucs_to_keysym('é'), 0xe9);
        assert_eq!(ucs_to_keysym('Ł'), 0x01a3);
        assert_eq!(ucs_to_keysym('€'), 0x20ac); // EuroSign
        assert_eq!(ucs_to_keysym('ㄱ'), 0x0ea1); // Hangul_Kiyeog
        assert_eq!(ucs_to_keysym('한'), 0x0100_d55c);
        assert!(UCS_TO_KEYSYM.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn keys() {
        let none = PhysicalKey::Code(KeyCode::KeyA);
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(keysym(&ch("A"), none, KeyLocation::Standard), Some(0x41));
        assert_eq!(keysym(&ch("7"), none, KeyLocation::Numpad), Some(0xffb7));
        assert_eq!(
            keysym(&Key::Named(NamedKey::Shift), none, KeyLocation::Right),
            Some(0xffe2)
        );
        assert_eq!(
            keysym(&Key::Named(NamedKey::Enter), none, KeyLocation::Numpad),
            Some(0xff8d)
        );
        assert_eq!(
            keysym(&Key::Named(NamedKey::F12), none, KeyLocation::Standard),
            Some(0xffc9)
        );
        assert_eq!(
            keysym(
                &Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
                PhysicalKey::Code(KeyCode::Lang1),
                KeyLocation::Standard
            ),
            Some(0xff31)
        );
    }
}
