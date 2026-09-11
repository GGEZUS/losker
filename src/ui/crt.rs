//! CRT effects layer — scanlines + vignette pre-rendered once into a cached
//! image (cheap blit), plus event-driven flashes (granted bloom, denied
//! tear glitch). Click-through.
//!
//! Ambience (scanlines/vignette/bloom color) is parameterized by `CrtStyle`
//! — `Default` is today's exact look. The denied-glitch stays red regardless:
//! it is semantic feedback, not decor.

use gtk4::{cairo, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Static CRT ambience, resolved from config by `ui::theme::crt_style`.
#[derive(Debug, Clone, Copy)]
pub struct CrtStyle {
    /// dark scanline alpha (0 = none)
    pub scanlines: f64,
    /// outer vignette gradient alpha (0 = none)
    pub vignette: f64,
    /// granted-bloom flash color (alpha stays 0.28 * f)
    pub bloom: (f64, f64, f64),
}

impl Default for CrtStyle {
    fn default() -> Self {
        Self {
            scanlines: 0.22,
            vignette: 0.55,
            bloom: (0.84, 1.0, 0.90),
        }
    }
}

pub struct CrtEffects {
    area: gtk4::DrawingArea,
    style: CrtStyle,
    flash: Cell<f64>,
    tear: Cell<u32>,
    cache: RefCell<Option<cairo::ImageSurface>>,
    cached_size: Cell<(i32, i32)>,
}

impl CrtEffects {
    pub fn new() -> Rc<Self> {
        Self::new_with(CrtStyle::default())
    }

    pub fn new_with(style: CrtStyle) -> Rc<Self> {
        let area = gtk4::DrawingArea::new();
        area.set_can_target(false); // click-through: purely visual

        let this = Rc::new(Self {
            area,
            style,
            flash: Cell::new(0.0),
            tear: Cell::new(0),
            cache: RefCell::new(None),
            cached_size: Cell::new((0, 0)),
        });

        {
            let weak = Rc::downgrade(&this);
            this.area.set_draw_func(move |_area, cr, w, h| {
                if let Some(this) = weak.upgrade() {
                    this.draw(cr, w, h);
                }
            });
        }

        // decay timer: only redraws while an effect is active — no idle cost
        {
            let weak = Rc::downgrade(&this);
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                match weak.upgrade() {
                    Some(this) => {
                        let f = this.flash.get();
                        let t = this.tear.get();
                        if f > 0.0 {
                            this.flash.set((f - 0.08).max(0.0));
                            this.area.queue_draw();
                        } else if t > 0 {
                            this.tear.set(t - 1);
                            this.area.queue_draw();
                        }
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                }
            });
        }

        this
    }

    pub fn widget(&self) -> &gtk4::DrawingArea {
        &self.area
    }

    /// phosphor bloom (ACCESS GRANTED)
    pub fn bloom(&self) {
        self.flash.set(1.0);
        self.area.queue_draw();
    }

    /// horizontal tear glitch (ACCESS DENIED), ~600ms
    pub fn glitch(&self) {
        self.tear.set(12);
        self.area.queue_draw();
    }

    fn draw(&self, cr: &cairo::Context, w: i32, h: i32) {
        // (re)build the static scanline+vignette image only on size change;
        // both ambience levels off → nothing static to paint
        if (self.style.scanlines > 0.0 || self.style.vignette > 0.0)
            && (self.cached_size.get() != (w, h) || self.cache.borrow().is_none())
        {
            let img = render_static(w, h, &self.style);
            *self.cache.borrow_mut() = Some(img);
            self.cached_size.set((w, h));
        }

        // single cached blit
        if let Some(img) = self.cache.borrow().as_ref() {
            cr.set_source_surface(img, 0.0, 0.0).unwrap();
            cr.paint().unwrap();
        }

        // denied: horizontal tear bands jittering per frame
        if self.tear.get() > 0 {
            let seed = fastrand_f64();
            for i in 0..4u32 {
                let band_y = h as f64 * (0.15 + 0.2 * (i as f64) + 0.04 * (seed + i as f64 * 7.3).sin());
                let band_h = 3.0 + (seed * 0.03 % 7.0);
                cr.set_source_rgba(1.0, 0.30, 0.37, 0.25);
                cr.rectangle(0.0, band_y, w as f64, band_h);
                cr.fill().unwrap();
            }
        }

        // granted: phosphor bloom flash
        let f = self.flash.get();
        if f > 0.0 {
            let (r, g, b) = self.style.bloom;
            cr.set_source_rgba(r, g, b, 0.28 * f);
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            cr.fill().unwrap();
        }
    }
}

/// The expensive part, done once per size: vignette gradient + scanlines.
fn render_static(w: i32, h: i32, style: &CrtStyle) -> cairo::ImageSurface {
    let img = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("surface");
    let cr = cairo::Context::new(&img).expect("context");

    // vignette: darker toward the edges, like CRT glass
    if style.vignette > 0.0 {
        let pat = cairo::RadialGradient::new(
            w as f64 * 0.5,
            h as f64 * 0.45,
            w as f64 * 0.28,
            w as f64 * 0.5,
            h as f64 * 0.45,
            w as f64 * 0.85,
        );
        pat.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
        pat.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, style.vignette);
        cr.set_source(&pat).unwrap();
        cr.rectangle(0.0, 0.0, w as f64, h as f64);
        cr.fill().unwrap();
    }

    // scanlines: 1 dark px every 3px
    if style.scanlines > 0.0 {
        cr.set_source_rgba(0.0, 0.0, 0.0, style.scanlines);
        let mut y = 2.0;
        while y < h as f64 {
            cr.rectangle(0.0, y, w as f64, 1.0);
            y += 3.0;
        }
        cr.fill().unwrap();
    }

    img
}

/// small xorshift pseudo-random (no deps)
fn fastrand_f64() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u32> = const { Cell::new(0x9e3779b9) };
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        s.set(x);
        (x % 997) as f64
    })
}
