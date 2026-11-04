//! TurboVNC connection info files (`.turbovnc`): one `Parameter=value`
//! per line, `[section]` lines ignored. `~/.vnc/default.turbovnc` holds
//! the defaults, as for the TurboVNC viewer.

use std::fs;
use std::io;
use std::path::Path;

use rfb_proto::consts::*;

use crate::{Grab, Options, Scale, Span};

/// The parameters of a connection info file, in order.
pub fn read(path: &Path) -> io::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)?;
    Ok(parse(&text))
}

fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', '!', '[']))
        .filter_map(|l| {
            let (name, value) = l.split_once(['=', ':'])?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn subsampling_name(s: i32) -> &'static str {
    match s {
        x if x == ENCODING_SUBSAMP_4X - ENCODING_SUBSAMP_1X => "4X",
        x if x == ENCODING_SUBSAMP_2X - ENCODING_SUBSAMP_1X => "2X",
        x if x == ENCODING_SUBSAMP_GRAY - ENCODING_SUBSAMP_1X => "Gray",
        _ => "1X",
    }
}

/// The options as a connection info file the TurboVNC viewer can read
/// too. Passwords are not saved.
pub fn render(o: &Options) -> String {
    let mut lines = vec!["[connection]".to_string()];
    if o.listen.is_none() {
        lines.push(format!("Server={}::{}", o.host, o.port));
    }
    if let Some(via) = &o.via {
        lines.push(format!("Via={via}"));
    }
    if let Some(user) = &o.user {
        lines.push(format!("User={user}"));
    }
    if let Some(f) = &o.password_file {
        lines.push(format!("PasswordFile={}", f.display()));
    }
    if !o.security_types.is_empty() {
        let names: Vec<&str> = o.security_types.iter().map(|t| t.name()).collect();
        lines.push(format!("SecurityTypes={}", names.join(",")));
    }
    if let Some(ca) = &o.x509_ca {
        lines.push(format!("X509CA={}", ca.display()));
    }
    lines.push("[options]".into());
    let encoding = match o.encoding {
        ENCODING_ZRLE => "ZRLE",
        ENCODING_HEXTILE => "Hextile",
        ENCODING_RAW => "Raw",
        _ => "Tight",
    };
    let flag = |b: bool| if b { "1" } else { "0" };
    lines.extend([
        format!("Encoding={encoding}"),
        format!("JPEG={}", flag(o.jpeg)),
        format!("Quality={}", o.quality),
        format!("Subsampling={}", subsampling_name(o.subsampling)),
        format!("CompressLevel={}", o.compress_level),
        format!("Shared={}", flag(o.shared)),
        format!("ViewOnly={}", flag(o.view_only)),
        format!(
            "DesktopSize={}",
            if o.auto_resize { "Auto" } else { "Server" }
        ),
        format!(
            "Scale={}",
            match o.scale {
                Scale::Percent(p) => p.to_string(),
                Scale::Auto => "Auto".into(),
                Scale::FixedRatio => "FixedRatio".into(),
            }
        ),
        format!(
            "Span={}",
            match o.span {
                Span::Primary => "Primary",
                Span::All => "All",
                Span::Auto => "Auto",
            }
        ),
        format!(
            "GrabKeyboard={}",
            match o.grab {
                Grab::FullScreen => "FS",
                Grab::Always => "Always",
                Grab::Manual => "Manual",
            }
        ),
    ]);
    lines.join("\n") + "\n"
}

pub fn write(path: &Path, o: &Options) -> io::Result<()> {
    fs::write(path, render(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_parameter_lines() {
        let text =
            "[connection]\nServer = myhost:2\n# comment\n\nQuality=80\n[options]\nViewOnly: 1\n";
        assert_eq!(
            parse(text),
            [
                ("Server".to_string(), "myhost:2".to_string()),
                ("Quality".into(), "80".into()),
                ("ViewOnly".into(), "1".into()),
            ]
        );
    }
}
