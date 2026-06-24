//! `OverlayStack` — the stack of active floating overlays (0..N).
//!
//! Provided via iodilos context. App renders the main layout plus, when one or
//! more overlays are active, a nested stack of absolutely-positioned boxes over
//! it — each later overlay paints above the previous, so a smaller picker can
//! float over a larger one (e.g. the thinking-intensity picker over the
//! `/model` picker).
//! Pushing appends a new top layer; popping runs that layer's optional
//! `on_close` teardown first. `pop_all` clears the whole stack (used when a
//! nested flow confirms and wants to return straight to the original view).
//!
//! The overlay carries a **content factory** (`Fn() -> View`), not a pre-built
//! view. App's overlay host builds the content under the mount owner,
//! wrapped in a sub-`Owner` (mirroring iodilos's `Show`) — when the active
//! overlay changes, and caches that owner so spurious effect re-runs do not
//! re-build (which would stack a fresh `on_key` per redraw). Building in the
//! render effect (not in a `spawn_local` task) ensures `use_context` /
//! `on_key` / `create_effect` resolve against the correct owner.

use std::rc::Rc;

use crossterm::event::KeyEvent;
use iodilos::prelude::*;

/// How an overlay's absolutely-positioned box is inset from the screen edges.
///
/// `FullBleed` runs edge-to-edge; `Inset { ratio }` pulls each side in by
/// `ratio` of the available dimension (clamped so opposite insets never
/// overlap). This is an app-level concept — overlays are a flown concern, not
/// an iodilos primitive — so the geometry lives here next to the stack that
/// consumes it, not in the iodilos component tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayGeometry {
    FullBleed,
    Inset { ratio: f32 },
}

impl OverlayGeometry {
    /// Translate this geometry into the uniform `Inset` applied to all four
    /// sides of the absolutely-positioned overlay box.
    pub fn inset(self) -> Inset {
        match self {
            OverlayGeometry::FullBleed => Inset::Length(0),
            OverlayGeometry::Inset { ratio } => {
                Inset::Percent((ratio * 100.0).clamp(0.0, 49.0))
            }
        }
    }

    /// The chrome this geometry wears: a full-bleed overlay has no border
    /// (it fills the screen), an inset overlay gets a rounded cyan frame so
    /// the floating region reads as a distinct surface.
    pub fn border(self) -> (BorderStyle, Color) {
        match self {
            OverlayGeometry::FullBleed => (BorderStyle::None, Color::Reset),
            OverlayGeometry::Inset { .. } => (BorderStyle::Round, Color::Cyan),
        }
    }
}

/// One active overlay. `content` is a factory because the node must be built
/// inside App's render effect (the mount owner) — not in the `spawn_local`
/// task that calls `push`.
pub struct ActiveOverlay {
    pub geometry: OverlayGeometry,
    pub dismissible: bool,
    /// Whether keys that the overlay content did not consume should continue
    /// through App's editor/router. Full-bleed conversation overlays use this
    /// so the active fork receives typing; modal pickers leave it false so
    /// stray keys do not mutate the hidden main editor.
    pub route_app_keys: bool,
    /// Builds the overlay's content view.
    pub content: Rc<dyn Fn() -> View>,
    /// Optional overlay-local key handler. App root calls this before routing
    /// unhandled keys to the active conversation.
    pub on_key: Option<Rc<dyn Fn(KeyEvent) -> bool>>,
    /// Optional teardown (btw uses it to drop its forked harness; model doesn't).
    pub on_close: Option<Rc<dyn Fn()>>,
}

pub struct OverlayStack {
    /// The active layers, bottom-first. The last element is the topmost layer.
    layers: Signal<Vec<Rc<ActiveOverlay>>>,
}

impl OverlayStack {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            layers: create_signal(Vec::new()),
        })
    }

    /// The topmost overlay, if any. This is the one that owns the keys.
    pub fn active(&self) -> Option<Rc<ActiveOverlay>> {
        self.layers.with(|l| l.last().cloned())
    }

    /// Reactive read of the full layer list, for the render effect. Reading
    /// inside an effect makes it re-run on push/pop.
    pub fn layers_signal(&self) -> Signal<Vec<Rc<ActiveOverlay>>> {
        self.layers
    }

    /// True when at least one overlay is open.
    pub fn is_active(&self) -> bool {
        self.layers.with(|l| !l.is_empty())
    }

    /// The number of layers currently stacked.
    pub fn depth(&self) -> usize {
        self.layers.with(|l| l.len())
    }

    /// Push an overlay onto the top of the stack. Unlike the earlier depth-1
    /// model, a push always succeeds: layers nest, so a smaller picker can
    /// float above an already-open one.
    pub fn push(&self, overlay: ActiveOverlay) -> bool {
        self.layers.update(|l| l.push(Rc::new(overlay)));
        true
    }

    /// Pop the topmost overlay, running its `on_close` first. No-op if empty.
    pub fn pop(&self) {
        let removed = self.layers.update(|l| l.pop());
        if let Some(overlay) = removed
            && let Some(on_close) = &overlay.on_close
        {
            on_close();
        }
    }

    /// Pop every overlay (running each `on_close`), returning to the original
    /// view. Used when a nested flow confirms and wants to skip the
    /// intermediate layers — e.g. confirming a thinking level applies the model
    /// and drops both the thinking and model overlays at once.
    pub fn pop_all(&self) {
        while self.is_active() {
            self.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay() -> ActiveOverlay {
        ActiveOverlay {
            geometry: OverlayGeometry::Inset { ratio: 0.125 },
            dismissible: true,
            route_app_keys: false,
            content: Rc::new(View::new),
            on_key: None,
            on_close: None,
        }
    }

    #[test]
    fn push_and_pop_round_trip() {
        let owner = create_root(|| {
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
    fn push_nests_layers_instead_of_rejecting() {
        let owner = create_root(|| {
            let stack = OverlayStack::new();
            assert!(stack.push(overlay()));
            // A second push now nests on top rather than being rejected.
            assert!(stack.push(overlay()));
            assert_eq!(stack.depth(), 2);
            assert!(stack.is_active());

            // active() is the topmost layer.
            assert!(stack.active().is_some());

            // Popping unwraps one layer at a time.
            stack.pop();
            assert_eq!(stack.depth(), 1);
            stack.pop();
            assert!(!stack.is_active());
        });
        owner.dispose();
    }

    #[test]
    fn pop_runs_on_close() {
        let owner = create_root(|| {
            let stack = OverlayStack::new();
            let fired = Rc::new(std::cell::Cell::new(false));
            let fired_for_close = Rc::clone(&fired);
            let o = ActiveOverlay {
                geometry: OverlayGeometry::FullBleed,
                dismissible: true,
                route_app_keys: false,
                content: Rc::new(View::new),
                on_key: None,
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
        let owner = create_root(|| {
            let stack = OverlayStack::new();
            stack.pop(); // must not panic
            assert!(!stack.is_active());
        });
        owner.dispose();
    }

    #[test]
    fn pop_all_clears_every_layer_running_each_on_close() {
        let owner = create_root(|| {
            let stack = OverlayStack::new();
            let count = Rc::new(std::cell::Cell::new(0u32));
            for _ in 0..3 {
                let count_for_close = Rc::clone(&count);
                stack.push(ActiveOverlay {
                    geometry: OverlayGeometry::Inset { ratio: 0.125 },
                    dismissible: true,
                    route_app_keys: false,
                    content: Rc::new(View::new),
                    on_key: None,
                    on_close: Some(Rc::new(move || {
                        count_for_close.set(count_for_close.get() + 1)
                    })),
                });
            }
            assert_eq!(stack.depth(), 3);
            stack.pop_all();
            assert!(!stack.is_active());
            assert_eq!(count.get(), 3, "every on_close should have run");
        });
        owner.dispose();
    }

    #[test]
    fn active_returns_topmost_after_nested_push() {
        let owner = create_root(|| {
            let stack = OverlayStack::new();
            let bottom = ActiveOverlay {
                geometry: OverlayGeometry::FullBleed,
                dismissible: true,
                route_app_keys: false,
                content: Rc::new(View::new),
                on_key: None,
                on_close: None,
            };
            stack.push(bottom);
            let top = ActiveOverlay {
                geometry: OverlayGeometry::Inset { ratio: 0.33 },
                dismissible: true,
                route_app_keys: false,
                content: Rc::new(View::new),
                on_key: None,
                on_close: None,
            };
            stack.push(top);

            let active = stack.active().expect("a top layer should be active");
            assert_eq!(
                active.geometry,
                OverlayGeometry::Inset { ratio: 0.33 },
                "active() should be the topmost layer"
            );
        });
        owner.dispose();
    }
}
