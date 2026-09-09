//! PAM authentication for the in-session lock screen (`osk-greeter --lock`).
//!
//! The lock runs as the logged-in user, so there is no greetd socket —
//! authentication goes straight to PAM (service "osk-greeter"), the same
//! route swaylock takes. pam_unix verifies the user's OWN password through
//! the suid-root unix_chkpwd helper, so no privileges are required and
//! other users' passwords are refused by the helper itself.
//!
//! Everything here runs on one worker thread per attempt: the raw pamh
//! pointer is created, used and destroyed inside `run_auth_sync`, never
//! crossing threads.

use crate::auth::Outcome;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

const PAM_SUCCESS: c_int = 0;
/// "Password:" — the only prompt style a lock screen ever answers
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_BUF_ERR: c_int = 5;
const SERVICE: &str = "osk-greeter";

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: extern "C" fn(
        num_msg: c_int,
        msg: *const *const PamMessage,
        resp: *mut *mut PamResponse,
        appdata_ptr: *mut c_void,
    ) -> c_int,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_strerror(pamh: *mut c_void, errnum: c_int) -> *const c_char;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
    // responses are freed by libpam with free() — allocate with malloc
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
}

/// What the conversation callback draws from: the password the user typed
/// into the OSK. `take()`n on the first secret prompt — if a stack asks
/// twice, the second ask gets nothing.
struct ConvData {
    password: Mutex<Option<CString>>,
}

extern "C" fn conv_cb(
    num_msg: c_int,
    msgs: *const *const PamMessage,
    resps: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    let data = unsafe { &*(appdata as *const ConvData) };
    let n = num_msg.max(0) as usize;
    let out = unsafe { calloc(n + 1, std::mem::size_of::<PamResponse>()) as *mut PamResponse };
    if out.is_null() {
        return PAM_BUF_ERR;
    }
    unsafe { *resps = out };

    for i in 0..n {
        let style = unsafe { (**msgs.add(i)).msg_style };
        if style != PAM_PROMPT_ECHO_OFF {
            continue; // info/error text: leave the (zeroed) slot empty
        }
        if let Some(answer) = data.password.lock().unwrap().take() {
            let bytes = answer.as_bytes_with_nul();
            let buf = unsafe { calloc(bytes.len(), 1) } as *mut c_char;
            if buf.is_null() {
                return PAM_BUF_ERR;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
                (*out.add(i)).resp = buf;
            }
        }
        // already consumed → leave the zeroed slot (resp = NULL): a stack
        // that asks twice gets nothing rather than an empty string
    }
    PAM_SUCCESS
}

/// Authenticate `username`'s `password` against PAM, reporting through `tx`
/// on a worker thread (the GTK main loop polls the channel).
pub fn authenticate(username: String, password: String, tx: Sender<Outcome>) {
    std::thread::spawn(move || {
        let _ = tx.send(run_auth_sync(&username, password));
    });
}

fn run_auth_sync(username: &str, password: String) -> Outcome {
    let service = match CString::new(SERVICE) {
        Ok(s) => s,
        Err(_) => return Outcome::Fatal("PAM: bad service name".into()),
    };
    let user = match CString::new(username) {
        Ok(u) => u,
        Err(_) => return Outcome::Fatal("PAM: bad username".into()),
    };
    let pw = match CString::new(password) {
        Ok(p) => p,
        // a NUL inside the password can never match a real one anyway
        Err(_) => return Outcome::AuthFailed("Authentication failure".into()),
    };

    let data = Box::into_raw(Box::new(ConvData {
        password: Mutex::new(Some(pw)),
    }));
    let conv = PamConv {
        conv: conv_cb,
        appdata_ptr: data as *mut c_void,
    };

    unsafe {
        let mut pamh: *mut c_void = std::ptr::null_mut();
        let started = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut pamh);
        if started != PAM_SUCCESS || pamh.is_null() {
            drop(Box::from_raw(data));
            return Outcome::Fatal(format!("PAM: start failed ({started}) — is /etc/pam.d/{SERVICE} installed?"));
        }

        let code = pam_authenticate(pamh, 0);
        // strerror needs a live handle — read it before pam_end
        let desc = if code == PAM_SUCCESS {
            String::new()
        } else {
            let s = pam_strerror(pamh, code);
            if s.is_null() {
                format!("PAM error {code}")
            } else {
                CStr::from_ptr(s).to_string_lossy().into_owned()
            }
        };
        pam_end(pamh, code);
        drop(Box::from_raw(data));

        if code == PAM_SUCCESS {
            Outcome::Granted
        } else {
            Outcome::AuthFailed(desc)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pam_service_file_exists_and_is_not_empty() {
        // a 0-byte /etc/pam.d/osk-greeter once shipped silently: PAM then
        // falls back to /etc/pam.d/other (pam_warn + deny-all) and EVERY
        // password fails with a perfectly misleading "Authentication
        // failure". This canary catches that deploy mistake.
        let meta = std::fs::metadata("/etc/pam.d/osk-greeter")
            .expect("/etc/pam.d/osk-greeter is missing — install it");
        assert!(
            meta.len() > 0,
            "/etc/pam.d/osk-greeter is EMPTY — reinstall it"
        );
    }

    #[test]
    fn wrong_password_is_never_granted() {
        // real libpam against the real (own) account — a wrong password must
        // come back as a failure, whatever the exact PAM error text is
        let user = std::env::var("USER").unwrap_or_else(|_| "nobody".into());
        let out = run_auth_sync(&user, "definitely-not-the-password-osk-test".into());
        assert!(
            !matches!(out, Outcome::Granted),
            "wrong password returned {out:?}"
        );
    }

    #[test]
    fn conv_hands_over_the_password_once() {
        let data = Box::into_raw(Box::new(ConvData {
            password: Mutex::new(Some(CString::new("s3cret").unwrap())),
        }));
        let msg = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: std::ptr::null(),
        };
        let msgs: *const PamMessage = &msg;
        let mut resp: *mut PamResponse = std::ptr::null_mut();

        let rc = conv_cb(1, &msgs, &mut resp, data as *mut c_void);
        assert_eq!(rc, PAM_SUCCESS);
        unsafe {
            let answered = CStr::from_ptr((*resp).resp).to_bytes();
            assert_eq!(answered, b"s3cret");
        }

        // second ask (a stack that re-prompts): nothing left to give
        let rc2 = conv_cb(1, &msgs, &mut resp, data as *mut c_void);
        assert_eq!(rc2, PAM_SUCCESS);
        unsafe {
            assert!((*resp).resp.is_null());
        }
        drop(unsafe { Box::from_raw(data) });
    }
}
