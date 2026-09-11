//! Last-user / last-session persistence.
//! File format: two `key = value` lines — deliberately not a TOML dep.

use std::fmt::Write as _;
use std::path::PathBuf;

pub const DEFAULT_SESSION: &str = "niri-session";

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub last_user: Option<String>,
    pub last_session: Option<String>,
}

impl State {
    pub fn path() -> PathBuf {
        std::env::var("LOSKER_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/lib/losker/state.toml"))
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let mut st = Self { last_user: None, last_session: None };
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("user = ") {
                    st.last_user = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("session = ") {
                    st.last_session = Some(v.trim().to_string());
                }
            }
        }
        st
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = String::new();
        if let Some(u) = &self.last_user {
            let _ = writeln!(out, "user = {u}");
        }
        if let Some(s) = &self.last_session {
            let _ = writeln!(out, "session = {s}");
        }
        std::fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "osk-state-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn roundtrip() {
        let p = tmp("roundtrip");
        let st = State {
            last_user: Some("rbc".into()),
            last_session: Some("niri-session".into()),
        };
        st.save_to(&p).unwrap();
        assert_eq!(State::load_from(&p), st);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_file_is_empty() {
        let p = tmp("missing");
        assert_eq!(State::load_from(&p), State { last_user: None, last_session: None });
    }

    #[test]
    fn partial_file() {
        let p = tmp("partial");
        std::fs::write(&p, "user = alice\n").unwrap();
        let st = State::load_from(&p);
        assert_eq!(st.last_user.as_deref(), Some("alice"));
        assert_eq!(st.last_session, None);
        let _ = std::fs::remove_file(&p);
    }
}
