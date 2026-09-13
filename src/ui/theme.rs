//! Theme tokens: the phosphor palette as generated `@define-color` lines,
//! plus the CRT style mapping.
//!
//! The token block is GENERATED here and prepended to `assets/style.css`
//! (which only references `@token`s). Rationale: GTK's behavior for two
//! same-name `@define-color`s is unspecified, so a "base sheet + override
//! block" trick could silently resolve either way — one definition per
//! name, emitted in one place, is the only safe shape.
//!
//! Identity guarantee: `token_block(None)` is byte-for-byte today's palette
//! (unit-tested), so no config file → the exact same CSS.

use crate::config::{Crt, Config, HexColor};

/// The stock phosphor ramp — byte-equal to what style.css held before the
/// token split (and to the mockup's CSS vars).
const STOCK: TokenSet = TokenSet {
    crt_black: "#050a07",
    phos: "#2bd96b",
    phos_hi: "#49ff88",
    phos_dim: "#1a8f4c",
    phos_label: "#15703f",
    phos_faint: "#0f4d2a",
    deep: "#11633a",
};

#[derive(Debug, Clone, Copy)]
struct TokenSet {
    crt_black: &'static str,
    phos: &'static str,
    phos_hi: &'static str,
    phos_dim: &'static str,
    phos_label: &'static str,
    phos_faint: &'static str,
    deep: &'static str,
}

/// Full stylesheet: token block + the rule sheet. The only CSS the app loads.
pub fn css(cfg: &Config) -> String {
    format!(
        "{}\n{}",
        token_block(cfg.accent),
        include_str!("../../assets/style.css")
    )
}

/// `@define-color` block for the accent (None = stock). Derived ramp when an
/// accent is set: hi = accent toward white, dim/label/faint/deep = accent
/// scaled down. The stock path emits the literal hexes unchanged.
pub fn token_block(accent: Option<HexColor>) -> String {
    let hex: fn((f64, f64, f64)) -> String = |(r, g, b)| {
        let to8 = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        format!("#{:02x}{:02x}{:02x}", to8(r), to8(g), to8(b))
    };
    let (phos, phos_hi, phos_dim, phos_label, phos_faint, deep) = match accent {
        None => (
            STOCK.phos.to_string(),
            STOCK.phos_hi.to_string(),
            STOCK.phos_dim.to_string(),
            STOCK.phos_label.to_string(),
            STOCK.phos_faint.to_string(),
            STOCK.deep.to_string(),
        ),
        Some(c) => (
            c.to_hex(),
            hex(mix(c, 0.30)),
            hex(scale(c, 0.65)),
            hex(scale(c, 0.53)),
            hex(scale(c, 0.36)),
            hex(scale(c, 0.50)),
        ),
    };
    format!(
        "@define-color crt_black {};\n\
         @define-color phos {};\n\
         @define-color phos_hi {};\n\
         @define-color phos_dim {};\n\
         @define-color phos_label {};\n\
         @define-color phos_faint {};\n\
         @define-color deep {};\n",
        STOCK.crt_black, phos, phos_hi, phos_dim, phos_label, phos_faint, deep
    )
}

/// accent moved 30% toward white
pub(crate) fn mix(c: HexColor, white: f64) -> (f64, f64, f64) {
    let (r, g, b) = c.rgb();
    (
        r * (1.0 - white) + white,
        g * (1.0 - white) + white,
        b * (1.0 - white) + white,
    )
}

/// accent scaled toward black
pub(crate) fn scale(c: HexColor, f: f64) -> (f64, f64, f64) {
    let (r, g, b) = c.rgb();
    (r * f, g * f, b * f)
}

// ── CRT overlay mapping ────────────────────────────────────────────────

/// CRT ambience levels. `Strong` (the default) is today's exact numbers.
pub fn crt_style(cfg: &Config) -> super::crt::CrtStyle {
    let mut style = match cfg.crt {
        Crt::Strong => super::crt::CrtStyle::default(),
        Crt::Subtle => super::crt::CrtStyle {
            scanlines: 0.10,
            vignette: 0.30,
            ..Default::default()
        },
        Crt::Off => super::crt::CrtStyle {
            scanlines: 0.0,
            vignette: 0.0,
            ..Default::default()
        },
    };
    if let Some(c) = cfg.accent {
        style.bloom = mix(c, 0.75);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_happy_and_sad() {
        assert_eq!(
            HexColor::parse("#49FF88"),
            Some(HexColor { r: 0x49, g: 0xff, b: 0x88 })
        );
        assert_eq!(HexColor::parse("49ff88"), None);
        assert_eq!(HexColor::parse("#49f"), None);
        assert_eq!(HexColor::parse("#49ff8z"), None);
    }

    /// THE identity guarantee: no accent → byte-for-byte today's palette.
    /// (Semicolons are REQUIRED after @define-color — GTK's parser errors
    /// on a missing one and the whole sheet degrades.)
    #[test]
    fn stock_token_block_is_the_literal_palette() {
        assert_eq!(
            token_block(None),
            "@define-color crt_black #050a07;\n\
             @define-color phos #2bd96b;\n\
             @define-color phos_hi #49ff88;\n\
             @define-color phos_dim #1a8f4c;\n\
             @define-color phos_label #15703f;\n\
             @define-color phos_faint #0f4d2a;\n\
             @define-color deep #11633a;\n"
        );
    }

    #[test]
    fn accent_block_carries_the_accent_and_a_derived_ramp() {
        // amber — high-contrast against stock green, deny-red kept apart
        let c = HexColor { r: 0xff, g: 0xb0, b: 0x00 };
        let block = token_block(Some(c));
        // the accent lands on the main token, verbatim (semicolon required!)
        assert!(block.contains("@define-color phos #ffb000;\n"));
        // derive the ramp independently per-channel and check direction:
        // hi is accent toward white (every channel >= accent)…
        let hi = HexColor::parse(
            block
                .split("@define-color phos_hi ")
                .nth(1)
                .unwrap()
                .split('\n')
                .next()
                .unwrap()
                .trim_end_matches(';'),
        )
        .unwrap();
        assert!(hi.r >= c.r && hi.g >= c.g && hi.b >= c.b);
        // …dim/label/faint are accent scaled down (every channel <= accent),
        // and deeper tokens are darker than shallower ones
        let tok = |name: &str| {
            HexColor::parse(
                block
                    .split(name)
                    .nth(1)
                    .unwrap()
                    .split([' ', '\n'])
                    .next()
                    .unwrap()
                    .trim_end_matches(';'),
            )
            .unwrap()
        };
        let dim = tok("phos_dim ");
        let label = tok("phos_label ");
        let faint = tok("phos_faint ");
        let deep = tok("deep ");
        for t in [dim, label, faint, deep] {
            assert!(t.r <= c.r && t.g <= c.g && t.b <= c.b);
        }
        assert!(deep.r >= faint.r && deep.g >= faint.g);
        // crt_black untouched by accent
        assert!(block.contains("@define-color crt_black #050a07;\n"));
    }

    #[test]
    fn css_ends_with_the_rule_sheet() {
        let sheet = include_str!("../../assets/style.css");
        assert!(css(&Config::default()).ends_with(sheet));
    }

    /// Strong = today's exact CRT numbers.
    #[test]
    fn crt_style_matrix() {
        let d = crt_style(&Config::default());
        assert_eq!((d.scanlines, d.vignette), (0.22, 0.55));
        assert_eq!(d.bloom, (0.84, 1.0, 0.90));

        let s = crt_style(&Config { crt: Crt::Subtle, ..Default::default() });
        assert_eq!((s.scanlines, s.vignette), (0.10, 0.30));

        let o = crt_style(&Config { crt: Crt::Off, ..Default::default() });
        assert_eq!((o.scanlines, o.vignette), (0.0, 0.0));
    }

    #[test]
    fn accent_recolors_bloom_only() {
        let c = HexColor { r: 0xff, g: 0x00, b: 0x00 };
        let s = crt_style(&Config { accent: Some(c), ..Default::default() });
        // 100% red toward white 75% → (1.0, 0.75, 0.75)
        assert_eq!(s.bloom, (1.0, 0.75, 0.75));
        // ambience numbers unchanged
        assert_eq!((s.scanlines, s.vignette), (0.22, 0.55));
    }
}
