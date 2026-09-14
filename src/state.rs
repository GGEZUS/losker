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

/// Login-form prefill when no state file exists yet (first boot): the
/// machine's human account, read from /etc/passwd. `Some` only when there
/// is exactly one candidate — the greeter has no username-entry UI, so an
/// ambiguous machine gets an empty prefill and submit refuses rather than
/// guessing at an account.
pub fn default_user() -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    let users = human_users(&passwd);
    if users.len() == 1 { users.into_iter().next() } else { None }
}

/// Human accounts in passwd(5) text: uid >= 1000, not `nobody` (65534),
/// and a shell that can actually log in. Pure core of [`default_user`].
fn human_users(passwd: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        let Ok(uid) = f[2].parse::<u32>() else { continue };
        let shell = f[6];
        if uid >= 1000
            && uid != 65534
            && !shell.ends_with("nologin")
            && shell != "/bin/false"
            && shell != "/usr/bin/false"
            && shell != "/bin/sync"
        {
            out.push(f[0].to_string());
        }
    }
    out
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

    #[test]
    fn human_users_filters_system_and_service_accounts() {
        let fixture = "\
root:x:0:0:root:/root:/bin/bash\n\
daemon:x:1:1::/usr/sbin:/usr/sbin/nologin\n\
git:x:988:988::/home/git:/usr/bin/git-shell\n\
svc:x:1001:1001::/var/svc:/sbin/nologin\n\
bot:x:1002:1002::/var/bot:/bin/false\n\
mirror:x:1003:1003::/var/mirror:/bin/sync\n\
nobody:x:65534:65534:nobody:/:/usr/bin/nologin\n\
alice:x:1000:1000:Alice:/home/alice:/bin/fish\n\
garbage line without colons\n\
short:x:1000:1000::/home\n";
        assert_eq!(human_users(fixture), vec!["alice".to_string()]);
    }

    #[test]
    fn human_users_reports_every_candidate_for_the_caller_to_judge() {
        let two = "a:x:1000:0::/home/a:/bin/sh\nb:x:1001:0::/home/b:/bin/sh\n";
        assert_eq!(human_users(two).len(), 2);
        assert!(human_users("root:x:0:0::/root:/bin/bash\n").is_empty());
        // default_user's exactly-one rule, exercised through the pure core
        let one = "a:x:1000:0::/home/a:/bin/sh\n";
        let users = human_users(one);
        assert_eq!(users.len(), 1, "one candidate must stay one");
    }
}
