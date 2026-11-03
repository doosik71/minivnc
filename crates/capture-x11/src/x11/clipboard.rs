//! Clipboard synchronisation with X selections, on its own connection and
//! thread: changes of CLIPBOARD (and PRIMARY) go to the clients, and text
//! from clients is offered as the owner of those selections, like TurboVNC
//! `cutpaste.c` does inside its X server.
//!
//! Transfers use a single property; INCR (incremental) transfers are not
//! supported.

use std::io;
use std::os::fd::AsFd;
use std::sync::mpsc;
use std::time::Duration;

use rfb_codec::clipboard::{latin1_to_string, string_to_latin1};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use super::xerr;

/// How often new text from clients is checked for.
const POLL: Duration = Duration::from_millis(50);

struct Atoms {
    clipboard: Atom,
    utf8_string: Atom,
    text: Atom,
    targets: Atom,
    timestamp: Atom,
    incr: Atom,
    /// Property on our window that conversions are stored in.
    property: Atom,
}

pub(super) struct Clipboard {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    /// Selections to follow and own: CLIPBOARD, and PRIMARY if synced.
    selections: Vec<Atom>,
    max: usize,
    /// Text from clients that we offer while owning the selections.
    owned: Option<String>,
    /// Last text sent to the clients, to skip repeats.
    last_sent: Option<String>,
    to_clients: mpsc::Sender<String>,
    from_clients: mpsc::Receiver<String>,
}

impl Clipboard {
    pub(super) fn new(
        conn: RustConnection,
        root: Window,
        primary: bool,
        max: usize,
        to_clients: mpsc::Sender<String>,
        from_clients: mpsc::Receiver<String>,
    ) -> io::Result<Self> {
        let atom = |name: &str| -> io::Result<Atom> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(xerr)?
                .reply()
                .map_err(xerr)?
                .atom)
        };
        let atoms = Atoms {
            clipboard: atom("CLIPBOARD")?,
            utf8_string: atom("UTF8_STRING")?,
            text: atom("TEXT")?,
            targets: atom("TARGETS")?,
            timestamp: atom("TIMESTAMP")?,
            incr: atom("INCR")?,
            property: atom("MINIVNC_SELECTION")?,
        };
        conn.xfixes_query_version(2, 0)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        let window = conn.generate_id().map_err(xerr)?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(xerr)?;
        let mut selections = vec![atoms.clipboard];
        if primary {
            selections.push(AtomEnum::PRIMARY.into());
        }
        for &s in &selections {
            conn.xfixes_select_selection_input(window, s, SelectionEventMask::SET_SELECTION_OWNER)
                .map_err(xerr)?;
        }
        conn.flush().map_err(xerr)?;
        Ok(Self {
            conn,
            window,
            atoms,
            selections,
            max,
            owned: None,
            last_sent: None,
            to_clients,
            from_clients,
        })
    }

    /// Runs until the X connection or the server goes away.
    pub(super) fn run(mut self) {
        if let Err(e) = self.event_loop() {
            warn!("clipboard synchronisation stopped: {e}");
        }
    }

    fn event_loop(&mut self) -> io::Result<()> {
        loop {
            match self.from_clients.try_recv() {
                Ok(text) => self.take_ownership(text)?,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            while let Some(event) = self.conn.poll_for_event().map_err(xerr)? {
                self.handle(event)?;
            }
            self.conn.flush().map_err(xerr)?;
            let timespec = Timespec {
                tv_sec: 0,
                tv_nsec: POLL.as_nanos() as _,
            };
            let fd = self.conn.stream().as_fd();
            match poll(&mut [PollFd::new(&fd, PollFlags::IN)], Some(&timespec)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn handle(&mut self, event: Event) -> io::Result<()> {
        match event {
            Event::XfixesSelectionNotify(e) => {
                // Our own ownership, or nobody owns it now.
                if e.owner == self.window || e.owner == NONE {
                    return Ok(());
                }
                self.conn
                    .convert_selection(
                        self.window,
                        e.selection,
                        self.atoms.utf8_string,
                        self.atoms.property,
                        e.selection_timestamp,
                    )
                    .map_err(xerr)?;
            }
            Event::SelectionNotify(e) => self.converted(e)?,
            Event::SelectionRequest(e) => self.request(e)?,
            Event::SelectionClear(e) => {
                debug!(selection = e.selection, "lost selection ownership");
            }
            _ => {}
        }
        Ok(())
    }

    /// A conversion we asked for has finished.
    fn converted(&mut self, e: SelectionNotifyEvent) -> io::Result<()> {
        if e.property == NONE {
            // Fall back to Latin-1 for owners without UTF8_STRING.
            if e.target == self.atoms.utf8_string {
                self.conn
                    .convert_selection(
                        self.window,
                        e.selection,
                        AtomEnum::STRING.into(),
                        self.atoms.property,
                        e.time,
                    )
                    .map_err(xerr)?;
            }
            return Ok(());
        }
        let reply = self
            .conn
            .get_property(
                true,
                self.window,
                self.atoms.property,
                AtomEnum::ANY,
                0,
                self.max.div_ceil(4) as u32,
            )
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        let text = if reply.type_ == self.atoms.incr {
            warn!("ignoring a large clipboard (INCR transfers are not supported)");
            return Ok(());
        } else if reply.type_ == AtomEnum::STRING.into() {
            latin1_to_string(&reply.value)
        } else {
            String::from_utf8_lossy(&reply.value).into_owned()
        };
        if text.is_empty() || self.last_sent.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        self.last_sent = Some(text.clone());
        // A closed channel means the source is gone; the loop ends soon.
        let _ = self.to_clients.send(text);
        Ok(())
    }

    /// Text from a client: own the selections and offer it.
    fn take_ownership(&mut self, text: String) -> io::Result<()> {
        for &s in &self.selections {
            self.conn
                .set_selection_owner(self.window, s, CURRENT_TIME)
                .map_err(xerr)?;
        }
        // Do not echo it back when it is converted.
        self.last_sent = Some(text.clone());
        self.owned = Some(text);
        Ok(())
    }

    /// Another X client wants the text we own (ICCCM).
    fn request(&mut self, e: SelectionRequestEvent) -> io::Result<()> {
        // Obsolete clients leave the property unset.
        let property = if e.property == NONE {
            e.target
        } else {
            e.property
        };
        let a = &self.atoms;
        let stored = match &self.owned {
            Some(text) if self.selections.contains(&e.selection) => {
                if e.target == a.targets {
                    let list = [
                        a.targets,
                        a.timestamp,
                        a.utf8_string,
                        a.text,
                        AtomEnum::STRING.into(),
                    ];
                    self.conn
                        .change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            AtomEnum::ATOM,
                            &list,
                        )
                        .map_err(xerr)?;
                    true
                } else if e.target == a.timestamp {
                    self.conn
                        .change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            AtomEnum::INTEGER,
                            &[CURRENT_TIME],
                        )
                        .map_err(xerr)?;
                    true
                } else if e.target == a.utf8_string || e.target == a.text {
                    self.conn
                        .change_property8(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            a.utf8_string,
                            text.as_bytes(),
                        )
                        .map_err(xerr)?;
                    true
                } else if e.target == u32::from(AtomEnum::STRING) {
                    self.conn
                        .change_property8(
                            PropMode::REPLACE,
                            e.requestor,
                            property,
                            AtomEnum::STRING,
                            &string_to_latin1(text),
                        )
                        .map_err(xerr)?;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        let notify = SelectionNotifyEvent {
            response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: e.time,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: if stored { property } else { NONE },
        };
        self.conn
            .send_event(false, e.requestor, EventMask::NO_EVENT, notify)
            .map_err(xerr)?;
        Ok(())
    }
}
