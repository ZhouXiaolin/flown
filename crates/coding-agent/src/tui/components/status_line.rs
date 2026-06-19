//! StatusLine — the top status bar.
//!
//! Reads `state.status` and renders a single `RichText` line: animated spinner
//! (when busy) + model + thinking level + project path + git branch + context
//! %, then a fill rule and the session name. The spinner frame advances via the
//! `every` tick registered in `runtime.rs`; this component just renders the
//! current snapshot.
//!
//! `build_line` is ported verbatim from the old `status_line.rs` (it returns
//! `Vec<Span>`), wrapped in a `Line` for `RichText`.

use std::rc::Rc;

use iodilos::prelude::*;

use crate::tui::state::{BUSY_FRAMES, StatusInfo, UiState};

#[component]
pub fn StatusLine() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let active_index = stack.active_index_signal();

    let node = Node::new_richtext();
    let seed = node.clone();
    create_effect(move || {
        // Track layer switches: re-read the active layer's status signal so an
        // overlap push / Ctrl+C pop re-renders this bar from the new layer.
        active_index.get();
        let active = stack.active();
        let status = Rc::clone(&active.state).status;
        let badge = stack.active_overlap_badge();
        let state_busy = active.state.busy.get();
        let line = status.with(|s| {
            if badge.is_some() || s.busy || state_busy {
                tracing::info!(
                    target: "flown::statusline",
                    layer = ?active.kind,
                    badge = badge.as_deref().unwrap_or(""),
                    state_busy,
                    status_busy = s.busy,
                    frame = s.frame,
                    "statusline render"
                );
            }
            build_line(s, 0, badge.as_deref())
        });
        seed.set_lines(vec![line]);
    });
    node
}

pub fn status_line_for_state(state: Rc<UiState>, badge: Option<String>) -> Node {
    let node = Node::new_richtext();
    let seed = node.clone();
    create_effect(move || {
        let state_busy = state.busy.get();
        let line = state.status.with(|s| {
            if badge.is_some() || s.busy || state_busy {
                tracing::info!(
                    target: "flown::statusline",
                    layer = "overlay",
                    badge = badge.as_deref().unwrap_or(""),
                    state_busy,
                    status_busy = s.busy,
                    frame = s.frame,
                    "statusline render"
                );
            }
            build_line(s, 0, badge.as_deref())
        });
        seed.set_lines(vec![line]);
    });
    node
}

/// Build the status line as a single `Line`. `width` is the available columns
/// (0 = unknown; the fill rule is skipped when width is 0, since iodilos lays
/// out via flexbox rather than a fixed terminal width here).
fn build_line(status: &StatusInfo, width: usize, badge: Option<&str>) -> Line<'static> {
    let mut spans = Vec::new();

    // Separator style: " · " (dot separator like oh-my-pi)
    let sep = " · ";
    let sep_style = Style::default().fg(Color::Rgb(80, 80, 90));

    // ── Left segments ──────────────────────────────────────────

    // 1. Pi icon (animated when busy)
    let pi_icon = if status.busy {
        BUSY_FRAMES.get(status.frame).copied().unwrap_or("◐")
    } else {
        "●"
    };
    let pi_color = if status.busy {
        Color::Yellow
    } else {
        Color::Cyan
    };
    spans.push(Span::styled(
        format!(" {pi_icon} "),
        Style::default().fg(pi_color).add_modifier(Modifier::BOLD),
    ));

    // 2. Model + thinking level
    if !status.model.is_empty() {
        let model_text = if status.model.starts_with("Claude ") {
            status.model[7..].to_string()
        } else {
            status.model.clone()
        };
        spans.push(Span::styled(model_text, Style::default().fg(Color::White)));
        if !status.thinking_level.is_empty() {
            spans.push(Span::styled(sep, sep_style));
            let thinking_color = if status.thinking_level == "off" {
                Color::Rgb(100, 100, 110)
            } else {
                Color::Rgb(180, 140, 255)
            };
            spans.push(Span::styled(
                status.thinking_level.clone(),
                Style::default().fg(thinking_color),
            ));
        }
    }

    // 3. Project path
    if !status.cwd.is_empty() {
        spans.push(Span::styled(sep, sep_style));
        spans.push(Span::styled(
            shorten_path(&status.cwd),
            Style::default().fg(Color::Rgb(100, 160, 220)),
        ));
    }

    // 4. Git branch
    if let Some(branch) = &status.git_branch {
        spans.push(Span::styled(sep, sep_style));
        let git_color = if status.git_dirty {
            Color::Rgb(220, 180, 60)
        } else {
            Color::Rgb(100, 200, 120)
        };
        spans.push(Span::styled(branch.clone(), Style::default().fg(git_color)));
    }

    // 5. Cache / Context
    spans.push(Span::styled(sep, sep_style));
    let ctx_color = if status.context_pct >= 0.9 {
        Color::Red
    } else if status.context_pct >= 0.7 {
        Color::Magenta
    } else if status.context_pct >= 0.5 {
        Color::Yellow
    } else {
        Color::Rgb(100, 200, 120)
    };
    let ctx_text = format!("{:.0}%", status.context_pct * 100.0);
    spans.push(Span::styled(ctx_text, Style::default().fg(ctx_color)));

    if status.cache_read > 0 || status.cache_write > 0 {
        spans.push(Span::styled(
            format!(
                " (↓{}/↑{})",
                format_tokens(status.cache_read),
                format_tokens(status.cache_write)
            ),
            Style::default().fg(Color::Rgb(80, 80, 90)),
        ));
    }

    // ── Fill and right segments ────────────────────────────────

    if width > 0 {
        let left_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let left_width = unicode_width::UnicodeWidthStr::width(left_text.as_str());
        let fill_width = width.saturating_sub(left_width + 2);
        if fill_width > 0 {
            spans.push(Span::styled(
                "─".repeat(fill_width),
                Style::default().fg(Color::Rgb(50, 50, 60)),
            ));
        }
    }

    if let Some(name) = &status.session_name {
        spans.push(Span::styled(
            format!(" {} ", name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    if let Some(badge) = badge {
        spans.push(Span::styled(
            format!(" {} ", badge),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

/// Shorten a path by replacing $HOME with ~
fn shorten_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

/// Format token count for display (e.g., 1234567 -> "1.2M")
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}

// Keep the Borders import live for the `view!` macro (status line has no border
// itself, but the prop set is referenced elsewhere).
#[allow(unused_imports)]
use Borders as _Borders;
