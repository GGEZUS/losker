//! ASCII-art spinner stage on the right side of the greeter.
//! The art itself is the user's choice — this module only renders frames.
//!
//! Frames file: /etc/greetd/osk-ascii.txt
//!   - optional header `# delay: <ms>` sets the frame cadence (default 100)
//!   - one frame per block, blocks separated by a line containing only `---`
//!   - blank blocks are dropped; single block = static art (no animation)
//! Until the file exists, a dim placeholder marks the slot.
//!
//! The deployed frames are the spinning arch logo, generated from
//! areofyl/fetch by tools/gen-ascii-art.py — regenerate with that script.

use gtk4::glib;
use gtk4::prelude::*;
use std::cell::Cell;

const FRAMES_FILE: &str = "/etc/greetd/osk-ascii.txt";
const DEFAULT_FRAME_MS: u64 = 100;

/// The animated art label.
///
/// The timer closure STRONGLY owns the frames and a label clone — the first
/// version kept its state in an `Rc<AsciiSpin>` held only by `build()`'s
/// locals, so a Weak in the timer died the moment building finished and the
/// art froze on frame 0 (boot-proven).
pub fn spin_label() -> gtk4::Label {
    let label = gtk4::Label::new(None);
    label.add_css_class("ascii-art");
    label.set_valign(gtk4::Align::Center);
    label.set_halign(gtk4::Align::Center);

    let (frames, frame_ms) = std::fs::read_to_string(FRAMES_FILE)
        .map(|text| parse_frames(&text))
        .unwrap_or((Vec::new(), DEFAULT_FRAME_MS));

    match frames.len() {
        0 => label.set_text("[ ascii slot ]"),
        1 => label.set_text(&frames[0]),
        _ => {
            label.set_text(&frames[0]);
            let label = label.clone();
            let idx = Cell::new(0usize);
            glib::timeout_add_local(std::time::Duration::from_millis(frame_ms), move || {
                let i = idx.get();
                label.set_text(&frames[i % frames.len()]);
                idx.set((i + 1) % frames.len());
                glib::ControlFlow::Continue
            });
        }
    }
    label
}

/// Parse the frames file: optional `# delay: <ms>` header, then blocks
/// separated by `---` lines. Blank blocks are dropped. Pure — unit-testable.
pub fn parse_frames(text: &str) -> (Vec<String>, u64) {
    let mut delay_ms = DEFAULT_FRAME_MS;
    let mut frames: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# delay:") {
            if let Ok(d) = rest.trim().parse::<u64>() {
                // sane floor: below ~60fps the label churn outpaces repaints
                delay_ms = d.clamp(16, 1000);
            }
        } else if line.trim() == "---" {
            frames.push(cur.join("\n"));
            cur.clear();
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        frames.push(cur.join("\n"));
    }
    (
        frames.into_iter().filter(|f| !f.trim().is_empty()).collect(),
        delay_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_blocks_and_reads_delay_header() {
        let text = "# delay: 50\n AAA \n---\n BBB \n";
        let (frames, delay) = parse_frames(text);
        // leading/trailing spaces are art positioning — preserved verbatim
        assert_eq!(frames, vec![" AAA ".to_string(), " BBB ".to_string()]);
        assert_eq!(delay, 50);
    }

    #[test]
    fn no_header_means_default_delay() {
        let (frames, delay) = parse_frames("only\n");
        assert_eq!(frames, vec!["only".to_string()]);
        assert_eq!(delay, DEFAULT_FRAME_MS);
    }

    #[test]
    fn shading_hash_lines_are_art_not_headers() {
        // the donut ramp uses '#' mid-art; only the exact `# delay:` prefix
        // is a directive
        let (frames, _) = parse_frames("#$@!\n~~\n");
        assert_eq!(frames, vec!["#$@!\n~~".to_string()]);
    }

    #[test]
    fn blank_blocks_are_dropped() {
        let text = "A\n---\n\n \n---\nB\n";
        let (frames, _) = parse_frames(text);
        assert_eq!(frames, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn delay_is_clamped_to_sane_bounds() {
        let (_, d0) = parse_frames("# delay: 1\nx\n");
        let (_, d1) = parse_frames("# delay: 9999\nx\n");
        assert_eq!((d0, d1), (16, 1000));
    }
}
