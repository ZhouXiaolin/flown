//! The conversation stack — multi-layer transcript support for `/btw`.
//!
//! Replaces the single `Rc<UiState>` as the thing TUI components read. The
//! stack always has a `Main` layer (never popped); `/btw` pushes a `Btw` layer
//! that forks the main session's history into a transient in-memory session,
//! runs concurrently, and is discarded on exit.
//!
//! Each layer is fully isolated: its own `UiState`, its own `AgentHarness`,
//! its own flume event channel + iodilos `spawn_local` pump. Events route
//! naturally — a harness is subscribed to exactly its layer's channel — so
//! exit is a plain teardown (unsubscribe → drop sender → pop).
//!
//! [`RuntimeControl`] is the iodilos-side capability handed to `/btw`'s
//! command handler (the hybrid handler model in `core::extensions`). It is
//! `Rc`-held and never crosses threads; it drives push/pop/send on the stack.

use std::rc::Rc;
use std::sync::Arc;

use flown_agent::harness::env::types::ExecutionEnv;
use flown_agent::harness::{AgentHarness, GetApiKeyAndHeadersFn};
use flown_agent::types::AgentTool;
use flown_ai::types::Model;
use iodilos::prelude::*;

use super::state::UiState;
use crate::config::Config;
use crate::core::extensions::ControlRuntime;

/// Which kind of conversation a layer holds. Determines exit/discard semantics.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerKind {
    /// The root conversation. Never popped.
    Main,
    /// A temporary side-conversation forked from main. Discarded on exit.
    Btw,
}

/// One conversation layer: an independent transcript + harness + event stream.
///
/// All fields are cheap-clone handles (`Rc`/`Arc`/`Sender`) so the struct
/// itself is `Clone` for moving into `spawn_local` closures. The teardown
/// token (`unsubscribe`) detaches the harness subscriber; `event_tx` dropping
/// ends the pump task.
pub struct ConversationLayer {
    pub kind: LayerKind,
    pub state: Rc<UiState>,
    /// `None` in session-only mode (no LLM). A btw layer built without a
    /// factory also ends up here and `/btw <msg>` surfaces an error instead.
    pub harness: Option<Arc<AgentHarness>>,
    pub event_tx: flume::Sender<flown_agent::harness::HarnessEvent>,
    /// Detaches the harness subscriber registered for this layer. Called on
    /// teardown before dropping `event_tx`. `None` for session-only layers.
    pub unsubscribe: Option<Box<dyn Fn()>>,
}

/// The conversation stack, shared via iodilos context. Components read
/// [`Self::active`] to get the visible layer; `/btw` drives it via
/// [`RuntimeControl`].
///
/// The btw factory is held as `Arc` (not `Rc`) because it must cross into a
/// `tokio::spawn` to build the harness; everything else is iodilos-local `Rc`.
pub struct ConversationStack {
    layers: RwSignal<Vec<Rc<ConversationLayer>>>,
    active_index: RwSignal<usize>,
    /// How to build a btw harness; `None` in session-only mode. `Arc` so it can
    /// be moved into a tokio task inside `enter_btw`.
    btw_factory: RwSignal<Option<Arc<BtwFactory>>>,
}

impl ConversationStack {
    /// Build a stack with a single Main layer. The Main layer's harness,
    /// event sender, and unsubscribe token come from `runtime.rs`'s mount
    /// (they're wired exactly as the old single-layer setup was).
    pub fn new(main: ConversationLayer, btw_factory: Option<Arc<BtwFactory>>) -> Rc<Self> {
        Rc::new(Self {
            layers: create_rw_signal(vec![Rc::new(main)]),
            active_index: create_rw_signal(0),
            btw_factory: create_rw_signal(btw_factory),
        })
    }

    /// The currently visible layer.
    pub fn active(&self) -> Rc<ConversationLayer> {
        let idx = self.active_index.get();
        self.layers.with(|ls| ls[idx].clone())
    }

    /// The Main layer (always index 0).
    pub fn main_layer(&self) -> Rc<ConversationLayer> {
        self.layers.with(|ls| ls[0].clone())
    }

    /// Whether the active layer is a btw layer.
    pub fn active_is_btw(&self) -> bool {
        self.active().kind == LayerKind::Btw
    }

    /// Current depth (1 = main only).
    pub fn depth(&self) -> usize {
        self.layers.with(|ls| ls.len())
    }

    /// Push a new layer and make it active. Returns its index.
    fn push(&self, layer: ConversationLayer) -> usize {
        let idx = self.layers.with(|ls| ls.len());
        self.layers.update(|ls| ls.push(Rc::new(layer)));
        self.active_index.set(idx);
        idx
    }

    /// Pop the active layer if it is a btw layer, returning it for teardown.
    /// Restores `active_index` to the previous layer. Returns `None` if the
    /// active layer is Main (never popped) or the stack would be emptied.
    fn pop_active(&self) -> Option<Rc<ConversationLayer>> {
        use std::cell::Cell;
        let idx = self.active_index.get();
        if idx == 0 {
            return None;
        }
        // RwSignal::update returns (), so capture the popped value via a Cell.
        let popped: Cell<Option<Rc<ConversationLayer>>> = Cell::new(None);
        let popped_ref = &popped;
        self.layers.update(|ls| {
            popped_ref.set(ls.pop());
        });
        let layer = popped.into_inner()?;
        // Only btw layers are poppable. (Main is index 0, guarded above.)
        self.active_index.set(idx.saturating_sub(1));
        Some(layer)
    }

    /// The btw factory, when present.
    pub fn btw_factory(&self) -> Option<Arc<BtwFactory>> {
        self.btw_factory.get()
    }
}

/// The tokio-side recipe for building a btw harness. Captured once at
/// bootstrap and cloned into each `enter_btw` call; inert data until used.
///
/// All fields are `Arc`/`Clone` so the factory is `Send + Sync` and can live
/// on the iodilos side (it only ever runs its async work inside a
/// `tokio::spawn`).
pub struct BtwFactory {
    pub model: Model,
    pub env: Arc<dyn ExecutionEnv>,
    pub built_in_tools: Vec<AgentTool>,
    pub system_prompt: String,
    pub api_key_fn: GetApiKeyAndHeadersFn,
    /// Read access to the main harness's session, for forking history.
    pub main_harness: Arc<AgentHarness>,
}

impl BtwFactory {
    /// Build a fresh btw harness: an in-memory session seeded with a fork of
    /// the main session's current branch, wrapped in a new `AgentHarness`.
    /// Runs on tokio (async). Returns only the harness — the iodilos bridge
    /// registers the harness's event subscriber (and keeps its unsubscribe
    /// token) after receiving it, because the event channel is created on the
    /// iodilos side.
    pub async fn build(&self) -> anyhow::Result<Arc<AgentHarness>> {
        use flown_agent::harness::session::{
            InMemorySessionRepo, MemorySessionCreateOptions, SessionRepo,
        };

        // Fork: copy the main session's current branch entries into a fresh
        // in-memory session. In-memory → discarded when the harness drops.
        let source = self.main_harness.session();
        let entries = source.get_branch(None).await;
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(MemorySessionCreateOptions { id: None })
            .await
            .map_err(|e| anyhow::anyhow!("btw session create failed: {e}"))?;
        for entry in &entries {
            session.storage().append_entry(entry.clone()).await;
        }

        let harness = AgentHarness::new(flown_agent::harness::AgentHarnessOptions {
            env: self.env.clone(),
            session,
            tools: self.built_in_tools.clone(),
            system_prompt: flown_agent::harness::SystemPromptConfig::Static(
                self.system_prompt.clone(),
            ),
            model: self.model.clone(),
            thinking_level: Some(flown_ai::types::ThinkingLevel::Off),
            get_api_key_and_headers: Some(self.api_key_fn.clone()),
            resources: None,
            stream_options: None,
            active_tool_names: None,
            steering_mode: None,
            follow_up_mode: None,
        });
        Ok(Arc::new(harness))
    }
}

/// The iodilos-side capability for `/btw`. Implements [`ControlRuntime`]; held
/// by `CommandSide` and called by the `/btw` control handler.
///
/// All operations act on [`ConversationStack::active`] — after `enter_btw`,
/// "active" is the new btw layer; after `exit_btw`, it's back to main. This
/// structurally enforces the stale-ctx discipline (a captured `Rc<UiState>`
/// is simply not used post-switch; everything goes through the stack).
pub struct RuntimeControl {
    stack: Rc<ConversationStack>,
    #[allow(dead_code)]
    config: Config,
}

impl RuntimeControl {
    pub fn new(stack: Rc<ConversationStack>, config: Config) -> Rc<Self> {
        Rc::new(Self { stack, config })
    }

    /// Access the underlying stack (for `app.rs` to query `active_is_btw`).
    pub fn stack(&self) -> &Rc<ConversationStack> {
        &self.stack
    }
}

impl ControlRuntime for RuntimeControl {
    fn enter_btw(&self, prompt: Option<String>) {
        let stack = self.stack.clone();

        // Build the event channel on the iodilos side; the harness (built on
        // tokio) will forward its events into event_tx via a subscriber.
        let (event_tx, event_rx) = flume::unbounded::<flown_agent::harness::HarnessEvent>();
        // Build channel: single-use, carries the built harness back from the
        // tokio builder task to this iodilos bridge. Flume (not oneshot) so the
        // bridge reuses the same recv_async().await-on-spawn_local pattern the
        // existing event pump proves (runtime.rs:216).
        let (build_tx, build_rx) = flume::unbounded::<BtwBuildResult>();

        let factory = match stack.btw_factory() {
            Some(f) => f,
            None => {
                stack.active().state.push_error(
                    "No LLM agent available. Cannot start a btw conversation.".to_string(),
                );
                return;
            }
        };

        // Depth guard: v1 rejects nesting btw-within-btw.
        if stack.active_is_btw() {
            stack
                .active()
                .state
                .push_system("Already in a btw conversation. Exit (Ctrl+C) before starting another.".to_string());
            return;
        }

        // 1. Tokio task: fork session + build harness, ship it back.
        let factory_for_task = factory.clone();
        tokio::spawn(async move {
            let result = match factory_for_task.build().await {
                Ok(harness) => BtwBuildResult::Ok(harness),
                Err(e) => BtwBuildResult::Err(e.to_string()),
            };
            let _ = build_tx.send(result);
        });

        // 2. Iodilos bridge: await the harness, then finish wiring the layer.
        let stack_for_bridge = stack.clone();
        let event_tx_for_sub = event_tx.clone();
        spawn_local(async move {
            let build = match build_rx.recv_async().await {
                Ok(b) => b,
                Err(_) => {
                    stack_for_bridge
                        .main_layer()
                        .state
                        .push_error("btw build channel closed unexpectedly.".to_string());
                    return;
                }
            };
            let (harness, unsubscribe) = match build {
                BtwBuildResult::Ok(h) => {
                    // Register the harness subscriber → forward events into
                    // this layer's event channel. Keep the unsubscribe token.
                    let tx = event_tx_for_sub.clone();
                    let unsub = h.subscribe(move |event: &flown_agent::harness::HarnessEvent| {
                        let _ = tx.send(event.clone());
                    });
                    (Some(h), Some(unsub))
                }
                BtwBuildResult::Err(msg) => {
                    stack_for_bridge
                        .main_layer()
                        .state
                        .push_error(format!("Could not start btw: {msg}"));
                    return;
                }
            };

            // 3. New UiState + event pump for the btw layer.
            let btw_state = Rc::new(UiState::new(TextAreaState::default()));
            let pump_state = Rc::clone(&btw_state);
            spawn_local(async move {
                let mut accumulated_text = String::new();
                let mut in_thinking = false;
                while let Ok(event) = event_rx.recv_async().await {
                    crate::tui::runtime::translate_event(
                        event,
                        &pump_state,
                        &mut accumulated_text,
                        &mut in_thinking,
                    );
                }
            });

            // 4. Push the layer; UI switches to it.
            let layer = ConversationLayer {
                kind: LayerKind::Btw,
                state: btw_state.clone(),
                harness: harness.clone(),
                event_tx,
                unsubscribe: unsubscribe.map(|f| Box::new(f) as Box<dyn Fn()>),
            };
            stack_for_bridge.push(layer);

            // 5. Optionally submit the prompt.
            if let Some(prompt) = prompt {
                if let Some(h) = &harness {
                    btw_state.push_user(&prompt);
                    btw_state.busy.set(true);
                    btw_state.status.update(|s| s.busy = true);
                    let h = Arc::clone(h);
                    tokio::spawn(async move {
                        let _ = h.prompt(&prompt, None).await;
                    });
                } else {
                    btw_state.push_error("No LLM agent available. Check your config.".to_string());
                }
            }
        });
    }

    fn exit_btw(&self) {
        let stack = self.stack.clone();
        // pop_active restores active_index to the previous layer.
        let Some(layer) = stack.pop_active() else {
            return;
        };
        // Teardown order is load-bearing: unsubscribe FIRST so the harness
        // stops emitting, then drop event_tx so the pump observes "all senders
        // gone" and exits, then abort any in-flight turn.
        if let Some(unsub) = &layer.unsubscribe {
            (unsub)();
        }
        if let Some(h) = &layer.harness {
            let h = Arc::clone(h);
            tokio::spawn(async move {
                let _ = h.abort().await;
            });
        }
        // Drop the layer last; its `event_tx` goes with it, ending the pump.
        drop(layer);
    }

    fn send_to_active(&self, text: String) {
        let layer = self.stack.active();
        if layer.state.busy.get() {
            return;
        }
        layer.state.push_user(&text);
        if let Some(h) = &layer.harness {
            layer.state.busy.set(true);
            layer.state.status.update(|s| s.busy = true);
            let h = Arc::clone(h);
            tokio::spawn(async move {
                let _ = h.prompt(&text, None).await;
            });
        } else {
            layer.state.push_error("No LLM agent available. Check your config.".to_string());
        }
    }

    fn notify_active(&self, text: String) {
        self.stack.active().state.push_system(text);
    }

    fn notify_error_active(&self, text: String) {
        self.stack.active().state.push_error(text);
    }

    fn clear_active(&self) {
        self.stack.active().state.clear();
    }

    fn active_is_btw(&self) -> bool {
        self.stack.active_is_btw()
    }
}

/// The result shipped over the btw build channel: a built harness or an error.
enum BtwBuildResult {
    Ok(Arc<AgentHarness>),
    Err(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A main-only stack reports depth 1 and active == main.
    #[test]
    fn stack_starts_with_main_only() {
        let (tx, _rx) = flume::unbounded();
        let main = ConversationLayer {
            kind: LayerKind::Main,
            state: Rc::new(UiState::new(TextAreaState::default())),
            harness: None,
            event_tx: tx,
            unsubscribe: None,
        };
        let stack = ConversationStack::new(main, None);
        assert_eq!(stack.depth(), 1);
        assert!(!stack.active_is_btw());
        assert_eq!(stack.active().kind, LayerKind::Main);
    }

    /// pop_active on a main-only stack is a no-op (Main is never popped).
    #[test]
    fn cannot_pop_main() {
        let (tx, _rx) = flume::unbounded();
        let main = ConversationLayer {
            kind: LayerKind::Main,
            state: Rc::new(UiState::new(TextAreaState::default())),
            harness: None,
            event_tx: tx,
            unsubscribe: None,
        };
        let stack = ConversationStack::new(main, None);
        assert!(stack.pop_active().is_none());
        assert_eq!(stack.depth(), 1);
    }
}
