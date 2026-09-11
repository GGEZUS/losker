//! TUI rendering: state → ratatui widgets. Phosphor-on-black to match the
//! house look. No state changes here — display only.

use super::state::{Focus, Model};
use crate::config::{Crt, Logo};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

const PHOS: Color = Color::Rgb(0x2b, 0xd9, 0x6b);
const PHOS_HI: Color = Color::Rgb(0x49, 0xff, 0x88);
const PHOS_DIM: Color = Color::Rgb(0x1a, 0x8f, 0x4c);
const WARN: Color = Color::Rgb(0xff, 0xb0, 0x00);

pub fn render(f: &mut Frame, m: &Model) {
    let outer = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(1),    // panes
        Constraint::Length(3), // status/footer
    ])
    .split(f.area());

    let header = Line::from(vec![
        Span::styled(" losker config", Style::default().fg(PHOS_HI).add_modifier(Modifier::BOLD)),
        Span::styled("  ·  applies at next greeter/lock start", Style::default().fg(PHOS_DIM)),
        Span::styled(if m.dirty { "  *unsaved" } else { "" }, Style::default().fg(WARN)),
    ]);
    f.render_widget(Paragraph::new(header), outer[0]);

    let panes = Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(outer[1]);
    render_settings(f, m, panes[0]);
    render_preview(f, m, panes[1]);

    let footer_lines = vec![
        Line::from(Span::styled(
            m.status.clone(),
            Style::default().fg(PHOS_HI),
        )),
        Line::from(vec![
            Span::styled("↑↓/jk focus · ←→/hl change · enter/space edit · s save · g custom logo · q quit", Style::default().fg(PHOS_DIM)),
        ]),
        Line::from(vec![
            Span::styled(
                if dry_run() { "DRY-RUN → " } else { "config → " },
                Style::default().fg(if dry_run() { WARN } else { PHOS_DIM }),
            ),
            Span::styled(
                m.config_path.display().to_string(),
                Style::default().fg(PHOS),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(footer_lines), outer[2]);
}

fn render_settings(f: &mut Frame, m: &Model, area: ratatui::layout::Rect) {
    let osk_val = vec![Span::styled(
        if m.cfg.osk { "enabled" } else { "disabled" },
        Style::default().fg(if m.cfg.osk { PHOS_HI } else { WARN }),
    )];

    let logo_val = vec![Span::styled(
        logo_name(m.current_logo()),
        Style::default().fg(PHOS),
    )];

    let res_val = vec![Span::styled(
        m.cfg.logo_res.name(),
        Style::default().fg(PHOS),
    )];

    // informed decision: pixels when a display told us, percentages if not
    let size_val = vec![Span::styled(
        match m.size_px {
            Some((w, h)) => format!("{} · ~{}×{} px", m.cfg.logo_size.name(), w, h),
            None => format!(
                "{} · ~{}% of the screen box",
                m.cfg.logo_size.name(),
                (m.cfg.logo_size.factor() * 100.0).round() as u32
            ),
        },
        Style::default().fg(PHOS),
    )];

    let accent = m.cfg.accent;
    let accent_val = vec![
        Span::styled(
            "██",
            Style::default()
                .fg(accent.map(|x| Color::Rgb(x.r, x.g, x.b)).unwrap_or(PHOS)),
        ),
        Span::styled(
            match accent {
                None => " stock phosphor".to_string(),
                Some(c) => format!(" {}", c.to_hex()),
            },
            Style::default().fg(PHOS),
        ),
    ];

    let crt_val = vec![Span::styled(
        match m.cfg.crt {
            Crt::Strong => "strong",
            Crt::Subtle => "subtle",
            Crt::Off => "off",
        },
        Style::default().fg(PHOS),
    )];

    let mut lines = vec![
        settings_row(m, Focus::Osk, "osk", osk_val),
        settings_row(m, Focus::Logo, "logo", logo_val),
        settings_row(m, Focus::Res, "logo_res", res_val),
        settings_row(m, Focus::Size, "logo_size", size_val),
        settings_row(m, Focus::Accent, "accent", accent_val),
        settings_row(m, Focus::Crt, "crt", crt_val),
        Line::from(""),
    ];

    if !m.cfg.osk {
        lines.push(Line::from(Span::styled(
            "TOUCH-ONLY WARNING",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "with the OSK off there is no way to type at the",
            Style::default().fg(WARN),
        )));
        lines.push(Line::from(Span::styled(
            "greeter or lock screen. only disable this when a",
            Style::default().fg(WARN),
        )));
        lines.push(Line::from(Span::styled(
            "physical keyboard is always connected.",
            Style::default().fg(WARN),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "the deck starts STOWED — the ▲ tab at the bottom",
            Style::default().fg(PHOS_DIM),
        )));
        lines.push(Line::from(Span::styled(
            "center of the screen raises and stows it.",
            Style::default().fg(PHOS_DIM),
        )));
    }

    if let Some(buf) = &m.input {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("logo key › ", Style::default().fg(PHOS_HI)),
            Span::styled(format!("{buf}▌"), Style::default().fg(PHOS_HI).add_modifier(Modifier::BOLD)),
        ]));
    }

    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(PHOS_DIM)).title(" settings ")),
        area,
    );
}

fn render_preview(f: &mut Frame, m: &Model, area: ratatui::layout::Rect) {
    let lines = match m.current_logo() {
        Logo::Hidden => vec![Line::from(Span::styled("(art hidden)", Style::default().fg(PHOS_DIM)))],
        _ if m.preview.is_empty() => {
            vec![Line::from(Span::styled("[ ascii slot ]", Style::default().fg(PHOS_DIM)))]
        }
        _ => {
            let frame = &m.preview[m.frame % m.preview.len()];
            frame
                .lines()
                .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(PHOS))))
                .collect()
        }
    };
    let mut meta_spans = vec![Span::styled(
        format!(
            "{} frames · {} ms/frame · {} · {} res",
            m.preview.len(),
            m.preview_ms,
            logo_name(m.current_logo()),
            m.preview_tier.name()
        ),
        Style::default().fg(PHOS_DIM),
    )];
    if let Some(note) = &m.preview_note {
        // a tier fallback must never read as the setting doing nothing
        meta_spans.push(Span::styled(format!(" — {note}"), Style::default().fg(WARN)));
    }
    let meta = Line::from(meta_spans);

    let inner = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(PHOS_DIM))
                .title(" logo preview "),
        ),
        inner[0],
    );
    f.render_widget(Paragraph::new(meta), inner[1]);
}

fn logo_name(l: &Logo) -> String {
    match l {
        Logo::Legacy => "arch (legacy)".into(),
        Logo::Hidden => "none".into(),
        Logo::Key(k) => k.clone(),
    }
}

fn settings_row(
    m: &Model,
    focused: Focus,
    name: &str,
    value_spans: Vec<Span<'static>>,
) -> Line<'static> {
    let marker = if m.focus == focused { "▶ " } else { "  " };
    let mut spans = vec![Span::styled(
        marker,
        Style::default().fg(PHOS_HI).add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        format!("{name:<9}"),
        Style::default()
            .fg(if m.focus == focused { PHOS_HI } else { PHOS })
            .add_modifier(Modifier::BOLD),
    ));
    spans.extend(value_spans);
    Line::from(spans)
}

/// env override active → the save will NOT hit /etc (dry-run)
fn dry_run() -> bool {
    std::env::var("LOSKER_CONFIG").is_ok_and(|v| !v.trim().is_empty())
}
