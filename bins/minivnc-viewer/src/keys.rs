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

#[cfg(test)]
mod tests {
    use super::*;

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
