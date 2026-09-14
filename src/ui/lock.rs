//! `--lock` — real screen locker over ext-session-lock-v1, via the
//! gtk4-session-lock bindings (gtk4-layer-shell ≥ 1.2).
//!
//! The compositor enforces everything a cosmetic fullscreen window cannot:
//! nothing renders above the lock surface, keyboard AND touch route into it,
//! compositor keybinds are dead. One lock surface per monitor, built inside
//! the Instance::monitor signal; each view is the greeter shell stripped to
//! what a lock screen means: no session picker, no power buttons, no user
//! row — one password is the only way out. The distro art is the hero:
//! sized to exactly fill the box the OSK leaves on ITS monitor. All views
//! share one password buffer; PAM authenticates the invoking user
//! (crate::pam), never greetd.
//!
//! Hard protocol rules this module lives by:
//! - every Instance signal is connected BEFORE lock() — ::failed can fire
//!   before lock() returns (another locker already holds the lock)
//! - lock() is called with the main loop running (from a timeout on the
//!   context); the monitor/locked emissions arrive as signal emissions
//! - assign_window_to_monitor takes a fresh, never-presented window; the
//!   library sizes and maps it, and destroys it on unlock/monitor removal —
//!   we never present(), resize-fight or hide it
//! - the process NEVER exits while is_locked(): every exit path goes
//!   through request_exit, which unlocks first and quits the loop on
//!   ::unlocked (dying locked would brick a keyboard-less tablet)
//!
//! Exit codes — the keybind wrapper respawns on anything but 0/1/2:
//!   0 authenticated + unlocked
//!   1 lock refused / protocol unsupported (nothing was ever mapped)
//!   2 no display (gtk init failed)
//!   3 lock lost without auth (compositor-side unlock) → respawn
//!   4 PAM unusable, failed open → respawn

use crate::auth::Outcome;
use crate::config::Config;
use crate::ui::{
    ascii, art_budget, crt, ensure_css, frow_password, install_panic_hook, osk, primary_monitor,
    theater, theme, update_clock,
};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use gtk4_session_lock::Instance;
use std::cell::{Cell, RefCell};

use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

pub const EXIT_UNLOCKED: i32 = 0;
pub const EXIT_NOT_ACQUIRED: i32 = 1;
pub const EXIT_NO_DISPLAY: i32 = 2;
const EXIT_LOCK_LOST: i32 = 3;
const EXIT_AUTH_FATAL: i32 = 4;
/// re-arm delay after ACCESS DENIED — typing stays live, only RET is gated
const COOLDOWN_MS: u64 = 2000;
/// gap between the monitor signal and mapping the surface: outputs that are
/// still reconfiguring (winit resize, mode/scale changes) must settle, or
/// GTK's first frame races the acked configure and the compositor kills us
const SETTLE_MS: u64 = 150;
/// how long the "ACCESS RESTORED" bloom (and the fatal error text) stays up
/// before request_exit tears the lock surface down
const TEARDOWN_MS: u64 = 900;
const FATAL_TEARDOWN_MS: u64 = 1500;

/// one monitor's lock surface; owned widgets are destroyed by the library
/// on unlock / monitor removal, the Rust side just drops its refs
struct LockView {
    window: gtk4::Window,
    pw_dots: gtk4::Label,
    authmsg: gtk4::Label,
    status: gtk4::Label,
    art: Option<gtk4::Label>,
    /// the stowed-by-default deck dock (None when cfg.osk is off)
    osk: Option<osk::OskDock>,
    effects: Rc<crt::CrtEffects>,
    boot: Rc<theater::BootLog>,
}

struct LockUi {
    /// Clone (Shared<>) — refcounted, no Rc wrapper needed
    main_loop: glib::MainLoop,
    /// GObject: Clone, !Send; signal callbacks receive &Instance anyway.
    /// In preview mode an idle instance that is never lock()ed.
    lock: Instance,
    /// look configuration (OSK on/off, logo, accent, crt level)
    cfg: Config,
    /// `--demo-l` preview: same view, plain window, PAM never armed
    demo: bool,
    username: String,
    tx: Mutex<Sender<Outcome>>,
    views: RefCell<Vec<LockView>>,
    buffer: RefCell<String>,
    /// auth in flight (same meaning as the greeter's busy flag)
    busy: Cell<bool>,
    /// post-failure gate on RET only — the OSK keeps accepting characters
    cooldown: Cell<bool>,
    /// success or fatal: input permanently dead, teardown in progress
    leaving: Cell<bool>,
    /// request_exit idempotence guard
    exiting: Cell<bool>,
    exit_rc: Cell<i32>,
}

pub fn run() -> i32 {
    install_panic_hook();
    let cfg = crate::config::load();
    crate::auth::trace(&format!(
        "config: osk={} logo={:?} accent={:?} crt={:?}",
        cfg.osk,
        cfg.logo,
        cfg.accent.map(|c| c.to_hex()),
        cfg.crt
    ));
    if gtk4::init().is_err() {
        eprintln!("losker: gtk init failed (no display?)");
        return EXIT_NO_DISPLAY;
    }
    ensure_css(&cfg);
    // needs an initialized GTK; may block one Wayland roundtrip
    if !gtk4_session_lock::is_supported() {
        crate::auth::trace("lock: ext-session-lock-v1 not supported by this compositor");
        eprintln!("losker: session lock protocol not supported here (X11? old compositor?)");
        return EXIT_NOT_ACQUIRED;
    }
    // preflight the PAM stack BEFORE locking: with the service file missing
    // or empty, pam_start still succeeds (PAM falls back to the deny-all
    // `other` stack) and EVERY password would be refused — a real lockout,
    // forever, on the device that has no keyboard to escape with. Refusing
    // to lock leaves the session usable; the misconfiguration is loud.
    match std::fs::metadata(crate::pam::SERVICE_FILE) {
        Ok(m) if m.len() > 0 => {}
        Ok(_) => {
            crate::auth::trace("lock: /etc/pam.d/losker is EMPTY — refusing to lock");
            eprintln!("losker: /etc/pam.d/losker is EMPTY — refusing to lock");
            return EXIT_NOT_ACQUIRED;
        }
        Err(_) => {
            crate::auth::trace("lock: /etc/pam.d/losker is missing — refusing to lock");
            eprintln!("losker: /etc/pam.d/losker is missing — refusing to lock");
            return EXIT_NOT_ACQUIRED;
        }
    }

    let (tx, rx) = std::sync::mpsc::channel::<Outcome>();
    // authenticate whoever is running it ($USER); unix_chkpwd would refuse
    // any other account anyway. No fallback literal: without a real account
    // to verify there is nothing to unlock against, so refuse to lock.
    let username = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| crate::state::State::load().last_user);
    let Some(username) = username else {
        crate::auth::trace("lock: no account to authenticate ($USER unset, no last-user state) — refusing to lock");
        eprintln!("losker: no account to authenticate — refusing to lock");
        return EXIT_NOT_ACQUIRED;
    };
    crate::auth::trace(&format!("lock: starting pid={} user={username}", std::process::id()));

    let main_loop = glib::MainLoop::new(None, false);
    // pessimistic default: ANY unexpected unlock exits 3 (respawn), never 0
    let ui = Rc::new(LockUi {
        main_loop: main_loop.clone(),
        lock: Instance::new(),
        cfg,
        demo: false,
        username,
        tx: Mutex::new(tx),
        views: RefCell::new(Vec::new()),
        buffer: RefCell::new(String::new()),
        busy: Cell::new(false),
        cooldown: Cell::new(false),
        leaving: Cell::new(false),
        exiting: Cell::new(false),
        exit_rc: Cell::new(EXIT_LOCK_LOST),
    });

    wire_signals(&ui);

    // auth outcomes: std mpsc polled from the main loop (same shape as the
    // greeter's poll in ui::run)
    {
        let ui = ui.clone();
        let rx = Rc::new(rx);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(outcome) = rx.try_recv() {
                on_outcome(&ui, outcome);
            }
            glib::ControlFlow::Continue
        });
    }

    // status clock across all views
    {
        let ui = ui.clone();
        glib::timeout_add_local(Duration::from_secs(20), move || {
            for v in ui.views.borrow().iter() {
                update_clock(&v.status);
            }
            glib::ControlFlow::Continue
        });
    }

    // allocation diagnostics, same habit as the greeter's 1.5s one-shot
    {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            let rss = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())
                })
                .unwrap_or_else(|| "?".into());
            let geo = ui.views.borrow().first().map(|v| {
                (
                    v.window.width(),
                    v.window.height(),
                    v.window.scale_factor(),
                    v.window.is_mapped(),
                )
            });
            let art = ui.views.borrow().first().and_then(|v| {
                v.art.as_ref().map(|l| {
                    let nat_h = l.measure(gtk4::Orientation::Vertical, -1).1;
                    format!("{}x{} (nat h {nat_h})", l.width(), l.height())
                })
            });
            let deck = ui.views.borrow().first().map(|v| match &v.osk {
                None => "disabled",
                Some(d) if d.dock.reveals_child() => "shown",
                Some(_) => "stowed",
            });
            crate::auth::trace(&format!(
                "lock alloc monitors={} window={geo:?} art={art:?} deck={deck:?} rss={rss}kB",
                ui.views.borrow().len()
            ));
        });
    }

    // lock() needs a running main loop — the monitor/locked/failed emissions
    // are signal emissions on this context
    {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(0), move || {
            crate::auth::trace("lock: calling lock()");
            let started = ui.lock.lock();
            if !started && !ui.lock.is_locked() {
                crate::auth::trace("lock: lock() failed immediately");
                request_exit(&ui, EXIT_NOT_ACQUIRED);
            }
            // if ::failed already fired, request_exit was a no-op duplicate
        });
    }

    main_loop.run();
    ui.exit_rc.get()
}

/// `--demo-l`: the lock view in a plain, self-fullscreened preview window —
/// no session-lock protocol, no PAM preflight, and PAM never armed (see
/// demo_enter). Lets the layout be judged without locking the session:
/// ESC closes, the literal password "demo" plays the granted path and
/// closes. Same config source as `--lock`, so OSK/logo/accent/crt all
/// match what the real lock will show.
pub fn run_preview() -> i32 {
    install_panic_hook();
    let cfg = crate::config::load();
    crate::auth::trace("lock preview: starting (PAM never armed here)");
    if gtk4::init().is_err() {
        eprintln!("losker: gtk init failed (no display?)");
        return EXIT_NO_DISPLAY;
    }
    ensure_css(&cfg);

    // a real sender for the struct's shape, but nothing ever sends in demo:
    // demo_enter feeds outcomes via timeouts, so the receiver just outlives
    // the loop unused
    let (tx, _rx) = std::sync::mpsc::channel::<Outcome>();
    let main_loop = glib::MainLoop::new(None, false);
    let ui = Rc::new(LockUi {
        main_loop: main_loop.clone(),
        lock: Instance::new(), // never lock()ed: request_exit just quits
        cfg,
        demo: true,
        username: String::new(), // no PAM, nobody to authenticate
        tx: Mutex::new(tx),
        views: RefCell::new(Vec::new()),
        buffer: RefCell::new(String::new()),
        busy: Cell::new(false),
        cooldown: Cell::new(false),
        leaving: Cell::new(false),
        exiting: Cell::new(false),
        exit_rc: Cell::new(EXIT_UNLOCKED),
    });

    let Some(monitor) = primary_monitor() else {
        eprintln!("losker: no monitor for the lock preview");
        return EXIT_NO_DISPLAY;
    };
    let view = build_lock_view(&ui, &monitor);
    // plain-window mapping: present + fullscreen on the primary monitor.
    // The exact-size backdrop pins the window's natural size to the monitor
    // geometry, so fullscreen allocates exactly what the lock surface would.
    view.window.present();
    view.window.fullscreen();
    update_clock(&view.status);
    ui.views.borrow_mut().push(view);

    {
        let ui = ui.clone();
        glib::timeout_add_local(Duration::from_secs(20), move || {
            for v in ui.views.borrow().iter() {
                update_clock(&v.status);
            }
            glib::ControlFlow::Continue
        });
    }
    // the hint waits out the boot typing (~1s) so it lands in the fixed
    // event slot instead of growing the panel mid-animation
    {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(1300), move || {
            boot_all(
                &ui,
                "[ HINT ] demo: ▲ tab raises the deck · 'demo' grants+exits · ESC quits",
            );
        });
    }

    main_loop.run();
    ui.exit_rc.get()
}

/// Every signal connected before lock() — in this order.
fn wire_signals(ui: &Rc<LockUi>) {
    let weak = Rc::downgrade(ui);
    let inst = &ui.lock;

    // can fire BEFORE lock() returns (another locker holds it): no views
    // exist yet, request_exit must be safe at zero views
    inst.connect_failed({
        let weak = weak.clone();
        move |_| {
            crate::auth::trace("lock: ::failed — lock not acquired");
            if let Some(ui) = weak.upgrade() {
                request_exit(&ui, EXIT_NOT_ACQUIRED);
            }
        }
    });

    // telemetry only — the compositor has blanked the outputs by now
    inst.connect_locked({
        let weak = weak.clone();
        move |_| {
            crate::auth::trace("lock: ::locked");
            if let Some(ui) = weak.upgrade() {
                boot_all(&ui, "[  OK  ] session locked");
            }
        }
    });

    // one surface per output: once per monitor at lock start, then on
    // hotplug while locked
    inst.connect_monitor({
        let weak = weak.clone();
        move |inst, monitor| {
            let Some(ui) = weak.upgrade() else { return };
            let view = build_lock_view(&ui, monitor);
            update_clock(&view.status);
            // Let the output settle before mapping: if a winit/monitor resize
            // is still in flight, GTK's first frame commits with stale
            // dimensions and Smithay-based compositors kill the client for it
            // ("Surface dimensions do not match acked configure"). The session
            // is already blanked here, so the gap is just black.
            let view = RefCell::new(Some(view));
            let inst2 = inst.clone();
            let mon = monitor.clone();
            glib::timeout_add_local_once(Duration::from_millis(SETTLE_MS), move || {
                let Some(view) = view.borrow_mut().take() else { return };
                {
                    let win = view.window.clone();
                    let ui = ui.clone();
                    mon.connect_invalidate(move |_| {
                        // output gone: the library unmaps/destroys the window
                        ui.views.borrow_mut().retain(|v| v.window != win);
                    });
                }
                // LAST — this maps the window; the library owns it from here
                inst2.assign_window_to_monitor(&view.window, &mon);
                crate::auth::trace(&format!(
                    "lock: surface assigned ({}x{}, scale {})",
                    mon.geometry().width(),
                    mon.geometry().height(),
                    mon.scale_factor()
                ));
                ui.views.borrow_mut().push(view);
            });
        }
    });

    // our unlock() or a compositor-side unlock; the library has already
    // destroyed the assigned windows when this fires
    inst.connect_unlocked(move |_| {
        crate::auth::trace("lock: ::unlocked — surfaces torn down");
        if let Some(ui) = weak.upgrade() {
            ui.views.borrow_mut().clear();
            ui.main_loop.quit();
        }
    });
}

/// The greeter shell, minus session picker and power buttons — a lock screen
/// offers exactly one path out: the password.
fn build_lock_view(ui: &Rc<LockUi>, monitor: &gdk::Monitor) -> LockView {
    let window = gtk4::Window::new();
    window.add_css_class("root");
    if ui.demo {
        // preview maps itself (present + fullscreen); the real path is mapped
        // by the library and never titled
        window.set_title(Some("losker — lock preview"));
    }
    // NO present(): the library maps it

    // ── root: overlay so the CRT layer paints over everything ──────────
    // The MAIN child is an exact-size transparent backdrop: it pins the
    // window's measured (natural) size to the monitor geometry, so GTK can
    // never commit a frame whose dimensions differ from the acked configure
    // — Smithay-based compositors turn that into a protocol kill. The real
    // UI and the CRT layer are overlay children (they don't affect sizing)
    // and expand to fill whatever is allocated.
    let geo = monitor.geometry();
    let backdrop = gtk4::DrawingArea::new();
    backdrop.set_content_width(geo.width());
    backdrop.set_content_height(geo.height());
    backdrop.set_can_target(false); // clicks pass through to the UI

    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&backdrop));
    let main = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    main.set_margin_top(0);
    main.set_margin_bottom(16);
    main.set_margin_start(34);
    main.set_margin_end(34);
    main.set_hexpand(true);
    main.set_vexpand(true);
    overlay.add_overlay(&main);

    // the effects overlay child is added LATER, after the OSK dock, so the
    // dock sits under the scanlines
    let effects = crt::CrtEffects::new_with(theme::crt_style(&ui.cfg));

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

    // ── two columns: bootlog + login form left, HERO art right ─────────
    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    content.set_vexpand(true);
    main.append(&content);

    let left = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(&left);

    // art slot — fitted to THIS monitor's safe box at the chosen size;
    // config can hide it entirely (logo = none). CENTERED vertically, and
    // the fit budget (not a pinned position) keeps it clear of the arrow.
    let mut art_lbl: Option<gtk4::Label> = None;
    if let Some(art) = ascii::spin_label_sized(&ui.cfg, Some(art_budget(monitor, ui.cfg.logo_size)))
    {
        let art_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        art_box.set_hexpand(true);
        art_box.set_valign(gtk4::Align::Center);
        art_box.append(&art);
        content.append(&art_box);
        art_lbl = Some(art);
    }

    let boot_label = gtk4::Label::new(None);
    boot_label.set_valign(gtk4::Align::Start);
    boot_label.set_halign(gtk4::Align::Start);
    // hidden for now (keeps its slot in the layout work): the theater and
    // all boot.event() feeds stay live — flip this back to restore the log
    boot_label.set_visible(false);
    left.append(&boot_label);
    let boot = Rc::new(theater::BootLog::new(boot_label));
    boot.start();

    // ── login block — deliberately compact: the art is the show ────────
    let login = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    login.set_vexpand(true);
    login.set_valign(gtk4::Align::Center);
    login.set_halign(gtk4::Align::Start);
    login.set_size_request(420, -1);

    let banner = gtk4::Label::new(Some("TERMINAL LOCKED"));
    banner.add_css_class("banner-sm");
    let sub = gtk4::Label::new(Some("RESTRICTED TERMINAL // PHOSPHOR/1"));
    sub.add_css_class("banner-sub");
    sub.set_halign(gtk4::Align::Start);
    let authmsg = gtk4::Label::new(None);
    authmsg.add_css_class("authmsg");
    authmsg.set_halign(gtk4::Align::Start);

    // no user row: the locker authenticates one known user, and naming
    // them on a locked screen is information for free
    let (pw_row, pw_dots, cursor_lbl) = frow_password("Password:");

    login.append(&banner);
    login.append(&sub);
    login.append(&authmsg);
    login.append(&pw_row);
    left.append(&login);

    // ── OSK dock across the full window width (config can drop it).
    // Starts STOWED — the ▲ tab at the bottom center raises it. Tab + deck
    // live in a bottom-anchored OVERLAY child: raising takes no layout
    // space, so the art and form never move and the deck slides over them.
    let osk_dock: Option<osk::OskDock> = if ui.cfg.osk {
        let dock = {
            let ui = ui.clone();
            osk::OskDock::new(ui.cfg.accent, move |ev| handle_osk_event(&ui, ev))
        };
        let dock_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        dock_box.set_valign(gtk4::Align::End);
        dock_box.set_halign(gtk4::Align::Center); // deck-width footprint, not
        // full-width: an overlay box wider than its content would swallow
        // clicks aimed at the UI beneath it
        dock_box.set_margin_bottom(16); // same floor as the main box
        dock_box.append(&dock.tab);
        dock_box.append(&dock.dock);
        overlay.add_overlay(&dock_box);
        Some(dock)
    } else {
        None
    };
    // the CRT layer goes in LAST among the overlay children so it paints
    // over the dock as well — the deck stays part of the phosphor picture
    overlay.add_overlay(effects.widget());

    // ── physical keyboard feeds the same buffer ────────────────────────
    {
        let ui = ui.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        // capture phase: see physical keys before any (formerly) focused
        // widget's default activation could eat them
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_k, keyval, _code, _state| {
            // preview escape hatch — the real lock has NO key that exits
            // (that is the entire point of a lock screen)
            if ui.demo && keyval == gtk4::gdk::Key::Escape {
                request_exit(&ui, EXIT_UNLOCKED);
                return glib::Propagation::Stop;
            }
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
                return glib::Propagation::Proceed;
            };
            handle_osk_event(&ui, ev);
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }

    // blinking block cursor
    {
        let cursor = cursor_lbl.clone();
        glib::timeout_add_local(Duration::from_millis(530), move || {
            cursor.set_opacity(if cursor.opacity() < 0.5 { 1.0 } else { 0.0 });
            glib::ControlFlow::Continue
        });
    }

    window.set_child(Some(&overlay));
    // NO present(): assign_window_to_monitor maps it

    LockView {
        window,
        pw_dots,
        authmsg,
        status,
        art: art_lbl,
        osk: osk_dock,
        effects,
        boot,
    }
}

fn handle_osk_event(ui: &Rc<LockUi>, ev: osk::OskEvent) {
    match ev {
        osk::OskEvent::Char(c) => ui.buffer.borrow_mut().push(c),
        osk::OskEvent::Backspace => {
            ui.buffer.borrow_mut().pop();
        }
        osk::OskEvent::Enter => return on_enter(ui),
    }
    set_dots_all(ui);
}

/// Single phase, straight to PAM — no greetd out here. One RET always means
/// "go" (the password is handed to the first PAM secret prompt).
fn on_enter(ui: &Rc<LockUi>) {
    if ui.busy.get() || ui.cooldown.get() || ui.leaving.get() {
        return;
    }
    if ui.buffer.borrow().is_empty() {
        set_msg_all(ui, "type the password first", false);
        return;
    }
    if ui.demo {
        return demo_enter(ui);
    }
    ui.busy.set(true);
    set_msg_all(ui, "verifying credentials …", false);
    let pw = ui.buffer.borrow().clone();
    ui.buffer.borrow_mut().clear();
    set_dots_all(ui);
    crate::auth::trace("ui: lock submit → PAM");
    crate::pam::authenticate(ui.username.clone(), pw, ui.tx.lock().unwrap().clone());
}

/// Preview-only submit (`--demo-l`): the exact same outcome visuals as the
/// real path, but NO PAM call ever — arming it here would strike the real
/// account's pam_faillock tally on every demo submit. Any password denies;
/// the literal "demo" grants, and the Granted path's teardown closes the
/// preview (nothing is locked, so request_exit just quits the loop).
fn demo_enter(ui: &Rc<LockUi>) {
    ui.busy.set(true);
    set_msg_all(ui, "verifying credentials …", false);
    let pw = ui.buffer.borrow().clone();
    ui.buffer.borrow_mut().clear();
    set_dots_all(ui);
    let granted = pw == "demo";
    let ui2 = ui.clone();
    glib::timeout_add_local_once(Duration::from_millis(450), move || {
        on_outcome(
            &ui2,
            if granted {
                Outcome::Granted
            } else {
                Outcome::AuthFailed("demo — PAM not armed in preview".into())
            },
        );
    });
}

fn on_outcome(ui: &Rc<LockUi>, outcome: Outcome) {
    crate::auth::trace(&format!("ui: lock outcome {outcome:?}"));
    ui.busy.set(false);
    if ui.leaving.get() {
        return; // late outcomes during teardown
    }
    match outcome {
        // pam.rs never emits this in lock mode (the conv answers ECHO_OFF
        // prompts from the queued password); show it and stay put
        Outcome::Prompt(msg) => set_msg_all(ui, &msg, false),
        Outcome::Granted => {
            ui.leaving.set(true);
            for v in ui.views.borrow().iter() {
                v.effects.bloom();
            }
            set_msg_all(ui, "ACCESS RESTORED — welcome back, operator", false);
            boot_all(ui, "[  OK  ] authentication passed");
            // keep the surface (and the message) up as long as the greeter's
            // success screen does — unlock() destroys the lock surfaces, and
            // dying while still locked is the one forbidden move
            let ui2 = ui.clone();
            glib::timeout_add_local_once(Duration::from_millis(TEARDOWN_MS), move || {
                request_exit(&ui2, EXIT_UNLOCKED);
            });
        }
        Outcome::AuthFailed(desc) => {
            ui.cooldown.set(true);
            for v in ui.views.borrow().iter() {
                v.effects.glitch();
            }
            // preview: say what actually grants — nobody's real password
            // works here, and "DENIED" alone reads as a broken preview
            if ui.demo {
                set_msg_all(ui, "DENIED — preview: only the password 'demo' grants", true);
            } else {
                set_msg_all(ui, "ACCESS DENIED — type the password and press RET", true);
            }
            boot_all(ui, &format!("[ FAILED ] authentication: {desc}"));
            ui.buffer.borrow_mut().clear();
            set_dots_all(ui);
            // typing stays live during the cooldown; only RET is gated, so a
            // retry can be pre-typed while the deck re-arms
            let ui2 = ui.clone();
            glib::timeout_add_local_once(Duration::from_millis(COOLDOWN_MS), move || {
                ui2.cooldown.set(false);
            });
        }
        Outcome::Fatal(desc) => {
            // PAM is unusable (service file gone, pam_start failed): no auth
            // was ever possible. swaylock fails open here too — locking a
            // keyboard-less tablet out forever is the worse outcome.
            ui.leaving.set(true);
            set_msg_all(ui, &format!("error: {desc}"), true);
            boot_all(ui, &format!("[ ERROR ] {desc}"));
            crate::auth::trace(&format!("lock: PAM unusable, failing open: {desc}"));
            let ui2 = ui.clone();
            glib::timeout_add_local_once(Duration::from_millis(FATAL_TEARDOWN_MS), move || {
                request_exit(&ui2, EXIT_AUTH_FATAL);
            });
        }
    }
}

/// Idempotent exit: decide the code once, unlock if still locked, quit the
/// loop on ::unlocked (or immediately when we hold no lock). The only code
/// path that ever ends the process — never std::process::exit while locked.
fn request_exit(ui: &Rc<LockUi>, rc: i32) {
    if ui.exiting.get() {
        return;
    }
    ui.exiting.set(true);
    ui.exit_rc.set(rc);
    crate::auth::trace(&format!(
        "lock: exiting rc={rc} is_locked={}",
        ui.lock.is_locked()
    ));
    if ui.lock.is_locked() {
        // ::unlocked fires once the compositor accepts it; that handler quits
        ui.lock.unlock();
    } else {
        ui.main_loop.quit();
    }
    // a lost ::unlocked must never wedge the process: re-check, retry, quit
    let ui2 = ui.clone();
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        if ui2.lock.is_locked() {
            ui2.lock.unlock(); // second try; ::unlocked quits the loop
        } else {
            ui2.main_loop.quit();
        }
    });
}

fn set_dots_all(ui: &LockUi) {
    let dots = "•".repeat(ui.buffer.borrow().len());
    for v in ui.views.borrow().iter() {
        v.pw_dots.set_text(&dots);
    }
}

fn set_msg_all(ui: &LockUi, text: &str, err: bool) {
    for v in ui.views.borrow().iter() {
        v.authmsg.set_label(text);
        if err {
            v.authmsg.add_css_class("err");
        } else {
            v.authmsg.remove_css_class("err");
        }
    }
}

fn boot_all(ui: &LockUi, line: &str) {
    for v in ui.views.borrow().iter() {
        v.boot.event(line);
    }
}
