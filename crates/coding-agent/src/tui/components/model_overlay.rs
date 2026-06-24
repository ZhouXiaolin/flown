//! `ModelOverlay` — the `/model` picker and its thinking-intensity follow-up.
//!
//! The model picker is a sectioned list of models from the configured
//! providers, grouped by provider, with a search input that filters by
//! id/name/provider. Selecting a non-reasoning model applies it directly.
//! Selecting a reasoning model pushes a second, narrower overlay (the
//! thinking-intensity picker) above the model picker: `Esc` on it returns to
//! the model picker; confirming a level applies both the model and the level
//! and drops the whole overlay stack back to the original view. The harness
//! emits `ModelUpdate`/`ThinkingLevelUpdate` so the prompt-box status line
//! updates.
//!
//! Both pickers are real `OverlayStack` layers, so the thinking picker floats
//! centered above the still-visible model picker (≈1/3 side margins vs the
//! model picker's 1/8).
//!
//! Pure helpers are split out so the filtering/sectioning logic is
//! unit-testable without a TUI.

use std::rc::Rc;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use flown_agent::AgentHarness;
use flown_ai::{ThinkingLevel, get_models, get_supported_thinking_levels};
use iodilos::prelude::*;

use crate::config::Config;
use crate::tui::overlay_stack::OverlayStack;

/// One selectable row in the model list: a model (with its provider). Thinking
/// intensity is chosen via a nested sub-popup after a model is picked, so it is
/// not a top-level row here.
#[derive(Clone)]
pub(crate) struct PickerRow {
    model: flown_ai::Model,
}

impl PartialEq for PickerRow {
    /// Identity is the `provider/id` key — the only thing `Tabled`'s keyed
    /// engine diffs on. `Model` itself has no `PartialEq`, but two rows with
    /// the same key refer to the same model, so comparing keys is sufficient.
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl PickerRow {
    fn provider(&self) -> String {
        self.model.provider.to_string()
    }
    /// Lowercased haystack used for case-insensitive filtering: the full
    /// `provider/id` plus the human name, so a search hits any of the three.
    fn haystack(&self) -> String {
        format!("{}/{} {}", self.provider(), self.model.id, self.model.name).to_lowercase()
    }
    fn label(&self) -> String {
        self.model.id.clone()
    }
    /// Stable identity for this row across filter changes — the value `Tabled`
    /// keys on and the value the `selected` signal carries.
    fn key(&self) -> String {
        format!("{}/{}", self.provider(), self.model.id)
    }
}

/// Build the flat, ordered list of every model across every provider, sorted
/// (provider asc, then id asc) so the section order and row order are stable
/// across filter changes.
#[cfg(test)]
pub(crate) fn build_all_rows() -> Vec<PickerRow> {
    build_rows_for_providers(flown_ai::get_providers())
}

pub(crate) fn build_configured_rows(config: &Config) -> Vec<PickerRow> {
    build_rows_for_providers(config.providers.keys().cloned())
}

fn build_rows_for_providers(providers: impl IntoIterator<Item = String>) -> Vec<PickerRow> {
    let mut rows = Vec::new();
    for provider in providers {
        for model in get_models(&provider) {
            rows.push(PickerRow { model });
        }
    }
    rows.sort_by(|a, b| {
        a.provider()
            .cmp(&b.provider())
            .then_with(|| a.model.id.cmp(&b.model.id))
    });
    rows
}

/// Filter `rows` by a case-insensitive query against each row's haystack,
/// returning owned clones of the matching rows. Owned (not borrowed) so the
/// filtered snapshot can live in a signal without lifetime gymnastics.
pub(crate) fn filter_rows(rows: &[PickerRow], query: &str) -> Vec<PickerRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        rows.to_vec()
    } else {
        rows.iter()
            .filter(|r| r.haystack().contains(&q))
            .cloned()
            .collect()
    }
}

/// Group filtered rows into `TableSection`s keyed by provider, preserving the
/// sorted order from `build_all_rows`. Each row's identity (the `Tabled` key) is
/// its `provider/id`, so a selected highlight stays glued to its model across
/// filter changes.
pub(crate) fn build_sections(filtered: &[PickerRow]) -> Vec<TableSection<PickerRow>> {
    let mut sections: Vec<TableSection<PickerRow>> = Vec::new();
    let mut current_provider: Option<String> = None;
    for row in filtered {
        let provider = row.provider();
        if Some(&provider) != current_provider.as_ref() {
            sections.push(TableSection::new(Vec::new()).with_title(provider.clone()));
            current_provider = Some(provider);
        }
        let section = sections.last_mut().expect("section just pushed");
        // Only the model id is shown — the human name was previously appended as
        // a dim description, but that cluttered each row. The name still feeds
        // the search haystack so it remains findable by typing.
        section.items.push(row.clone());
    }
    sections
}

pub struct ModelOverlayParts {
    pub content: Rc<dyn Fn() -> View>,
    pub on_key: Rc<dyn Fn(KeyEvent) -> bool>,
}

/// Build the `/model` overlay content and key handler.
///
/// State is created here (in the command-pump task that pushes the overlay) but
/// the content is only *rendered* later, under App's mount owner — so reading
/// the signals inside the view re-runs the view, not the build.
pub fn model_overlay_parts(
    harness: Arc<AgentHarness>,
    overlay_stack: Rc<OverlayStack>,
    config: Config,
) -> ModelOverlayParts {
    let initial_model = futures::executor::block_on(harness.model());
    let _initial_thinking = futures::executor::block_on(harness.thinking_level());
    let rows = Rc::new(build_configured_rows(&config));

    let query = create_signal(String::new());
    // Navigation is index-based (Up/Down/Enter all reason about a flat index
    // into the filtered list), so the internal selection state stays a `usize`.
    // `Tabled` keys its selection by identity, though, so a derived memo
    // (`selected_key`) resolves the index to its `provider/id` for the view.
    let selected_idx = create_signal(0usize);

    // Filtered view of `rows`, memoized over the query. Reading it inside the
    // view re-renders only when the query changes.
    let rows_rc = Rc::clone(&rows);
    let query_for_filter = query;
    let filtered = create_memo(move || {
        let q = query_for_filter.get_clone();
        filter_rows(&rows_rc, &q)
    });

    // Resolve the flat selection index to the selected row's key, clamping to
    // the (possibly just-shrunk) filtered list. `Tabled` consumes this.
    let filtered_for_key = filtered;
    let selected_idx_for_key = selected_idx;
    let selected_key: ReadSignal<Option<String>> = create_memo(move || {
        let rows = filtered_for_key.get_clone();
        let idx = selected_idx_for_key.get();
        rows.get(idx).map(|r| r.key())
    });

    let on_key: Rc<dyn Fn(KeyEvent) -> bool> = Rc::new({
        let harness = Arc::clone(&harness);
        let overlay_stack = Rc::clone(&overlay_stack);
        move |key: KeyEvent| -> bool {
            let ctrl_c =
                key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
            if ctrl_c {
                overlay_stack.pop();
                return true;
            }

            // Printable / editing keys drive the search query.
            if let Some(ch) = printable_char(&key) {
                query.update(|q| q.push(ch));
                selected_idx.set(0);
                return true;
            }
            match key.code {
                KeyCode::Backspace => {
                    query.update(|q| {
                        q.pop();
                    });
                    selected_idx.set(0);
                    true
                }
                KeyCode::Down => {
                    let max = filtered.get_clone().len().saturating_sub(1);
                    selected_idx.set((selected_idx.get() + 1).min(max));
                    true
                }
                KeyCode::Up => {
                    selected_idx.set(selected_idx.get().saturating_sub(1));
                    true
                }
                KeyCode::Esc => {
                    // Close this model picker, returning to the original view.
                    overlay_stack.pop();
                    true
                }
                KeyCode::Enter => {
                    let filtered = filtered.get_clone();
                    let idx = selected_idx.get();
                    let Some(row) = filtered.get(idx) else {
                        return true;
                    };
                    let levels = get_supported_thinking_levels(&row.model);
                    if levels.len() <= 1 {
                        // Non-reasoning (or only Off): apply immediately and
                        // drop the picker back to the original view.
                        apply_model(
                            &harness,
                            &overlay_stack,
                            &row.model,
                            levels.first().cloned(),
                        );
                    } else {
                        // Reasoning: push a thinking-intensity overlay above
                        // this one. Esc on it returns here; Enter applies the
                        // model + level and drops both overlays.
                        push_thinking_overlay(
                            Arc::clone(&harness),
                            Rc::clone(&overlay_stack),
                            row.model.clone(),
                            levels,
                        );
                    }
                    true
                }
                _ => false,
            }
        }
    });

    let content: Rc<dyn Fn() -> View> = Rc::new({
        let selected = selected_key;
        let query = *query;
        let initial_model = initial_model.clone();
        move || model_overlay_view(filtered, selected, query, initial_model.clone())
    });

    ModelOverlayParts { content, on_key }
}

/// Extract a printable char from a key event (letters/digits/punctuation,
/// ignoring Ctrl/Alt/Shift-as-modifier combos). `None` for non-text keys.
fn printable_char(key: &KeyEvent) -> Option<char> {
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Char(ch) => Some(ch),
        _ => None,
    }
}

/// Handle a key while the thinking-intensity sub-popup is open. The model was
/// captured when the sub-popup opened; confirming a level applies both the
/// model and the level, then closes the overlay.
/// Apply a model (and, if given, a thinking level) then drop every overlay —
/// confirming a selection returns straight to the original view, updating the
/// status line via the harness's emitted events.
fn apply_model(
    harness: &Arc<AgentHarness>,
    overlay_stack: &Rc<OverlayStack>,
    model: &flown_ai::Model,
    level: Option<ThinkingLevel>,
) {
    let h = Arc::clone(harness);
    let model = model.clone();
    tokio::spawn(async move {
        h.set_model(model).await;
        if let Some(level) = level {
            h.set_thinking_level(level).await;
        }
    });
    overlay_stack.pop_all();
}

/// Push a thinking-intensity overlay above the model picker. `Esc` on it pops
/// just this layer (returning to the model picker); `Enter` applies the model
/// and chosen level and pops the whole stack back to the original view.
fn push_thinking_overlay(
    harness: Arc<AgentHarness>,
    overlay_stack: Rc<OverlayStack>,
    model: flown_ai::Model,
    levels: Vec<ThinkingLevel>,
) {
    let parts = thinking_overlay_parts(harness, Rc::clone(&overlay_stack), model, levels);
    let overlay = crate::tui::overlay_stack::ActiveOverlay {
        // Narrower than the model picker (1/3 inset each side) so it floats
        // centered above it.
        geometry: crate::tui::overlay_stack::OverlayGeometry::Inset { ratio: 0.33 },
        dismissible: true,
        route_app_keys: false,
        content: parts.content,
        on_key: Some(parts.on_key),
        on_close: None,
    };
    overlay_stack.push(overlay);
}

/// Build the thinking-intensity overlay content and key handler.
fn thinking_overlay_parts(
    harness: Arc<AgentHarness>,
    overlay_stack: Rc<OverlayStack>,
    model: flown_ai::Model,
    levels: Vec<ThinkingLevel>,
) -> ModelOverlayParts {
    // Navigation is index-based; the view receives a keyed selection memo.
    let selected_idx = create_signal(0usize);
    let levels = Rc::new(levels);
    let model = Rc::new(model);

    // Resolve the flat index to its `ThinkingLevel` so `Tabled` can key on it.
    let levels_for_key = Rc::clone(&levels);
    let selected_idx_for_key = selected_idx;
    let selected_key: ReadSignal<Option<ThinkingLevel>> = create_memo(move || {
        levels_for_key.get(selected_idx_for_key.get()).cloned()
    });

    let on_key: Rc<dyn Fn(KeyEvent) -> bool> = Rc::new({
        let harness = Arc::clone(&harness);
        let overlay_stack = Rc::clone(&overlay_stack);
        let levels = Rc::clone(&levels);
        let model = Rc::clone(&model);
        move |key: KeyEvent| -> bool {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                overlay_stack.pop();
                return true;
            }
            let max = levels.len().saturating_sub(1);
            match key.code {
                KeyCode::Down => {
                    selected_idx.set((selected_idx.get() + 1).min(max));
                    true
                }
                KeyCode::Up => {
                    selected_idx.set(selected_idx.get().saturating_sub(1));
                    true
                }
                KeyCode::Esc => {
                    // Drop just this layer; the model picker beneath is still
                    // on the stack and takes back the keys.
                    overlay_stack.pop();
                    true
                }
                KeyCode::Enter => {
                    let level = levels.get(selected_idx.get()).cloned();
                    apply_model(&harness, &overlay_stack, &model, level);
                    true
                }
                _ => false,
            }
        }
    });

    let content: Rc<dyn Fn() -> View> = Rc::new({
        let selected = selected_key;
        let levels = Rc::clone(&levels);
        move || thinking_overlay_view(selected, &levels)
    });

    ModelOverlayParts { content, on_key }
}

fn model_overlay_view(
    filtered: ReadSignal<Vec<PickerRow>>,
    selected: ReadSignal<Option<String>>,
    query: ReadSignal<String>,
    initial_model: flown_ai::Model,
) -> View {
    // The filtered rows, memoized over the query. Reading it inside the view
    // re-renders only when the query changes.
    let sections = create_memo(move || {
        let filtered = filtered.get_clone();
        build_sections(&filtered)
    });

    // Each row's view: the selected (▶) and current/active (◆) markers.
    // `is_selected` is a per-row memo from `Tabled`, so the highlight moves at
    // attribute level when the selection changes. The current-model marker is
    // static per row (derived from `initial_model`), so it is computed once.
    let initial_model_for_cell = initial_model.clone();
    let list = view! {
        div(flex_direction = FlexDirection::Column, flex_grow = 1.0_f32) {
            Tabled(
                sections = sections,
                selected = selected,
                // The overlay is inset 1/8 (≈3/4 of the screen), so a tall
                // terminal comfortably holds the full provider+model list.
                // Anything beyond the window scrolls via Up/Down.
                max_visible = 32,
                key = |row: &PickerRow| row.key(),
                view = move |row: FlatRow<PickerRow, String>| match row {
                    FlatRow::Header { title } => View::from(
                        tags::p().color(Color::DarkGrey).children(format!(" {title}")),
                    ),
                    FlatRow::Body { item, is_selected, .. } => {
                        let is_current = item.model.id == initial_model_for_cell.id
                            && item.model.provider == initial_model_for_cell.provider;
                        // Selection marker shows for the highlighted (▶) row; the
                        // current/active model is marked with ◆ so it stays
                        // distinguishable even when it is also the selected row.
                        let prefix = if is_current { "◆ " } else { "  " };
                        let label = item.label();
                        View::from(
                            tags::div()
                                .background_color(move || if is_selected.get() {
                                    Color::Yellow
                                } else {
                                    Color::Reset
                                })
                                .children(tags::p().color(move || if is_selected.get() {
                                    Color::Black
                                } else if is_current {
                                    Color::Green
                                } else {
                                    Color::White
                                }).children(move || {
                                    if is_selected.get() {
                                        format!("▶ {}", label)
                                    } else {
                                        format!("{prefix}{label}")
                                    }
                                })),
                        )
                    }
                },
            )
        }
    };

    let search = View::from_dynamic(move || {
        let query_display = query.get_clone();
        // A single bordered input field: a one-cell magnifier glyph sits
        // flush against the live query, with a trailing caret. No extra
        // padding around the glyph keeps it one column wide. The field is
        // pinned to one content row and the full overlay width so it reads
        // as a search bar rather than a wrapped label.
        tags::div()
            .width(iodilos::Size::Percent(100.0))
            // Border (1) + one text row (1) + border (1) = 3 rows: a single
            // search bar line, full overlay width, that does not grow/shrink.
            .height(iodilos::Size::Length(3))
            .flex_shrink(0.0)
            .border_style(BorderStyle::Round)
            .border_color(Color::DarkGrey)
            .flex_direction(FlexDirection::Row)
            .children((
                tags::span().color(Color::Cyan).children("🔍"),
                tags::span()
                    .color(Color::White)
                    .children(format!(" {query_display}")),
                tags::span().color(Color::DarkGrey).children("_"),
            ))
    });

    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .flex_grow(1.0_f32)
            .padding(1)
            .children((search, list)),
    )
}

/// Render the thinking-intensity overlay body as a single-section table — the
/// same vertical list component the model picker uses, so it looks and behaves
/// identically (one level per row, ▶ arrow + color on the selected row, Up/Down
/// to move). The levels come straight from the model's supported set (e.g.
/// `off` … `xhigh`).
fn thinking_overlay_view(
    selected: ReadSignal<Option<ThinkingLevel>>,
    levels: &[ThinkingLevel],
) -> View {
    // Capture the static level list by value so the memo is `'static`; the
    // memo gives the `ReadSignal<Vec<TableSection<_>>>` `Tabled` expects.
    let levels = levels.to_vec();
    let sections = create_memo(move || {
        vec![TableSection::new(levels.clone()).with_title("thinking intensity")]
    });

    View::from(
        tags::div()
            .flex_direction(FlexDirection::Column)
            .flex_grow(1.0_f32)
            .padding(1)
            .children(view! {
                Tabled(
                    sections = sections,
                    selected = selected,
                    max_visible = 16,
                    key = |level: &ThinkingLevel| level.clone(),
                    view = |row: FlatRow<ThinkingLevel, ThinkingLevel>| match row {
                        FlatRow::Header { title } => View::from(
                            tags::p().color(Color::DarkGrey).children(format!(" {title}")),
                        ),
                        FlatRow::Body { item, is_selected, .. } => {
                            let label = format!("{item:?}").to_lowercase();
                            View::from(
                                tags::div()
                                    .background_color(move || if is_selected.get() {
                                        Color::Yellow
                                    } else {
                                        Color::Reset
                                    })
                                    .children(tags::p().color(move || if is_selected.get() {
                                        Color::Black
                                    } else {
                                        Color::White
                                    }).children(move || {
                                        if is_selected.get() {
                                            format!("▶ {label}")
                                        } else {
                                            format!("  {label}")
                                        }
                                    })),
                            )
                        }
                    },
                )
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_all_rows_is_grouped_and_sorted() {
        let rows = build_all_rows();
        // Every registry provider contributes at least one model in the test
        // environment (the built-in generated table is non-empty).
        assert!(!rows.is_empty());
        // Sorted by (provider, id): adjacent same-provider rows are id-sorted.
        for window in rows.windows(2) {
            let a = &window[0];
            let b = &window[1];
            let ord = a
                .provider()
                .cmp(&b.provider())
                .then_with(|| a.model.id.cmp(&b.model.id));
            assert!(
                ord != std::cmp::Ordering::Greater,
                "rows not sorted: {} > {}",
                a.provider(),
                b.provider()
            );
        }
    }

    #[test]
    fn configured_rows_only_include_configured_providers() {
        let mut config = Config::default();
        config.providers.insert(
            "deepseek".to_string(),
            crate::config::ProviderConfig {
                provider_type: "deepseek".to_string(),
                key: "test".to_string(),
            },
        );

        let rows = build_configured_rows(&config);

        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row.provider() == "deepseek"));
    }

    #[test]
    fn configured_rows_are_empty_without_configured_providers() {
        let config = Config::default();

        let rows = build_configured_rows(&config);

        assert!(rows.is_empty());
    }

    #[test]
    fn filter_rows_matches_id_name_or_provider() {
        let rows = build_all_rows();
        // Filter by a fragment that appears in some model id/name/provider.
        let any_id = rows.first().map(|r| r.model.id.clone()).unwrap_or_default();
        let lower = any_id.to_lowercase();
        let filtered = filter_rows(&rows, &lower);
        assert!(
            filtered.iter().any(|r| r.model.id == any_id),
            "filtering by an existing id should keep that row"
        );
    }

    #[test]
    fn empty_query_keeps_all_rows() {
        let rows = build_all_rows();
        assert_eq!(filter_rows(&rows, "").len(), rows.len());
        assert_eq!(filter_rows(&rows, "   ").len(), rows.len());
    }

    #[test]
    fn build_sections_groups_by_provider() {
        let rows = build_all_rows();
        let filtered: Vec<PickerRow> = rows.iter().take(5).cloned().collect();
        let sections = build_sections(&filtered);
        // Section titles are unique provider names, in first-seen order.
        let mut titles = sections.iter().filter_map(|s| s.title.as_deref());
        let first = titles.next();
        assert!(first.is_some());
        // No duplicate titles.
        let mut seen = std::collections::HashSet::new();
        for s in &sections {
            if let Some(t) = &s.title {
                assert!(seen.insert(t.clone()), "duplicate section title {t}");
            }
        }
    }

    #[test]
    fn printable_char_filters_out_control_combos() {
        let mk = |code, mods| KeyEvent::new(code, mods);
        assert_eq!(
            printable_char(&mk(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some('a')
        );
        assert_eq!(
            printable_char(&mk(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            printable_char(&mk(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
    }
}
