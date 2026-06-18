//! `OverlayStack` — tracks the single active floating overlay (0 or 1).
//!
//! Provided via iodilos context. App renders the main layout plus, when an
//! overlay is active, a top-level `OverlayBox` over it. Pushing replaces any
//! active overlay (we support depth 1 in v1); popping runs the overlay's
//! optional `on_close` teardown first.
//!
//! The overlay carries a **content factory** (`Fn() -> Node`), not a pre-built
//! node. App's `OverlayLayer` builds the content ONCE — under the mount owner,
//! wrapped in a sub-`Owner` (mirroring iodilos's `Show`) — when the active
//! overlay changes, and caches that owner so spurious effect re-runs do not
//! re-build (which would stack a fresh `on_key` per redraw). Building in the
//! render effect (not in a `spawn_local` task) ensures `use_context` /
//! `on_key` / `create_effect` resolve against the correct owner.

use std::rc::Rc;

use iodilos::prelude::*;
use iodilos::OverlayGeometry;

/// One active overlay. `content` is a factory because the node must be built
/// inside App's render effect (the mount owner) — not in the `spawn_local`
/// task that calls `push`.
pub struct ActiveOverlay {
    pub geometry: OverlayGeometry,
    pub dismissible: bool,
    /// Builds the overlay's content Node. Invoked exactly once per overlay
    /// (when App mounts it), under the mount owner.
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

    /// Push an overlay. Returns whether it took effect — rejected if another
    /// overlay is already active (v1 supports depth 1).
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

    fn overlay() -> ActiveOverlay {
        ActiveOverlay {
            geometry: OverlayGeometry::Inset { ratio: 0.125 },
            dismissible: true,
            content: Rc::new(|| Node::new_text()),
            on_close: None,
        }
    }

    #[test]
    fn push_and_pop_round_trip() {
        let (_, owner) = create_root(|| {
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
        let (_, owner) = create_root(|| {
            let stack = OverlayStack::new();
            assert!(stack.push(overlay()));
            assert!(!stack.push(overlay()), "second push must be rejected");
            assert!(stack.is_active());
        });
        owner.dispose();
    }

    #[test]
    fn pop_runs_on_close() {
        let (_, owner) = create_root(|| {
            let stack = OverlayStack::new();
            let fired = Rc::new(std::cell::Cell::new(false));
            let fired_for_close = Rc::clone(&fired);
            let o = ActiveOverlay {
                geometry: OverlayGeometry::FullBleed,
                dismissible: true,
                content: Rc::new(|| Node::new_text()),
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
        let (_, owner) = create_root(|| {
            let stack = OverlayStack::new();
            stack.pop(); // must not panic
            assert!(!stack.is_active());
        });
        owner.dispose();
    }
}
