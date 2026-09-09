//! Boot-log + live stats panel.
//! All panel lines are pre-allocated as blanks at startup and typed into
//! by index — the panel is full-height from frame one, so nothing below
//! it (the login form) ever shifts as lines appear.
//!
//! Layout of the fixed panel:
//!   lines 0..BOOT_KEEP        boot sequence (stays forever)
//!   lines BOOT_KEEP..+SLOTS   live stats (values refresh every 5s)
//!   line  BOOT_KEEP+SLOTS     runtime events (auth results, power asks)

use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// live stat slots (values refresh in place)
const STAT_SLOTS: usize = 4;
/// boot lines that stay on screen forever after typing out
const BOOT_KEEP: usize = 6;
/// total panel height in lines (boot + stats + one event line)
const TOTAL_LINES: usize = BOOT_KEEP + STAT_SLOTS + 1;

pub struct BootLog {
    label: gtk4::Label,
    lines: RefCell<Vec<String>>,
    stats_on: Cell<bool>,
}

impl BootLog {
    pub fn new(label: gtk4::Label) -> Self {
        label.add_css_class("bootlog");
        label.set_valign(gtk4::Align::Start);
        label.set_halign(gtk4::Align::Start);
        label.set_yalign(0.0);
        Self {
            label,
            // full-height from the start: blank slots, typed into by index
            lines: RefCell::new((0..TOTAL_LINES).map(|_| String::new()).collect()),
            stats_on: Cell::new(false),
        }
    }

    fn render(&self) {
        let text = self.lines.borrow().join("\n");
        self.label.set_text(&text);
    }

    fn set_line(&self, idx: usize, text: &str) {
        let mut lines = self.lines.borrow_mut();
        if idx < lines.len() {
            lines[idx] = text.to_string();
        }
        drop(lines);
        self.render();
    }

    /// Append a runtime event (auth results, power asks) — occupies the
    /// single event slot, replacing any previous event. Never moves.
    pub fn event(&self, line: &str) {
        if self.stats_on.get() {
            self.set_line(BOOT_KEEP + STAT_SLOTS, line);
        } else {
            self.lines.borrow_mut().push(line.to_string());
            self.render();
        }
    }

    /// Type the boot sequence, then freeze it into a fixed stats panel.
    pub fn start(self: &Rc<Self>) {
        type_next(Rc::downgrade(self), 0);
    }

    /// called when boot typing finishes: fill the live stat slots
    fn freeze_panel(&self) {
        for (slot, text) in stat_lines().into_iter().take(STAT_SLOTS).enumerate() {
            self.set_line(BOOT_KEEP + slot, &text);
        }
        self.stats_on.set(true);
    }
}

// ── boot sequence ─────────────────────────────────────────────────────

fn boot_lines() -> Vec<String> {
    let kernel = read_trim("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into());
    let mem_total = meminfo_gib("MemTotal").map(|g| format!("{g:.1}G")).unwrap_or_else(|| "?".into());
    let batt = battery_line();

    vec![
        format!("[    0.000000] linux {kernel} on x86_64"),
        format!("[    0.113207] memory mapped .......... {mem_total}"),
        batt,
        "[    0.184221] crt deflection coils .......... ok".into(),
        "[    0.390014] scanlines @ 15.6kHz .......... ok".into(),
        "[    1.002931] touch entry deck .......... detected".into(),
    ]
}

fn type_next(this: std::rc::Weak<BootLog>, idx: usize) {
    let lines = boot_lines();
    let Some(line) = lines.get(idx).cloned() else {
        // boot done — freeze the panel and start the refresh loop
        if let Some(log) = this.upgrade() {
            log.freeze_panel();
            stats_tick(Rc::downgrade(&log), 0);
        }
        return;
    };
    if let Some(log) = this.upgrade() {
        log.set_line(idx, &line);
    }
    glib::timeout_add_local_once(std::time::Duration::from_millis(next_delay()), move || {
        type_next(this, idx + 1)
    });
}

fn next_delay() -> u64 {
    90 + u64::from(glib::random_int() % 140)
}

// ── fixed stats panel ─────────────────────────────────────────────────

fn stats_tick(this: std::rc::Weak<BootLog>, round: u32) {
    let Some(log) = this.upgrade() else { return };
    if round > 0 {
        // refresh every slot's value in place — the panel never grows
        for (slot, text) in stat_lines().into_iter().take(STAT_SLOTS).enumerate() {
            log.set_line(BOOT_KEEP + slot, &text);
        }
    }
    glib::timeout_add_local_once(std::time::Duration::from_millis(5000), move || {
        stats_tick(this, round + 1)
    });
}

/// Real readings from the running system, one line per fixed slot.
/// Memory values come from ONE meminfo read — separate reads race the
/// kernel counters and garble the line ("6.66G free of /.6G").
fn stat_lines() -> Vec<String> {
    let mut out = Vec::with_capacity(STAT_SLOTS);

    // slot 0: load + uptime
    if let (Some(load), Some(up)) = (loadavg(), uptime()) {
        out.push(format!("[ sys  ] load {load} · up {up}"));
    } else if let Some(up) = uptime() {
        out.push(format!("[ sys  ] up {up}"));
    }

    // slot 1: memory (single snapshot)
    if let Some((used, avail, total)) = meminfo_snapshot() {
        out.push(format!(
            "[ mem  ] {used:.1}G used · {avail:.1}G free of {total:.1}G"
        ));
    }

    // slot 2: battery
    if let Some(b) = battery_detail() {
        out.push(b);
    }

    // slot 3: thermals
    if let Some(t) = cpu_temp() {
        out.push(format!("[ therm] cpu {t:.0}°C"));
    }

    out
}

/// one consistent (used, available, total) memory read in GiB
fn meminfo_snapshot() -> Option<(f64, f64, f64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut avail = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.trim().split_whitespace().next()?.parse::<f64>().ok();
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = rest.trim().split_whitespace().next()?.parse::<f64>().ok();
        }
    }
    let (total, avail) = (total?, avail?);
    Some((
        (total - avail) / 1_048_576.0,
        avail / 1_048_576.0,
        total / 1_048_576.0,
    ))
}

// ── /proc & /sys readers ──────────────────────────────────────────────

fn read_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn meminfo_gib(key: &str) -> Option<f64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
            let kib: f64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kib / 1_048_576.0);
        }
    }
    None
}

fn loadavg() -> Option<String> {
    let s = read_trim("/proc/loadavg")?;
    let mut it = s.split_whitespace();
    let (l1, l2, l3) = (it.next()?, it.next()?, it.next()?);
    Some(format!("{l1} {l2} {l3}"))
}

fn uptime() -> Option<String> {
    let s = read_trim("/proc/uptime")?;
    let secs: f64 = s.split_whitespace().next()?.parse().ok()?;
    let total = secs as u64;
    let d = total / 86400;
    let h = (total % 86400) / 3600;
    let m = (total % 3600) / 60;
    Some(if d > 0 {
        format!("{d}d {h:02}h {m:02}m")
    } else {
        format!("{h:02}h {m:02}m")
    })
}

fn battery_detail() -> Option<String> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in dir.flatten() {
        let base = entry.path();
        let cap = read_trim(&base.join("capacity").to_string_lossy())?;
        let status = read_trim(&base.join("status").to_string_lossy())
            .unwrap_or_else(|| "UNKNOWN".into())
            .to_lowercase();
        let icon = match status.as_str() {
            "charging" | "full" => "++",
            "discharging" => "--",
            _ => "==",
        };
        return Some(format!("[ batt ] {cap}% {icon} ({status})"));
    }
    None
}

fn cpu_temp() -> Option<f64> {
    for zone in 0..8 {
        if let Some(s) = read_trim(&format!("/sys/class/thermal/thermal_zone{zone}/temp")) {
            if let Ok(milli) = s.parse::<f64>() {
                if milli > 0.0 && milli < 150_000.0 {
                    return Some(milli / 1000.0);
                }
            }
        }
    }
    None
}

fn battery_line() -> String {
    match battery_detail() {
        Some(b) => b,
        None => "[    0.214808] battery .......... absent".into(),
    }
}
