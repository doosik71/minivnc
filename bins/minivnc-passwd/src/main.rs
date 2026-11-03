//! Creates VNC password files compatible with TurboVNC `vncpasswd`.
//!
//! Supports `vncpasswd [-v] [FILE]`, `vncpasswd -f`, and one-time passwords
//! for a running server (`-o`, `-c`). The user ACL options (`-a`, `-r`,
//! `-t`) are not supported.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use rfb_auth::{MAX_PASSWORD_LEN, Password, Passwords};

const USAGE: &str = "\
usage: minivnc-passwd [-v] [FILE]
       minivnc-passwd -f
       minivnc-passwd -o [-v] [-display DISPLAY]
       minivnc-passwd -c [-display DISPLAY]

  FILE   password file to write (default: ~/.vnc/passwd)
  -v     also ask for a view-only password without asking first
         (with -o: also make a view-only one-time password)
  -f     read the password (and an optional view-only password) from
         standard input, one per line, and write the file to standard output
  -o     make a one-time password for the server on the X display
         (default: $DISPLAY) and print it; it works for one connection
  -c     clear the one-time password";

/// `vncpasswd` refuses shorter passwords when prompting.
const MIN_PROMPTED_LEN: usize = 6;

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("minivnc-passwd: {msg}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut from_stdin = false;
    let mut also_view = false;
    let mut file = None;
    let mut otp = None;
    let mut display = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-f" => from_stdin = true,
            "-v" => also_view = true,
            "-o" => otp = Some(true),
            "-c" => otp = Some(false),
            "-display" => display = Some(args.next().ok_or("-display needs a value")?),
            s if s.starts_with('-') => {
                eprintln!("{USAGE}");
                return Err(String::new());
            }
            _ if file.is_some() => return Err("more than one file name".into()),
            _ => file = Some(PathBuf::from(arg)),
        }
    }

    if let Some(set) = otp {
        return one_time_password(set, also_view, display.as_deref());
    }

    if from_stdin {
        if file.is_some() {
            return Err("cannot specify filename with -f".into());
        }
        let passwords = read_stdin().map_err(|e| format!("could not read password: {e}"))?;
        return io::stdout()
            .write_all(&passwords.encode())
            .map_err(|e| e.to_string());
    }

    let file = match file {
        Some(f) => f,
        None => {
            let dir = home_dir()?.join(".vnc");
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let f = dir.join("passwd");
            eprintln!("Using password file {}", f.display());
            f
        }
    };

    let full = ask_password()?;
    if !also_view {
        eprint!("Would you like to enter a view-only password (y/n)? ");
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        also_view = answer.trim_start().starts_with(['y', 'Y']);
    } else {
        eprintln!("Enter the view-only password");
    }
    let view_only = if also_view {
        Some(ask_password()?)
    } else {
        None
    };

    Passwords { full, view_only }
        .write_file(&file)
        .map_err(|e| format!("cannot write password file {}: {e}", file.display()))
}

/// TurboVNC `vncpasswd -o`: eight decimal digits from a random number,
/// given to the server through the X display.
fn one_time_password(set: bool, also_view: bool, display: Option<&str>) -> Result<(), String> {
    let mut value = Vec::new();
    if set {
        let random = rfb_auth::challenge().map_err(|e| e.to_string())?;
        let mut digits = |i: usize, what: &str| {
            let n = u32::from_be_bytes(random[i..i + 4].try_into().expect("4 bytes"));
            let p: String = format!("{n:08}").chars().take(MAX_PASSWORD_LEN).collect();
            eprintln!("{what} one-time password: {p}");
            value.extend_from_slice(p.as_bytes());
        };
        digits(0, "Full control");
        if also_view {
            digits(4, "View-only");
        }
    }
    set_otp(display, &value)
}

#[cfg(target_os = "linux")]
fn set_otp(display: Option<&str>, value: &[u8]) -> Result<(), String> {
    capture_x11::set_otp(display, value).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn set_otp(_: Option<&str>, _: &[u8]) -> Result<(), String> {
    Err("one-time passwords need the X11 server (Linux)".into())
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "no HOME environment variable".into())
}

/// Keeps the first 8 bytes, like the C version (which may split a UTF-8
/// character; the result must match what viewers derive from the same text).
fn truncate(p: &str) -> Password {
    if p.len() > MAX_PASSWORD_LEN {
        eprintln!("Warning: password truncated to the length of {MAX_PASSWORD_LEN}.");
    }
    Password::new(p.as_bytes())
}

fn prompt(text: &str) -> Result<String, String> {
    rpassword::prompt_password(text).map_err(|e| format!("can't get password: {e}"))
}

fn ask_password() -> Result<Password, String> {
    loop {
        let p = prompt("Password: ")?;
        if p.len() < MIN_PROMPTED_LEN {
            return Err("Password too short".into());
        }
        let p = truncate(&p);
        if Password::new(prompt("Verify:   ")?.as_bytes()) == p {
            return Ok(p);
        }
        eprintln!("Passwords do not match. Please try again.\n");
    }
}

/// `vncpasswd -f`: first line is the full-control password, an optional
/// second line the view-only password.
fn read_stdin() -> io::Result<Passwords> {
    let mut lines = io::stdin().lock().lines();
    let full = lines.next().ok_or(io::ErrorKind::UnexpectedEof)??;
    let view_only = lines.next().transpose()?;
    Ok(Passwords {
        full: truncate(&full),
        view_only: view_only.as_deref().map(truncate),
    })
}
