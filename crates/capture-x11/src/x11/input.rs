//! Input injection with XTest; the keymap comes from XKB.

use std::io;
use std::sync::{Arc, mpsc};

use rfb_proto::consts::{EDS_RESULT_NO_RESOURCES, EDS_RESULT_PROHIBITED};
use rfb_server::InputSink;
use tracing::{debug, info, warn};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _, ModeInfo};
use x11rb::protocol::xkb::{self, ConnectionExt as _, MapPart};
use x11rb::protocol::xproto::{
    AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::xerr;
use crate::keymap::{Action, Key, KeyType, Keyboard, KeyboardDevice, Keymap, TypeEntry};
use crate::qnum_table::{QNUM_TO_XORGEVDEV, QNUM_TO_XORGKBD};

/// Pointer buttons forwarded: 9 as in TurboVNC `PtrAddEvent`, plus the
/// extra ones of the ExtendedMouseButtons extension.
const BUTTONS: u16 = 15;

pub struct X11Input {
    dev: XDevice,
    keyboard: Keyboard,
    pointer: Option<(u16, u16)>,
    buttons: u16,
    /// QEMU key code -> X keycode for the server's XKB keycodes.
    qnum: Option<&'static [u8]>,
    clipboard: mpsc::Sender<String>,
}

struct XDevice {
    conn: RustConnection,
    root: u32,
    keymap: Option<Arc<Keymap>>,
}

impl X11Input {
    pub(super) fn new(
        conn: RustConnection,
        root: u32,
        clipboard: mpsc::Sender<String>,
    ) -> io::Result<Self> {
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
        let qnum = qnum_table(&conn, root)?;
        Ok(Self {
            dev: XDevice {
                conn,
                root,
                keymap: None,
            },
            keyboard: Keyboard::new(),
            pointer: None,
            buttons: 0,
            qnum,
            clipboard,
        })
    }
}

/// The QEMU key code table for the server's XKB rules, as in TurboVNC
/// `QEMUExtKeyboardEventInit`: evdev keycodes for the `evdev` rules, kbd
/// keycodes for `base` and `xorg`, none otherwise.
fn qnum_table(conn: &RustConnection, root: u32) -> io::Result<Option<&'static [u8]>> {
    let atom = conn
        .intern_atom(false, b"_XKB_RULES_NAMES")
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?
        .atom;
    let names = conn
        .get_property(false, root, atom, AtomEnum::STRING, 0, 1024)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?
        .value;
    let rules = names.split(|&b| b == 0).next().unwrap_or_default();
    let table: Option<&'static [u8]> = match rules {
        b"evdev" => Some(&QNUM_TO_XORGEVDEV),
        b"base" | b"xorg" => Some(&QNUM_TO_XORGKBD),
        _ => None,
    };
    let rules = String::from_utf8_lossy(rules);
    match table {
        Some(_) => info!("XKB rules {rules}: QEMU key codes enabled"),
        None => warn!("QEMU key codes need evdev or xorg XKB rules, not {rules:?}: disabled"),
    }
    Ok(table)
}

/// Resizes the X screen with RandR, limited to the sizes it allows; returns
/// the size set or an `EDS_RESULT_*` code.
///
/// The output switches to a WxH mode (created if needed) while the screen
/// is large enough for both sizes, so a failure never leaves the output
/// off. Xvfb cannot change its output mode; then only the screen size
/// changes, which Xvfb allows up to its initial size.
fn resize_screen(
    conn: &RustConnection,
    root: u32,
    width: u16,
    height: u16,
) -> io::Result<Result<(u16, u16), u16>> {
    if conn
        .extension_information(randr::X11_EXTENSION_NAME)
        .map_err(xerr)?
        .is_none()
    {
        return Ok(Err(EDS_RESULT_PROHIBITED));
    }
    conn.randr_query_version(1, 3)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let range = conn
        .randr_get_screen_size_range(root)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    // Unlike TurboVNC, which refuses, grow as far as the X server allows
    // (Xvfb: its initial size); the viewer shows the rest as a margin.
    let (asked_w, asked_h) = (width, height);
    let width = width.clamp(range.min_width, range.max_width);
    let height = height.clamp(range.min_height, range.max_height);
    if (width, height) != (asked_w, asked_h) {
        info!(
            "{asked_w}x{asked_h} is beyond the X screen's {}x{} to {}x{}: using {width}x{height}",
            range.min_width, range.min_height, range.max_width, range.max_height
        );
    }
    // Millimetres at 96 DPI.
    let mm = |px: u16| (u32::from(px) * 254 / 960).max(1);
    let set_screen = |w: u16, h: u16| -> io::Result<()> {
        conn.randr_set_screen_size(root, w, h, mm(w), mm(h))
            .map_err(xerr)?
            .check()
            .map_err(xerr)
    };
    let geometry = conn
        .get_geometry(root)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let current = (geometry.width, geometry.height);

    let res = conn
        .randr_get_screen_resources_current(root)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let mut active = None;
    for &crtc in &res.crtcs {
        let info = conn
            .randr_get_crtc_info(crtc, res.config_timestamp)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        if info.mode != x11rb::NONE && !info.outputs.is_empty() {
            active = Some((crtc, info));
            break;
        }
    }
    let Some((crtc, info)) = active else {
        set_screen(width, height)?;
        return Ok(Ok((width, height)));
    };

    let switch_mode = || -> io::Result<()> {
        let existing = res
            .modes
            .iter()
            .find(|m| (m.width, m.height) == (width, height));
        let mode = match existing {
            Some(m) => m.id,
            None => {
                let name = format!("{width}x{height}");
                let mode = ModeInfo {
                    id: 0,
                    width,
                    height,
                    dot_clock: u32::from(width) * u32::from(height) * 60,
                    hsync_start: width,
                    hsync_end: width,
                    htotal: width,
                    hskew: 0,
                    vsync_start: height,
                    vsync_end: height,
                    vtotal: height,
                    name_len: name.len() as u16,
                    mode_flags: 0u32.into(),
                };
                conn.randr_create_mode(root, mode, name.as_bytes())
                    .map_err(xerr)?
                    .reply()
                    .map_err(xerr)?
                    .mode
            }
        };
        for &output in &info.outputs {
            let modes = conn
                .randr_get_output_info(output, res.config_timestamp)
                .map_err(xerr)?
                .reply()
                .map_err(xerr)?
                .modes;
            if !modes.contains(&mode) {
                conn.randr_add_output_mode(output, mode)
                    .map_err(xerr)?
                    .check()
                    .map_err(xerr)?;
            }
        }
        let big = (width.max(current.0), height.max(current.1));
        if big != current {
            set_screen(big.0, big.1)?;
        }
        let status = conn
            .randr_set_crtc_config(
                crtc,
                x11rb::CURRENT_TIME,
                res.config_timestamp,
                0,
                0,
                mode,
                info.rotation,
                &info.outputs,
            )
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?
            .status;
        if status != randr::SetConfig::SUCCESS {
            return Err(io::Error::other(format!("CRTC configuration {status:?}")));
        }
        Ok(())
    };
    if let Err(e) = switch_mode() {
        debug!("cannot switch the output mode ({e}); resizing the screen only");
    }
    set_screen(width, height)?;
    Ok(Ok((width, height)))
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

    fn qemu_keys(&self) -> bool {
        self.qnum.is_some()
    }

    fn qemu_key(&mut self, down: bool, keysym: u32, keycode: u32) -> io::Result<()> {
        // TurboVNC `ExtKeyEvent`: fall back to the keysym for unknown codes.
        let x_keycode = self
            .qnum
            .and_then(|t| t.get(keycode as usize))
            .copied()
            .filter(|&kc| keycode != 0 && kc != 0);
        match x_keycode {
            Some(kc) => {
                self.keyboard.raw_key(&mut self.dev, kc, keysym, down)?;
                self.dev.conn.flush().map_err(xerr)
            }
            None if keysym != 0 => self.key(down, keysym),
            None => Ok(()),
        }
    }

    fn set_clipboard(&mut self, text: &str) -> io::Result<()> {
        // A closed channel means the clipboard thread stopped; it logged why.
        let _ = self.clipboard.send(text.to_string());
        Ok(())
    }

    fn set_desktop_size(&mut self, width: u16, height: u16) -> Result<(u16, u16), u16> {
        match resize_screen(&self.dev.conn, self.dev.root, width, height) {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!("desktop resize to {width}x{height} failed: {e}");
                Err(EDS_RESULT_NO_RESOURCES)
            }
        }
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
