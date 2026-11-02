//! Keysym -> keycode translation for injecting VNC key events into an X
//! server, ported from TurboVNC `input-xkb.c` and `kbdptr.c` (which borrow
//! from TigerVNC).
//!
//! VNC clients send keysyms; X needs keycodes. For each key press we search
//! the XKB keymap for a key that produces the keysym in the current modifier
//! state, faking Shift and Level 3 (AltGr) presses when another level is
//! needed, and bind unknown keysyms to unused keycodes.
//!
//! This module is platform independent: [`KeyboardDevice`] abstracts the X
//! server, so the logic is tested with a simulated keyboard.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use tracing::{debug, info, warn};

pub const SHIFT_MASK: u32 = 1 << 0;
pub const LOCK_MASK: u32 = 1 << 1;

const XK_TAB: u32 = 0xff09;
const XK_SHIFT_L: u32 = 0xffe1;
const XK_SHIFT_R: u32 = 0xffe2;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_CONTROL_R: u32 = 0xffe4;
const XK_META_L: u32 = 0xffe7;
const XK_META_R: u32 = 0xffe8;
const XK_ALT_L: u32 = 0xffe9;
const XK_ALT_R: u32 = 0xffea;
const XK_SUPER_L: u32 = 0xffeb;
const XK_SUPER_R: u32 = 0xffec;
const XK_HYPER_L: u32 = 0xffed;
const XK_HYPER_R: u32 = 0xffee;
const XK_NUM_LOCK: u32 = 0xff7f;
const XK_MODE_SWITCH: u32 = 0xff7e;
const XK_ISO_LEVEL3_SHIFT: u32 = 0xfe03;

/// Keysyms with the same meaning, tried when the first has no key
/// (TurboVNC `altKeysym`).
const ALT_KEYSYMS: &[(u32, u32)] = &[
    (XK_SHIFT_L, XK_SHIFT_R),
    (XK_CONTROL_L, XK_CONTROL_R),
    (XK_META_L, XK_META_R),
    (XK_ALT_L, XK_ALT_R),
    (XK_SUPER_L, XK_SUPER_R),
    (XK_HYPER_L, XK_HYPER_R),
    (0xff80, 0x0020), // KP_Space, space
    (0xff89, XK_TAB), // KP_Tab
    (0xff8d, 0xff0d), // KP_Enter, Return
    (0xff91, 0xffbe), // KP_F1..F4, F1..F4
    (0xff92, 0xffbf),
    (0xff93, 0xffc0),
    (0xff94, 0xffc1),
    (0xff95, 0xff50), // KP_Home, Home
    (0xff96, 0xff51), // KP_Left, Left
    (0xff97, 0xff52), // KP_Up, Up
    (0xff98, 0xff53), // KP_Right, Right
    (0xff99, 0xff54), // KP_Down, Down
    (0xff9a, 0xff55), // KP_Page_Up, Page_Up
    (0xff9b, 0xff56), // KP_Page_Down, Page_Down
    (0xff9c, 0xff57), // KP_End, End
    (0xff9d, 0xff58), // KP_Begin, Begin
    (0xff9e, 0xff63), // KP_Insert, Insert
    (0xff9f, 0xffff), // KP_Delete, Delete
    (0xffbd, 0x003d), // KP_Equal, equal
    (0xffaa, 0x002a), // KP_Multiply, asterisk
    (0xffab, 0x002b), // KP_Add, plus
    (0xffac, 0x002c), // KP_Separator, comma
    (0xffad, 0x002d), // KP_Subtract, minus
    (0xffae, 0x002e), // KP_Decimal, period
    (0xffaf, 0x002f), // KP_Divide, slash
    (0xffb0, 0x0030), // KP_0..KP_9, 0..9
    (0xffb1, 0x0031),
    (0xffb2, 0x0032),
    (0xffb3, 0x0033),
    (0xffb4, 0x0034),
    (0xffb5, 0x0035),
    (0xffb6, 0x0036),
    (0xffb7, 0x0037),
    (0xffb8, 0x0038),
    (0xffb9, 0x0039),
    (XK_ISO_LEVEL3_SHIFT, XK_MODE_SWITCH),
];

/// One entry of an XKB key type: modifiers `mods_mask` select `level`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeEntry {
    pub active: bool,
    pub mods_mask: u8,
    pub level: u8,
    pub preserve: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyType {
    /// Modifiers the type looks at.
    pub mods_mask: u8,
    pub map: Vec<TypeEntry>,
}

/// The parts of XKB key actions this code needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Action {
    #[default]
    None,
    SetMods {
        mask: u8,
        use_modmap: bool,
    },
    LockMods {
        mask: u8,
        use_modmap: bool,
    },
    Other,
}

/// One keycode's symbols and actions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    /// XKB group info: group count in bits 0-3, out-of-range handling in
    /// bits 4-7.
    pub group_info: u8,
    /// Levels per group.
    pub width: u8,
    /// Key type index per group.
    pub types: [u8; 4],
    /// `width` keysyms per group.
    pub syms: Vec<u32>,
    /// Empty, or one action per keysym.
    pub actions: Vec<Action>,
}

impl Key {
    fn num_groups(&self) -> usize {
        usize::from(self.group_info & 0x0f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub min_keycode: u8,
    pub types: Vec<KeyType>,
    /// Keys from `min_keycode` upwards.
    pub keys: Vec<Key>,
    /// Real modifiers bound to each keycode.
    pub modmap: [u8; 256],
}

/// Lower and upper case of a keysym (XKB `XkbConvertCase`), for Latin-1 and
/// Unicode keysyms; other keysyms have no case here.
pub fn convert_case(keysym: u32) -> (u32, u32) {
    match keysym {
        0x41..=0x5a => (keysym + 0x20, keysym),
        0x61..=0x7a => (keysym, keysym - 0x20),
        0xc0..=0xde if keysym != 0xd7 => (keysym + 0x20, keysym),
        0xe0..=0xfe if keysym != 0xf7 => (keysym, keysym - 0x20),
        0x0100_0000..=0x0110_ffff => {
            let Some(c) = char::from_u32(keysym - 0x0100_0000) else {
                return (keysym, keysym);
            };
            (
                single_char(c.to_lowercase(), keysym),
                single_char(c.to_uppercase(), keysym),
            )
        }
        _ => (keysym, keysym),
    }
}

/// The Unicode keysym of a one-character case mapping, else `fallback`.
fn single_char(mut chars: impl Iterator<Item = char>, fallback: u32) -> u32 {
    match (chars.next(), chars.next()) {
        (Some(c), None) => u32::from(c) + 0x0100_0000,
        _ => fallback,
    }
}

impl Keymap {
    pub fn max_keycode(&self) -> u8 {
        (usize::from(self.min_keycode) + self.keys.len())
            .saturating_sub(1)
            .min(255) as u8
    }

    fn key(&self, keycode: u8) -> Option<&Key> {
        self.keys
            .get(usize::from(keycode.checked_sub(self.min_keycode)?))
    }

    fn keycodes(&self) -> impl DoubleEndedIterator<Item = u8> + use<> {
        self.min_keycode..=self.max_keycode()
    }

    /// The group a key uses in `state` (TurboVNC `XkbKeyEffectiveGroup`).
    fn effective_group(&self, key: &Key, state: u32) -> Option<usize> {
        let n = key.num_groups();
        if n == 0 {
            return None;
        }
        let group = ((state >> 13) & 3) as usize;
        Some(if group < n {
            group
        } else {
            match key.group_info & 0xc0 {
                0x40 => n - 1,
                0x80 => match usize::from((key.group_info & 0x30) >> 4) {
                    g if g < n => g,
                    _ => 0,
                },
                _ => group % n,
            }
        })
    }

    /// Index into `syms`/`actions` and the type's consumed modifiers.
    fn column(&self, keycode: u8, state: u32) -> Option<(&Key, usize, u8)> {
        let key = self.key(keycode)?;
        let group = self.effective_group(key, state)?;
        let ty = self.types.get(usize::from(key.types[group]))?;
        let mut col = group * usize::from(key.width);
        let mut preserve = 0;
        if let Some(e) = ty
            .map
            .iter()
            .find(|e| e.active && (state as u8 & ty.mods_mask) == e.mods_mask)
        {
            col += usize::from(e.level);
            preserve = e.preserve;
        }
        Some((key, col, ty.mods_mask & !preserve))
    }

    /// Keysym of `keycode` in `state` and the modifiers its type may consume
    /// (`XkbTranslateKeyCode`); keysym 0 = none.
    pub fn translate(&self, keycode: u8, state: u32) -> (u32, u8) {
        match self.column(keycode, state) {
            Some((key, col, mods)) => (key.syms.get(col).copied().unwrap_or(0), mods),
            None => (0, 0),
        }
    }

    /// The action of `keycode` in `state` (`XkbKeyActionPtr`).
    pub fn action(&self, keycode: u8, state: u32) -> Option<Action> {
        let (key, col, _) = self.column(keycode, state)?;
        key.actions.get(col).copied()
    }

    /// A key producing `keysym` in exactly `state`.
    pub fn find_keycode(&self, keysym: u32, state: u32) -> Option<u8> {
        self.keycodes().find(|&kc| {
            let (mut ks, consumed) = self.translate(kc, state);
            if ks == 0 {
                return false;
            }
            // XkbTranslateKeyCode returns the potentially consumed
            // modifiers; Lock applies only if it was not consumed.
            if state & !u32::from(consumed) & LOCK_MASK != 0 {
                ks = convert_case(ks).1;
            }
            ks == keysym
        })
    }

    /// TurboVNC `KeysymToKeycode` with `new_state`: tries `state`, then with
    /// Shift toggled, Level 3 toggled, and both.
    pub fn keysym_to_keycode(&self, keysym: u32, state: u32) -> Option<(u8, u32)> {
        if let Some(kc) = self.find_keycode(keysym, state) {
            return Some((kc, state));
        }
        let shift = state ^ SHIFT_MASK;
        if let Some(kc) = self.find_keycode(keysym, shift) {
            return Some((kc, shift));
        }
        let l3 = u32::from(self.level_three_mask(state));
        if l3 == 0 {
            return None;
        }
        let level3 = (state & !l3) | (!state & l3);
        if let Some(kc) = self.find_keycode(keysym, level3) {
            return Some((kc, level3));
        }
        let both = level3 ^ SHIFT_MASK;
        self.find_keycode(keysym, both).map(|kc| (kc, both))
    }

    fn set_mods_mask(&self, keycode: u8, action: Option<Action>) -> Option<u8> {
        match action? {
            Action::SetMods { mask, use_modmap } => Some(if use_modmap {
                self.modmap[usize::from(keycode)]
            } else {
                mask
            }),
            _ => None,
        }
    }

    fn level_three_key(&self, state: u32) -> Option<u8> {
        self.find_keycode(XK_ISO_LEVEL3_SHIFT, state)
            .or_else(|| self.find_keycode(XK_MODE_SWITCH, state))
    }

    /// Modifier mask of the Level 3 (AltGr) key, or 0 (`GetLevelThreeMask`).
    pub fn level_three_mask(&self, state: u32) -> u8 {
        let state = state & !0xff;
        self.level_three_key(state)
            .and_then(|kc| self.set_mods_mask(kc, self.action(kc, state)))
            .unwrap_or(0)
    }

    /// A key that sets Shift (`PressShift`), if Shift is not already on.
    pub fn shift_key(&self, state: u32) -> Option<u8> {
        if state & SHIFT_MASK != 0 {
            return None;
        }
        self.keycodes().find(|&kc| {
            self.set_mods_mask(kc, self.action(kc, state))
                .is_some_and(|m| u32::from(m) & SHIFT_MASK != 0)
        })
    }

    /// Pressed keys that set any of `mask` (`ReleaseShift` /
    /// `ReleaseLevelThree`).
    pub fn pressed_modifier_keys(&self, state: u32, mask: u8, pressed: &[u8; 32]) -> Vec<u8> {
        if state as u8 & mask == 0 {
            return Vec::new();
        }
        self.keycodes()
            .filter(|&kc| pressed[usize::from(kc / 8)] & (1 << (kc % 8)) != 0)
            .filter(|&kc| {
                self.set_mods_mask(kc, self.action(kc, state))
                    .is_some_and(|m| m & mask != 0)
            })
            .collect()
    }

    /// The Level 3 key to press (`PressLevelThree`).
    pub fn level_three_press_key(&self, state: u32) -> Option<u8> {
        let mask = self.level_three_mask(state);
        if mask == 0 || state as u8 & mask != 0 {
            return None;
        }
        let kc = self.level_three_key(state)?;
        matches!(self.action(kc, state), Some(Action::SetMods { .. })).then_some(kc)
    }

    pub fn is_lock_modifier(&self, keycode: u8, state: u32) -> bool {
        matches!(self.action(keycode, state), Some(Action::LockMods { .. }))
    }

    /// Whether NumLock changes the meaning of `keycode`
    /// (`IsAffectedByNumLock`).
    pub fn is_affected_by_numlock(&self, keycode: u8, state: u32) -> bool {
        let state = state & !0xff;
        let Some(numlock) = self.find_keycode(XK_NUM_LOCK, state) else {
            return false;
        };
        let numlock_mask = match self.action(numlock, state) {
            // As in TurboVNC (and TigerVNC), the modmap of the key being
            // tested, not of the NumLock key, is used here.
            Some(Action::LockMods { mask, use_modmap }) => {
                if use_modmap {
                    self.modmap[usize::from(keycode)]
                } else {
                    mask
                }
            }
            _ => return false,
        };
        let Some(key) = self.key(keycode) else {
            return false;
        };
        let Some(group) = self.effective_group(key, state) else {
            return false;
        };
        self.types
            .get(usize::from(key.types[group]))
            .is_some_and(|t| t.mods_mask & numlock_mask != 0)
    }

    /// The highest keycode without symbols, for binding unknown keysyms.
    pub fn unused_keycode(&self) -> Option<u8> {
        self.keycodes()
            .rev()
            .find(|&kc| self.key(kc).is_some_and(|k| k.num_groups() == 0))
    }
}

/// What the keyboard logic needs from the X server.
pub trait KeyboardDevice {
    /// The current keymap (reloaded after changes).
    fn keymap(&mut self) -> io::Result<Arc<Keymap>>;
    /// XKB lookup state: modifiers in bits 0-7, group in bits 13-14.
    fn state(&mut self) -> io::Result<u32>;
    /// Bitmap of pressed keycodes.
    fn pressed_keys(&mut self) -> io::Result<[u8; 32]>;
    fn fake_key(&mut self, keycode: u8, down: bool) -> io::Result<()>;
    /// Binds `keycode` to `lower`/`upper` (equal for keysyms without case).
    fn set_keysyms(&mut self, keycode: u8, lower: u32, upper: u32) -> io::Result<()>;
}

/// Turns keysym events into fake key presses (TurboVNC `KeyEvent`).
#[derive(Debug, Default)]
pub struct Keyboard {
    /// Keycode pressed for each keysym still down, so releases match.
    pressed: HashMap<u8, u32>,
}

impl Keyboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key_event(
        &mut self,
        dev: &mut impl KeyboardDevice,
        keysym: u32,
        down: bool,
    ) -> io::Result<()> {
        if !down {
            match self
                .pressed
                .iter()
                .find(|&(_, &ks)| ks == keysym)
                .map(|(&kc, _)| kc)
            {
                Some(kc) => {
                    self.pressed.remove(&kc);
                    dev.fake_key(kc, false)?;
                }
                None => debug!("unexpected release of keysym {keysym:#x}"),
            }
            return Ok(());
        }

        let mut km = dev.keymap()?;
        let state = dev.state()?;
        let mut found = km.keysym_to_keycode(keysym, state);

        // Shift+Alt is often mapped to Meta: use that rather than faking.
        if state & SHIFT_MASK != 0 && (keysym == XK_ALT_L || keysym == XK_ALT_R) {
            let meta_sym = if keysym == XK_ALT_L {
                XK_META_L
            } else {
                XK_META_R
            };
            let alt = km.find_keycode(keysym, state & !SHIFT_MASK);
            let meta = km.find_keycode(meta_sym, state);
            if meta.is_some() && alt == meta {
                debug!("replacing Shift+Alt with Shift+Meta");
                found = meta.map(|kc| (kc, state));
            }
        }

        // Try equivalent keysyms if there is no exact match.
        if found.is_none() {
            found = alternatives(keysym).find_map(|alt| km.keysym_to_keycode(alt, state));
        }

        // There is no lock synchronisation, so lock keys are ignored.
        if let Some((kc, new_state)) = found
            && km.is_lock_modifier(kc, new_state)
        {
            debug!("ignoring lock key {keysym:#x}");
            return Ok(());
        }

        let (mut keycode, mut new_state) = match found {
            Some(f) => f,
            None => {
                let Some(kc) = km.unused_keycode() else {
                    warn!("could not add new keysym {keysym:#x}: no free keycode");
                    return Ok(());
                };
                let (lower, upper) = convert_case(keysym);
                dev.set_keysyms(kc, lower, upper)?;
                info!("mapped unknown keysym {keysym:#x} to keycode {kc}");
                km = dev.keymap()?;
                match km.keysym_to_keycode(keysym, state) {
                    Some(f) => f,
                    None => {
                        warn!("cannot generate keycode for newly added keysym {keysym:#x}");
                        return Ok(());
                    }
                }
            }
        };

        // Avoid faking Shift on the keypad (where X lets Shift toggle the
        // NumLock meaning) if an alternative keysym works without it.
        if (state ^ new_state) & SHIFT_MASK != 0 && km.is_affected_by_numlock(keycode, state) {
            let alt = alternatives(keysym).find_map(|alt| {
                let (kc, s) = km.keysym_to_keycode(alt, state)?;
                ((state ^ s) & SHIFT_MASK == 0 || !km.is_affected_by_numlock(kc, state))
                    .then_some((kc, s))
            });
            if let Some((kc, s)) = alt {
                keycode = kc;
                new_state = s;
            }
        }

        debug!("keysym {keysym:#x}: state {state:#x} -> keycode {keycode} in {new_state:#x}");

        // Clients send plain Tab; keep the user's Shift for Shift+Tab.
        if keysym == XK_TAB && state & SHIFT_MASK != 0 {
            new_state |= SHIFT_MASK;
        }

        let level_three_mask = if (new_state & !SHIFT_MASK) != (state & !SHIFT_MASK) {
            u32::from(km.level_three_mask(state))
        } else {
            0
        };
        let pressed = dev.pressed_keys()?;

        let mut shift_press = None;
        let mut shift_release = Vec::new();
        if state & SHIFT_MASK == 0 && new_state & SHIFT_MASK != 0 {
            let Some(kc) = km.shift_key(state) else {
                warn!("no modifier key for a Shift press");
                return Ok(());
            };
            dev.fake_key(kc, true)?;
            shift_press = Some(kc);
        } else if state & SHIFT_MASK != 0 && new_state & SHIFT_MASK == 0 {
            shift_release = km.pressed_modifier_keys(state, SHIFT_MASK as u8, &pressed);
            if shift_release.is_empty() {
                warn!("no modifier key for a Shift release");
                return Ok(());
            }
            for &kc in &shift_release {
                dev.fake_key(kc, false)?;
            }
        }

        let mut l3_press = None;
        let mut l3_release = Vec::new();
        if state & level_three_mask == 0 && new_state & level_three_mask != 0 {
            let Some(kc) = km.level_three_press_key(state) else {
                warn!("no modifier key for a Level 3 press");
                return Ok(());
            };
            dev.fake_key(kc, true)?;
            l3_press = Some(kc);
        } else if state & level_three_mask != 0 && new_state & level_three_mask == 0 {
            l3_release = km.pressed_modifier_keys(state, level_three_mask as u8, &pressed);
            if l3_release.is_empty() {
                warn!("no modifier key for a Level 3 release");
                return Ok(());
            }
            for &kc in &l3_release {
                dev.fake_key(kc, false)?;
            }
        }

        dev.fake_key(keycode, true)?;
        if let Some((&old, _)) = self
            .pressed
            .iter()
            .find(|&(&kc, &ks)| ks == keysym && kc != keycode)
        {
            warn!("keysym {keysym:#x} generated by both keys {old} and {keycode}");
            self.pressed.remove(&old);
        }
        self.pressed.insert(keycode, keysym);

        // Undo the fake modifiers.
        if let Some(kc) = l3_press {
            dev.fake_key(kc, false)?;
        }
        for &kc in &l3_release {
            dev.fake_key(kc, true)?;
        }
        if let Some(kc) = shift_press {
            dev.fake_key(kc, false)?;
        }
        for &kc in &shift_release {
            dev.fake_key(kc, true)?;
        }
        Ok(())
    }

    /// Releases every key the X server reports as down
    /// (`KbdReleaseAllKeys`).
    pub fn release_all(&mut self, dev: &mut impl KeyboardDevice) -> io::Result<()> {
        self.pressed.clear();
        let pressed = dev.pressed_keys()?;
        for kc in 0..=255u8 {
            if pressed[usize::from(kc / 8)] & (1 << (kc % 8)) != 0 {
                dev.fake_key(kc, false)?;
            }
        }
        Ok(())
    }
}

fn alternatives(keysym: u32) -> impl Iterator<Item = u32> {
    ALT_KEYSYMS.iter().filter_map(move |&(a, b)| match keysym {
        k if k == a => Some(b),
        k if k == b => Some(a),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOD5: u8 = 0x80;
    const ONE_LEVEL: u8 = 0;
    const TWO_LEVEL: u8 = 1;
    const ALPHABETIC: u8 = 2;
    const FOUR_LEVEL: u8 = 3;

    fn entry(mods_mask: u8, level: u8) -> TypeEntry {
        TypeEntry {
            active: true,
            mods_mask,
            level,
            preserve: 0,
        }
    }

    fn key(ty: u8, syms: &[u32], action: Action) -> Key {
        let width = syms.len() as u8;
        Key {
            group_info: 1,
            width,
            types: [ty; 4],
            syms: syms.to_vec(),
            actions: if action == Action::None {
                vec![]
            } else {
                vec![action; syms.len()]
            },
        }
    }

    /// A small US-like layout with AltGr (Level 3 on Mod5).
    fn us_keymap() -> Keymap {
        let mut keys = vec![Key::default(); 256 - 8];
        let mut set = |kc: u8, k: Key| keys[usize::from(kc) - 8] = k;
        set(10, key(TWO_LEVEL, &[0x31, 0x21], Action::None)); // 1 !
        set(23, key(TWO_LEVEL, &[XK_TAB, 0xfe20], Action::None)); // Tab ISO_Left_Tab
        set(24, key(FOUR_LEVEL, &[0x71, 0x51, 0x40, 0x40], Action::None)); // q Q @ @
        set(38, key(ALPHABETIC, &[0x61, 0x41], Action::None)); // a A
        set(
            50,
            key(
                ONE_LEVEL,
                &[XK_SHIFT_L],
                Action::SetMods {
                    mask: SHIFT_MASK as u8,
                    use_modmap: false,
                },
            ),
        );
        set(
            64,
            key(
                TWO_LEVEL,
                &[XK_ALT_L, XK_META_L],
                Action::SetMods {
                    mask: 0x08,
                    use_modmap: false,
                },
            ),
        );
        set(
            66,
            key(
                ONE_LEVEL,
                &[0xffe5],
                Action::LockMods {
                    mask: LOCK_MASK as u8,
                    use_modmap: false,
                },
            ),
        ); // Caps_Lock
        set(
            108,
            key(
                ONE_LEVEL,
                &[XK_ISO_LEVEL3_SHIFT],
                Action::SetMods {
                    mask: MOD5,
                    use_modmap: false,
                },
            ),
        );
        Keymap {
            min_keycode: 8,
            types: vec![
                KeyType {
                    mods_mask: 0,
                    map: vec![],
                },
                KeyType {
                    mods_mask: SHIFT_MASK as u8,
                    map: vec![entry(SHIFT_MASK as u8, 1)],
                },
                KeyType {
                    mods_mask: (SHIFT_MASK | LOCK_MASK) as u8,
                    map: vec![entry(SHIFT_MASK as u8, 1), entry(LOCK_MASK as u8, 1)],
                },
                KeyType {
                    mods_mask: SHIFT_MASK as u8 | MOD5,
                    map: vec![
                        entry(SHIFT_MASK as u8, 1),
                        entry(MOD5, 2),
                        entry(SHIFT_MASK as u8 | MOD5, 3),
                    ],
                },
            ],
            keys,
            modmap: [0; 256],
        }
    }

    /// Simulated X keyboard: modifier state follows SetMods keys held down.
    struct Sim {
        km: Arc<Keymap>,
        down: [u8; 32],
        events: Vec<(u8, bool)>,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                km: Arc::new(us_keymap()),
                down: [0; 32],
                events: vec![],
            }
        }
    }

    impl KeyboardDevice for Sim {
        fn keymap(&mut self) -> io::Result<Arc<Keymap>> {
            Ok(Arc::clone(&self.km))
        }
        fn state(&mut self) -> io::Result<u32> {
            Ok((0..=255u8)
                .filter(|&kc| self.down[usize::from(kc / 8)] & (1 << (kc % 8)) != 0)
                .filter_map(|kc| match self.km.action(kc, 0) {
                    Some(Action::SetMods { mask, .. }) => Some(u32::from(mask)),
                    _ => None,
                })
                .fold(0, |a, m| a | m))
        }
        fn pressed_keys(&mut self) -> io::Result<[u8; 32]> {
            Ok(self.down)
        }
        fn fake_key(&mut self, kc: u8, down: bool) -> io::Result<()> {
            let bit = 1 << (kc % 8);
            if down {
                self.down[usize::from(kc / 8)] |= bit;
            } else {
                self.down[usize::from(kc / 8)] &= !bit;
            }
            self.events.push((kc, down));
            Ok(())
        }
        fn set_keysyms(&mut self, kc: u8, lower: u32, upper: u32) -> io::Result<()> {
            let k = if lower == upper {
                key(ONE_LEVEL, &[lower], Action::None)
            } else {
                key(ALPHABETIC, &[lower, upper], Action::None)
            };
            Arc::make_mut(&mut self.km).keys[usize::from(kc) - 8] = k;
            Ok(())
        }
    }

    fn press(kb: &mut Keyboard, sim: &mut Sim, keysym: u32) -> Vec<(u8, bool)> {
        sim.events.clear();
        kb.key_event(sim, keysym, true).unwrap();
        std::mem::take(&mut sim.events)
    }

    #[test]
    fn plain_and_shifted_letters() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        assert_eq!(press(&mut kb, &mut sim, 0x61), [(38, true)]);
        kb.key_event(&mut sim, 0x61, false).unwrap();
        // 'A' without Shift held: fake Shift around the key.
        assert_eq!(
            press(&mut kb, &mut sim, 0x41),
            [(50, true), (38, true), (50, false)]
        );
        kb.key_event(&mut sim, 0x41, false).unwrap();
        assert_eq!(sim.events.last(), Some(&(38, false)));
    }

    #[test]
    fn client_shift_is_respected_or_undone() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        press(&mut kb, &mut sim, XK_SHIFT_L);
        // Shift is down: 'A' needs nothing extra ...
        assert_eq!(press(&mut kb, &mut sim, 0x41), [(38, true)]);
        // ... but '1' needs Shift released temporarily.
        assert_eq!(
            press(&mut kb, &mut sim, 0x31),
            [(50, false), (10, true), (50, true)]
        );
        // Shift+Tab keeps Shift.
        assert_eq!(press(&mut kb, &mut sim, XK_TAB), [(23, true)]);
    }

    #[test]
    fn level_three_is_faked() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        assert_eq!(
            press(&mut kb, &mut sim, 0x40),
            [(108, true), (24, true), (108, false)]
        );
    }

    #[test]
    fn lock_keys_are_ignored_and_alternatives_used() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        assert!(press(&mut kb, &mut sim, 0xffe5).is_empty());
        // Shift_R has no key; Shift_L is used instead.
        assert_eq!(press(&mut kb, &mut sim, XK_SHIFT_R), [(50, true)]);
    }

    #[test]
    fn unknown_keysyms_get_a_free_keycode() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        // Greek small alpha (Unicode keysym): bound to the highest free key.
        let alpha = 0x0100_03b1;
        assert_eq!(press(&mut kb, &mut sim, alpha), [(255, true)]);
        assert_eq!(convert_case(alpha), (alpha, 0x0100_0391));
        kb.key_event(&mut sim, alpha, false).unwrap();
        // Hangul (no case) reuses the next free key below.
        assert_eq!(press(&mut kb, &mut sim, 0x0100_d55c), [(254, true)]);
    }

    #[test]
    fn release_all_releases_server_state() {
        let (mut kb, mut sim) = (Keyboard::new(), Sim::new());
        press(&mut kb, &mut sim, XK_SHIFT_L);
        press(&mut kb, &mut sim, 0x61);
        sim.events.clear();
        kb.release_all(&mut sim).unwrap();
        assert_eq!(sim.events, [(38, false), (50, false)]);
        assert_eq!(sim.state().unwrap(), 0);
    }

    #[test]
    fn caps_lock_state_uses_upper_case() {
        let km = us_keymap();
        // Lock alone selects level 1 for ALPHABETIC keys.
        assert_eq!(km.find_keycode(0x41, LOCK_MASK), Some(38));
    }
}
