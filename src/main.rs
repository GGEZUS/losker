//! losker — terminal-cyberpunk greeter + lock screen with built-in OSK.

mod auth;
mod config;
mod greetd_ipc;
mod pam;
mod sessions;
mod state;
mod tui;
mod ui;

use ui::Mode;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--version") | Some("-V") => {
            println!("losker {}", env!("CARGO_PKG_VERSION"));
        }
        // --demo-g: the greeter shell; --demo-l: the lock view in a plain
        // preview window (no session-lock, PAM never armed — ESC closes)
        Some("--demo-g") => std::process::exit(ui::run(Mode::Demo)),
        Some("--demo-l") => std::process::exit(ui::lock::run_preview()),
        Some("--lock") => std::process::exit(ui::lock::run()),
        // owns the terminal — must stay ahead of any gtk init
        Some("--config") => std::process::exit(tui::run()),
        Some(other) => {
            eprintln!("unknown option: {other}");
            std::process::exit(2);
        }
        None => std::process::exit(ui::run(Mode::Greeter)),
    }
}
