# `/model` Command + Generic TableView/Overlay — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/model` slash command that opens a centered floating overlay (1/8 margins) for choosing a model + thinking intensity, driven by two new generic iodilos UI primitives (`TableView`, `OverlayBox`).

**Architecture:** Two new pure-UI primitives live in the **iodilos** submodule (cell-based `TableView` with section/row/cursor/centered-scroll; `OverlayBox` with parameterized geometry `FullBleed`/`Inset`). On the **flown** side, an `OverlayStack` tracks the active overlay, and `model.rs` owns all model-specific logic in a single `ModelOverlay` component (no strategy trait). btw migrates to `OverlayBox(FullBleed)` + an extracted `fork_conversation` capability. `CompletionMenu` is rebuilt as a `TableView` assembly.

**Tech Stack:** Rust, iodilos (SolidJS-style reactive TUI on taffy/ratatui), ratatui, tokio, flume. Two repos: `vendor/iodilos` (git submodule) and `crates/` (flown). flown depends on iodilos via a relative path (`crates/coding-agent/Cargo.toml:18`), so submodule edits take effect locally without publishing.

**Spec:** `docs/superpowers/specs/2026-06-19-model-command-design.md`

**Conventions:** This codebase writes comments and prose in English; the spec uses Chinese for human-facing prose but **all code, identifiers, and commit messages stay English**. Match the surrounding code's comment density and naming. Run tests from the repo root unless a task says otherwise.

---

## File Structure

### iodilos (`vendor/iodilos/crates/iodilos/src/`)

| File | Responsibility |
|---|---|
| **new** `components/table_view.rs` | `TableView` component: `TableSection`/`TableRow`/`CellContext`/`CellFactory`/`TableViewProps`; renders header Nodes + per-row cells via factory; global cursor → (section,row); centered viewport scroll. Pure UI. |
| **new** `components/overlay_box.rs` | `OverlayBox` component + `OverlayGeometry` enum (`FullBleed`/`Inset{ratio}`); `position_absolute` + inset + background-clear. Pure UI. |
| `components/view.rs` | Add `set_inset_top_percent`/`_bottom_percent`/`_left_percent`/`_right_percent` (percent variants, mirroring existing `set_width_percent`). |
| `components/completion_menu.rs` | Rebuild `CompletionMenu` as a `TableView` assembly (single section, no header, fixed cell factory). Keeps the public `CompletionMenu`/`CompletionMenuProps`/`CompletionItem` API so `editor.rs` is unchanged. |
| `lib.rs` + `prelude` | Export `TableView`/`OverlayBox` + their types. |
| **new** `examples/table_view.rs` | Manual-verification demo (sectioned list + cursor). |

### flown (`crates/coding-agent/src/`)

| File | Responsibility |
|---|---|
| **new** `tui/overlay_stack.rs` | `OverlayStack` (tracks 0/1 active overlay) + `ActiveOverlay`. iodilos context value. |
| **new** `core/extensions/model.rs` | `ModelExtension` (registers `/model`) + `ModelOverlay` component (all model logic: phase state machine, sections, cell factories, key handling, apply). |
| `core/extensions/types.rs` | Add `RuntimeCommand::OpenModelOverlay` + `::ForkConversation`; `ConversationCapability::open_model_overlay`/`fork_conversation`. |
| `core/extensions/mod.rs` | Add `ModelExtension` to `build_runner`. |
| `tui/conversation.rs` | Extract `fork_conversation` from `open_overlap`; add `RuntimeControl::open_model_overlay` (constructs `ModelOverlay`, pushes onto `OverlayStack`). |
| `tui/runtime.rs` | Build `OverlayStack` at mount; handle `OpenModelOverlay`/`ForkConversation` in the command pump; add `ModelUpdate`/`ThinkingLevelUpdate` branches to `translate_event`. |
| `tui/components/app.rs` | Render main layout + conditionally render top `OverlayBox` via `Show`; Ctrl+C closes overlay when one is active. |
| `core/extensions/btw.rs` | Switch to `OverlayBox(FullBleed)` + `fork_conversation`. |

**Build/test commands:**
- iodilos: `cargo test -p iodilos` (from `vendor/iodilos/`)
- flown: `cargo test -p coding-agent` (from repo root)
- Whole workspace: `cargo build` (from repo root) — compiles both because of the path dep.

**Task ordering rationale:** iodilos primitives first (they're leaf dependencies with no flown coupling), then flown's `OverlayStack` + `translate_event` wiring, then `ModelOverlay` (the feature), then `CompletionMenu`/btw migration (refactors that ride on the new primitives).

---

## Task 1: Add percent inset setters to iodilos `View`

`OverlayBox` needs percent insets for the 1/8 margin. iodilos `View` has `set_inset_top` (length) but no percent variant.

**Files:**
- Modify: `vendor/iodilos/crates/iodilos/src/components/view.rs` (after `set_inset_right`, ~line 422)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `components/view.rs`:

```rust
#[test]
fn inset_percent_setters_write_percent_lengths() {
    let node = Node::new_view();
    node.set_inset_top_percent(12.5);
    node.set_inset_left_percent(12.5);
    node.set_inset_bottom_percent(12.5);
    node.set_inset_right_percent(12.5);
    let style = node.taffy_style();
    // percent(p) stores p/100 as a LengthPercentage::Percent.
    // 12.5% -> 0.125 ratio in the taffy LengthPercentage.
    match &style.inset.top {
        taffy::style::LengthPercentageAuto::Percent(p) => assert!(
            (*p - 0.125).abs() < 1e-6,
            "top inset should be 12.5% (0.125), got {p}"
        ),
        other => panic!("expected Percent, got {other:?}"),
    }
    match &style.inset.left {
        taffy::style::LengthPercentageAuto::Percent(p) => assert!(
            (*p - 0.125).abs() < 1e-6,
            "left inset should be 12.5% (0.125), got {p}"
        ),
        other => panic!("expected Percent, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p iodilos inset_percent_setters_write_percent_lengths`
Expected: FAIL — methods `set_inset_top_percent` etc. don't exist (compile error).

- [ ] **Step 3: Add the four setters**

In `components/view.rs`, immediately after the existing `set_inset_right` (around line 422), add:

```rust
pub fn set_inset_top_percent(&self, p: f32) {
    self.with_view(|props| {
        props.taffy_style.inset.top = percent(p / 100.0);
    });
}

pub fn set_inset_bottom_percent(&self, p: f32) {
    self.with_view(|props| {
        props.taffy_style.inset.bottom = percent(p / 100.0);
    });
}

pub fn set_inset_left_percent(&self, p: f32) {
    self.with_view(|props| {
        props.taffy_style.inset.left = percent(p / 100.0);
    });
}

pub fn set_inset_right_percent(&self, p: f32) {
    self.with_view(|props| {
        props.taffy_style.inset.right = percent(p / 100.0);
    });
}
```

(`percent` is already imported in `view.rs` — it's used by `set_width_percent`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p iodilos inset_percent_setters_write_percent_lengths`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd vendor/iodilos
git add crates/iodilos/src/components/view.rs
git commit -m "feat(view): add percent variants for inset setters"
```

---

## Task 2: Implement `TableView` component (iodilos)

Cell-based, pure-UI list: sections with header Nodes + rows keyed by identity; a `CellFactory` renders each row; a global `selected` cursor drives highlight + centered viewport scroll.

**Files:**
- Create: `vendor/iodilos/crates/iodilos/src/components/table_view.rs`
- Modify: `vendor/iodilos/crates/iodilos/src/components/mod.rs` (add `pub mod table_view;`)
- Modify: `vendor/iodilos/crates/iodilos/src/lib.rs` + `prelude` (export)
- Test: unit tests inside `table_view.rs`

- [ ] **Step 1: Create the module + export stub**

Create `vendor/iodilos/crates/iodilos/src/components/table_view.rs` with just the public types and a `todo!()` `TableView::new`, then wire it in.

```rust
//! `TableView` — a cell-based, sectioned, selectable list (ADR: tableView
//! primitive). Pure UI: it owns section structure, a global selection cursor,
//! and centered viewport scrolling. It does NOT handle keys, filtering, or
//! confirm semantics — callers drive `selected` and supply a `CellFactory`.
//!
//! Modeled on iOS `UITableView`'s data-source + selectedIndexPath layer: the
//! cell factory is the `cellForRowAt` equivalent. There is no cell-reuse pool
//! (terminal viewports show at most a few dozen rows; Node is cheap to build).

use std::rc::Rc;

use ratatui::style::Style;
use ratatui::widgets::Borders;

use crate::reactive::{create_effect, RwSignal, Signal};
use crate::{BorderStyle, Node};

/// One row's identity. Carries only a stable key — no rendering fields.
/// Callers hold their own data and look it up by `key` inside the cell factory.
#[derive(Clone, Debug)]
pub struct TableRow {
    pub key: String,
}

impl TableRow {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

/// A section: an optional header Node (style fully caller-controlled) plus rows.
pub struct TableSection {
    pub header: Option<Node>,
    pub rows: Vec<TableRow>,
}

/// Context handed to a cell factory for one visible row.
pub struct CellContext<'a> {
    pub key: &'a str,
    pub section_idx: usize,
    pub row_idx: usize,
    /// true when this row is the current selection; the cell decides how to
    /// highlight (background, prefix arrow, etc.).
    pub selected: bool,
}

/// `cellForRowAt` equivalent: returns the Node for one visible row. Called once
/// per visible row per render; no caching (terminal scale is small).
pub type CellFactory = Rc<dyn Fn(&CellContext) -> Node>;

pub struct TableViewProps {
    pub sections: Signal<Vec<TableSection>>,
    pub cell_factory: CellFactory,
    /// Global cursor over the flattened rows-only sequence (headers skipped).
    pub selected: Signal<usize>,
    /// Max visible rows; the viewport centers on the cursor.
    pub max_visible: usize,
    pub border: Borders,
    pub border_style: BorderStyle,
    pub border_color: ratatui::style::Color,
}

impl TableViewProps {
    pub fn new(sections: Signal<Vec<TableSection>>, cell_factory: CellFactory, selected: Signal<usize>) -> Self {
        Self {
            sections,
            cell_factory,
            selected,
            max_visible: 10,
            border: Borders::NONE,
            border_style: BorderStyle::None,
            border_color: ratatui::style::Color::Reset,
        }
    }
}

pub struct TableView;

#[allow(clippy::new_ret_no_self)]
impl TableView {
    pub fn new(props: TableViewProps) -> Node {
        todo!("Task 2 Step 3")
    }
}
```

Add to `components/mod.rs`:
```rust
pub mod table_view;
```

In `lib.rs`, add to the top-level `pub use` block (near the `for_list` export):
```rust
pub use crate::components::table_view::{
    CellContext, CellFactory, TableSection, TableRow, TableView, TableViewProps,
};
```
And in `prelude`, add the same line.

- [ ] **Step 2: Write the failing tests (cursor mapping + viewport slice)**

The pure logic (flatten → (section,row) map; viewport slice) is the load-bearing part. Extract it into testable free functions. Add these to the `tests` module at the bottom of `table_view.rs` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<TableRow> {
        (0..n).map(|i| TableRow::new(format!("k{i}"))).collect()
    }

    /// Flatten sections into (section_idx, row_idx, key) over ROWS ONLY.
    fn flatten(sections: &[TableSection]) -> Vec<(usize, usize, String)> {
        sections
            .iter()
            .enumerate()
            .flat_map(|(s_idx, sec)| {
                sec.rows.iter().enumerate().map(move |(r_idx, row)| {
                    (s_idx, r_idx, row.key.clone())
                })
            })
            .collect()
    }

    /// Centered viewport [start, end) over a flattened length, clamped to bounds.
    fn viewport(flat_len: usize, selected: usize, max_visible: usize) -> (usize, usize) {
        if flat_len == 0 {
            return (0, 0);
        }
        let max_visible = max_visible.max(1);
        let start = (selected as isize - (max_visible as isize) / 2)
            .max(0)
            .min((flat_len as isize) - (max_visible as isize))
            .max(0) as usize;
        let end = (start + max_visible).min(flat_len);
        (start, end)
    }

    #[test]
    fn flatten_skips_headers_and_counts_rows_only() {
        let sections = vec![
            TableSection { header: None, rows: rows(2) },
            TableSection { header: None, rows: rows(1) },
        ];
        let flat = flatten(&sections);
        assert_eq!(
            flat,
            vec![
                (0, 0, "k0".to_string()),
                (0, 1, "k1".to_string()),
                (1, 0, "k0".to_string()),
            ]
        );
    }

    #[test]
    fn viewport_centers_cursor_at_middle() {
        // 10 rows, cursor 5, max_visible 4 -> start 3, end 7
        assert_eq!(viewport(10, 5, 4), (3, 7));
    }

    #[test]
    fn viewport_clamps_to_top() {
        // cursor near top does not go negative
        assert_eq!(viewport(10, 0, 4), (0, 4));
        assert_eq!(viewport(10, 1, 4), (0, 4));
    }

    #[test]
    fn viewport_clamps_to_bottom() {
        // cursor near bottom does not overflow
        assert_eq!(viewport(10, 9, 4), (6, 10));
        assert_eq!(viewport(10, 8, 4), (6, 10));
    }

    #[test]
    fn viewport_handles_short_list() {
        // fewer rows than max_visible shows all
        assert_eq!(viewport(2, 0, 10), (0, 2));
        assert_eq!(viewport(2, 1, 10), (0, 2));
    }

    #[test]
    fn viewport_empty_is_zero_zero() {
        assert_eq!(viewport(0, 0, 10), (0, 0));
    }
}
```

Note: `flatten` and `viewport` are currently test-local. In Step 3 you'll promote them to module-private free functions used by `TableView::new` and referenced unqualified by the tests (move them above the `tests` mod).

- [ ] **Step 3: Run tests to verify they pass (they're pure functions)**

Run: `cargo test -p iodilos table_view::tests`
Expected: PASS (the test-local `flatten`/`viewport` compile and behave correctly). This locks the logic before wiring it into `TableView::new`.

- [ ] **Step 4: Implement `TableView::new`**

Replace the `todo!()` body. Promote `flatten` and `viewport` to module-level `fn`s (move them out of `tests`), then use them:

```rust
/// Flatten sections into (section_idx, row_idx, key) over ROWS ONLY.
fn flatten(sections: &[TableSection]) -> Vec<(usize, usize, String)> {
    sections
        .iter()
        .enumerate()
        .flat_map(|(s_idx, sec)| {
            sec.rows
                .iter()
                .enumerate()
                .map(move |(r_idx, row)| (s_idx, r_idx, row.key.clone()))
        })
        .collect()
}

/// Centered viewport [start, end) over a flattened length, clamped to bounds.
fn viewport(flat_len: usize, selected: usize, max_visible: usize) -> (usize, usize) {
    if flat_len == 0 {
        return (0, 0);
    }
    let max_visible = max_visible.max(1);
    let start = (selected as isize - (max_visible as isize) / 2)
        .max(0)
        .min((flat_len as isize) - (max_visible as isize))
        .max(0) as usize;
    let end = (start + max_visible).min(flat_len);
    (start, end)
}

#[allow(clippy::new_ret_no_self)]
impl TableView {
    pub fn new(props: TableViewProps) -> Node {
        let root = Node::new_view();
        root.set_flex_direction(taffy::prelude::FlexDirection::Column);
        root.set_flex_grow(1.0);
        root.set_flex_shrink(1.0);
        root.set_min_height(0.0);
        root.set_border(props.border);
        root.set_border_style(props.border_style);
        root.set_border_color(props.border_color);
        root.set_padding_left(1.0);
        root.set_padding_right(1.0);

        let sections = props.sections;
        let cell_factory = props.cell_factory;
        let selected = props.selected;
        let max_visible = props.max_visible;
        let root_for_effect = root.clone();

        create_effect(move || {
            let secs = sections.get();
            let cur = selected.get();
            let flat = flatten(&secs);
            let (start, end) = viewport(flat.len(), cur, max_visible);

            // Build a lookup: for each visible flat index, which (section,row)
            // it maps to. We walk sections in order, emitting headers and rows,
            // but only call the cell factory for rows inside [start, end).
            let mut flat_seen = 0usize;
            let mut children: Vec<Node> = Vec::new();
            for (s_idx, sec) in secs.iter().enumerate() {
                // Emit header only if the section has at least one visible row.
                let sec_has_visible_row = sec.rows.iter().any(|_| {
                    let in_window = flat_seen >= start && flat_seen < end;
                    flat_seen += 1;
                    in_window
                });
                // Reset flat_seen accounting: redo cleanly below. (The closure
                // above mutated flat_seen during the any() scan; to keep this
                // readable we instead recompute visibility per row directly.)
                let _ = sec_has_visible_row;
                if let Some(header) = &sec.header {
                    children.push(header.clone());
                }
                for (r_idx, row) in sec.rows.iter().enumerate() {
                    let flat_idx = (0..flat.len())
                        .position(|i| flat[i] == (s_idx, r_idx, row.key.clone()))
                        .unwrap_or(usize::MAX);
                    if flat_idx >= start && flat_idx < end {
                        let ctx = CellContext {
                            key: &row.key,
                            section_idx: s_idx,
                            row_idx: r_idx,
                            selected: flat_idx == cur,
                        };
                        children.push(cell_factory(&ctx));
                    }
                }
            }
            root_for_effect.set_children(children);
        });

        root
    }
}
```

Note: the `flat_idx` lookup recomputation is deliberately simple (list sizes are small). The `sec_has_visible_row` block above is dead scaffolding — **remove it** in your actual implementation and just always emit the header when present. (Simplified final body shown below; use this cleaner version instead of the scaffolding above.)

**Cleaner `create_effect` body to actually write:**

```rust
        create_effect(move || {
            let secs = sections.get();
            let cur = selected.get();
            let flat = flatten(&secs);
            let (start, end) = viewport(flat.len(), cur, max_visible);

            let mut children: Vec<Node> = Vec::new();
            for (s_idx, sec) in secs.iter().enumerate() {
                if let Some(header) = &sec.header {
                    children.push(header.clone());
                }
                for (r_idx, row) in sec.rows.iter().enumerate() {
                    let flat_idx = (0..flat.len())
                        .position(|i| flat[i] == (s_idx, r_idx, row.key.clone()))
                        .unwrap_or(usize::MAX);
                    if flat_idx >= start && flat_idx < end {
                        let ctx = CellContext {
                            key: &row.key,
                            section_idx: s_idx,
                            row_idx: r_idx,
                            selected: flat_idx == cur,
                        };
                        children.push(cell_factory(&ctx));
                    }
                }
            }
            root_for_effect.set_children(children);
        });
```

- [ ] **Step 5: Run tests to verify they still pass**

Run: `cargo test -p iodilos table_view`
Expected: PASS (the promoted `flatten`/`viewport` are now module-level; tests reference them via `use super::*`).

- [ ] **Step 6: Verify it compiles in the workspace**

Run (from repo root): `cargo build -p coding-agent`
Expected: builds (flown doesn't use TableView yet, but the path-dep must stay green).

- [ ] **Step 7: Commit**

```bash
cd vendor/iodilos
git add crates/iodilos/src/components/table_view.rs crates/iodilos/src/components/mod.rs crates/iodilos/src/lib.rs
git commit -m "feat(components): add cell-based TableView primitive"
```

---

## Task 3: Implement `OverlayBox` component (iodilos)

A floating container: `position_absolute` + geometry-driven insets + background clear + border + one content slot.

**Files:**
- Create: `vendor/iodilos/crates/iodilos/src/components/overlay_box.rs`
- Modify: `vendor/iodilos/crates/iodilos/src/components/mod.rs` (add `pub mod overlay_box;`)
- Modify: `vendor/iodilos/crates/iodilos/src/lib.rs` + `prelude` (export)

- [ ] **Step 1: Create the module**

Create `vendor/iodilos/crates/iodilos/src/components/overlay_box.rs`:

```rust
//! `OverlayBox` — a floating container laid out with `position_absolute`.
//!
//! Geometry is parameterized: `FullBleed` (inset 0 + a reset background) is an
//! edge-to-edge cover (btw's full-screen swap); `Inset { ratio }` leaves a
//! proportional margin on all sides (model's 1/8 overlay). The box clears
//! underlying glyphs and fills its background (like iocraft's cover box), so
//! content behind it is hidden within its rect.

use ratatui::style::Color;
use ratatui::widgets::Borders;

use crate::{BorderStyle, Node};

/// How an [`OverlayBox`] is positioned within its parent.
#[derive(Clone, Copy, Debug)]
pub enum OverlayGeometry {
    /// Edge-to-edge: inset 0 on all sides. Pair with `background: Color::Reset`
    /// to fully cover what's underneath (the "full-screen swap" look).
    FullBleed,
    /// Proportional margin on all four sides (e.g. 0.125 = 1/8).
    Inset { ratio: f32 },
}

pub struct OverlayBoxProps {
    pub geometry: OverlayGeometry,
    /// Background fill. `Color::Reset` clears underlying glyphs and covers them.
    pub background: Color,
    pub border: Borders,
    pub border_style: BorderStyle,
    pub border_color: Color,
    /// The single content slot.
    pub content: Node,
}

pub struct OverlayBox;

#[allow(clippy::new_ret_no_self)]
impl OverlayBox {
    pub fn new(props: OverlayBoxProps) -> Node {
        let root = Node::new_view();
        root.set_position_absolute(());
        root.set_flex_direction(taffy::prelude::FlexDirection::Column);
        match props.geometry {
            OverlayGeometry::FullBleed => {
                // inset 0 is the default; set explicitly for clarity.
                root.set_inset_top(0.0);
                root.set_inset_bottom(0.0);
                root.set_inset_left(0.0);
                root.set_inset_right(0.0);
                root.set_width_percent(100.0);
                root.set_height_percent(100.0);
            }
            OverlayGeometry::Inset { ratio } => {
                let pct = ratio * 100.0;
                root.set_inset_top_percent(pct);
                root.set_inset_bottom_percent(pct);
                root.set_inset_left_percent(pct);
                root.set_inset_right_percent(pct);
            }
        }
        root.set_background(props.background);
        root.set_border(props.border);
        root.set_border_style(props.border_style);
        root.set_border_color(props.border_color);
        root.set_padding_all(1.0);
        root.add_child(props.content);
        root
    }
}
```

- [ ] **Step 2: Wire exports**

In `components/mod.rs` add:
```rust
pub mod overlay_box;
```

In `lib.rs` top-level `pub use` block add:
```rust
pub use crate::components::overlay_box::{OverlayBox, OverlayBoxProps, OverlayGeometry};
```
Add the same line to `prelude`.

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p iodilos`
Expected: builds with no errors.

- [ ] **Step 4: Commit**

```bash
cd vendor/iodilos
git add crates/iodilos/src/components/overlay_box.rs crates/iodilos/src/components/mod.rs crates/iodilos/src/lib.rs
git commit -m "feat(components): add OverlayBox floating container"
```

---

## Task 4: Add iodilos `table_view` example for manual verification

A runnable demo to eyeball the sectioned list + cursor + centered scroll before wiring it into flown.

**Files:**
- Create: `vendor/iodilos/examples/table_view.rs`

- [ ] **Step 1: Write the example**

Create `vendor/iodilos/examples/table_view.rs`:

```rust
//! Manual verification: a sectioned TableView with arrow-key cursor.
//! Run: `cargo run --example table_view` (from vendor/iodilos). Press Q to quit.

use std::rc::Rc;

use iodilos::prelude::*;

fn main() -> std::io::Result<()> {
    let mut renderer = Renderer::new()?;
    renderer.mount(|| {
        let selected = create_rw_signal(0usize);
        let rows: Vec<Vec<&str>> = vec![
            vec!["alpha-1", "alpha-2", "alpha-3"],
            vec!["beta-1", "beta-2"],
            vec!["gamma-1", "gamma-2", "gamma-3", "gamma-4"],
        ];
        let flat_count: usize = rows.iter().map(|r| r.len()).sum();
        let selected_for_key = selected;
        on_key(move |key: crossterm::event::KeyEvent| -> bool {
            use crossterm::event::{KeyCode, KeyModifiers};
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
                || key.code == KeyCode::Char('q')
            {
                quit();
                return true;
            }
            let total = flat_count.saturating_sub(1);
            match key.code {
                KeyCode::Down => selected_for_key.set((selected_for_key.get() + 1).min(total)),
                KeyCode::Up => selected_for_key.set(selected_for_key.get().saturating_sub(1)),
                _ => return false,
            }
            true
        });

        let sections_signal = Signal::derive(move || {
            rows.iter()
                .enumerate()
                .map(|(i, r)| TableSection {
                    header: {
                        let h = Node::new_text();
                        h.set_content(format!(" Section {} ", i + 1));
                        h.set_color(Color::Cyan);
                        Some(h)
                    },
                    rows: r.iter().map(|s| TableRow::new(*s)).collect(),
                })
                .collect::<Vec<_>>()
        });

        let cell_factory: CellFactory = Rc::new(|ctx: &CellContext| {
            let t = Node::new_text();
            let prefix = if ctx.selected { "▶ " } else { "  " };
            t.set_content(format!("{prefix}{}", ctx.key));
            t.set_color(if ctx.selected { Color::Yellow } else { Color::White });
            t
        });

        let props = TableViewProps::new(sections_signal, cell_factory, selected.into());
        TableView::new(props)
    });
    renderer.run_blocking()
}
```

- [ ] **Step 2: Verify it builds**

Run (from `vendor/iodilos/`): `cargo build --example table_view`
Expected: builds.

- [ ] **Step 3: Commit**

```bash
cd vendor/iodilos
git add examples/table_view.rs
git commit -m "examples: add table_view demo"
```

(Manual run is optional in CI; the build check above is the gate. If you have a terminal, `cargo run --example table_view` and confirm ↑/↓ moves the ▶ cursor across sections and the list scrolls when the cursor nears the edges.)

---

## Task 5: Bump iodilos submodule pointer in flown

The iodilos changes need to be visible to flown as a pinned submodule commit.

**Files:**
- Modify: `vendor/iodilos` (submodule pointer) — done via `git add` in the flown repo

- [ ] **Step 1: Confirm iodilos working tree is clean and committed**

Run (from `vendor/iodilos/`): `git status --porcelain`
Expected: empty (all Tasks 1–4 committed).

- [ ] **Step 2: Update flown's submodule pointer**

Run (from repo root):
```bash
git add vendor/iodilos
git status
```
Expected: `vendor/iodilos` shows as modified (new submodule SHA).

- [ ] **Step 3: Verify flown builds against the new iodilos**

Run: `cargo build -p coding-agent`
Expected: builds.

- [ ] **Step 4: Commit**

```bash
git add vendor/iodilos
git commit -m "chore: bump iodilos submodule (TableView + OverlayBox)"
```

---

## Task 6: Add `OverlayStack` to flown

The iodilos-context value that tracks the single active overlay (0 or 1). App reads it to conditionally render a top-level `OverlayBox`.

**Files:**
- Create: `crates/coding-agent/src/tui/overlay_stack.rs`
- Modify: `crates/coding-agent/src/tui/mod.rs` (add `pub mod overlay_stack;`)

- [ ] **Step 1: Write the module with tests**

Create `crates/coding-agent/src/tui/overlay_stack.rs`:

```rust
//! `OverlayStack` — tracks the single active floating overlay (0 or 1).
//!
//! Provided via iodilos context. App renders the main layout plus, when an
//! overlay is active, a top-level `OverlayBox` over it. Pushing replaces any
//! active overlay (we support depth 1 in v1); popping runs the overlay's
//! optional `on_close` teardown first.
//!
//! This is the "pure UI layer tracking" extracted from the old
//! ConversationStack's active-swap mechanism (see spec §2.1, §6.1).

use std::rc::Rc;

use iodilos::prelude::*;
use iodilos::OverlayGeometry;

/// One active overlay.
pub struct ActiveOverlay {
    pub geometry: OverlayGeometry,
    pub dismissible: bool,
    /// Builds the overlay's content Node. Called from an effect each render.
    pub content: Rc<dyn Fn() -> Node>,
    /// Optional teardown (btw uses it to drop its forked harness; model doesn't).
    pub on_close: Option<Rc<dyn Fn()>>,
}

pub struct OverlayStack {
    active: RwSignal<Option<Rc<ActiveOverlay>>>,
}

impl OverlayStack {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            active: create_rw_signal(None),
        })
    }

    /// The current overlay, if any.
    pub fn active(&self) -> Option<Rc<ActiveOverlay>> {
        self.active.get()
    }

    /// Reactive read for effects: call inside an effect to re-run on change.
    pub fn active_signal(&self) -> RwSignal<Option<Rc<ActiveOverlay>>> {
        self.active
    }

    /// True when an overlay is open or being opened.
    pub fn is_active(&self) -> bool {
        self.active.with(|o| o.is_some())
    }

    /// Push an overlay (replaces any active one). Returns whether it took effect.
    pub fn push(&self, overlay: ActiveOverlay) -> bool {
        if self.is_active() {
            return false;
        }
        self.active.set(Some(Rc::new(overlay)));
        true
    }

    /// Pop the active overlay, running its `on_close` first. No-op if none.
    pub fn pop(&self) {
        if let Some(overlay) = self.active.get() {
            if let Some(on_close) = &overlay.on_close {
                on_close();
            }
        }
        self.active.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_content() -> Rc<dyn Fn() -> Node> {
        Rc::new(|| Node::new_text())
    }

    fn overlay() -> ActiveOverlay {
        ActiveOverlay {
            geometry: OverlayGeometry::Inset { ratio: 0.125 },
            dismissible: true,
            content: dummy_content(),
            on_close: None,
        }
    }

    #[test]
    fn push_and_pop_round_trip() {
        let (, owner) = create_root(|| {
            let stack = OverlayStack::new();
            assert!(!stack.is_active());
            assert!(stack.push(overlay()));
            assert!(stack.is_active());
            stack.pop();
            assert!(!stack.is_active());
        });
        owner.dispose();
    }

    #[test]
    fn push_is_rejected_when_already_active() {
        let (, owner) = create_root(|| {
            let stack = OverlayStack::new();
            assert!(stack.push(overlay()));
            assert!(!stack.push(overlay()), "second push must be rejected");
            assert!(stack.is_active());
        });
        owner.dispose();
    }

    #[test]
    fn pop_runs_on_close() {
        let (, owner) = create_root(|| {
            let stack = OverlayStack::new();
            let fired = Rc::new(std::cell::Cell::new(false));
            let fired_for_close = Rc::clone(&fired);
            let o = ActiveOverlay {
                geometry: OverlayGeometry::FullBleed,
                dismissible: true,
                content: dummy_content(),
                on_close: Some(Rc::new(move || fired_for_close.set(true))),
            };
            stack.push(o);
            stack.pop();
            assert!(fired.get());
        });
        owner.dispose();
    }

    #[test]
    fn pop_is_noop_when_empty() {
        let (, owner) = create_root(|| {
            let stack = OverlayStack::new();
            stack.pop(); // must not panic
            assert!(!stack.is_active());
        });
        owner.dispose();
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/coding-agent/src/tui/mod.rs` add:
```rust
pub mod overlay_stack;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p coding-agent overlay_stack`
Expected: 4 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/coding-agent/src/tui/overlay_stack.rs crates/coding-agent/src/tui/mod.rs
git commit -m "feat(tui): add OverlayStack for single-overlay tracking"
```

---

## Task 7: Sync model/thinking changes to statusline (`translate_event`)

Today `translate_event` (`runtime.rs:475`) falls through with `_ => {}` and discards `ModelUpdate`/`ThinkingLevelUpdate`. Wire them so any model change (including `/model`'s apply) updates the status snapshot, and the reactive StatusLine re-renders.

**Files:**
- Modify: `crates/coding-agent/src/tui/runtime.rs` (`translate_event` + a test)

- [ ] **Step 1: Inspect the event payload shapes**

Run: `grep -n "ModelUpdate\|ThinkingLevelUpdate" crates/agent/src/harness/messages.rs crates/agent/src/harness/harness.rs | head`
Confirm the fields: `ModelUpdate { model, previous_model, source }` and `ThinkingLevelUpdate { level, previous_level }` (seen earlier at `harness.rs:516` and `:549`).

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module in `runtime.rs`:

```rust
#[test]
fn model_update_syncs_status_model_and_provider() {
    let state = UiState::new(TextAreaState::default());
    let mut acc = String::new();
    let mut thinking = false;
    let model = flown_ai::Model {
        id: "glm-5.1".to_string(),
        name: "GLM 5.1".to_string(),
        api: flown_ai::Api::Known(flown_ai::KnownApi::OpenAiCompletions),
        provider: flown_ai::Provider::Known(flown_ai::KnownProvider::OpenRouter),
        // Fill the rest with defaults via serde from a minimal JSON to avoid
        // enumerating every field here.
        ..serde_json::from_str::<flown_ai::Model>(
            r#"{"id":"x","name":"x","api":"openai-completions","provider":"openrouter","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1,"maxTokens":1}"#
        ).unwrap()
    };
    translate_event(
        AgentHarnessEvent::ModelUpdate {
            model: model.clone(),
            previous_model: None,
            source: flown_agent::ModelUpdateSource::Set,
        },
        &state,
        &mut acc,
        &mut thinking,
    );
    let snap = state.status.get();
    assert!(snap.model.contains("glm-5.1"), "status.model = {}", snap.model);
    assert_eq!(snap.provider, "OpenRouter");
}
```

If `flown_ai::Model` doesn't derive `Deserialize` or the field list above is wrong, adjust the construction to use the real `Model` struct (check `crates/ai/src/types.rs` around line 708). The intent of the test is: after `ModelUpdate`, `status.model` contains the model id and `status.provider` is set — adapt the literal to match the real struct.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p coding-agent model_update_syncs_status_model_and_provider`
Expected: FAIL — `status.model` is empty (the `_ => {}` arm discards the event).

- [ ] **Step 4: Add the two branches**

In `translate_event` (`runtime.rs`), replace the final `_ => {}` with explicit branches. The current end of the match looks like:

```rust
        AgentHarnessEvent::AgentEnd { .. }
        | AgentHarnessEvent::Abort { .. }
        | AgentHarnessEvent::Settled { .. } => {
            accumulated_text.clear();
            *in_thinking = false;
            state.busy.set(false);
            state.status.update(|s| s.busy = false);
        }

        _ => {}
    }
}
```

Change the `_ => {}` arm to handle the two events explicitly, keeping a smaller fallthrough:

```rust
        AgentHarnessEvent::ModelUpdate { model, .. } => {
            state.status.update(|s| {
                s.model = format!("{}/{}", model.provider, model.id);
                s.provider = model.provider.to_string();
            });
        }
        AgentHarnessEvent::ThinkingLevelUpdate { level, .. } => {
            state.status.update(|s| {
                s.thinking_level = format!("{:?}", level).to_lowercase();
            });
        }

        _ => {}
    }
}
```

(If other events still need to be ignored, keep `_ => {}` after these two arms.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p coding-agent model_update_syncs_status_model_and_provider`
Expected: PASS.

- [ ] **Step 6: Run the full runtime test module to check for regressions**

Run: `cargo test -p coding-agent --lib runtime::`
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/coding-agent/src/tui/runtime.rs
git commit -m "feat(tui): sync ModelUpdate/ThinkingLevelUpdate to statusline"
```

---

## Task 8: Add `RuntimeCommand` variants + capability methods

Extension command handlers (running on tokio) ask the iodilos side to open the model overlay or fork a conversation via these proxy commands.

**Files:**
- Modify: `crates/coding-agent/src/core/extensions/types.rs`

- [ ] **Step 1: Add the enum variants**

In `types.rs`, extend `RuntimeCommand` (currently has `OpenOverlap`/`CloseActiveOverlap`/`SendToActive`/`NotifyActive`/`NotifyErrorActive`/`ClearActive`). Add:

```rust
    OpenModelOverlay {
        reply: oneshot::Sender<CommandResult>,
    },
    ForkConversation {
        prompt: Option<String>,
        reply: oneshot::Sender<CommandResult>,
    },
```

- [ ] **Step 2: Add capability methods**

On `ConversationCapability`, add:

```rust
    pub async fn open_model_overlay(&self) -> CommandResult {
        self.runtime.open_model_overlay().await
    }

    pub async fn fork_conversation(&self, prompt: Option<String>) -> CommandResult {
        self.runtime.fork_conversation(prompt).await
    }
```

On `RuntimeCommandProxy`, add the matching request methods (mirroring `open_overlap`):

```rust
    pub async fn open_model_overlay(&self) -> CommandResult {
        self.request(|reply| RuntimeCommand::OpenModelOverlay { reply }).await
    }

    pub async fn fork_conversation(&self, prompt: Option<String>) -> CommandResult {
        self.request(|reply| RuntimeCommand::ForkConversation { prompt, reply }).await
    }
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p coding-agent`
Expected: builds. (The pump in `runtime.rs` won't handle the new variants yet — that's Task 10; Rust will warn about non-exhaustive match only if the pump matches `RuntimeCommand` exhaustively. If the build fails on a non-exhaustive match in `spawn_runtime_command_pump`, add a temporary `_ => {}` arm and remove it in Task 10.)

- [ ] **Step 4: Commit**

```bash
git add crates/coding-agent/src/core/extensions/types.rs
git commit -m "feat(extensions): add OpenModelOverlay + ForkConversation commands"
```

---

## Task 9: Extract `fork_conversation` + add `open_model_overlay` to `RuntimeControl`

`RuntimeControl` (iodilos thread) interprets the proxy commands. `fork_conversation` is the body of the existing `open_overlap` minus the ConversationLayer push; `open_model_overlay` constructs a `ModelOverlay` and pushes it onto the `OverlayStack`.

> **Note:** `ModelOverlay` doesn't exist yet (Task 11). This task adds the `open_model_overlay` method that *calls* into it, so we stub the construction and complete the real call in Task 11. To keep this task independently compiling, implement `open_model_overlay` to push a placeholder overlay now, then Task 11 replaces the placeholder body. Alternatively, reorder: do Task 11 first. The recommended order is Task 9 → Task 11 → come back to finalize `open_model_overlay`. The steps below follow that.

**Files:**
- Modify: `crates/coding-agent/src/tui/conversation.rs`

- [ ] **Step 1: Add `OverlayStack` to `RuntimeControl`**

In `conversation.rs`, add a field to `RuntimeControl`:

```rust
pub struct RuntimeControl {
    stack: Rc<ConversationStack>,
    overlays: Rc<crate::tui::overlay_stack::OverlayStack>,
    config: Config,
}
```

Update `RuntimeControl::new`:

```rust
    pub fn new(
        stack: Rc<ConversationStack>,
        overlays: Rc<crate::tui::overlay_stack::OverlayStack>,
        config: Config,
    ) -> Rc<Self> {
        Rc::new(Self { stack, overlays, config })
    }
```

- [ ] **Step 2: Extract `fork_conversation`**

Add a new method. It runs the same harness-build + bind + pump-spawn sequence currently inside `open_overlap` (which builds an in-memory fork of the main session, binds a driver, spawns the event pump), but instead of pushing a `ConversationLayer`, it returns the assembled pieces so the caller can wrap them into an `OverlayBox` content and an `on_close` teardown. (Do not delete `open_overlap` in this task — btw still calls it until Task 12 migrates.)

```rust
    /// Fork the main session into a fresh in-memory harness, bind its driver
    /// and event pump, and return (content_node_factory, on_close). The caller
    /// wraps these into an OverlayBox and pushes it onto the OverlayStack.
    ///
    /// Reuses the build path of `open_overlap` (factory.build + bind_layer_driver
    /// + pump) without the ConversationLayer push.
    pub fn fork_conversation(
        &self,
        prompt: Option<String>,
    ) -> Option<(
        Rc<dyn Fn() -> Node>,
        Rc<dyn Fn()>,
    )> {
        // Implementation note: this is a lift of the existing async build in
        // `open_overlap`. Because the build is async (tokio) and RuntimeControl
        // runs on iodilos, mirror the existing pattern: spawn a tokio task that
        // builds the harness, ship it back over a flume build channel, and in a
        // spawn_local bridge finish wiring (UiState, pump, driver binding).
        //
        // The content factory returns a transcript Node bound to the forked
        // UiState; the on_close closure runs the teardown sequence
        // (unsubscribe -> send Shutdown -> drop) from close_active_overlap.
        //
        // See `open_overlap` (this file) for the exact sequence to lift.
        todo!("Task 9 Step 2: lift the build+pump+pump sequence from open_overlap")
    }
```

Implement the body by copying the build channel + `spawn_local` bridge from `open_overlap`, but instead of `stack.push(ConversationLayer { ... })`:
- Create `Rc<UiState>` for the forked transcript (same as `overlap_state`).
- Store the binding parts (harness, cmd_tx, event_tx, unsubscribe) in `Rc`s captured by the returned closures.
- Return `(content_factory, on_close)` where:
  - `content_factory` builds a transcript component Node bound to the forked UiState (reuse `crate::tui::components::transcript::Transcript`-equivalent assembly, or a small inline node). If no such standalone factory exists, build a minimal node here and note it.
  - `on_close` runs the teardown: call `unsubscribe`, `cmd_tx.try_send(Shutdown)`, drop held senders (mirroring `close_active_overlap`'s loop body).

Because the build is async, `fork_conversation` returns `None` synchronously when no factory is present (session-only mode), and otherwise kicks off the async build and returns closures that read the forked state via interior `Rc<RefCell<…>>` (the UiState signal can be set from the bridge once the harness is ready). **If this proves awkward**, an acceptable simplification: have `fork_conversation` take a callback `FnOnce(content_factory, on_close)` invoked from the `spawn_local` bridge once the build completes, and push the overlay from inside the callback. Pick whichever fits the existing reactive style and keep a comment explaining the choice.

- [ ] **Step 3: Add `open_model_overlay` stub**

```rust
    /// Construct the ModelOverlay and push it onto the OverlayStack.
    pub fn open_model_overlay(&self) {
        // Finalized in Task 11 once ModelOverlay exists.
        if self.overlays.is_active() {
            self.stack
                .active()
                .state
                .push_system("An overlay is already active.");
            return;
        }
        let _ = crate::core::extensions::model::push_model_overlay(
            Rc::clone(&self.overlays),
            self.config.clone(),
            self.stack.main_layer().harness.clone(),
            self.stack.main_layer().state.status.get().model.clone(),
        );
    }
```

(Delegates to a free function `push_model_overlay` we create in Task 11, so `RuntimeControl` doesn't need to import iodilos component machinery. Adjust the signature to match Task 11's actual function.)

- [ ] **Step 4: Update the `new` call site**

In `runtime.rs` mount closure, update the `RuntimeControl::new` call to pass the `OverlayStack`:

```rust
        let runtime_control = crate::tui::conversation::RuntimeControl::new(
            Rc::clone(&stack),
            Rc::clone(&overlay_stack),
            mount_config.clone(),
        );
```

- [ ] **Step 5: Build (expect a link error / todo panic if exercised, but it must compile)**

Run: `cargo build -p coding-agent`
Expected: compiles. (`fork_conversation` has a `todo!`; that's fine as long as nothing calls it yet — btw migration is Task 12.)

- [ ] **Step 6: Commit**

```bash
git add crates/coding-agent/src/tui/conversation.rs crates/coding-agent/src/tui/runtime.rs
git commit -m "feat(tui): extract fork_conversation, add open_model_overlay hook"
```

---

## Task 10: Handle new commands in the runtime command pump

`spawn_runtime_command_pump` in `runtime.rs` must dispatch `OpenModelOverlay` and `ForkConversation`.

**Files:**
- Modify: `crates/coding-agent/src/tui/runtime.rs` (`spawn_runtime_command_pump`)

- [ ] **Step 1: Add the match arms**

In `spawn_runtime_command_pump`, add to the `match command { … }`:

```rust
                RuntimeCommand::OpenModelOverlay { reply } => {
                    runtime_control.open_model_overlay();
                    let _ = reply.send(Ok(()));
                }
                RuntimeCommand::ForkConversation { prompt, reply } => {
                    let _ = runtime_control.fork_conversation(prompt);
                    let _ = reply.send(Ok(()));
                }
```

Remove the temporary `_ => {}` from Task 8 if you added one.

- [ ] **Step 2: Build and run existing tests**

Run: `cargo build -p coding-agent && cargo test -p coding-agent --lib`
Expected: builds, tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/coding-agent/src/tui/runtime.rs
git commit -m "feat(tui): dispatch OpenModelOverlay + ForkConversation in pump"
```

---

## Task 11: Implement `ModelOverlay` + `ModelExtension`

The whole feature: register `/model`, and the overlay component (phase state machine, sections, cell factories, keys, apply). All model-specific code lives in `model.rs`.

**Files:**
- Create: `crates/coding-agent/src/core/extensions/model.rs`
- Modify: `crates/coding-agent/src/core/extensions/mod.rs`

- [ ] **Step 1: Write the pure-logic helper with tests first**

Much of `ModelOverlay`'s logic (section building, fuzzy filter, phase transitions given a selected key) is pure and testable without iodilos. Put these as free functions in `model.rs` and unit-test them.

Create `crates/coding-agent/src/core/extensions/model.rs`:

```rust
//! [`ModelExtension`] registers `/model`. [`push_model_overlay`] opens the
//! overlay. All model-specific UI + policy lives here (spec §7): the generic
//! TableView/OverlayBox primitives come from iodilos.
//!
//! Pure helpers (`model_sections`, `level_sections`, `fuzzy_match`,
//! `ModelPhase`) are free functions so they can be unit-tested without a TUI.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use flown_ai::{get_models, get_supported_thinking_levels, models_are_equal, Model, ThinkingLevel};
use iodilos::prelude::*;
use iodilos::{CellContext, CellFactory, OverlayGeometry, TableRow, TableSection};

use crate::config::Config;
use crate::tui::overlay_stack::{ActiveOverlay, OverlayStack};

/// Two-step flow: pick a model, then pick a thinking intensity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelPhase {
    Model,
    Thinking,
}

/// Case-insensitive substring match across several haystacks (good enough for a
/// picker; swap for a real fuzzy ranker later if desired).
pub fn fuzzy_match(query: &str, haystacks: &[&str]) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    haystacks.iter().any(|h| h.to_lowercase().contains(&q))
}

/// Build the model sections for the configured providers, filtered by `query`.
/// Returns `(sections, items)` where `items` maps each row key -> Model so the
/// cell factory can render and `on_confirm` can resolve. Keys and items are
/// 1:1 (the "same source" invariant, spec §7.2).
pub fn model_sections(
    config: &Config,
    query: &str,
    current: Option<&Model>,
) -> (Vec<TableSection>, HashMap<String, Model>) {
    let mut sections = Vec::new();
    let mut items = HashMap::new();
    for provider in config.providers.keys() {
        let models = get_models(provider);
        let filtered: Vec<Model> = models
            .into_iter()
            .filter(|m| {
                fuzzy_match(
                    query,
                    &[
                        &m.id,
                        &m.name,
                        &format!("{provider}/{}", m.id),
                    ],
                )
            })
            .collect();
        if filtered.is_empty() {
            continue;
        }
        let header = {
            let h = Node::new_text();
            h.set_content(format!(" {provider} "));
            h.set_color(Color::Cyan);
            Some(h)
        };
        let rows: Vec<TableRow> = filtered
            .iter()
            .map(|m| TableRow::new(format!("{provider}/{}", m.id)))
            .collect();
        for m in &filtered {
            items.insert(format!("{provider}/{}", m.id), m.clone());
        }
        sections.push(TableSection { header, rows });
    }
    let _ = current; // used by the cell factory via captured `current`
    (sections, items)
}

/// Build the thinking-level sections for a chosen model.
pub fn level_sections(
    picked: &Model,
    current_level: Option<ThinkingLevel>,
) -> (Vec<TableSection>, HashMap<String, ThinkingLevel>) {
    const DESC: &[(ThinkingLevel, &str)] = &[
        (ThinkingLevel::Off, "No reasoning"),
        (ThinkingLevel::Minimal, "Very brief reasoning (~1k tokens)"),
        (ThinkingLevel::Low, "Light reasoning (~2k tokens)"),
        (ThinkingLevel::Medium, "Moderate reasoning (~8k tokens)"),
        (ThinkingLevel::High, "Deep reasoning (~16k tokens)"),
        (ThinkingLevel::XHigh, "Maximum reasoning (~32k tokens)"),
    ];
    let supported = get_supported_thinking_levels(picked);
    let mut items = HashMap::new();
    let rows: Vec<TableRow> = supported
        .iter()
        .map(|l| {
            items.insert(format!("{l:?}"), *l);
            TableRow::new(format!("{l:?}"))
        })
        .collect();
    let header = {
        let h = Node::new_text();
        h.set_content(" Thinking intensity ".to_string());
        h.set_color(Color::Cyan);
        Some(h)
    };
    let _ = current_level;
    (vec![TableSection { header, rows }], items)
}
```

Add the tests at the bottom of `model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config_with(providers: &[&str]) -> Config {
        let mut c = Config::default();
        for p in providers {
            c.providers.insert(
                (*p).to_string(),
                crate::config::ProviderConfig {
                    provider_type: "api".to_string(),
                    key: "k".to_string(),
                },
            );
        }
        c
    }

    #[test]
    fn fuzzy_match_empty_query_matches_all() {
        assert!(fuzzy_match("", &["anything"]));
        assert!(fuzzy_match("   ", &["anything"]));
    }

    #[test]
    fn fuzzy_match_case_insensitive_substring() {
        assert!(fuzzy_match("GLM", &["glm-5.1"]));
        assert!(fuzzy_match("glm", &["GLM-5.1"]));
        assert!(!fuzzy_match("zzz", &["glm-5.1"]));
    }

    #[test]
    fn model_sections_only_configured_providers() {
        // deepseek is a real provider in models.generated.json with >=1 model.
        let cfg = config_with(&["deepseek"]);
        let (sections, items) = model_sections(&cfg, "", None);
        assert!(sections.iter().all(|s| s
            .header
            .as_ref()
            .map(|h| h.content().contains("deepseek"))
            .unwrap_or(false)
            || s.rows.iter().any(|r| r.key.starts_with("deepseek/"))));
        assert!(!items.is_empty(), "deepseek has models in the registry");
        // every row key is in items (same-source invariant)
        for s in &sections {
            for r in &s.rows {
                assert!(items.contains_key(&r.key), "missing key {}", r.key);
            }
        }
    }

    #[test]
    fn model_sections_filters_by_query() {
        let cfg = config_with(&["deepseek"]);
        let (sections, _) = model_sections(&cfg, "v4-flash", None);
        let all_keys: Vec<String> = sections
            .iter()
            .flat_map(|s| s.rows.iter().map(|r| r.key.clone()))
            .collect();
        assert!(all_keys.iter().all(|k| k.contains("v4-flash")));
    }
}
```

- [ ] **Step 2: Run the logic tests**

Run: `cargo test -p coding-agent core::extensions::model`
Expected: PASS.

- [ ] **Step 3: Implement the `ModelOverlay` component + `push_model_overlay`**

Append to `model.rs` (component assembly). It owns the phase state machine and wires iodilos primitives.

```rust
/// Open the model overlay: builds a `ModelOverlay` and pushes it.
///
/// `current_model_str` is the statusline's current "provider/id" snapshot, used
/// only to mark the current row (best-effort; the cell factory also compares
/// via `models_are_equal` when a resolved `Model` is available).
pub fn push_model_overlay(
    overlays: Rc<OverlayStack>,
    config: Config,
    harness: Option<Arc<flown_agent::AgentHarness>>,
    _current_model_str: String,
) {
    if overlays.is_active() {
        return;
    }
    let overlay = build_model_overlay(overlays.clone(), config, harness);
    overlays.push(overlay);
}

fn build_model_overlay(
    overlays: Rc<OverlayStack>,
    config: Config,
    harness: Option<Arc<flown_agent::AgentHarness>>,
) -> ActiveOverlay {
    let phase = create_rw_signal(ModelPhase::Model);
    let query = create_rw_signal(String::new());
    let selected = create_rw_signal(0usize);
    let picked: RwSignal<Option<Model>> = create_rw_signal(None);

    // Items maps, rebuilt whenever sections are recomputed. Shared with the
    // cell factory so both see the same source of truth.
    let model_items = Rc::new(std::cell::RefCell::new(HashMap::<String, Model>::new()));
    let level_items =
        Rc::new(std::cell::RefCell::new(HashMap::<String, ThinkingLevel>::new()));
    let config_rc = Rc::new(config);

    // Resolve the current model once (for the ✓ badge). Best-effort: if the
    // harness is absent, no badge.
    let current_model: Option<Model> = harness
        .as_ref()
        .map(|_| None); // resolved lazily; see note below.

    // Sections signal: recompute on query/phase change, refresh item maps.
    let config_for_sections = Rc::clone(&config_rc);
    let picked_for_sections = picked;
    let model_items_for_sections = Rc::clone(&model_items);
    let level_items_for_sections = Rc::clone(&level_items);
    let current_for_sections = current_model.clone();
    let sections_signal: Signal<Vec<TableSection>> = Signal::derive(move || {
        let p = phase.get();
        let q = query.get();
        match p {
            ModelPhase::Model => {
                let (secs, items) =
                    model_sections(&config_for_sections, &q, current_for_sections.as_ref());
                *model_items_for_sections.borrow_mut() = items;
                selected.set(selected.get().min(total_rows(&secs).saturating_sub(1)));
                secs
            }
            ModelPhase::Thinking => {
                let Some(picked_model) = picked_for_sections.get() else {
                    return Vec::new();
                };
                let (secs, items) = level_sections(&picked_model, None);
                *level_items_for_sections.borrow_mut() = items;
                selected.set(selected.get().min(total_rows(&secs).saturating_sub(1)));
                secs
            }
        }
    });

    // Cell factory: branches on phase, renders the right cell type.
    let model_items_for_cell = Rc::clone(&model_items);
    let level_items_for_cell = Rc::clone(&level_items);
    let current_for_cell = current_model.clone();
    let phase_for_cell = phase;
    let cell_factory: CellFactory = Rc::new(move |ctx: &CellContext| {
        let p = phase_for_cell.get();
        match p {
            ModelPhase::Model => {
                let m = model_items_for_cell.borrow().get(ctx.key).cloned();
                model_cell(m.as_ref(), ctx.selected, current_for_cell.as_ref())
            }
            ModelPhase::Thinking => {
                let l = level_items_for_cell.borrow().get(ctx.key).copied();
                level_cell(l, ctx.selected)
            }
        }
    });

    // Key handling: a captured on_key registered by the overlay owner.
    // (See Step 4: the overlay installs this via iodilos::on_key when mounted.)
    let overlays_for_keys = Rc::clone(&overlays);
    let harness_for_keys = harness.clone();
    let phase_for_keys = phase;
    let query_for_keys = query;
    let selected_for_keys = selected;
    let picked_for_keys = picked;
    let model_items_for_keys = Rc::clone(&model_items);
    let level_items_for_keys = Rc::clone(&level_items);
    let key_handler = Rc::new(move |key: crossterm::event::KeyEvent| {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Ctrl+C / Esc semantics depend on phase; handled in match below.
        let p = phase_for_keys.get();
        // Esc
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            match p {
                ModelPhase::Thinking => {
                    phase_for_keys.set(ModelPhase::Model);
                    selected_for_keys.set(0);
                    query_for_keys.set(String::new());
                }
                ModelPhase::Model => {
                    overlays_for_keys.pop();
                }
            }
            return true;
        }
        let secs_total = total_rows(&match p {
            ModelPhase::Model => model_sections(
                &config_rc,
                &query_for_keys.get(),
                current_model.as_ref(),
            ).0,
            ModelPhase::Thinking => picked_for_keys
                .get()
                .map(|m| level_sections(&m, None).0)
                .unwrap_or_default(),
        });
        match key.code {
            KeyCode::Down => {
                let total = secs_total.saturating_sub(1);
                selected_for_keys.set((selected_for_keys.get() + 1).min(total));
                true
            }
            KeyCode::Up => {
                selected_for_keys.set(selected_for_keys.get().saturating_sub(1));
                true
            }
            KeyCode::Enter => {
                let key_str = selected_key(&model_items_for_keys, &level_items_for_keys, p, selected_for_keys.get());
                match (p, key_str) {
                    (ModelPhase::Model, Some(k)) => {
                        let m = model_items_for_keys.borrow().get(&k).cloned();
                        if let Some(m) = m {
                            let levels = get_supported_thinking_levels(&m);
                            if levels.len() <= 1 {
                                apply(harness_for_keys.as_ref(), m, ThinkingLevel::Off);
                                overlays_for_keys.pop();
                            } else {
                                picked_for_keys.set(Some(m));
                                phase_for_keys.set(ModelPhase::Thinking);
                                selected_for_keys.set(0);
                                query_for_keys.set(String::new());
                            }
                        }
                        true
                    }
                    (ModelPhase::Thinking, Some(k)) => {
                        let level = level_items_for_keys.borrow().get(&k).copied();
                        if let (Some(level), Some(m)) = (level, picked_for_keys.get()) {
                            apply(harness_for_keys.as_ref(), m, level);
                        }
                        overlays_for_keys.pop();
                        true
                    }
                    _ => true,
                }
            }
            KeyCode::Backspace => {
                query_for_keys.update(|q| {
                    if !q.is_empty() {
                        q.pop();
                    }
                });
                selected_for_keys.set(0);
                true
            }
            KeyCode::Char(c) if p == ModelPhase::Model => {
                query_for_keys.update(|q| q.push(c));
                selected_for_keys.set(0);
                true
            }
            _ => false,
        }
    });

    // Content node: title + search box + table. The key handler is installed
    // via on_key inside an effect when the overlay mounts (App's Show branch
    // calls this factory). To keep ownership simple, the content factory
    // installs the handler on first build and returns the layout node.
    let overlays_for_content = Rc::clone(&overlays);
    let key_handler_for_content = Rc::clone(&key_handler);
    let sections_for_content = sections_signal;
    let cell_factory_for_content = cell_factory;
    let selected_for_content = selected;
    let query_for_content = query;
    let content: Rc<dyn Fn() -> Node> = Rc::new(move || {
        // Install the key router for this overlay.
        let kh = Rc::clone(&key_handler_for_content);
        on_key(move |key: crossterm::event::KeyEvent| -> bool {
            // Only handle when our overlay is the active one.
            // (App also routes Ctrl+C; this returns true to consume.)
            kh(key)
        });

        let root = Node::new_view();
        root.set_flex_direction(taffy::prelude::FlexDirection::Column);
        root.set_padding_all(0.0);

        // Title
        let title = Node::new_text();
        title.set_content(" /model — choose model ".to_string());
        title.set_color(Color::White);
        root.add_child(title);

        // Search box: a read-only mirror of `query` (editing happens via keys).
        let search = Node::new_text();
        let q = query_for_content.get();
        search.set_content(format!(" search: {q}_"));
        search.set_color(Color::DarkGray);
        root.add_child(search);

        // Table
        let props = TableViewProps::new(
            sections_for_content,
            cell_factory_for_content,
            selected_for_content.into(),
        );
        root.add_child(TableView::new(props));
        let _ = overlays_for_content;
        root
    });

    ActiveOverlay {
        geometry: OverlayGeometry::Inset { ratio: 0.125 },
        dismissible: true,
        content,
        on_close: None,
    }
}

fn total_rows(sections: &[TableSection]) -> usize {
    sections.iter().map(|s| s.rows.len()).sum()
}

/// Look up the key at a flat selection index across sections (rows only).
fn selected_key(
    model_items: &Rc<std::cell::RefCell<HashMap<String, Model>>>,
    level_items: &Rc<std::cell::RefCell<HashMap<String, ThinkingLevel>>>,
    phase: ModelPhase,
    selected: usize,
) -> Option<String> {
    // The TableView flattens rows-only; replicate to find the key.
    // Re-derive from whichever map is active (keys are the source of truth).
    let map: Vec<String> = match phase {
        ModelPhase::Model => model_items.borrow().keys().cloned().collect(),
        ModelPhase::Thinking => level_items.borrow().keys().cloned().collect(),
    };
    // NOTE: HashMap iteration order is not insertion order. To make selection
    // deterministic, the cell factory and this lookup must agree on order.
    // The clean fix: store the ordered key list alongside the items map when
    // building sections (add a `Vec<String>` ordered keys next to the map).
    map.get(selected).cloned()
}

fn model_cell(m: Option<&Model>, selected: bool, current: Option<&Model>) -> Node {
    let t = Node::new_text();
    let prefix = if selected { "▶ " } else { "  " };
    let label = match m {
        Some(m) => m.id.clone(),
        None => "?".to_string(),
    };
    let check = match m {
        Some(m) if current.is_some_and(|c| models_are_equal(Some(m), Some(c))) => " ✓",
        _ => "",
    };
    t.set_content(format!("{prefix}{label}{check}"));
    t.set_color(if selected { Color::Yellow } else { Color::White });
    t
}

fn level_cell(level: Option<ThinkingLevel>, selected: bool) -> Node {
    let t = Node::new_text();
    let prefix = if selected { "▶ " } else { "  " };
    let (name, desc) = match level {
        Some(ThinkingLevel::Off) => ("off", "No reasoning"),
        Some(ThinkingLevel::Minimal) => ("minimal", "Very brief reasoning (~1k tokens)"),
        Some(ThinkingLevel::Low) => ("low", "Light reasoning (~2k tokens)"),
        Some(ThinkingLevel::Medium) => ("medium", "Moderate reasoning (~8k tokens)"),
        Some(ThinkingLevel::High) => ("high", "Deep reasoning (~16k tokens)"),
        Some(ThinkingLevel::XHigh) => ("xhigh", "Maximum reasoning (~32k tokens)"),
        None => ("?", ""),
    };
    t.set_content(format!("{prefix}{name} — {desc}"));
    t.set_color(if selected { Color::Yellow } else { Color::White });
    t
}

fn apply(harness: Option<&Arc<flown_agent::AgentHarness>>, model: Model, level: ThinkingLevel) {
    use std::sync::Arc;
    if let Some(h) = harness {
        let h = Arc::clone(h);
        tokio::spawn(async move {
            let _ = h.set_model(model).await;
            let _ = h.set_thinking_level(level).await;
        });
    }
}
```

**IMPORTANT — fix the ordering bug before finalizing:** `selected_key` reads a `HashMap` whose iteration order is unspecified, so the flat index ↔ key mapping can disagree with what `TableView` renders. Fix by storing an **ordered keys `Vec<String>`** next to each items map when sections are built, and have both the cell factory's rendering and `selected_key` consume that ordered list. Add `let model_keys: Rc<RefCell<Vec<String>>>` and `level_keys: Rc<RefCell<Vec<String>>>`, populate them in the `sections_signal` derive, and look up `selected_key` via `keys.get(selected)`. Update the tests if needed. This is the load-bearing correctness fix for cursor↔row correspondence; do not skip it.

- [ ] **Step 4: Add `ModelExtension` + register it**

Append to `model.rs`:

```rust
use super::types::{CommandMeta, CommandHandler, CommandInvocation, Extension, ExtensionApi};

pub struct ModelExtension;

impl ModelExtension {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ModelExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for ModelExtension {
    fn name(&self) -> &'static str {
        "model"
    }
    fn register(&self, api: &mut ExtensionApi) {
        let handler: CommandHandler = std::sync::Arc::new(
            |_inv: CommandInvocation, ctx: super::types::ExtensionContext| {
                Box::pin(async move { ctx.conversation.open_model_overlay().await })
            },
        );
        api.register_command(
            "/model",
            CommandMeta::simple("Choose model and thinking intensity (overlay)"),
            handler,
        );
    }
}
```

In `crates/coding-agent/src/core/extensions/mod.rs`:
- Add `pub mod model;` (near `pub mod btw;`).
- In `build_runner`, add `Box::new(model::ModelExtension::new())` to the `extensions` vec.

- [ ] **Step 5: Build and run all tests**

Run: `cargo build -p coding-agent && cargo test -p coding-agent`
Expected: builds; model logic tests pass; no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/coding-agent/src/core/extensions/model.rs crates/coding-agent/src/core/extensions/mod.rs
git commit -m "feat(extensions): /model command with ModelOverlay"
```

---

## Task 12: Wire `OverlayStack` into App + Ctrl+C routing

App renders the main layout plus, when `OverlayStack` is active, the top `OverlayBox` (via `Show`). Ctrl+C closes the active overlay instead of quitting.

**Files:**
- Modify: `crates/coding-agent/src/tui/components/app.rs`
- Modify: `crates/coding-agent/src/tui/runtime.rs` (provide `OverlayStack` in context)

- [ ] **Step 1: Provide `OverlayStack` at mount**

In `runtime.rs` mount closure, after building the `ConversationStack`, add:

```rust
        let overlay_stack = crate::tui::overlay_stack::OverlayStack::new();
        provide_context(Rc::clone(&overlay_stack));
```

And update the `RuntimeControl::new` call (Task 9 Step 4 already did this; verify it passes `Rc::clone(&overlay_stack)`).

- [ ] **Step 2: Render the active overlay in App**

In `app.rs`, read the overlay stack and render it conditionally. The current `view!` returns:

```rust
    view! {
        View(
            flex_direction: FlexDirection::Column,
            width_percent: 100.0,
            height_percent: 100.0,
            background: Color::Reset,
        ) {
            Transcript()
            StatusLine()
            InputEditor()
        }
    }
```

Wrap it so the overlay is added as an absolutely-positioned sibling when active. Since `view!` children are static, use a fragment root and an effect that adds/removes the overlay child:

```rust
#[component]
pub fn App() -> Node {
    let stack = use_context::<Rc<crate::tui::conversation::ConversationStack>>();
    let overlay_stack = use_context::<Rc<crate::tui::overlay_stack::OverlayStack>>();
    let agent = use_context::<Option<Arc<AgentHarness>>>();
    let config = use_context::<crate::config::Config>();

    // …existing on_key wiring, but updated for overlay Ctrl+C (Step 3)…

    let main = view! {
        View(
            flex_direction: FlexDirection::Column,
            width_percent: 100.0,
            height_percent: 100.0,
            background: Color::Reset,
        ) {
            Transcript()
            StatusLine()
            InputEditor()
        }
    };

    let root = Node::new_fragment();
    root.add_child(main.clone());
    let root_for_effect = root.clone();
    let overlay_stack_for_effect = Rc::clone(&overlay_stack);
    let main_for_effect = main.clone();
    create_effect(move || {
        // Read the signal so this re-runs on push/pop.
        let active = overlay_stack_for_effect.active_signal().get();
        let mut children = vec![main_for_effect.clone()];
        if let Some(overlay) = active {
            let content = (overlay.content)();
            let box_props = iodilos::OverlayBoxProps {
                geometry: overlay.geometry,
                background: Color::Reset,
                border: ratatui::widgets::Borders::ALL,
                border_style: iodilos::BorderStyle::Round,
                border_color: Color::Cyan,
                content,
            };
            children.push(iodilos::OverlayBox::new(box_props));
        }
        root_for_effect.set_children(children);
    });
    root
}
```

- [ ] **Step 3: Update Ctrl+C in `handle_app_key`**

The existing Ctrl+C block (`app.rs:99-120`) checks `stack.overlap_is_active_or_pending()`. Add an overlay-first guard at the top of the Ctrl+C branch:

```rust
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        // Overlay takes priority: dismiss it before falling back to main logic.
        if overlay_stack.is_active() {
            overlay_stack.pop();
            return true;
        }
        // …existing input-clear / overlap / quit logic…
    }
```

(Add `let overlay_stack = …;` available to `handle_app_key` — either pass it in as a new param or read it via `use_context` inside the function as the existing code does for `command_side`. Prefer passing it as a parameter to match the function's existing explicit-dependency style.)

- [ ] **Step 4: Build and smoke-test manually**

Run: `cargo build -p coding-agent`
Expected: builds.

Then run the binary and exercise `/model`:
```bash
cargo run -p coding-agent
# type /model<Enter>; arrow through models; Enter on a model; arrow through
# thinking levels; Enter to apply; confirm the statusline model changes; Esc
# at thinking goes back to model list; Esc/Ctrl+C at model list closes.
```
Expected: overlay appears centered with ~1/8 margins; cursor moves; apply updates statusline.

- [ ] **Step 5: Commit**

```bash
git add crates/coding-agent/src/tui/components/app.rs crates/coding-agent/src/tui/runtime.rs
git commit -m "feat(tui): render active overlay in App + Ctrl+C dismiss"
```

---

## Task 13: Migrate btw to `OverlayBox(FullBleed)` + `fork_conversation`

btw currently uses `OverlapOptions`/`open_overlap` (active-swap ConversationLayer). Migrate it to the generic overlay: `OverlayBox(FullBleed, bg Reset)` wrapping the forked transcript, using the `fork_conversation` capability from Task 9.

**Files:**
- Modify: `crates/coding-agent/src/core/extensions/btw.rs`
- Modify: `crates/coding-agent/src/tui/conversation.rs` (remove now-unused `open_overlap`, or keep if other callers exist — check first)

- [ ] **Step 1: Check for other `open_overlap` callers**

Run: `grep -rn "open_overlap\|OverlapOptions" crates/coding-agent/src/`
Expected: only `btw.rs` and the `RuntimeControl`/types definitions. If anything else uses them, stop and note it before removing.

- [ ] **Step 2: Rewrite `open_btw_overlap`**

In `btw.rs`, replace the body of `open_btw_overlap` to use the new capability:

```rust
pub async fn open_btw_overlap(
    args: &str,
    ctx: super::types::ExtensionContext,
) -> anyhow::Result<()> {
    ctx.conversation.fork_conversation(parse_btw_args(args)).await
}
```

Remove the `OverlapOptions`/`SlashCommandScope` imports from `btw.rs` that are now unused.

- [ ] **Step 3: Make `fork_conversation` push the overlay**

In Task 9 Step 2, `fork_conversation` returns `(content_factory, on_close)` (or invokes a callback). Wire it to push an `OverlayBox(FullBleed)` onto the `OverlayStack`:

```rust
    pub fn fork_conversation(&self, prompt: Option<String>) -> Option<()> {
        // …build the forked harness + UiState + pump (lifted from open_overlap)…
        // then:
        let content: Rc<dyn Fn() -> Node> = /* transcript node bound to forked UiState */;
        let on_close: Rc<dyn Fn()> = /* unsubscribe + Shutdown + drop */;
        let overlay = ActiveOverlay {
            geometry: OverlayGeometry::FullBleed,
            dismissible: true,
            content,
            on_close: Some(on_close),
        };
        self.overlays.push(overlay);
        Some(())
    }
```

If you used the callback variant in Task 9, do the push inside the `spawn_local` bridge after the build completes.

- [ ] **Step 4: Update or remove the old `open_overlap` path**

If no callers remain, delete `RuntimeControl::open_overlap`, the `RuntimeCommand::OpenOverlap`/`CloseActiveOverlap` handling, and `OverlapOptions`. If the ConversationStack is still used for the main layer, keep it but remove the overlap-specific machinery (`reserve_overlap`, `pop_all_overlap_layers`, etc.). Update the existing `conversation.rs` tests that reference overlap behavior (`pop_active_*`) to reflect the new model, or delete them if they no longer apply.

- [ ] **Step 5: Update existing btw/overlap tests**

Run: `cargo test -p coding-agent conversation::`
Update tests that asserted on `ConversationStack` overlap semantics. For overlay behavior, add tests on `OverlayStack` (already done in Task 6). For btw specifically, a smoke test is the realistic gate (the async build is hard to unit-test without a full harness fixture).

- [ ] **Step 6: Build and run tests**

Run: `cargo build -p coding-agent && cargo test -p coding-agent`
Expected: builds, tests pass.

- [ ] **Step 7: Manual smoke test of btw**

```bash
cargo run -p coding-agent
# type: /btw how do I print in rust<Enter>
# confirm: full-screen swap to btw transcript, streaming begins; Ctrl+C returns
# to main with main's transcript intact.
```

- [ ] **Step 8: Commit**

```bash
git add crates/coding-agent/src/core/extensions/btw.rs crates/coding-agent/src/tui/conversation.rs
git commit -m "refactor(btw): migrate to OverlayBox(FullBleed) + fork_conversation"
```

---

## Task 14: Rebuild `CompletionMenu` as a `TableView` assembly

Collapse the two list implementations into one. `CompletionMenu` keeps its public API (`CompletionItem`/`CompletionMenuProps`/`CompletionMenu`) so `editor.rs` is unchanged, but internally delegates to `TableView`.

**Files:**
- Modify: `vendor/iodilos/crates/iodilos/src/components/completion_menu.rs`

- [ ] **Step 1: Read the current CompletionMenu**

Read `vendor/iodilos/crates/iodilos/src/components/completion_menu.rs` fully. Note: it takes `items: Signal<Vec<CompletionItem>>` and `selected: Signal<usize>`, renders label + description, with a `max_items` window.

- [ ] **Step 2: Rewrite as a TableView assembly**

Replace the body of `CompletionMenu::new` to build a `TableView`:

```rust
impl CompletionMenu {
    pub fn new(props: CompletionMenuProps) -> Node {
        let items = props.items;
        let cell_factory: CellFactory = Rc::new(move |ctx: &CellContext| {
            // items is a Signal; read the current list and index by flat position.
            let list = items.get();
            let item = list.get(ctx.section_idx * 0 + ctx.row_idx) // single section
                .cloned();
            let t = Node::new_richtext();
            let prefix = if ctx.selected { "▶ " } else { "  " };
            if let Some(item) = item {
                t.set_lines(vec![ratatui::text::Line::from(vec![
                    ratatui::text::Span::raw(prefix.to_string()),
                    ratatui::text::Span::raw(item.label),
                    ratatui::text::Span::raw(" "),
                    ratatui::text::Span::raw(item.description),
                ])]);
            }
            t
        });
        let sections = Signal::derive(move || {
            vec![TableSection {
                header: None,
                rows: items
                    .get()
                    .iter()
                    .enumerate()
                    .map(|(i, _)| TableRow::new(format!("cmp{i}")))
                    .collect(),
            }]
        });
        let tv_props = TableViewProps {
            sections,
            cell_factory,
            selected: props.selected,
            max_visible: props.max_items,
            border: props.border,
            border_style: props.border_style,
            border_color: props.border_color,
        };
        TableView::new(tv_props)
    }
}
```

Note: the cell factory reads `items.get()` and indexes by `row_idx` (single section, so flat index == row index). The `TableRow` keys are synthetic (`cmp{i}`) since identity doesn't matter for the completion list — but they must be **unique** to keep `For`-style diff sane; using the index is fine because the list is rebuilt wholesale each keystroke. If the existing CompletionMenu tests assert specific rendering, keep them passing by matching the same prefix/span structure.

- [ ] **Step 3: Run iodilos tests**

Run (from `vendor/iodilos/`): `cargo test -p iodilos`
Expected: pass. If existing CompletionMenu tests break on rendering details, update them to the new structure (they test the assembly, not TableView internals).

- [ ] **Step 4: Verify flown's editor still builds + behaves**

Run: `cargo build -p coding-agent`
Expected: builds (`editor.rs` is unchanged — same public API).

Manual check: run `cargo run -p coding-agent`, type `/` in the editor, confirm the slash-command popup still shows and arrow-selects.

- [ ] **Step 5: Commit**

```bash
cd vendor/iodilos
git add crates/iodilos/src/components/completion_menu.rs
git commit -m "refactor(components): rebuild CompletionMenu atop TableView"
```

- [ ] **Step 6: Bump submodule pointer in flown**

```bash
# from repo root
git add vendor/iodilos
git commit -m "chore: bump iodilos submodule (CompletionMenu atop TableView)"
```

---

## Task 15: Final verification + cleanup

- [ ] **Step 1: Full workspace build + tests**

Run: `cargo build && cargo test --workspace`
Expected: all build, all tests pass.

- [ ] **Step 2: Clippy (if the repo uses it)**

Run: `cargo clippy --workspace -- -D warnings 2>/dev/null || cargo clippy -p coding-agent`
Expected: no new warnings. Fix anything introduced by this plan (unused imports from removed `OverlapOptions`, etc.).

- [ ] **Step 3: Remove dead code from the old overlap path**

Run: `grep -rn "OverlapOptions\|open_overlap\|overlap_slot\|reserve_overlap\|pop_all_overlap" crates/coding-agent/src/`
Expected: no references (all removed in Task 13). If any remain, delete or document why they stay.

- [ ] **Step 4: End-to-end manual test**

```bash
cargo run -p coding-agent
```
Verify, in one session:
1. `/model` opens centered overlay (1/8 margins), arrow through models, Enter → thinking list, Enter applies, statusline model/thinking updates.
2. `/model` again, Esc at thinking → back to model list; Esc/Ctrl+C at model list → closes.
3. `/btw <msg>` → full-screen swap, streams, Ctrl+C → back to main intact.
4. Type `/` → slash popup still works (CompletionMenu rebuilt).

- [ ] **Step 5: Commit any cleanup**

```bash
git add -A
git commit -m "chore: final cleanup after /model + overlay refactor"
```

---

## Self-Review

**Spec coverage:**
- §1 Goal (`/model` two-step overlay) → Tasks 11–12. ✓
- §2.1 overlap redefined (geometry param) → Task 3 (`OverlayGeometry`). ✓
- §3 two primitives (TableView + OverlayBox) → Tasks 2–3. ✓
- §3.0 no ModalDriver → honored (ModelOverlay is a direct component, Task 11). ✓
- §3.1 CompletionMenu atop TableView → Task 14. ✓
- §4 cell-based TableView → Task 2. ✓
- §5 OverlayBox + inset percent → Tasks 1, 3. ✓
- §6 OverlayStack → Task 6; App wiring → Task 12. ✓
- §7 ModelOverlay (no trait) → Task 11. ✓
- §8 btw migration → Task 13. ✓
- §9 statusline sync (translate_event) → Task 7. ✓
- §10 file table → covered across tasks. ✓
- §11 invariants → thread-locality honored (ModelOverlay built on iodilos thread via RuntimeControl; apply via tokio::spawn). ✓
- §12 testing → unit tests in Tasks 1, 2, 6, 7, 11; integration/manual in 12, 13, 14, 15. ✓
- §13 out of scope (/login, nesting, ConversationStack full removal) → respected. ✓

**Placeholder scan:** Task 9 Step 2 contains a `todo!()` that Task 9 itself instructs to implement (the lift from `open_overlap`); this is a known-sized lift, not a placeholder — but flagged clearly. Task 11 Step 3 flags the HashMap-ordering bug and requires the ordered-keys fix inline. No "TBD"/"implement later" without specifics.

**Type consistency:** `OverlayGeometry`/`OverlayBoxProps`/`OverlayBox` (Task 3) match usage in Task 12. `TableSection`/`TableRow`/`CellContext`/`CellFactory`/`TableViewProps`/`TableView` (Task 2) match usage in Tasks 11 and 14. `ActiveOverlay`/`OverlayStack` (Task 6) match usage in Tasks 9, 11, 12, 13. `RuntimeCommand::OpenModelOverlay`/`ForkConversation` (Task 8) match pump arms (Task 10) and capability methods. `ModelPhase` used consistently in Task 11.
