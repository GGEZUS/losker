//! UI shell: window, CSS, layout (boot log / login block / OSK deck), the
//! CRT overlay, and the auth wiring (greetd handshake via auth::Auth).

pub mod ascii;
pub mod crt;
pub mod osk;
pub mod theater;

use crate::auth::{Auth, Outcome};
use crate::sessions::{self, Session};
use crate::state::{self, State};
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy)]
pub enum Mode {
    Greeter,
    Demo,
    Lock,
}

pub fn run(mode: Mode) -> i32 {
    // greeter panics are invisible (stderr not captured) — name them in the
    // trace so a crash names its own site instead of just reloading
    std::panic::set_hook(Box::new(|info| {
        crate::auth::trace(&format!("PANIC: {info}"));
    }));
    // Plain gtk init — no GtkApplication: the greeter context has no sane
    // session bus, and Application registration drags in portals/unique-app
    // machinery that crash-loops there (observed in the greetd journal).
    if gtk4::init().is_err() {
        eprintln!("osk-greeter: gtk init failed (no display?)");
        return 2;
    }
    build(mode);
    glib::MainLoop::new(None, false).run();
    0
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// no session in flight — RET starts the greetd handshake
    Fresh,
    /// PAM asked for the password — RET submits it
    Password,
}

struct Ui {
    /// in-session lock screen: PAM auth instead of the greetd handshake,
    /// no session picker, no power actions
    lock: bool,
    buffer: RefCell<String>,
    busy: Cell<bool>,
    stage: Cell<Stage>,
    /// password typed at Fresh stage, auto-submitted when PAM asks for it —
    /// one RET always means "go"
    pending_pw: RefCell<Option<String>>,
    tx: std::sync::Mutex<std::sync::mpsc::Sender<Outcome>>,
    pw_dots: gtk4::Label,
    authmsg: gtk4::Label,
    boot: Rc<theater::BootLog>,
    effects: Rc<crt::CrtEffects>,
    auth: Auth,
    username: String,
    sessions: RefCell<Vec<Session>>,
    session_idx: Cell<usize>,
    session_lbl: gtk4::Label,
    st: State,
}

fn build(mode: Mode) {
    let demo = matches!(mode, Mode::Demo);
    let lock = matches!(mode, Mode::Lock);
    crate::auth::trace(&format!(
        "ui: starting (demo={demo} lock={lock}) pid={} user={:?}",
        std::process::id(),
        std::env::var("USER").ok()
    ));
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(include_str!("../../assets/style.css"));
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk4::Window::new();
    window.add_css_class("root");
    // "osk-lock" is the title the niri session window-rule matches on
    window.set_title(Some(if lock { "osk-lock" } else { "osk-greeter" }));
    window.set_default_size(1440, 960);

    // ── root: overlay so the CRT layer paints over everything ──────────
    let overlay = gtk4::Overlay::new();
    let main = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    // no top margin: the log starts at the true top of the screen
    main.set_margin_top(0);
    main.set_margin_bottom(16);
    main.set_margin_start(34);
    main.set_margin_end(34);
    overlay.set_child(Some(&main));

    let effects = crt::CrtEffects::new();
    overlay.add_overlay(effects.widget());

    // ── status line (top right) ────────────────────────────────────────
    let top = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    top.set_hexpand(true);
    let spacer = gtk4::Label::new(None);
    spacer.set_hexpand(true);
    let status = gtk4::Label::new(Some("tty1 · surarch · --:--"));
    status.add_css_class("statusline");
    top.append(&spacer);
    top.append(&status);
    main.append(&top);

    // ── two columns: bootlog + login form left, ascii-art slot right ────
    // the OSK lives BELOW this row, on the full window width, so the deck
    // stays screen-centered — the art must never push it around
    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    content.set_vexpand(true);
    main.append(&content);

    let left = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(&left);

    let art = ascii::spin_label();
    let art_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    art_box.set_hexpand(true);
    art_box.set_valign(gtk4::Align::Center);
    art_box.append(&art);
    content.append(&art_box);

    // ── boot log / fixed stats panel ───────────────────────────────────
    let boot_label = gtk4::Label::new(None);
    boot_label.set_valign(gtk4::Align::Start);
    boot_label.set_halign(gtk4::Align::Start);
    left.append(&boot_label);
    let boot = Rc::new(theater::BootLog::new(boot_label));
    boot.start();

    {
        let status = status.clone();
        update_clock(&status);
        glib::timeout_add_local(std::time::Duration::from_secs(20), move || {
            update_clock(&status);
            glib::ControlFlow::Continue
        });
    }

    // ── persistent state + sessions ────────────────────────────────────
    let st = State::load();
    // lock authenticates whoever is running it ($USER); unix_chkpwd would
    // refuse any other account anyway
    let username = if lock {
        std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .or_else(|| st.last_user.clone())
            .unwrap_or_else(|| "rbc".into())
    } else {
        st.last_user.clone().unwrap_or_else(|| "rbc".into())
    };
    let sess_list = sessions::list();
    let session_idx = Cell::new(0);
    if let Some(saved) = &st.last_session {
        if let Some(i) = sess_list.iter().position(|s| &s.cmd == saved) {
            session_idx.set(i);
        }
    }
    let current_session = sess_list
        .get(session_idx.get())
        .cloned()
        .unwrap_or(Session {
            name: "niri".into(),
            cmd: state::DEFAULT_SESSION.into(),
        });

    // ── login block ────────────────────────────────────────────────────
    let login = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    login.set_vexpand(true);
    login.set_valign(gtk4::Align::Center);
    login.set_halign(gtk4::Align::Start);
    login.set_size_request(760, -1);

    let banner = gtk4::Label::new(Some(if lock {
        "TERMINAL LOCKED"
    } else {
        "AUTHORIZATION REQUIRED"
    }));
    banner.add_css_class("banner");
    let sub = gtk4::Label::new(Some("RESTRICTED TERMINAL // PHOSPHOR/1"));
    sub.add_css_class("banner-sub");
    sub.set_halign(gtk4::Align::Start);
    let authmsg = gtk4::Label::new(None);
    authmsg.add_css_class("authmsg");
    authmsg.set_halign(gtk4::Align::Start);

    let user_row = frow("surarch login:", &username, None);
    let (pw_row, pw_dots, cursor_lbl) = frow_password("Password:");
    let session_row = frow("session:", &format!("{} ▾", current_session.name), Some("dim"));
    let session_lbl = session_row
        .last_child()
        .and_then(|c| c.downcast::<gtk4::Label>().ok())
        .expect("session frow value label");

    // tap the session row to cycle available sessions
    {
        let gesture = gtk4::GestureClick::new();
        gesture.connect_pressed({
            let list = sess_list.clone();
            let idx = session_idx.clone();
            let lbl = session_lbl.clone();
            move |_g, _n, _x, _y| {
                if list.is_empty() {
                    return;
                }
                let next = (idx.get() + 1) % list.len();
                idx.set(next);
                lbl.set_label(&format!("{} ▾", list[next].name));
            }
        });
        session_lbl.add_controller(gesture);
    }

    let meta = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    meta.set_hexpand(true);
    let pwr_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 14);
    let reboot = power_label("[ REBOOT ]");
    let poweroff = power_label("[ POWEROFF ]");
    pwr_box.append(&reboot);
    pwr_box.append(&poweroff);
    let hint = gtk4::Label::new(Some("RET AUTHENTICATES"));
    hint.add_css_class("hint");
    hint.set_hexpand(true);
    hint.set_halign(gtk4::Align::End);
    meta.set_margin_top(18);
    meta.append(&pwr_box);
    meta.append(&hint);

    login.append(&banner);
    login.append(&sub);
    login.append(&authmsg);
    login.append(&user_row);
    login.append(&pw_row);
    login.append(&session_row);
    login.append(&meta);
    left.append(&login);

    if lock {
        // a lock screen offers one path out: the password. No session
        // picker (the session is already running), no reboot shortcut.
        session_row.set_visible(false);
        reboot.set_visible(false);
        poweroff.set_visible(false);
    }

    // ── the shared UI state ────────────────────────────────────────────
    let (tx, rx) = std::sync::mpsc::channel::<Outcome>();
    let ui = Rc::new(Ui {
        lock,
        buffer: RefCell::new(String::new()),
        busy: Cell::new(false),
        stage: Cell::new(Stage::Fresh),
        pending_pw: RefCell::new(None),
        tx: std::sync::Mutex::new(tx),
        pw_dots,
        authmsg,
        boot,
        effects,
        auth: Auth::new(demo),
        username,
        sessions: RefCell::new(sess_list),
        session_idx,
        session_lbl,
        st,
    });

    // ── auth outcomes: std mpsc polled from the main loop ──────────────
    {
        let ui = ui.clone();
        let rx = std::rc::Rc::new(rx);
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            while let Ok(outcome) = rx.try_recv() {
                handle_outcome(&ui, outcome);
            }
            glib::ControlFlow::Continue
        });
    }

    // ── RET handling (from OSK or physical keyboard) ───────────────────
    let submit = {
        let ui = ui.clone();
        move || {
            if ui.busy.get() {
                return;
            }

            // lock mode: single phase, straight to PAM — no greetd out here
            if ui.lock {
                if ui.buffer.borrow().is_empty() {
                    set_msg(&ui, "type the password first", false);
                    return;
                }
                ui.busy.set(true);
                set_msg(&ui, "verifying credentials …", false);
                let pw = ui.buffer.borrow().clone();
                ui.buffer.borrow_mut().clear();
                ui.pw_dots.set_text("");
                crate::auth::trace("ui: lock submit → PAM");
                crate::pam::authenticate(
                    ui.username.clone(),
                    pw,
                    ui.tx.lock().unwrap().clone(),
                );
                return;
            }

            let session_cmd = ui
                .sessions
                .borrow()
                .get(ui.session_idx.get())
                .map(|s| s.cmd.clone())
                .unwrap_or_else(|| state::DEFAULT_SESSION.into());

            match ui.stage.get() {
                Stage::Fresh => {
                    if ui.buffer.borrow().is_empty() {
                        set_msg(&ui, "type the password first", false);
                        return;
                    }
                    // queue the typed password; it auto-submits when PAM asks
                    let pw_len = ui.buffer.borrow().len();
                    *ui.pending_pw.borrow_mut() = Some(ui.buffer.borrow().clone());
                    ui.busy.set(true);
                    set_msg(&ui, "opening session …", false);
                    crate::auth::trace(&format!("ui: submit fresh (pw {pw_len} chars)"));
                    let tx = ui.tx.lock().unwrap().clone();
                    ui.auth.begin(ui.username.clone(), session_cmd, tx);
                }
                Stage::Password => {
                    if ui.buffer.borrow().is_empty() {
                        set_msg(&ui, "type the password first", false);
                        return;
                    }
                    ui.busy.set(true);
                    set_msg(&ui, "verifying credentials …", false);
                    crate::auth::trace("ui: submit password");
                    let pw = ui.buffer.borrow().clone();
                    let tx = ui.tx.lock().unwrap().clone();
                    ui.auth.answer(pw, session_cmd, tx);
                }
            }
        }
    };

    // ── OSK ────────────────────────────────────────────────────────────
    let osk = {
        let ui = ui.clone();
        let submit = submit.clone();
        Rc::new(osk::Osk::new(move |ev| {
            match ev {
                osk::OskEvent::Char(c) => ui.buffer.borrow_mut().push(c),
                osk::OskEvent::Backspace => {
                    ui.buffer.borrow_mut().pop();
                }
                osk::OskEvent::Enter => return submit(),
            }
            let dots = "•".repeat(ui.buffer.borrow().len());
            ui.pw_dots.set_text(&dots);
        }))
    };
    osk.container.set_margin_top(14);
    // full window width → the deck centers on the screen, as before the
    // art column existed
    main.append(&osk.container);

    // ── physical keyboard feeds the same buffer ────────────────────────
    {
        let ui = ui.clone();
        let submit = submit.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        // capture phase: see physical keys before any (formerly) focused
        // widget's default activation could eat them
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_k, keyval, _code, _state| {
            let char_ev = keyval
                .to_unicode()
                .filter(|c| !c.is_control())
                .map(osk::OskEvent::Char);
            let ev = match keyval {
                gtk4::gdk::Key::BackSpace => Some(osk::OskEvent::Backspace),
                gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter => Some(osk::OskEvent::Enter),
                _ => char_ev,
            };
            let Some(ev) = ev else {
                return gtk4::glib::Propagation::Proceed;
            };
            match ev {
                osk::OskEvent::Char(c) => ui.buffer.borrow_mut().push(c),
                osk::OskEvent::Backspace => {
                    ui.buffer.borrow_mut().pop();
                }
                osk::OskEvent::Enter => {
                    submit();
                    return gtk4::glib::Propagation::Stop;
                }
            }
            let dots = "•".repeat(ui.buffer.borrow().len());
            ui.pw_dots.set_text(&dots);
            gtk4::glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }

    // blinking block cursor
    {
        let cursor = cursor_lbl.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(530), move || {
            cursor.set_opacity(if cursor.opacity() < 0.5 { 1.0 } else { 0.0 });
            glib::ControlFlow::Continue
        });
    }

    // power: same commands regreet uses (polkit-allowed from the greeter)
    {
        let ui_r = ui.clone();
        attach_click(&reboot, move || {
            set_msg(&ui_r, "rebooting …", false);
            ui_r.boot.event("[ ASKED ] systemctl reboot");
            let _ = std::process::Command::new("systemctl").arg("reboot").spawn();
        });
        let ui_p = ui.clone();
        attach_click(&poweroff, move || {
            set_msg(&ui_p, "powering off …", false);
            ui_p.boot.event("[ ASKED ] systemctl poweroff");
            let _ = std::process::Command::new("systemctl").arg("poweroff").spawn();
        });
    }

    window.set_child(Some(&overlay));
    window.present();

    // allocation diagnostics — the greeter's /tmp is a private namespace
    // (greetd PrivateTmp), so this goes to the greeter-owned state dir
    {
        let win = window.clone();
        let osk_c = osk.container.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            let rss = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())
                })
                .unwrap_or_else(|| "?".into());
            crate::auth::trace(&format!(
                "alloc window={}x{} scale={} osk={}x{} rss={}kB",
                win.width(),
                win.height(),
                win.scale_factor(),
                osk_c.width(),
                osk_c.height(),
                rss
            ));
        });
    }
}

fn handle_outcome(ui: &Rc<Ui>, outcome: Outcome) {
    crate::auth::trace(&format!("ui: outcome {outcome:?}"));
    ui.busy.set(false);
    match outcome {
        Outcome::Prompt(msg) => {
            // password was already typed at Fresh stage: auto-submit it
            // (take() into a local first — same if-let temporary-borrow trap)
            let pending = ui.pending_pw.borrow_mut().take();
            if let Some(pw) = pending {
                ui.busy.set(true);
                set_msg(ui, "verifying credentials …", false);
                let session_cmd = ui
                    .sessions
                    .borrow()
                    .get(ui.session_idx.get())
                    .map(|s| s.cmd.clone())
                    .unwrap_or_else(|| state::DEFAULT_SESSION.into());
                ui.buffer.borrow_mut().clear();
                ui.pw_dots.set_text("");
                ui.auth.answer(pw, session_cmd, ui.tx.lock().unwrap().clone());
            } else {
                ui.stage.set(Stage::Password);
                ui.buffer.borrow_mut().clear();
                ui.pw_dots.set_text("");
                set_msg(ui, &msg, false);
            }
        }
        Outcome::Granted => {
            if ui.lock {
                ui.effects.bloom();
                set_msg(ui, "ACCESS RESTORED — welcome back, operator", false);
                ui.boot.event("[  OK  ] authentication passed");
                glib::timeout_add_local_once(std::time::Duration::from_millis(900), || {
                    std::process::exit(0);
                });
                return;
            }
            ui.effects.bloom();
            set_msg(ui, "ACCESS GRANTED — welcome, operator", false);
            ui.boot.event("[  OK  ] authentication passed");
            // persist last user + session for the next boot
            let idx = ui.session_idx.get();
            let cmd = ui
                .sessions
                .borrow()
                .get(idx)
                .map(|s| s.cmd.clone())
                .unwrap_or_else(|| state::DEFAULT_SESSION.into());
            let mut st = ui.st.clone();
            st.last_user = Some(ui.username.clone());
            st.last_session = Some(cmd);
            if let Err(e) = st.save() {
                eprintln!("osk-greeter: state save failed: {e}");
            }
            ui.boot.event("[  OK  ] session starting …");
            glib::timeout_add_local_once(std::time::Duration::from_millis(900), || {
                // greetd tears us down itself; this covers demo mode
                std::process::exit(0);
            });
        }
        Outcome::AuthFailed(desc) => {
            ui.effects.glitch();
            set_msg(ui, "ACCESS DENIED — type the password and press RET", true);
            ui.boot
                .event(&format!("[ FAILED ] authentication: {desc}"));
            ui.auth.cancel();
            ui.stage.set(Stage::Fresh);
            ui.pending_pw.borrow_mut().take();
            ui.buffer.borrow_mut().clear();
            ui.pw_dots.set_text("");
        }
        Outcome::Fatal(desc) => {
            set_msg(ui, &format!("error: {desc}"), true);
            ui.boot.event(&format!("[ ERROR ] {desc}"));
            ui.stage.set(Stage::Fresh);
        }
    }
}

fn set_msg(ui: &Ui, text: &str, err: bool) {
    ui.authmsg.set_label(text);
    if err {
        ui.authmsg.add_css_class("err");
    } else {
        ui.authmsg.remove_css_class("err");
    }
}

fn update_clock(status: &gtk4::Label) {
    if let Ok(now) = glib::DateTime::now_local() {
        let text = format!("tty1 · surarch · {:02}:{:02}", now.hour(), now.minute());
        status.set_label(&text);
    }
}

fn frow(label: &str, value: &str, style: Option<&str>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    row.add_css_class("frow");
    let l = gtk4::Label::new(Some(label));
    l.add_css_class("frow-label");
    let v = gtk4::Label::new(Some(value));
    v.add_css_class("frow-val");
    if style == Some("dim") {
        v.add_css_class("dim");
    }
    row.append(&l);
    row.append(&v);
    row
}

fn frow_password(label: &str) -> (gtk4::Box, gtk4::Label, gtk4::Label) {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    row.add_css_class("frow");
    let l = gtk4::Label::new(Some(label));
    l.add_css_class("frow-label");

    // dots + cursor in a zero-gap group: the block must sit flush against
    // the last typed character, not float a spacing-width ahead of it
    let group = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let dots = gtk4::Label::new(None);
    dots.add_css_class("frow-val");
    let cursor = gtk4::Label::new(Some("█"));
    cursor.add_css_class("cursor");
    group.append(&dots);
    group.append(&cursor);

    row.append(&l);
    row.append(&group);
    (row, dots, cursor)
}

/// clickable label — avoids GTK button theming entirely in the greeter
fn power_label(text: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.add_css_class("pwr");
    l
}

fn attach_click(label: &gtk4::Label, on_click: impl Fn() + 'static) {
    let g = gtk4::GestureClick::new();
    g.connect_pressed(move |_, _, _, _| on_click());
    label.add_controller(g);
}
