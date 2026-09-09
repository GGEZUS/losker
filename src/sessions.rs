//! Session discovery from wayland-sessions desktop files (same source
//! regreet uses). Each session = (display name, exec command).

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub name: String,
    pub cmd: String,
}

pub fn list() -> Vec<Session> {
    let mut out = list_from(Path::new("/usr/share/wayland-sessions"));
    // prefer niri first (single-user tablet: the daily driver on top)
    out.sort_by(|a, b| {
        let an = a.name.eq_ignore_ascii_case("niri");
        let bn = b.name.eq_ignore_ascii_case("niri");
        bn.cmp(&an).then_with(|| a.name.cmp(&b.name))
    });
    out
}

pub fn list_from(dir: &Path) -> Vec<Session> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut name = None;
        let mut cmd = None;
        let mut no_display = false;
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("Name=") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Exec=") {
                cmd = Some(v.trim().to_string());
            } else if line.trim() == "NoDisplay=true" {
                no_display = true;
            }
        }
        if no_display {
            continue;
        }
        if let (Some(name), Some(cmd)) = (name, cmd) {
            if !cmd.is_empty() {
                out.push(Session { name, cmd });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, file: &str, content: &str) {
        std::fs::write(dir.join(file), content).unwrap();
    }

    #[test]
    fn parses_and_filters() {
        let d = std::env::temp_dir().join(format!("osk-sess-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "niri.desktop", "[Desktop Entry]\nName=Niri\nExec=niri-session\n");
        write(&d, "hidden.desktop", "[Desktop Entry]\nName=Hidden\nExec=x\nNoDisplay=true\n");
        write(&d, "bash.desktop", "[Desktop Entry]\nName=Bash\nExec=bash\n");
        write(&d, "noexec.desktop", "[Desktop Entry]\nName=NoExec\n");

        let mut s = list_from(&d);
        s.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Bash");
        assert_eq!(s[1].name, "Niri");
        assert_eq!(s[1].cmd, "niri-session");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn niri_sorts_first() {
        let d = std::env::temp_dir().join(format!("osk-sess-sort-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        write(&d, "a.desktop", "[Desktop Entry]\nName=Alpha\nExec=a\n");
        write(&d, "n.desktop", "[Desktop Entry]\nName=niri\nExec=niri-session\n");
        let s = list_from(&d);
        // list_from doesn't sort; use list() ordering logic manually
        let mut s = s;
        s.sort_by(|a, b| {
            let an = a.name.eq_ignore_ascii_case("niri");
            let bn = b.name.eq_ignore_ascii_case("niri");
            bn.cmp(&an).then_with(|| a.name.cmp(&b.name))
        });
        assert_eq!(s[0].name, "niri");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn missing_dir_is_empty() {
        assert!(list_from(Path::new("/nonexistent-sessions-dir")).is_empty());
    }
}
