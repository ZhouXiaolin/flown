//! Input editor — flown-specific control over iodilos-prompt.

use std::borrow::Cow;
use std::rc::Rc;

use iodilos::prelude::*;
use iodilos_prompt::{
    PromptTheme, PromptView, StatusField, StatusLine as PromptStatusLine,
};

use crate::tui::editor;
use crate::tui::state::{StatusInfo, TerminalSize, UiState};

pub(crate) const SLASH_MENU_MAX_VISIBLE: usize = 5;

pub fn input_editor() -> View {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let term_size = use_context::<TerminalSize>();
    let active_index = stack.active_index_signal();

    // The non-shrinking container is built ONCE (not under a Dynamic) so its
    // size is fixed by its content and the transcript's flex_grow above can
    // push it to the bottom of the App column. Only the inner content is
    // dynamic: it re-renders on active-layer switches, terminal resizes, and
    // input/status changes.
    let menu_and_prompt = View::from_dynamic(move || {
        active_index.get(); // re-run when the active layer switches
        let active = stack.active();
        let state = Rc::clone(&active.state);
        let badge = stack.active_overlap_badge();
        input_editor_content(state, badge, term_size)
    });

    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .flex_shrink(0.0)
            .width(Size::Percent(100.0))
            .children(menu_and_prompt),
    )
}

pub fn input_editor_for_state(state: Rc<UiState>, tail_label: Option<String>) -> View {
    let term_size = use_context::<TerminalSize>();
    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .flex_shrink(0.0)
            .width(Size::Percent(100.0))
            .children(input_editor_content(state, tail_label, term_size)),
    )
}

/// Build the popup + prompt content for one state. The slash-completion popup
/// is driven by its own memos (so it opens/closes reactively); the prompt box
/// is a Dynamic that re-renders on input/status/resize.
fn input_editor_content(
    state: Rc<UiState>,
    tail_label: Option<String>,
    term_size: TerminalSize,
) -> View {
    let slash_popup_for_items = state.slash_popup;
    let slash_popup_for_selected = state.slash_popup;
    let popup_items = create_memo(move || {
        slash_popup_for_items.with(|popup| editor::completion_items(popup.as_ref()))
    });
    // `CompletionMenuProps.selected` is keyed by the candidate's label
    // (`Option<String>`), not by a flat index, so the highlight stays glued to
    // its item as the list filters. Translate the popup's flat `selected`
    // index into the corresponding label here.
    let popup_selected: ReadSignal<Option<String>> = create_memo(move || {
        let items = popup_items.get_clone();
        let idx = slash_popup_for_selected
            .with(|popup| popup.as_ref().map_or(0, |p| p.selected));
        items.get(idx).map(|item| item.label.clone())
    });
    let menu = completion_menu(CompletionMenuProps {
        items: popup_items,
        selected: popup_selected,
        max_visible: SLASH_MENU_MAX_VISIBLE,
        border_color: Color::DarkGrey,
    });
    let prompt = prompt_view_for_state(Rc::clone(&state), tail_label, term_size);
    View::from(tags::div().children((menu, prompt)))
}

fn prompt_view_for_state(
    state: Rc<UiState>,
    tail_label: Option<String>,
    term_size: TerminalSize,
) -> View {
    let theme = PromptTheme::default();
    // Render the prompt via the iodilos-prompt `PromptView` producer — the same
    // self-drawing leaf the `PromptBox` component uses internally. Unlike the
    // framework border model (which can't place input on the rounded bottom
    // edge — `Edges::BOTTOM` is exclusive), `PromptView` draws the whole frame
    // itself: statusline on the top `╭─ … ─╮`, input text sitting directly on
    // the rounded bottom `╰─ … ─╯`, with vertical `│ … │` sides per wrapped
    // line. Input re-wraps at the layout width inside `measure`/`render`, so we
    // still rebuild on a width change to let the producer re-measure.
    View::from_dynamic(move || {
        term_size.cols.get();
        let input = state.input.get_clone();
        let status = state.status.get_clone();
        let statusline = prompt_status_line(&status, tail_label.as_deref());
        View::leaf(Box::new(PromptView::new(
            &statusline,
            &input.text(),
            input.cursor_char(),
            // No blink wiring: the caret is always drawn.
            true,
            &theme,
        )))
    })
}

fn prompt_status_line(status: &StatusInfo, tail_label: Option<&str>) -> PromptStatusLine {
    let model = if status.model.is_empty() {
        "session".to_string()
    } else if status.thinking_level.is_empty() {
        status.model.clone()
    } else {
        format!("{} · {}", status.model, status.thinking_level)
    };

    let cwd = if status.cwd.is_empty() {
        "?".to_string()
    } else {
        shorten_path(&status.cwd)
    };

    let fields = vec![
        StatusField {
            icon: Cow::Borrowed("⬢"),
            text: Cow::Owned(model),
            color: Color::Cyan,
        },
        StatusField {
            icon: Cow::Borrowed("📁"),
            text: Cow::Owned(cwd),
            color: Color::Blue,
        },
    ];
    let git_branch = status.git_branch.as_ref().map(|branch| StatusField {
        icon: Cow::Borrowed("⑂"),
        text: Cow::Owned(branch.clone()),
        color: if status.git_dirty {
            Color::Yellow
        } else {
            Color::Green
        },
    });

    // The rightmost status-line marker. A tail label (e.g. "BTW" on a forked
    // conversation overlay) replaces the plain prompt cursor so the indicator
    // sits at the far right of the status line, not as a left-side field.
    let (tail, tail_color) = match tail_label.map(str::trim).filter(|s| !s.is_empty()) {
        Some(label) => (Cow::Owned(label.to_string()), Color::Magenta),
        None => (Cow::Borrowed("▶"), Color::DarkGrey),
    };

    PromptStatusLine {
        brand: Cow::Borrowed("flown"),
        brand_color: if status.busy {
            Color::Yellow
        } else {
            Color::Cyan
        },
        fields: match git_branch {
            Some(branch) => vec![fields[0].clone(), fields[1].clone(), branch],
            None => fields,
        },
        tail,
        tail_color,
    }
}

fn shorten_path(path: &str) -> String {
    let home = dirs::home_dir();
    if let Some(home) = home {
        let home = home.display().to_string();
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_menu_shows_at_most_five_rows() {
        assert_eq!(SLASH_MENU_MAX_VISIBLE, 5);
    }

    #[test]
    fn prompt_status_line_uses_iodilos_example_icons() {
        let status = StatusInfo {
            model: "model-x".to_string(),
            cwd: "/tmp/flown".to_string(),
            git_branch: Some("main".to_string()),
            ..StatusInfo::default()
        };

        let line = prompt_status_line(&status, Some("BTW"));

        assert_eq!(line.brand.as_ref(), "flown");
        // A tail label moves to the rightmost slot (replacing the plain ▶) and
        // is no longer a left-side field.
        assert_eq!(line.tail.as_ref(), "BTW");
        assert_eq!(line.tail_color, Color::Magenta);
        let icons = line
            .fields
            .iter()
            .map(|field| field.icon.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(icons, ["⬢", "📁", "⑂"]);
    }

    #[test]
    fn prompt_status_line_tail_defaults_to_prompt_cursor() {
        let status = StatusInfo {
            model: "model-x".to_string(),
            cwd: "/tmp/flown".to_string(),
            git_branch: None,
            ..StatusInfo::default()
        };

        let line = prompt_status_line(&status, None);

        assert_eq!(line.tail.as_ref(), "▶");
        assert_eq!(line.tail_color, Color::DarkGrey);
    }
}
