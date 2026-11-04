//! Windows: the viewer has no console (`windows_subsystem = "windows"`),
//! so it asks for the server, passwords and certificate decisions with
//! dialog boxes (templates in `minivnc-viewer.rc`) and reports errors in
//! message boxes.

// Win32 calls throughout.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::iter;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::EM_SETREADONLY;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DialogBoxParamW, EndDialog, GetDlgItem, GetDlgItemTextW, ICON_BIG, ICON_SMALL, IDCANCEL, IDOK,
    IDYES, LoadIconW, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
    MB_YESNO, MESSAGEBOX_STYLE, MessageBoxW, SendDlgItemMessageW, SendMessageW, SetDlgItemTextW,
    WM_COMMAND, WM_INITDIALOG, WM_SETICON,
};

/// Resource IDs, as in `minivnc-viewer.rc`.
const IDI_ICON: u16 = 1;
const IDD_CONNECT: u16 = 101;
const IDD_LOGIN: u16 = 102;
const IDC_SERVER: i32 = 1001;
const IDC_USER: i32 = 1002;
const IDC_PASSWORD: i32 = 1003;

const TITLE: &str = "MiniVNC Viewer";

/// The server shown in the password dialog.
static SERVER: OnceLock<String> = OnceLock::new();

/// What a dialog shows and, after OK, what was entered.
#[derive(Default)]
struct Fields {
    server: String,
    user: String,
    password: String,
    server_read_only: bool,
}

thread_local! {
    /// The fields of the open dialog; the dialogs are modal.
    static FIELDS: RefCell<Fields> = RefCell::default();
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(iter::once(0)).collect()
}

fn item_text(dlg: HWND, id: i32) -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetDlgItemTextW(dlg, id, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe extern "system" fn dialog_proc(dlg: HWND, msg: u32, wparam: WPARAM, _: LPARAM) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                let icon = LoadIconW(GetModuleHandleW(ptr::null()), IDI_ICON as _);
                for size in [ICON_SMALL, ICON_BIG] {
                    SendMessageW(dlg, WM_SETICON, size as WPARAM, icon as LPARAM);
                }
                let focus = FIELDS.with_borrow(|f| {
                    let has_user = !GetDlgItem(dlg, IDC_USER).is_null();
                    let has_server = !GetDlgItem(dlg, IDC_SERVER).is_null();
                    if has_server {
                        SetDlgItemTextW(dlg, IDC_SERVER, wide(&f.server).as_ptr());
                        SendDlgItemMessageW(
                            dlg,
                            IDC_SERVER,
                            EM_SETREADONLY,
                            WPARAM::from(f.server_read_only),
                            0,
                        );
                    }
                    if has_user {
                        SetDlgItemTextW(dlg, IDC_USER, wide(&f.user).as_ptr());
                    }
                    // The first field still to fill.
                    if has_server && f.server.is_empty() {
                        IDC_SERVER
                    } else if has_user && f.user.is_empty() {
                        IDC_USER
                    } else {
                        IDC_PASSWORD
                    }
                });
                SetFocus(GetDlgItem(dlg, focus));
                // The focus is set.
                0
            }
            WM_COMMAND => {
                let id = (wparam & 0xffff) as i32;
                if id == IDOK {
                    FIELDS.with_borrow_mut(|f| {
                        if !GetDlgItem(dlg, IDC_SERVER).is_null() {
                            f.server = item_text(dlg, IDC_SERVER).trim().to_string();
                        }
                        if !GetDlgItem(dlg, IDC_USER).is_null() {
                            f.user = item_text(dlg, IDC_USER).trim().to_string();
                        }
                        f.password = item_text(dlg, IDC_PASSWORD);
                    });
                }
                if id == IDOK || id == IDCANCEL {
                    EndDialog(dlg, id as isize);
                    return 1;
                }
                0
            }
            _ => 0,
        }
    }
}

/// Shows a dialog; `None` if it was cancelled.
fn show(template: u16, fields: Fields) -> Option<Fields> {
    FIELDS.set(fields);
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(ptr::null()),
            template as _,
            ptr::null_mut(),
            Some(dialog_proc),
            0,
        )
    };
    let fields = FIELDS.take();
    (result == IDOK as isize).then_some(fields)
}

/// The connection dialog: the server (filled with `server`) and the VNC
/// password, which may be left empty.
pub fn connect(server: &str) -> Option<(String, String)> {
    let f = show(
        IDD_CONNECT,
        Fields {
            server: server.to_string(),
            ..Fields::default()
        },
    )?;
    Some((f.server, f.password))
}

/// Sets the server the password dialog names.
pub fn set_server(server: String) {
    let _ = SERVER.set(server);
}

/// The VNC password, asked during authentication.
pub fn password() -> Option<String> {
    let f = show(
        IDD_CONNECT,
        Fields {
            server: SERVER.get().cloned().unwrap_or_default(),
            server_read_only: true,
            ..Fields::default()
        },
    )?;
    Some(f.password)
}

/// User name and password for Unix Login / Plain authentication.
pub fn login(user: Option<&str>) -> Option<(String, String)> {
    let f = show(
        IDD_LOGIN,
        Fields {
            user: user.unwrap_or_default().to_string(),
            ..Fields::default()
        },
    )?;
    Some((f.user, f.password))
}

fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            wide(text).as_ptr(),
            wide(TITLE).as_ptr(),
            style,
        )
    }
}

/// Yes/No about a certificate problem; No is the default.
pub fn confirm(question: &str) -> bool {
    message_box(question, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
}

pub fn error(text: &str) {
    message_box(text, MB_OK | MB_ICONERROR);
}

pub fn info(text: &str) {
    message_box(text, MB_OK | MB_ICONINFORMATION);
}

/// Attaches to the console of the process that started the viewer, if it
/// has one, so that messages printed now reach it.
pub fn attach_console() -> bool {
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}
