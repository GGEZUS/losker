//! Built-in on-screen keyboard — US QWERTY, shift + symbols layers.
//! Custom-drawn: keycaps are cairo on a DrawingArea, hit-tested by touch.
//! The look must NOT depend on GTK button CSS, which proved unreliable in
//! the greeter context (buttons rendered with the fallback light theme
//! regardless of the provider). Emits events to the app only — no
//! virtual-keyboard protocol, no wvkbd.

use gtk4::{cairo, prelude::*};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OskEvent {
    Char(char),
    Backspace,
    Enter,
}

const LETTERS: [&[&str]; 3] = [
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l", ";"],
    &["z", "x", "c", "v", "b", "n", "m", ",", ".", "/"],
];
const NUMBERS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];
const SYM_LETTERS: [&[&str]; 3] = [
    &["~", "`", "|", "\\", "{", "}", "[", "]", "<", ">"],
    &["-", "_", "+", "=", ":", "\"", "'", "?", "!", "*"],
    &["€", "§", "¶", "†", "©", "°", "·", "…", "—", "\\"],
];
const SYM_NUMBERS: &[&str] = &["!", "@", "#", "$", "%", "^", "&", "*", "(", ")"];

// metrics (logical px)
const KEY_W: f64 = 66.0;
const KEY_H: f64 = 62.0;
const GAP: f64 = 6.0;
const SP_W: f64 = 96.0;
const SPACE_W: f64 = 320.0;
const RET_W: f64 = 140.0;
const STAGGER_A: f64 = 36.0;
const STAGGER_Z: f64 = 54.0;
const DEPTH: f64 = 5.0;
const RADIUS: f64 = 3.0;

// palette — dark dyesub caps in a dim room
const CAP_BG: (f64, f64, f64) = (0.227, 0.208, 0.173); // #3a352c
const CAP_SIDE: (f64, f64, f64) = (0.133, 0.122, 0.098);
const CAP_BORDER: (f64, f64, f64) = (0.168, 0.153, 0.125);
const CAP_PRESSED: (f64, f64, f64) = (0.184, 0.165, 0.133);
const LEGEND: (f64, f64, f64) = (0.659, 0.635, 0.557); // #a8a28e khaki
const LEGEND_DIM: (f64, f64, f64) = (0.42, 0.392, 0.333);
const SP_BG: (f64, f64, f64) = (0.196, 0.18, 0.149);
const SP_SIDE: (f64, f64, f64) = (0.118, 0.102, 0.082);
const LATCH_BG: (f64, f64, f64) = (0.431, 0.396, 0.322); // shift engaged
const LATCH_FG: (f64, f64, f64) = (0.082, 0.071, 0.051);

#[derive(Clone, Copy, PartialEq)]
enum KeyKind {
    Normal,
    Modifier,
    Latch,  // shift when engaged
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyAction {
    Char(char),
    Backspace,
    Enter,
    ToggleShift,
    ToggleLayer,
}

struct KeyDef {
    label: String,
    x: f64,
    y: f64,
    w: f64,
    kind: KeyKind,
    action: KeyAction,
}

struct DeckState {
    shifted: bool,
    /// caps latch: shift pressed twice stays engaged until pressed again
    shift_latched: bool,
    sym_layer: bool,
    pressed: Option<(usize, usize)>,
    layout: Vec<Vec<KeyDef>>,
    total: (f64, f64),
    on_event: Box<dyn Fn(OskEvent)>,
}

pub struct Osk {
    pub container: gtk4::Box,
    area: gtk4::DrawingArea,
    state: Rc<RefCell<DeckState>>,
}

impl Osk {
    pub fn new(on_event: impl Fn(OskEvent) + 'static) -> Self {
        let container = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        container.add_css_class("osk");

        let cap = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let l = gtk4::Label::new(Some("MANUAL ENTRY DECK"));
        let r = gtk4::Label::new(Some("P1-COMPATIBLE"));
        l.add_css_class("osk-cap");
        r.add_css_class("osk-cap");
        l.set_hexpand(true);
        r.set_halign(gtk4::Align::End);
        cap.append(&l);
        cap.append(&r);
        container.append(&cap);

        let area = gtk4::DrawingArea::new();
        area.set_halign(gtk4::Align::Center);
        area.set_valign(gtk4::Align::End);

        let state = Rc::new(RefCell::new(DeckState {
            shifted: false,
            shift_latched: false,
            sym_layer: false,
            pressed: None,
            layout: Vec::new(),
            total: (0.0, 0.0),
            on_event: Box::new(on_event),
        }));

        relayout(&state, &area);

        {
            let weak = Rc::downgrade(&state);
            area.set_draw_func(move |_area, cr, _w, _h| {
                if let Some(st) = weak.upgrade() {
                    draw_deck(&st.borrow(), cr);
                }
            });
        }

        // touch: press marks the keycap down; release on the same key fires it
        {
            let gesture = gtk4::GestureClick::new();
            let state_g = state.clone();
            let area_g = area.clone();
            gesture.connect_pressed(move |_g, _n, x, y| {
                // bind to a local FIRST: an `if let` keeps the condition's
                // temporary (the Ref borrow) alive for the whole block,
                // which collides with the borrow_mut below (boot-proven crash)
                let hit = hit_test(&state_g.borrow(), x, y);
                if let Some(hit) = hit {
                    state_g.borrow_mut().pressed = Some(hit);
                    area_g.queue_draw();
                }
            });
            let state_r = state.clone();
            let area_r = area.clone();
            gesture.connect_released(move |_g, _n, x, y| {
                let hit = hit_test(&state_r.borrow(), x, y);
                let pressed = state_r.borrow_mut().pressed.take();
                if let (Some(p), Some(h)) = (pressed, hit) {
                    if p == h && apply_key(&state_r, h) {
                        relayout(&state_r, &area_r); // layer switch: new legends
                    }
                }
                area_r.queue_draw();
            });
            let state_s = state.clone();
            let area_s = area.clone();
            gesture.connect_stopped(move |_g| {
                state_s.borrow_mut().pressed = None;
                area_s.queue_draw();
            });
            area.add_controller(gesture);
        }

        container.append(&area);
        Self {
            container,
            area,
            state,
        }
    }
}

/// Rebuild the key layout for the current layer; also (re)size the area.
fn relayout(state: &Rc<RefCell<DeckState>>, area: &gtk4::DrawingArea) {
    let (rows, total) = {
        let st = state.borrow();
        layout_rows(st.sym_layer)
    };
    {
        let mut st = state.borrow_mut();
        st.layout = rows;
        st.total = total;
    }
    area.set_size_request(total.0 as i32, total.1 as i32);
    area.queue_draw();
}

/// Pure layout computation — no widgets, unit-testable.
fn layout_rows(sym_layer: bool) -> (Vec<Vec<KeyDef>>, (f64, f64)) {
    let (letters, nums) = if sym_layer {
        (SYM_LETTERS, SYM_NUMBERS)
    } else {
        (LETTERS, NUMBERS)
    };

    // rows as (left offset, entries) before centering
    type Entry = (String, f64, KeyKind, KeyAction);
    let mut rows: Vec<(f64, Vec<Entry>)> = Vec::new();

    let mk = |s: &str| s.to_uppercase();
    let char_of = |s: &str| s.chars().next().unwrap_or('\0');

    let mut r0 = Vec::new();
    for k in nums {
        r0.push((mk(k), KEY_W, KeyKind::Normal, KeyAction::Char(char_of(k))));
    }
    rows.push((0.0, r0));

    for row in [&letters[0], &letters[1]] {
        let mut r = Vec::new();
        for k in *row {
            r.push((mk(k), KEY_W, KeyKind::Normal, KeyAction::Char(char_of(k))));
        }
        rows.push((STAGGER_A, r));
    }

    let mut rz = Vec::new();
    rz.push(("SHIFT".into(), SP_W, KeyKind::Modifier, KeyAction::ToggleShift));
    for k in letters[2] {
        rz.push((mk(k), KEY_W, KeyKind::Normal, KeyAction::Char(char_of(k))));
    }
    rz.push(("BKSP".into(), SP_W, KeyKind::Modifier, KeyAction::Backspace));
    rows.push((STAGGER_Z, rz));

    let mut rb = Vec::new();
    let layer_lbl = if sym_layer { "ABC" } else { "NUM" };
    rb.push((layer_lbl.into(), SP_W, KeyKind::Modifier, KeyAction::ToggleLayer));
    rb.push(("SPACE".into(), SPACE_W, KeyKind::Space, KeyAction::Char(' ')));
    rb.push(("RET".into(), RET_W, KeyKind::Modifier, KeyAction::Enter));
    rows.push((0.0, rb));

    // widths per row, then center every row around the widest
    let widths: Vec<f64> = rows
        .iter()
        .map(|(off, entries)| {
            off + entries.iter().map(|e| e.1).sum::<f64>()
                + GAP * entries.len().saturating_sub(1) as f64
        })
        .collect();
    let total_w = widths.iter().cloned().fold(0.0_f64, f64::max);
    let total_h = (KEY_H + GAP) * rows.len() as f64;

    let mut layout = Vec::new();
    for (ri, ((off, entries), row_w)) in rows.iter().zip(widths.iter()).enumerate() {
        let y = ri as f64 * (KEY_H + GAP);
        let x0 = (total_w - row_w) / 2.0;
        let mut x = x0 + off;
        let mut defs = Vec::new();
        for (label, w, kind, action) in entries {
            defs.push(KeyDef {
                label: label.to_string(),
                x,
                y,
                w: *w,
                kind: *kind,
                action: *action,
            });
            x += w + GAP;
        }
        layout.push(defs);
    }
    (layout, (total_w, total_h))
}

fn hit_test(st: &DeckState, x: f64, y: f64) -> Option<(usize, usize)> {
    for (ri, row) in st.layout.iter().enumerate() {
        for (ki, k) in row.iter().enumerate() {
            if x >= k.x && x <= k.x + k.w && y >= k.y && y <= k.y + KEY_H {
                return Some((ri, ki));
            }
        }
    }
    None
}

/// Apply a key action; returns true when the deck needs a relayout.
/// State is mutated and the event computed under one short borrow; the
/// handler runs AFTER the borrow drops — calling back into GTK while
/// holding a RefCell borrow re-enters the draw func and panics.
fn apply_key(state: &Rc<RefCell<DeckState>>, (ri, ki): (usize, usize)) -> bool {
    enum Emit {
        None,
        Char(char),
        Backspace,
        Enter,
    }
    let (emit, rebuild) = {
        let mut st = state.borrow_mut();
        let action = match st.layout.get(ri).and_then(|r| r.get(ki)) {
            Some(k) => k.action,
            None => return false,
        };
        match action {
            KeyAction::Char(c) => {
                let out = if st.shifted || st.shift_latched {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c
                };
                if st.shifted {
                    st.shifted = false; // one-shot; the latch persists
                }
                (Emit::Char(out), false)
            }
            KeyAction::Backspace => (Emit::Backspace, false),
            KeyAction::Enter => (Emit::Enter, false),
            KeyAction::ToggleShift => {
                // tap 1: one-shot · tap 2: latch (caps) · tap 3: release
                if st.shift_latched {
                    st.shift_latched = false;
                } else if st.shifted {
                    st.shifted = false;
                    st.shift_latched = true;
                } else {
                    st.shifted = true;
                }
                (Emit::None, false)
            }
            KeyAction::ToggleLayer => {
                st.sym_layer = !st.sym_layer;
                (Emit::None, true)
            }
        }
    };
    // borrow released — safe to call back into GTK/UI
    match emit {
        Emit::None => {}
        Emit::Char(c) => (state.borrow().on_event)(OskEvent::Char(c)),
        Emit::Backspace => (state.borrow().on_event)(OskEvent::Backspace),
        Emit::Enter => (state.borrow().on_event)(OskEvent::Enter),
    }
    rebuild
}

// ── drawing ───────────────────────────────────────────────────────────

fn draw_deck(st: &DeckState, cr: &cairo::Context) {
    for (ri, row) in st.layout.iter().enumerate() {
        for (ki, k) in row.iter().enumerate() {
            let pressed = st.pressed == Some((ri, ki));
            let latched = matches!(k.kind, KeyKind::Modifier)
                && matches!(k.action, KeyAction::ToggleShift)
                && (st.shifted || st.shift_latched);
            draw_keycap(cr, k, pressed, latched);
        }
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::PI / 2.0);
    cr.arc(x + r, y + h - r, r, std::f64::consts::PI / 2.0, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::PI / 2.0);
    cr.close_path();
}

fn draw_keycap(cr: &cairo::Context, k: &KeyDef, pressed: bool, latched: bool) {
    let (face, side) = if latched {
        (LATCH_BG, CAP_SIDE)
    } else if pressed {
        (CAP_PRESSED, CAP_PRESSED)
    } else {
        match k.kind {
            KeyKind::Normal => (CAP_BG, CAP_SIDE),
            KeyKind::Modifier | KeyKind::Latch => (SP_BG, SP_SIDE),
            KeyKind::Space => (SP_BG, SP_SIDE),
        }
    };

    if pressed {
        // key traveled down: single flush face
        cr.set_source_rgb(face.0, face.1, face.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H, RADIUS);
        cr.fill().unwrap();
    } else {
        // keycap side (depth), then the top face above it
        cr.set_source_rgb(side.0, side.1, side.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H, RADIUS);
        cr.fill().unwrap();
        cr.set_source_rgb(face.0, face.1, face.2);
        rounded_rect(cr, k.x, k.y, k.w, KEY_H - DEPTH, RADIUS);
        cr.fill().unwrap();
    }

    // hairline border
    cr.set_source_rgb(CAP_BORDER.0, CAP_BORDER.1, CAP_BORDER.2);
    cr.set_line_width(1.0);
    rounded_rect(cr, k.x + 0.5, k.y + 0.5, k.w - 1.0, KEY_H - 1.0, RADIUS);
    cr.stroke().unwrap();

    // legend
    let legend = if latched {
        LATCH_FG
    } else if k.kind == KeyKind::Space {
        LEGEND_DIM
    } else if k.kind == KeyKind::Modifier {
        LEGEND_DIM
    } else {
        LEGEND
    };
    let big = k.kind == KeyKind::Normal;
    cr.select_font_face(
        if big { "DejaVu Sans" } else { "DejaVu Sans Mono" },
        cairo::FontSlant::Normal,
        if big { cairo::FontWeight::Bold } else { cairo::FontWeight::Normal },
    );
    cr.set_font_size(if big { 20.0 } else { 13.0 });
    let ext = cr.text_extents(&k.label).unwrap();
    let tx = k.x + (k.w - ext.width()) / 2.0 - ext.x_bearing();
    let ty = k.y + (KEY_H - DEPTH) / 2.0 - ext.height() / 2.0 - ext.y_bearing();
    cr.set_source_rgb(legend.0, legend.1, legend.2);
    cr.move_to(tx, ty);
    let _ = cr.show_text(&k.label);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_bounded_and_centered() {
        for sym in [false, true] {
            let (rows, (tw, th)) = layout_rows(sym);
            assert_eq!(rows.len(), 5);
            assert!(tw > 0.0 && th > 0.0);
            for row in &rows {
                for k in row {
                    assert!(k.x >= -0.01, "key left of deck: {}", k.x);
                    assert!(k.x + k.w <= tw + 0.01, "key past right edge");
                    assert!(k.y + super::KEY_H <= th + 0.01);
                }
            }
        }
    }

    #[test]
    fn shift_legend_is_uppercase_but_action_emits_lowercase() {
        let (rows, _) = layout_rows(false);
        let q = &rows[1][0]; // rows[0] is the number row
        assert_eq!(q.label, "Q");
        assert_eq!(q.action, KeyAction::Char('q'));
    }
}
