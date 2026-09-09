//! Auth engine: drives the greetd handshake off the GTK main thread and
//! reports outcomes back through a main-context channel (Send-safe; the
//! UI side touches widgets).

use crate::greetd_ipc::{self, ErrorType, GreetdClient, Request, Response};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Step-level trace for boot debugging — greetd gives the greeter a private
/// /tmp, so this lives in the greeter-owned state dir where we can read it.
pub fn trace(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/lib/osk-greeter/trace.log")
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "[{now:>12}] {msg}");
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// PAM wants the password (message text, usually "Password: ")
    Prompt(String),
    /// start_session succeeded — greetd is taking over
    Granted,
    /// wrong credentials; session torn down, retry from create_session
    AuthFailed(String),
    /// protocol/transport failure
    Fatal(String),
}

pub struct Auth {
    client: Arc<Mutex<Option<GreetdClient>>>,
    demo: bool,
}

impl Auth {
    pub fn new(demo: bool) -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            demo,
        }
    }

    /// Kick off the session for `username`. Outcomes: Prompt (type the
    /// password), Granted (no auth required), AuthFailed, Fatal.
    pub fn begin(&self, username: String, session_cmd: String, tx: Sender<Outcome>) {
        if self.demo {
            let tx2 = tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                let _ = tx2.send(Outcome::Prompt("Password: ".into()));
            });
            return;
        }

        let client_slot = self.client.clone();
        std::thread::spawn(move || {
            trace(&format!("begin: spawning for user {username}"));
            let mut guard = client_slot.lock().unwrap();
            if guard.is_none() {
                match GreetdClient::connect() {
                    Ok(c) => {
                        trace("begin: connected to greetd socket");
                        *guard = Some(c);
                    }
                    Err(e) => {
                        trace(&format!("begin: connect FAILED: {e}"));
                        drop(guard);
                        let _ = tx.send(Outcome::Fatal(e.to_string()));
                        return;
                    }
                }
            }
            let Some(client) = guard.as_mut() else { return };

            trace("begin: sending create_session");
            let outcome = match client.request(&Request::CreateSession { username }) {
                Ok(resp) => {
                    trace(&format!("begin: response {resp:?}"));
                    match resp {
                        Response::AuthMessage { auth_message, .. } => {
                            Outcome::Prompt(auth_message)
                        }
                        // session created without any auth question: start right away
                        Response::Success => start(client, &session_cmd),
                        Response::Error { error_type, description } => {
                            classify(error_type, description)
                        }
                    }
                }
                Err(e) => {
                    trace(&format!("begin: request FAILED: {e}"));
                    Outcome::Fatal(e.to_string())
                }
            };
            drop(guard);
            let _ = tx.send(outcome);
        });
    }

    /// Answer the password prompt; on success, start the session.
    pub fn answer(&self, password: String, session_cmd: String, tx: Sender<Outcome>) {
        if self.demo {
            let tx2 = tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(700));
                if password == "fail" {
                    let _ = tx2.send(Outcome::AuthFailed("Authentication failure".into()));
                } else {
                    let _ = tx2.send(Outcome::Granted);
                }
            });
            return;
        }

        let client_slot = self.client.clone();
        std::thread::spawn(move || {
            trace("answer: spawning");
            let mut guard = client_slot.lock().unwrap();
            let Some(client) = guard.as_mut() else {
                let _ = tx.send(Outcome::Fatal("no greetd session in progress".into()));
                return;
            };

            trace("answer: sending post_auth_message_response");
            let outcome = match client.request(&Request::PostAuthMessageResponse {
                response: Some(password),
            }) {
                Ok(resp) => {
                    trace(&format!("answer: response {resp:?}"));
                    match resp {
                        // PAM follow-up question (rare): ask again
                        Response::AuthMessage { auth_message, .. } => {
                            Outcome::Prompt(auth_message)
                        }
                        Response::Success => start(client, &session_cmd),
                        Response::Error { error_type, description } => {
                            classify(error_type, description)
                        }
                    }
                }
                Err(e) => {
                    trace(&format!("answer: request FAILED: {e}"));
                    Outcome::Fatal(e.to_string())
                }
            };
            drop(guard);
            let _ = tx.send(outcome);
        });
    }

    /// Tear down any in-flight session (after a failure, before retrying).
    pub fn cancel(&self) {
        if self.demo {
            return;
        }
        let client_slot = self.client.clone();
        std::thread::spawn(move || {
            let mut guard = client_slot.lock().unwrap();
            if let Some(client) = guard.as_mut() {
                let _ = client.request(&Request::CancelSession);
            }
        });
    }
}

fn start(client: &mut GreetdClient, session_cmd: &str) -> Outcome {
    trace(&format!("start: sending start_session [{session_cmd}]"));
    match client.request(&Request::StartSession {
        cmd: vec![session_cmd.to_string()],
        env: Vec::new(),
    }) {
        Ok(Response::Success) => {
            trace("start: success");
            Outcome::Granted
        }
        Ok(Response::Error { description, .. }) => {
            trace(&format!("start: error: {description}"));
            Outcome::Fatal(description)
        }
        Ok(_) => Outcome::Fatal("unexpected response to start_session".into()),
        Err(e) => {
            trace(&format!("start: FAILED: {e}"));
            Outcome::Fatal(e.to_string())
        }
    }
}

fn classify(error_type: ErrorType, description: String) -> Outcome {
    match error_type {
        ErrorType::AuthError => Outcome::AuthFailed(description),
        ErrorType::Error => Outcome::Fatal(description),
    }
}

// keep the protocol types referenced for doc cohesion
#[allow(dead_code)]
fn _uses(_: greetd_ipc::AuthMessageType) {}
