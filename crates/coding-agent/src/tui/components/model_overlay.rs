//! `ModelOverlay` — the `/model` picker: a sectioned TableView listing models
//! (grouped by provider) and supported thinking levels, with arrow navigation,
//! Enter to apply, and Esc to dismiss.
//!
//! Cursor↔row mapping is kept stable by ordering the row keys once (a
//! `Vec<String>`) rather than relying on `HashMap` iteration order; the flat
//! cursor index always addresses the same `key`.

use std::rc::Rc;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use flown_agent::AgentHarness;
use flown_ai::{get_models, get_supported_thinking_levels, models_are_equal, ThinkingLevel};
use iodilos::prelude::*;

use crate::tui::overlay_stack::OverlayStack;

/// A single selectable row in the model overlay: either a model (with the
/// provider it belongs to) or a thinking level.
#[derive(Clone)]
enum PickerRow {
    Model { provider: String, model: flown_ai::Model },
    Thinking(ThinkingLevel),
}

/// Build the ordered flat list of rows and their keys.
fn build_rows(
    models: &[flown_ai::Model],
    thinking: &[ThinkingLevel],
) -> Vec<PickerRow> {
    let mut rows: Vec<PickerRow> = Vec::new();
    for m in models {
        rows.push(PickerRow::Model {
            provider: m.provider.to_string(),
            model: m.clone(),
        });
    }
    for t in thinking {
        rows.push(PickerRow::Thinking(t.clone()));
    }
    rows
}

fn row_label(row: &PickerRow) -> String {
    match row {
        PickerRow::Model { model, .. } => format!("{}/{}", model.provider, model.id),
        PickerRow::Thinking(t) => format!("thinking::{:?}", t).to_lowercase(),
    }
}

/// Render the model overlay. `on_close` is invoked after a selection is applied
/// (Enter) or on Esc.
pub fn model_overlay(harness: Arc<AgentHarness>, overlay_stack: Rc<OverlayStack>) -> Node {
    // Snapshot the candidate models + thinking levels ONCE at mount. The set is
    // derived from the current model's provider's catalog; if there's no current
    // model, fall back to an empty list (the overlay still opens, just empty).
    // Both accessors are async; block_on at mount is acceptable (the overlay
    // opens in response to a user keystroke, not on the hot render path).
    let initial_model = futures::executor::block_on(harness.model());
    let initial_thinking = futures::executor::block_on(harness.thinking_level());
    let models = get_models(&initial_model.provider.to_string());
    let thinking = get_supported_thinking_levels(&initial_model);
    let rows = build_rows(&models, &thinking);
    let rows_rc = Rc::new(rows.clone());

    // Stable ordered keys. The cursor indexes into this Vec — never a HashMap.
    let keys: Vec<String> = rows.iter().map(row_label).collect();
    let total = keys.len();
    let selected = create_rw_signal(0usize);

    // ONE key handler, registered at mount (not per-render). It mutates the
    // `selected` signal; the TableView effect re-renders on change.
    let selected_for_key = selected;
    let harness_for_key = Arc::clone(&harness);
    let overlay_for_key = Rc::clone(&overlay_stack);
    let rows_for_key = Rc::clone(&rows_rc);
    on_key(move |key: KeyEvent| -> bool {
        let max = total.saturating_sub(1);
        match key.code {
            KeyCode::Down => {
                selected_for_key.set((selected_for_key.get() + 1).min(max));
                true
            }
            KeyCode::Up => {
                selected_for_key.set(selected_for_key.get().saturating_sub(1));
                true
            }
            KeyCode::Esc => {
                overlay_for_key.pop();
                true
            }
            KeyCode::Enter => {
                let idx = selected_for_key.get().min(max);
                let row = &rows_for_key[idx];
                apply_row(&harness_for_key, &overlay_for_key, row);
                true
            }
            _ => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    overlay_for_key.pop();
                    true
                } else {
                    false
                }
            }
        }
    });

    // The overlay's root: a column with a title and the TableView. The
    // OverlayBox wrapping + inset geometry is applied by the caller (the
    // RuntimeControl method that pushes onto OverlayStack).
    let root = Node::new_view();
    root.set_flex_direction(iodilos::taffy::prelude::FlexDirection::Column);
    root.set_flex_grow(1.0);
    root.set_padding_all(1.0);

    let title = Node::new_text();
    title.set_content(" /model — select a model or thinking level ".to_string());
    title.set_color(Color::Cyan);
    root.add_child(title);

    // Sections: models by provider, then thinking levels. Headers are styled
    // Nodes; rows are PickerRows addressed by their key.
    let mut sections: Vec<TableSection> = Vec::new();
    let mut by_provider: std::collections::BTreeMap<String, Vec<flown_ai::Model>> =
        std::collections::BTreeMap::new();
    for m in &models {
        by_provider
            .entry(m.provider.to_string())
            .or_default()
            .push(m.clone());
    }
    for (provider, provider_models) in by_provider {
        let header = Node::new_text();
        header.set_content(format!(" {provider} "));
        header.set_color(Color::DarkGray);
        sections.push(TableSection {
            header: Some(header),
            rows: provider_models
                .iter()
                .map(|m| TableRow::new(format!("{provider}/{}", m.id)))
                .collect(),
        });
    }
    if !thinking.is_empty() {
        let header = Node::new_text();
        header.set_content(" thinking ".to_string());
        header.set_color(Color::DarkGray);
        sections.push(TableSection {
            header: Some(header),
            rows: thinking
                .iter()
                .map(|t| TableRow::new(format!("thinking::{:?}", t).to_lowercase()))
                .collect(),
        });
    }
    let sections_signal = Signal::derive(move || sections.clone());
    let selected_read = Signal::derive({
        let sel = selected;
        move || sel.get()
    });

    let rows_for_cells = Rc::clone(&rows_rc);
    let cell_factory: CellFactory = Rc::new(move |ctx: &CellContext| {
        // The row key encodes provider/id or thinking::level; find the row.
        let row = rows_for_cells
            .iter()
            .find(|r| row_label(r) == ctx.key)
            .cloned();
        // "Current" = this row is the model the harness had at overlay-open
        // time. Compared by id/provider against the mount-time snapshot.
        let current_key = format!("{}/{}", initial_model.provider, initial_model.id);
        let is_current = match &row {
            Some(PickerRow::Model { model, .. }) => {
                models_are_equal(Some(model), Some(&initial_model))
                    || format!("{}/{}", model.provider, model.id) == current_key
            }
            Some(PickerRow::Thinking(t)) => *t == initial_thinking,
            None => false,
        };
        let text = Node::new_text();
        let prefix = if ctx.selected {
            "▶ "
        } else if is_current {
            "● "
        } else {
            "  "
        };
        let label = match &row {
            Some(r) => row_label(r),
            None => ctx.key.to_string(),
        };
        text.set_content(format!("{prefix}{label}"));
        text.set_color(if ctx.selected {
            Color::Yellow
        } else if is_current {
            Color::Green
        } else {
            Color::White
        });
        text
    });

    let mut props = TableViewProps::new(sections_signal, cell_factory, selected_read);
    props.max_visible = 16;
    let table = TableView::new(props);
    root.add_child(table);
    root
}

/// Apply a selection: set the model/thinking level on the (tokio) harness, then
/// close the overlay. The harness's async setter runs on tokio; we spawn it.
fn apply_row(harness: &Arc<AgentHarness>, overlay: &Rc<OverlayStack>, row: &PickerRow) {
    match row {
        PickerRow::Model { model, .. } => {
            let h = Arc::clone(harness);
            let model = model.clone();
            tokio::spawn(async move {
                h.set_model(model).await;
            });
            overlay.pop();
        }
        PickerRow::Thinking(level) => {
            let h = Arc::clone(harness);
            let level = level.clone();
            tokio::spawn(async move {
                h.set_thinking_level(level).await;
            });
            overlay.pop();
        }
    }
}
