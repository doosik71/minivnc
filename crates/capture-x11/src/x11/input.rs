//! Input injection with XTest; the keymap comes from XKB.

use std::io;
use std::sync::Arc;

use rfb_server::InputSink;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xkb::{self, ConnectionExt as _, MapPart};
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::xerr;
use crate::keymap::{Action, Key, KeyType, Keyboard, KeyboardDevice, Keymap, TypeEntry};

/// Pointer buttons forwarded, as in TurboVNC `PtrAddEvent`.
const BUTTONS: u16 = 9;

pub struct X11Input {
    dev: XDevice,
    keyboard: Keyboard,
    pointer: Option<(u16, u16)>,
    buttons: u16,
}

struct XDevice {
    conn: RustConnection,
    root: u32,
    keymap: Option<Arc<Keymap>>,
}

impl X11Input {
    pub(super) fn new(conn: RustConnection, root: u32) -> io::Result<Self> {
        for ext in [xtest::X11_EXTENSION_NAME, xkb::X11_EXTENSION_NAME] {
            if conn.extension_information(ext).map_err(xerr)?.is_none() {
                return Err(io::Error::other(format!(
                    "X server lacks the {ext} extension"
                )));
            }
        }
        conn.xtest_get_version(2, 2)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        let xkb = conn
            .xkb_use_extension(1, 0)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        if !xkb.supported {
            return Err(io::Error::other("X server's XKB version is not supported"));
        }
        Ok(Self {
            dev: XDevice {
                conn,
                root,
                keymap: None,
            },
            keyboard: Keyboard::new(),
            pointer: None,
            buttons: 0,
        })
    }
}

impl XDevice {
    fn fake(&self, type_: u8, detail: u8, x: i16, y: i16) -> io::Result<()> {
        self.conn
            .xtest_fake_input(type_, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)
            .map_err(xerr)?;
        Ok(())
    }
}

fn mods(m: impl Into<u16>) -> u8 {
    m.into() as u8
}

/// Converts an XKB `GetMap` reply into the platform-independent model.
fn load_keymap(conn: &RustConnection) -> io::Result<Keymap> {
    let parts =
        MapPart::KEY_TYPES | MapPart::KEY_SYMS | MapPart::KEY_ACTIONS | MapPart::MODIFIER_MAP;
    let r = conn
        .xkb_get_map(
            xkb::ID::USE_CORE_KBD.into(),
            parts,
            0u16.into(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0u16.into(),
            0,
            0,
            0,
            0,
            0,
            0,
        )
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let types = r
        .map
        .types_rtrn
        .unwrap_or_default()
        .into_iter()
        .map(|t| KeyType {
            mods_mask: mods(t.mods_mask),
            map: t
                .map
                .iter()
                .enumerate()
                .map(|(i, e)| TypeEntry {
                    active: e.active,
                    mods_mask: mods(e.mods_mask),
                    level: e.level,
                    preserve: if t.has_preserve {
                        t.preserve.get(i).map_or(0, |p| mods(p.mask))
                    } else {
                        0
                    },
                })
                .collect(),
        })
        .collect();

    let span = usize::from(r.max_key_code) - usize::from(r.min_key_code) + 1;
    let mut keys = vec![Key::default(); span];
    let index =
        |kc: u8, first: u8| usize::from(kc) + usize::from(first) - usize::from(r.min_key_code);
    for (i, s) in r.map.syms_rtrn.unwrap_or_default().into_iter().enumerate() {
        if let Some(k) = keys.get_mut(index(i as u8, r.first_key_sym)) {
            k.group_info = s.group_info;
            k.width = s.width;
            k.types = s.kt_index;
            k.syms = s.syms;
        }
    }
    if let Some(acts) = r.map.key_actions {
        let mut all = acts.acts_rtrn_acts.into_iter();
        for (i, &count) in acts.acts_rtrn_count.iter().enumerate() {
            let list: Vec<Action> = all
                .by_ref()
                .take(count.into())
                .map(convert_action)
                .collect();
            if let Some(k) = keys.get_mut(index(i as u8, r.first_key_action)) {
                k.actions = list;
            }
        }
    }
    let mut modmap = [0u8; 256];
    for m in r.map.modmap_rtrn.unwrap_or_default() {
        modmap[usize::from(m.keycode)] = mods(m.mods);
    }
    Ok(Keymap {
        min_keycode: r.min_key_code,
        types,
        keys,
        modmap,
    })
}

fn convert_action(a: xkb::Action) -> Action {
    let set = a.as_setmods();
    let use_modmap = u8::from(set.flags) & u8::from(xkb::SA::USE_MOD_MAP_MODS) != 0;
    let mask = mods(set.mask);
    match set.type_ {
        xkb::SAType::NO_ACTION => Action::None,
        xkb::SAType::SET_MODS => Action::SetMods { mask, use_modmap },
        xkb::SAType::LOCK_MODS => {
            let lock = a.as_lockmods();
            Action::LockMods {
                mask: mods(lock.mask),
                use_modmap: u8::from(lock.flags) & u8::from(xkb::SA::USE_MOD_MAP_MODS) != 0,
            }
        }
        _ => Action::Other,
    }
}

impl KeyboardDevice for XDevice {
    fn keymap(&mut self) -> io::Result<Arc<Keymap>> {
        // Any mapping change invalidates the cached keymap.
        while let Some(event) = self.conn.poll_for_event().map_err(xerr)? {
            if matches!(event, Event::MappingNotify(_)) {
                self.keymap = None;
            }
        }
        if self.keymap.is_none() {
            self.keymap = Some(Arc::new(load_keymap(&self.conn)?));
        }
        Ok(Arc::clone(self.keymap.as_ref().expect("loaded above")))
    }

    fn state(&mut self) -> io::Result<u32> {
        let s = self
            .conn
            .xkb_get_state(xkb::ID::USE_CORE_KBD.into())
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        // TurboVNC uses the lookup state, but for XTest input the X server
        // leaves `lookup_mods` of the core keyboard at 0 (seen with Xvfb);
        // the effective `mods` are kept up to date.
        Ok(u32::from(mods(s.mods)) | (u32::from(u8::from(s.group)) << 13))
    }

    fn pressed_keys(&mut self) -> io::Result<[u8; 32]> {
        Ok(self
            .conn
            .query_keymap()
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?
            .keys)
    }

    fn fake_key(&mut self, keycode: u8, down: bool) -> io::Result<()> {
        self.fake(
            if down {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            },
            keycode,
            0,
            0,
        )
    }

    fn set_keysyms(&mut self, keycode: u8, lower: u32, upper: u32) -> io::Result<()> {
        // The server derives the XKB type (ONE_LEVEL / ALPHABETIC) from the
        // core mapping, as TurboVNC `AddKeysym` sets explicitly.
        let syms = if lower == upper {
            vec![lower]
        } else {
            vec![lower, upper]
        };
        self.conn
            .change_keyboard_mapping(1, keycode, syms.len() as u8, &syms)
            .map_err(xerr)?
            .check()
            .map_err(xerr)?;
        self.keymap = None;
        Ok(())
    }
}

impl InputSink for X11Input {
    fn key(&mut self, down: bool, keysym: u32) -> io::Result<()> {
        self.keyboard.key_event(&mut self.dev, keysym, down)?;
        self.dev.conn.flush().map_err(xerr)
    }

    fn pointer(&mut self, x: u16, y: u16, buttons: u16) -> io::Result<()> {
        if self.pointer != Some((x, y)) {
            // Detail 0 = absolute motion.
            self.dev.fake(MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)?;
            self.pointer = Some((x, y));
        }
        for i in 0..BUTTONS {
            let bit = 1 << i;
            if (buttons ^ self.buttons) & bit != 0 {
                let type_ = if buttons & bit != 0 {
                    BUTTON_PRESS_EVENT
                } else {
                    BUTTON_RELEASE_EVENT
                };
                self.dev.fake(type_, (i + 1) as u8, 0, 0)?;
            }
        }
        self.buttons = buttons;
        self.dev.conn.flush().map_err(xerr)
    }

    fn release_all_keys(&mut self) -> io::Result<()> {
        self.keyboard.release_all(&mut self.dev)?;
        self.dev.conn.flush().map_err(xerr)
    }
}
