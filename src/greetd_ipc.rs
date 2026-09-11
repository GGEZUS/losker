//! greetd IPC client — JSON lines over the unix socket in $GREETD_SOCK.
//! Protocol: greetd(7). Client speaks first; greetd replies per request.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "type")]
pub enum Request {
    #[serde(rename = "create_session")]
    CreateSession { username: String },

    #[serde(rename = "post_auth_message_response")]
    PostAuthMessageResponse { response: Option<String> },

    #[serde(rename = "start_session")]
    StartSession { cmd: Vec<String>, env: Vec<String> },

    #[serde(rename = "cancel_session")]
    CancelSession,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "type")]
pub enum Response {
    #[serde(rename = "success")]
    Success,

    #[serde(rename = "auth_message")]
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },

    #[serde(rename = "error")]
    Error {
        error_type: ErrorType,
        description: String,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    AuthError,
    Error,
}

#[derive(Debug)]
pub enum IpcError {
    NoSocket,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::NoSocket => write!(f, "GREETD_SOCK is not set — is greetd running?"),
            IpcError::Io(e) => write!(f, "greetd socket io: {e}"),
            IpcError::Json(e) => write!(f, "greetd protocol parse: {e}"),
        }
    }
}

impl From<std::io::Error> for IpcError {
    fn from(e: std::io::Error) -> Self {
        IpcError::Io(e)
    }
}

impl From<serde_json::Error> for IpcError {
    fn from(e: serde_json::Error) -> Self {
        IpcError::Json(e)
    }
}

#[derive(Debug)]
pub struct GreetdClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl GreetdClient {
    pub fn connect() -> Result<Self, IpcError> {
        let path = std::env::var("GREETD_SOCK").map_err(|_| IpcError::NoSocket)?;
        Self::connect_to(&path)
    }

    pub fn connect_to(path: &str) -> Result<Self, IpcError> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    pub fn request(&mut self, req: &Request) -> Result<Response, IpcError> {
        let frame = encode_frame(req)?;
        self.stream.write_all(&frame)?;
        self.stream.flush()?;
        let resp = read_frame(&mut self.reader)?;
        Ok(resp)
    }
}

/// greetd wire format: [len: u32 native byte-order][JSON payload] — no
/// newline (confirmed against the greetd_ipc crate protocol spec).
const MAX_FRAME: usize = 8 * 1024 * 1024;

pub fn encode_frame(req: &Request) -> Result<Vec<u8>, IpcError> {
    let payload = serde_json::to_vec(req)?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn read_frame<R: std::io::Read>(r: &mut R) -> Result<Response, IpcError> {
    let mut lenb = [0u8; 4];
    r.read_exact(&mut lenb)?;
    let len = u32::from_ne_bytes(lenb) as usize;
    if len > MAX_FRAME {
        return Err(IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("greetd frame length {len} exceeds sanity cap"),
        )));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    if buf.is_empty() {
        return Err(IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "greetd closed the connection",
        )));
    }
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Minimal in-process greetd: password auth cycle for user "rbc",
    /// correct password "hunter2", optional failure injection.
    struct MockGreetd {
        path: std::path::PathBuf,
        _handle: std::thread::JoinHandle<()>,
    }

    impl MockGreetd {
        fn spawn(fail_first: bool) -> Self {
            let path = std::env::temp_dir().join(format!(
                "losker-test-{}-{:?}.sock",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let listener = UnixListener::bind(&path).expect("bind mock socket");
            let p = path.clone();
            let handle = std::thread::spawn(move || {
                let (conn, _) = listener.accept().expect("accept");
                let mut conn = conn;
                let reader = BufReader::new(conn.try_clone().unwrap());
                let mut reader = reader;
                let mut failed = !fail_first;
                let mut password: Option<String> = None;
                loop {
                    // read one length-prefixed frame (greetd wire format)
                    let mut lenb = [0u8; 4];
                    if reader.read_exact(&mut lenb).is_err() {
                        break;
                    }
                    let len = u32::from_ne_bytes(lenb) as usize;
                    let mut buf = vec![0u8; len];
                    reader.read_exact(&mut buf).unwrap();
                    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
                    let reply = match v["type"].as_str().unwrap() {
                        "create_session" => {
                            password = None;
                            serde_json::json!({
                                "type": "auth_message",
                                "auth_message_type": "secret",
                                "auth_message": "Password: "
                            })
                        }
                        "post_auth_message_response" => {
                            let resp = v["response"].as_str().unwrap_or("");
                            if password.is_none() && !failed && resp != "hunter2" {
                                failed = true;
                                serde_json::json!({
                                    "type": "error",
                                    "error_type": "auth_error",
                                    "description": "Authentication failure"
                                })
                            } else {
                                password = Some(resp.to_string());
                                serde_json::json!({"type": "success"})
                            }
                        }
                        "start_session" => serde_json::json!({"type": "success"}),
                        _ => serde_json::json!({
                            "type": "error", "error_type": "error", "description": "unknown"
                        }),
                    };
                    let payload = reply.to_string().into_bytes();
                    let mut out = Vec::with_capacity(payload.len() + 4);
                    out.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
                    out.extend_from_slice(&payload);
                    conn.write_all(&out).unwrap();
                }
                let _ = std::fs::remove_file(&p);
            });
            Self { path, _handle: handle }
        }
    }

    fn client_for(mock: &MockGreetd) -> GreetdClient {
        // direct path: tests run in parallel and must not share the env var
        GreetdClient::connect_to(mock.path.to_str().unwrap()).expect("connect to mock")
    }

    #[test]
    fn wire_frame_is_length_prefixed_native_u32() {
        // the exact greetd wire format: [len: u32 native][JSON], no newline
        let frame = encode_frame(&Request::CancelSession).unwrap();
        let json = br#"{"type":"cancel_session"}"#;
        let mut expected = Vec::new();
        expected.extend_from_slice(&(json.len() as u32).to_ne_bytes());
        expected.extend_from_slice(json);
        assert_eq!(frame, expected);

        // and the reader side understands the same framing (response frame)
        let json = br#"{"type":"success"}"#;
        let mut resp_frame = Vec::new();
        resp_frame.extend_from_slice(&(json.len() as u32).to_ne_bytes());
        resp_frame.extend_from_slice(json);
        let mut stream = std::io::Cursor::new(resp_frame);
        let resp = read_frame(&mut stream).unwrap();
        assert_eq!(resp, Response::Success);
    }

    #[test]
    fn wire_format_roundtrip() {
        assert_eq!(
            serde_json::to_string(&Request::CreateSession {
                username: "rbc".into()
            })
            .unwrap(),
            r#"{"type":"create_session","username":"rbc"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::CancelSession).unwrap(),
            r#"{"type":"cancel_session"}"#
        );
        let r: Response =
            serde_json::from_str(r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#)
                .unwrap();
        assert_eq!(
            r,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password: ".into()
            }
        );
    }

    #[test]
    fn full_success_cycle() {
        let mock = MockGreetd::spawn(false);
        let mut c = client_for(&mock);

        let r = c
            .request(&Request::CreateSession {
                username: "rbc".into(),
            })
            .unwrap();
        assert!(matches!(r, Response::AuthMessage { auth_message_type: AuthMessageType::Secret, .. }));

        let r = c
            .request(&Request::PostAuthMessageResponse {
                response: Some("hunter2".into()),
            })
            .unwrap();
        assert_eq!(r, Response::Success);

        let r = c
            .request(&Request::StartSession {
                cmd: vec!["niri-session".into()],
                env: vec![],
            })
            .unwrap();
        assert_eq!(r, Response::Success);
    }

    #[test]
    fn auth_failure_cycle() {
        let mock = MockGreetd::spawn(true);
        let mut c = client_for(&mock);

        c.request(&Request::CreateSession {
            username: "rbc".into(),
        })
        .unwrap();

        let r = c
            .request(&Request::PostAuthMessageResponse {
                response: Some("wrong".into()),
            })
            .unwrap();
        match r {
            Response::Error { error_type, .. } => assert_eq!(error_type, ErrorType::AuthError),
            other => panic!("expected auth error, got {other:?}"),
        }

        // second attempt succeeds (mock resets state on create_session)
        c.request(&Request::CancelSession).unwrap();
        c.request(&Request::CreateSession {
            username: "rbc".into(),
        })
        .unwrap();
        let r = c
            .request(&Request::PostAuthMessageResponse {
                response: Some("hunter2".into()),
            })
            .unwrap();
        assert_eq!(r, Response::Success);
    }

    #[test]
    fn missing_socket_is_a_clean_error() {
        std::env::set_var("GREETD_SOCK", "/nonexistent/losker-test.sock");
        let err = GreetdClient::connect().unwrap_err();
        assert!(matches!(err, IpcError::Io(_)), "got {err:?}");
    }
}
