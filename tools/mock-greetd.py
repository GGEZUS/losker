#!/usr/bin/env python3
"""Mock greetd IPC server for testing greeter clients.

Speaks the REAL greetd wire protocol: each message is
    [len: u32 native byte-order][JSON payload]
(see the greetd_ipc crate protocol docs). Cycles:
create_session -> asks password -> check -> start_session -> success.

Usage: mock-greetd.py [socket-path]   (default /tmp/mock-greetd.sock)
Set GREETD_SOCK=<socket-path> for the client under test.
Correct password: "hunter2" (anything else -> auth_error once, then accept
the retry so failure+recovery can be exercised).
"""
import json
import os
import socket
import struct
import sys
import threading

SOCK = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mock-greetd.sock"
PASSWORD = "hunter2"

def send_msg(conn, obj):
    payload = json.dumps(obj).encode()
    conn.sendall(struct.pack("=I", len(payload)) + payload)
    print(f"  -> {obj}", flush=True)

def recv_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise ConnectionResetError
        buf += chunk
    return buf

def handle(conn):
    authed = False
    failed_once = False
    try:
        while True:
            (length,) = struct.unpack("=I", recv_exact(conn, 4))
            req = json.loads(recv_exact(conn, length))
            print(f"<- {req}", flush=True)
            t = req.get("type")
            if t == "create_session":
                authed = False
                send_msg(conn, {"type": "auth_message",
                                "auth_message_type": "secret",
                                "auth_message": "Password: "})
            elif t == "post_auth_message_response":
                resp = req.get("response") or ""
                if resp == PASSWORD or failed_once:
                    authed = True
                    failed_once = False
                    send_msg(conn, {"type": "success"})
                else:
                    failed_once = True
                    send_msg(conn, {"type": "error",
                                    "error_type": "auth_error",
                                    "description": "Authentication failure"})
            elif t == "start_session":
                print("  (session started; real greetd would exec here)",
                      flush=True)
                send_msg(conn, {"type": "success"})
            elif t == "cancel_session":
                authed = False
                send_msg(conn, {"type": "success"})
            else:
                send_msg(conn, {"type": "error", "error_type": "error",
                                "description": f"mock: unknown {t}"})
    except (ConnectionResetError, BrokenPipeError):
        pass
    finally:
        conn.close()

def main():
    if os.path.exists(SOCK):
        os.unlink(SOCK)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(SOCK)
    srv.listen(4)
    print(f"mock greetd listening on {SOCK}", flush=True)
    while True:
        conn, _ = srv.accept()
        print("client connected", flush=True)
        threading.Thread(target=handle, args=(conn,), daemon=True).start()

if __name__ == "__main__":
    main()
