//! osk-greeter — terminal-cyberpunk greeter + lock screen with built-in OSK.

mod auth;
mod greetd_ipc;
mod pam;
mod sessions;
mod state;
mod ui;

use ui::Mode;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--version") | Some("-V") => {
            println!("osk-greeter {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--demo") => std::process::exit(ui::run(Mode::Demo)),
        Some("--lock") => std::process::exit(ui::run(Mode::Lock)),
        Some(other) => {
            eprintln!("unknown option: {other}");
            std::process::exit(2);
        }
        None => std::process::exit(ui::run(Mode::Greeter)),
    }
}
