//! The conversation stack — multi-layer transcript support for extension overlaps.
//!
//! Replaces the single `Rc<UiState>` as the thing TUI components read. The
//! stack always has a `Main` layer (never popped); extensions may open an
//! overlap that forks the main session's history into a transient in-memory
//! session, runs concurrently, and is discarded on close.
//!
//! Each layer is fully isolated: its own `UiState`, its own `AgentHarness`,
//! its own flume event channel + iodilos `spawn_local` pump. Events route
//! naturally — a harness is subscribed to exactly its layer's channel — so
//! exit is a plain teardown (unsubscribe → drop sender → pop).
//!
//! [`RuntimeControl`] is the iodilos-side runtime command interpreter. It is
//! `Rc`-held and never crosses threads; extension-facing command proxies send
//! requests back to this owner thread.

use std::rc::Rc;
use std::sync::Arc;

use flown_agent::{
    AgentHarness, AgentHarnessEvent, AgentTool, ExecutionEnv, GetApiKeyAndHeadersFn,
};
use flown_ai::Model;
use iodilos::prelude::*;

use super::state::UiState;
use crate::config::Config;
use crate::core::extensions::{OverlapOptions, SlashCommandScope};
use crate::tui::editor::EditorState;

/// Which kind of conversation a layer holds. Determines exit/discard semantics.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerKind {
    /// The root conversation. Never popped.
    Main,
    /// An extension-owned overlap rendered above Main. Discarded on close.
    Overlap,
}

#[derive(Clone, Debug)]
pub struct OverlapMeta {
    pub extension_id: String,
    pub badge: Option<String>,
    pub single_instance_key: Option<String>,
    pub dismissible: bool,
    pub slash_commands: SlashCommandScope,
}

impl From<&OverlapOptions> for OverlapMeta {
    fn from(options: &OverlapOptions) -> Self {
        Self {
            extension_id: options.extension_id.clone(),
            badge: options.badge.clone(),
            single_instance_key: options.single_instance_key.clone(),
            dismissible: options.dismissible,
            slash_commands: options.slash_commands,
        }
    }
}

/// One conversation layer: an independent transcript + harness + event stream.
///
/// All fields are cheap-clone handles (`Rc`/`Arc`/`Sender`) so the struct
/// itself is `Clone` for moving into `spawn_local` closures. The teardown
/// token (`unsubscribe`) detaches the harness subscriber; `event_tx` dropping
/// ends the pump task; `cmd_tx` drops to shut down the layer's driver task.
pub struct ConversationLayer {
    pub kind: LayerKind,
    pub overlap: Option<OverlapMeta>,
    pub state: Rc<UiState>,
    /// `None` in session-only mode (no LLM). An overlap built without a factory
    /// also ends up here and its extension surfaces an error instead.
    pub harness: Option<Arc<AgentHarness>>,
    pub event_tx: flume::Sender<AgentHarnessEvent>,
    /// Detaches the harness subscriber registered for this layer. Called on
    /// teardown before dropping `event_tx`. `None` for session-only layers.
    pub unsubscribe: Option<Box<dyn Fn()>>,
    /// Send side of the per-layer command channel. The driver task owns the
    /// receiver and awaits each command directly (no per-prompt `tokio::spawn`).
    /// `None` for session-only layers (no harness → no driver). Closing an
    /// overlap sends `Shutdown` here, then drops this sender so the driver
    /// observes "all senders gone" and exits deterministically.
    pub cmd_tx: Option<tokio::sync::mpsc::Sender<LayerCommand>>,
}

impl ConversationLayer {
    /// Queue `text` as a user prompt for this layer's driver to await. Called
    /// from the iodilos `on_key` thread; never blocks (uses `try_send`). The
    /// caller is responsible for setting busy state on the layer's UiState
    /// before calling (only when `Queued` is returned).
    pub(crate) fn submit_prompt(&self, text: String) -> SubmitOutcome {
        match &self.cmd_tx {
            Some(tx) => match tx.try_send(LayerCommand::Prompt(text)) {
                Ok(()) => SubmitOutcome::Queued,
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => SubmitOutcome::DriverGone,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => SubmitOutcome::ChannelFull,
            },
            None => SubmitOutcome::NoAgent,
        }
    }
}

/// Result of queuing a prompt for a layer's driver.
pub(crate) enum SubmitOutcome {
    /// The prompt was queued; the driver will run it.
    Queued,
    /// No harness/driver on this layer (session-only mode).
    NoAgent,
    /// The driver has exited; the prompt cannot run.
    DriverGone,
    /// The command channel is full (driver wedged). The prompt was dropped.
    ChannelFull,
}

/// A command queued by the iodilos side for the layer's tokio driver to await.
///
/// This is the iodilos→tokio bridge: `on_key` (sync, iodilos thread) only
/// `try_send`s a command; the driver task (tokio, owns the harness `Arc`)
/// receives it and runs the turn inline with `prompt().await`. The driver is
/// single-threaded per layer, so turns never overlap — matching the harness's
/// `phase == Idle` single-flight guarantee.
pub(crate) enum LayerCommand {
    /// Submit a user prompt as a new agent turn.
    Prompt(String),
    /// Stop the driver after the current turn (if any) unwinds. Sent on
    /// overlap close; the driver runs `abort().await` first so the in-flight
    /// turn (if any) is interrupted cleanly, then breaks the loop.
    Shutdown,
}

/// Capacity for the per-layer command channel. `on_key` submits at most one
/// command per keypress; 8 is ample headroom and keeps `try_send` from ever
/// blocking on a full buffer.
const LAYER_CMD_CHANNEL_CAPACITY: usize = 8;

/// The complete harness↔transcript binding, produced atomically by
/// `bind_layer_driver`.
///
/// Design invariant: **one `AgentHarness` binds to exactly one transcript (its
/// `event_rx` → pump → `UiState`) or is silent. Two harnesses never share a
/// transcript, and one harness never feeds two transcripts.** This struct is
/// the embodiment of that invariant: it is created in a single atomic call
/// (`bind_layer_driver`) that wires the harness to exactly one flume channel,
/// and its parts move together onto one `ConversationLayer`. The driver task
/// owns the harness `Arc`; when the driver exits (after `Shutdown`) it drops.
pub(crate) struct LayerBinding {
    /// A clone of the bound harness, for the layer's defensive use (force-abort
    /// on a wedged command channel). The driver holds its own clone and is the
    /// authoritative owner; this is a second strong ref dropped with the layer.
    pub harness: Arc<AgentHarness>,
    /// Send side of the driver's command channel. `on_key` `try_send`s here.
    pub cmd_tx: tokio::sync::mpsc::Sender<LayerCommand>,
    /// Send side of the harness→pump event channel. Stored on the layer so the
    /// pump can be torn down by dropping it.
    pub event_tx: flume::Sender<AgentHarnessEvent>,
    /// Receive side of the event channel. Consumed by the pump task the caller
    /// spawns (on iodilos) to feed its `UiState`.
    pub event_rx: flume::Receiver<AgentHarnessEvent>,
    /// Detaches the harness subscriber. Stored on the layer; called on teardown
    /// before dropping `event_tx`.
    pub unsubscribe: Box<dyn Fn()>,
}

/// Atomically bind a harness to a transcript and start its driver.
///
/// This is the **single** place a harness is wired to the UI. In one call:
///   1. a fresh `flume::unbounded` event channel is created (this harness's
///      one and only transcript pipe);
///   2. the harness subscribes to forward into that channel — the returned
///      unsubscribe token is held by the driver, so the binding lives exactly
///      as long as the driver;
///   3. the driver task is spawned (tokio) owning the harness `Arc`;
///   4. if `initial_prompt` is `Some`, it is queued on the driver's priority
///      channel so it runs before any user input.
///
/// The caller then spawns the event pump (iodilos `spawn_local`) feeding its
/// `UiState` from the returned `event_rx`. Because steps 1–3 are atomic, there
/// is no window where the harness is alive but unbound, or bound to the wrong
/// channel. The harness↔transcript link is structural, not conventional.
pub(crate) fn bind_layer_driver(
    harness: Arc<AgentHarness>,
    initial_prompt: Option<String>,
) -> LayerBinding {
    let (event_tx, event_rx) = flume::unbounded();

    // Subscribe the harness to forward events into THIS channel only. The
    // unsubscribe token returns to the caller (recorded on the layer) so the
    // teardown path can stop emissions before dropping event_tx. The binding's
    // atomicity guarantee still holds: the channel, subscriber and driver are
    // all created in this one call, so there is never a window where the
    // harness is wired to a different transcript.
    let sub_tx = event_tx.clone();
    let unsubscribe = harness.subscribe(move |event, _signal| {
        let sub_tx = sub_tx.clone();
        Box::pin(async move {
            let _ = sub_tx.send(event);
        })
    });

    // Command channel + optional initial prompt on a priority rx.
    let (cmd_tx, mut cmd_rx) =
        tokio::sync::mpsc::channel::<LayerCommand>(LAYER_CMD_CHANNEL_CAPACITY);
    let (initial_tx, initial_rx) = tokio::sync::mpsc::channel::<LayerCommand>(1);
    if let Some(p) = initial_prompt {
        let _ = initial_tx.try_send(LayerCommand::Prompt(p));
    }
    // initial_tx drops here; the receiver observes "no senders" once the
    // initial prompt (if any) is drained, signalling the driver to stop
    // prioritising it.
    drop(initial_tx);

    let h = Arc::clone(&harness);
    tokio::spawn(async move {
        let mut initial_rx = Some(initial_rx);
        loop {
            // Receive the next command. Both the initial channel and the live
            // command channel are polled; initial is drained first (biased).
            let cmd = tokio::select! {
                biased;
                cmd = async {
                    match &mut initial_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match cmd {
                        Some(c) => c,
                        None => {
                            // initial channel exhausted: don't poll it again.
                            initial_rx = None;
                            continue;
                        }
                    }
                }
                cmd = cmd_rx.recv() => match cmd {
                    Some(c) => c,
                    None => break, // all senders dropped → exit
                }
            };

            match cmd {
                LayerCommand::Prompt(text) => {
                    tracing::info!(
                        target: "flown::driver",
                        text_len = text.len(),
                        "layer driver: prompt dispatched",
                    );
                    // Race the prompt against a concurrent Shutdown. If Shutdown
                    // arrives while the turn is in flight, abort() interrupts the
                    // in-flight stream (via its AbortSignal select branch) so
                    // prompt() returns and the driver can break. Without this
                    // race, a stuck stream would wedge the driver forever and
                    // close's Shutdown would never be observed.
                    tokio::select! {
                        biased;
                        // A Shutdown that lands during the turn: abort first
                        // (interrupts the stream), then stop the driver.
                        shutdown = recv_shutdown(&mut initial_rx, &mut cmd_rx) => {
                            tracing::info!(target: "flown::driver", "layer driver: shutdown during turn");
                            let _ = h.abort().await;
                            let _ = shutdown; // already handled
                            break;
                        }
                        result = h.prompt(&text, None) => {
                            match &result {
                                Ok(message) => tracing::info!(
                                    target: "flown::driver",
                                    stop_reason = ?message.stop_reason,
                                    "layer driver: prompt returned",
                                ),
                                Err(error) => tracing::warn!(
                                    target: "flown::driver",
                                    error = ?error,
                                    "layer driver: prompt returned error",
                                ),
                            }
                        }
                    }
                }
                LayerCommand::Shutdown => {
                    tracing::info!(target: "flown::driver", "layer driver: shutdown (idle)");
                    // No turn in flight; abort is a cheap no-op but keeps
                    // semantics uniform (clears queues, emits Abort).
                    let _ = h.abort().await;
                    break;
                }
            }
        }
    });

    LayerBinding {
        harness: Arc::clone(&harness),
        cmd_tx,
        event_tx,
        event_rx,
        unsubscribe,
    }
}

/// Wait for a `Shutdown` command on either channel while a turn is in flight.
///
/// Called inside the driver's `prompt() vs Shutdown` race. It polls both the
/// initial channel and the live command channel, ignoring any `Prompt` that
/// arrives (a Prompt during a running turn is a no-op — the harness is busy
/// and would reject it anyway) and returning once a `Shutdown` is observed or
/// both channels close. The returned `bool` is `true` when a `Shutdown` was
/// seen (the caller breaks), `false` when the channels closed without one.
async fn recv_shutdown(
    initial_rx: &mut Option<tokio::sync::mpsc::Receiver<LayerCommand>>,
    cmd_rx: &mut tokio::sync::mpsc::Receiver<LayerCommand>,
) -> bool {
    loop {
        let cmd = tokio::select! {
            biased;
            cmd = async {
                match initial_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => match cmd {
                Some(c) => Some(c),
                None => { *initial_rx = None; continue; }
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(c) => Some(c),
                None => return false, // all senders dropped
            },
        };
        match cmd {
            Some(LayerCommand::Shutdown) => return true,
            // Ignore a Prompt during a running turn; the harness is busy.
            Some(LayerCommand::Prompt(_)) => continue,
            None => return false,
        }
    }
}

/// The conversation stack, shared via iodilos context. Components read
/// [`Self::active`] to get the visible layer; extensions drive it via
/// [`RuntimeControl`].
///
/// The overlap factory is held as `Arc` (not `Rc`) because it must cross into a
/// `tokio::spawn` to build the harness; everything else is iodilos-local `Rc`.
pub struct ConversationStack {
    layers: Signal<Vec<Rc<ConversationLayer>>>,
    active_index: Signal<usize>,
    /// `Some(token)` while an overlap is either being built or already present.
    /// This closes the race where repeated extension commands can all pass the
    /// active-layer guard before the first async build pushes its layer.
    overlap_slot: Signal<Option<OverlapSlot>>,
    next_overlap_token: Signal<u64>,
    /// How to build an overlap harness; `None` in session-only mode. `Arc` so
    /// it can be moved into a tokio task inside `open_overlap`.
    overlap_factory: Signal<Option<Arc<AgentOverlapFactory>>>,
}

impl ConversationStack {
    /// Build a stack with a single Main layer. The Main layer's harness,
    /// event sender, and unsubscribe token come from `runtime.rs`'s mount
    /// (they're wired exactly as the old single-layer setup was).
    pub fn new(
        main: ConversationLayer,
        overlap_factory: Option<Arc<AgentOverlapFactory>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            layers: create_signal(vec![Rc::new(main)]),
            active_index: create_signal(0),
            overlap_slot: create_signal(None),
            next_overlap_token: create_signal(0),
            overlap_factory: create_signal(overlap_factory),
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

    /// Whether the active layer is an extension overlap.
    pub fn active_is_overlap(&self) -> bool {
        self.active().kind == LayerKind::Overlap
    }

    /// Whether an overlap is visible, present deeper in the stack, or still
    /// being built. Ctrl+C uses this so a pending overlap can be cancelled before
    /// its async build finishes.
    pub fn overlap_is_active_or_pending(&self) -> bool {
        self.overlap_slot.get_clone().is_some()
            || self
                .layers
                .with(|ls| ls.iter().any(|layer| layer.kind == LayerKind::Overlap))
    }

    pub fn active_overlap_meta(&self) -> Option<OverlapMeta> {
        self.active().overlap.clone()
    }

    pub fn active_overlap_badge(&self) -> Option<String> {
        self.active_overlap_meta().and_then(|meta| meta.badge)
    }

    pub fn active_slash_command_scope(&self) -> SlashCommandScope {
        self.active_overlap_meta()
            .map(|meta| meta.slash_commands)
            .unwrap_or_default()
    }

    pub fn active_overlap_is_dismissible(&self) -> bool {
        self.active_overlap_meta()
            .map(|meta| meta.dismissible)
            .unwrap_or(false)
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

    /// Pop the active layer if it is an overlap, returning it for teardown.
    /// Restores `active_index` to the previous layer. Returns `None` if the
    /// active layer is Main (never popped) or the stack would be emptied.
    fn pop_active(&self) -> Option<Rc<ConversationLayer>> {
        use std::cell::Cell;
        let idx = self.active_index.get();
        if idx == 0 {
            return None;
        }
        // Signal::update returns (), so capture the popped value via a Cell.
        let popped: Cell<Option<Rc<ConversationLayer>>> = Cell::new(None);
        let popped_ref = &popped;
        batch(|| {
            // Keep every reactive observer in a valid state. Effects re-run
            // after the batch, so they see active_index already pointing at the
            // previous layer and layers already missing the popped overlap.
            self.active_index.set(idx.saturating_sub(1));
            self.layers.update(|ls| {
                popped_ref.set(ls.pop());
            });
        });
        let layer = popped.into_inner()?;
        Some(layer)
    }

    /// Reserve the overlap slot and return its token. Returns `None` when an
    /// overlap already exists or a previous one is still building.
    fn reserve_overlap(&self, key: Option<String>) -> Option<u64> {
        if self.overlap_is_active_or_pending() {
            return None;
        }
        let token = self.next_overlap_token.get().wrapping_add(1);
        self.next_overlap_token.set(token);
        self.overlap_slot.set(Some(OverlapSlot { token, key }));
        Some(token)
    }

    fn is_overlap_token_current(&self, token: u64) -> bool {
        self.overlap_slot
            .get_clone()
            .map(|slot| slot.token == token)
            .unwrap_or(false)
    }

    fn release_overlap_token(&self, token: u64) {
        if self.is_overlap_token_current(token) {
            self.overlap_slot.set(None);
        }
    }

    /// Drop every overlap layer and return them for teardown. This is intentionally
    /// stronger than `pop_active`: if repeated extension commands managed to queue
    /// several layers in an older build, one exit returns the user to a clean
    /// Main stack and releases all transient harnesses.
    fn pop_all_overlap_layers(&self) -> Vec<Rc<ConversationLayer>> {
        let mut popped = Vec::new();
        batch(|| {
            self.active_index.set(0);
            self.overlap_slot.set(None);
            self.layers.update(|ls| {
                if ls.len() > 1 {
                    popped.extend(ls.drain(1..));
                }
            });
        });
        popped
    }

    /// The overlap factory, when present.
    pub fn overlap_factory(&self) -> Option<Arc<AgentOverlapFactory>> {
        self.overlap_factory.get_clone()
    }

    /// The active-layer index signal. Components read this inside an effect to
    /// track layer switches (e.g. overlap push / Ctrl+C pop): reading it
    /// registers the dependency, so the effect re-runs when the active layer
    /// changes. Without this, a component fixes its `state` at mount and never
    /// reacts to push/pop.
    pub fn active_index_signal(&self) -> Signal<usize> {
        self.active_index
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OverlapSlot {
    token: u64,
    #[allow(dead_code)]
    key: Option<String>,
}

/// The tokio-side recipe for building an overlap harness. Captured once at
/// bootstrap and cloned into each `open_overlap` call; inert data until used.
///
/// All fields are `Arc`/`Clone` so the factory is `Send + Sync` and can live
/// on the iodilos side (it only ever runs its async work inside a
/// `tokio::spawn`).
pub struct AgentOverlapFactory {
    pub model: Model,
    pub env: Arc<dyn ExecutionEnv>,
    pub built_in_tools: Vec<AgentTool>,
    pub system_prompt: String,
    pub api_key_fn: GetApiKeyAndHeadersFn,
    /// Read access to the main harness's session, for forking history.
    pub main_harness: Arc<AgentHarness>,
}

impl AgentOverlapFactory {
    /// Build a fresh overlap harness: an in-memory session seeded with a fork of
    /// the main session's current branch, wrapped in a new `AgentHarness`.
    /// Runs on tokio (async). Returns only the harness — the iodilos bridge
    /// registers the harness's event subscriber (and keeps its unsubscribe
    /// token) after receiving it, because the event channel is created on the
    /// iodilos side.
    pub async fn build(&self) -> anyhow::Result<Arc<AgentHarness>> {
        use flown_agent::{InMemorySessionRepo, MemorySessionCreateOptions, SessionRepo};

        // Fork: copy the main session's current branch entries into a fresh
        // in-memory session. In-memory → discarded when the harness drops.
        let source = self.main_harness.session();
        let entries = source.get_branch(None).await;
        let repo = InMemorySessionRepo::new();
        let session = repo
            .create(MemorySessionCreateOptions { id: None })
            .await
            .map_err(|e| anyhow::anyhow!("overlap session create failed: {e}"))?;
        for entry in &entries {
            session.storage().append_entry(entry.clone()).await;
        }

        let harness = AgentHarness::new(flown_agent::AgentHarnessOptions {
            env: self.env.clone(),
            session,
            tools: self.built_in_tools.clone(),
            system_prompt: flown_agent::SystemPromptConfig::Static(self.system_prompt.clone()),
            model: self.model.clone(),
            thinking_level: Some(flown_ai::ThinkingLevel::Off),
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

/// The iodilos-side runtime command interpreter.
///
/// All operations act on [`ConversationStack::active`] — after `open_overlap`,
/// "active" is the new overlap; after close, it's back to main. This
/// structurally enforces the stale-ctx discipline (a captured `Rc<UiState>`
/// is simply not used post-switch; everything goes through the stack).
pub struct RuntimeControl {
    stack: Rc<ConversationStack>,
    /// The single-overlay stack model: `/model` pushes an inset overlay; btw
    /// forks push a full-bleed overlay. Depth is 1 in v1.
    overlay_stack: Rc<crate::tui::overlay_stack::OverlayStack>,
    harness: Option<Arc<AgentHarness>>,
    #[allow(dead_code)]
    config: Config,
}

impl RuntimeControl {
    pub fn new(
        stack: Rc<ConversationStack>,
        overlay_stack: Rc<crate::tui::overlay_stack::OverlayStack>,
        harness: Option<Arc<AgentHarness>>,
        config: Config,
    ) -> Rc<Self> {
        Rc::new(Self {
            stack,
            overlay_stack,
            harness,
            config,
        })
    }
}

impl RuntimeControl {
    pub fn open_overlap(&self, options: OverlapOptions) {
        tracing::info!(extension = %options.extension_id, "open overlap");
        let stack = self.stack.clone();
        let initial_prompt = options.initial_prompt.clone();
        let overlap_meta = OverlapMeta::from(&options);

        // Build channel: single-use, carries the built harness back from the
        // tokio builder task to this iodilos bridge. Flume (not oneshot) so the
        // bridge reuses the same recv_async().await-on-spawn_local pattern the
        // existing event pump proves (runtime.rs:216).
        let (build_tx, build_rx) = flume::unbounded::<OverlapBuildResult>();

        let factory = match stack.overlap_factory() {
            Some(f) => f,
            None => {
                stack
                    .active()
                    .state
                    .push_error("No LLM agent available. Cannot start overlap.".to_string());
                return;
            }
        };

        // Single-instance guard, including the async build window before the
        // overlap has been pushed.
        let Some(overlap_token) = stack.reserve_overlap(options.single_instance_key.clone()) else {
            stack
                .active()
                .state
                .push_system("An overlap is already active. Close it before opening another.");
            return;
        };
        tracing::info!(extension = %options.extension_id, token = overlap_token, "overlap slot reserved");

        // 1. Tokio task: fork session + build harness, ship it back.
        let factory_for_task = factory.clone();
        tokio::spawn(async move {
            let result = match factory_for_task.build().await {
                Ok(harness) => OverlapBuildResult::Ok(harness),
                Err(e) => OverlapBuildResult::Err(e.to_string()),
            };
            let _ = build_tx.send(result);
        });

        // 2. Iodilos bridge: await the harness, then atomically bind it to a
        // fresh transcript and start its driver.
        let extension_id = options.extension_id.clone();
        let stack_for_bridge = stack.clone();
        use_future(async move {
            let build = match build_rx.recv_async().await {
                Ok(b) => b,
                Err(_) => {
                    stack_for_bridge.release_overlap_token(overlap_token);
                    stack_for_bridge
                        .main_layer()
                        .state
                        .push_error("overlap build channel closed unexpectedly.".to_string());
                    return;
                }
            };
            if !stack_for_bridge.is_overlap_token_current(overlap_token) {
                tracing::info!(
                    target: "flown::overlap",
                    extension = %extension_id,
                    token = overlap_token,
                    "overlap build finished after cancellation; dropping layer"
                );
                return;
            }

            // 3. New UiState for the overlap layer. Seed its status snapshot
            // from the main layer so the status line keeps showing
            // model/provider/cwd/git (only busy/spinner differs as the turn runs).
            let overlap_state = Rc::new(UiState::new(EditorState::default()));
            let main_status = stack_for_bridge.main_layer().state.status.get_clone();
            overlap_state.status.set(main_status);

            // Bind harness → transcript atomically (channel + subscriber + driver
            // in one call) or surface a build error. This is the single place an
            // overlap harness is wired to the UI, so the one-harness-one-transcript
            // invariant is structural here.
            let binding = match build {
                OverlapBuildResult::Ok(h) => Some(bind_layer_driver(h, initial_prompt.clone())),
                OverlapBuildResult::Err(msg) => {
                    stack_for_bridge.release_overlap_token(overlap_token);
                    stack_for_bridge
                        .main_layer()
                        .state
                        .push_error(format!("Could not start overlap: {msg}"));
                    return;
                }
            };

            // The driver owns the initial prompt now; mark busy so the UI shows
            // the turn in progress when the layer is pushed.
            if let Some(prompt) = &initial_prompt {
                if binding.is_some() {
                    overlap_state.push_user(prompt);
                    overlap_state.busy.set(true);
                    overlap_state.status.update(|s| s.busy = true);
                } else {
                    overlap_state
                        .push_error("No LLM agent available. Check your config.".to_string());
                }
            }

            // Unpack the binding (Some whenever we got here with a harness).
            let LayerBinding {
                harness,
                cmd_tx,
                event_tx,
                event_rx,
                unsubscribe,
            } = match binding {
                Some(b) => b,
                None => return,
            };
            let pump_state = Rc::clone(&overlap_state);
            let pump_extension_id = extension_id.clone();
            use_future(async move {
                let mut accumulated_text = String::new();
                let mut in_thinking = false;
                while let Ok(event) = event_rx.recv_async().await {
                    log_overlap_pump_event(&pump_extension_id, &event);
                    log_overlap_state(
                        &pump_extension_id,
                        "before",
                        &event,
                        &pump_state,
                        accumulated_text.len(),
                        in_thinking,
                    );
                    crate::tui::runtime::translate_event(
                        event,
                        &pump_state,
                        &mut accumulated_text,
                        &mut in_thinking,
                    );
                    log_overlap_state_after_translate(
                        &pump_extension_id,
                        &pump_state,
                        accumulated_text.len(),
                        in_thinking,
                    );
                }
            });

            // 4. Push the layer; UI switches to it. The binding's parts all
            // ride on the layer: cmd_tx (for close's Shutdown), event_tx (its
            // drop ends the pump), unsubscribe (stops emissions), and the
            // harness Arc (held so close can force-abort on a wedged channel).
            // The initial prompt — if any — was already queued to the driver's
            // priority `initial_rx` during bind, so there is nothing to spawn.
            let layer = ConversationLayer {
                kind: LayerKind::Overlap,
                overlap: Some(overlap_meta),
                state: overlap_state.clone(),
                harness: Some(harness),
                event_tx,
                unsubscribe: Some(unsubscribe),
                cmd_tx: Some(cmd_tx),
            };
            stack_for_bridge.push(layer);
        });
    }

    /// Fork the main session into a transient full-bleed overlay (btw). The
    /// fork owns its own `UiState` and driver; the bottom App surface remains
    /// mounted on the main conversation while the overlay view renders
    /// transcript/status/input for the fork.
    ///
    /// `reply` is answered once the overlay is up (or with an error if the
    /// build fails or another overlay is already active).
    pub fn fork_conversation(
        &self,
        initial_prompt: Option<String>,
        reply: tokio::sync::oneshot::Sender<crate::core::extensions::types::CommandResult>,
    ) {
        let stack = self.stack.clone();
        let overlay_stack = Rc::clone(&self.overlay_stack);
        let config = self.config.clone();
        if overlay_stack.is_active() {
            stack
                .active()
                .state
                .push_system("An overlay is already active. Close it before opening another.");
            let _ = reply.send(Ok(()));
            return;
        }

        let factory = match stack.overlap_factory() {
            Some(f) => f,
            None => {
                stack
                    .active()
                    .state
                    .push_error("No LLM agent available. Cannot fork conversation.".to_string());
                let _ = reply.send(Ok(()));
                return;
            }
        };

        // Build channel (single-use): carries the forked harness back from tokio.
        let (build_tx, build_rx) = flume::unbounded::<OverlapBuildResult>();

        // 1. Tokio task: fork session + build harness, ship it back.
        let factory_for_task = factory.clone();
        tokio::spawn(async move {
            let result = match factory_for_task.build().await {
                Ok(harness) => OverlapBuildResult::Ok(harness),
                Err(e) => OverlapBuildResult::Err(e.to_string()),
            };
            let _ = build_tx.send(result);
        });

        // 2. Iodilos bridge: await the harness, bind it, and push the overlay.
        let prompt_for_bridge = initial_prompt.clone();
        use_future(async move {
            let build = match build_rx.recv_async().await {
                Ok(b) => b,
                Err(_) => {
                    stack
                        .main_layer()
                        .state
                        .push_error("fork build channel closed unexpectedly.".to_string());
                    let _ = reply.send(Ok(()));
                    return;
                }
            };

            let binding = match build {
                OverlapBuildResult::Ok(h) => bind_layer_driver(h, prompt_for_bridge.clone()),
                OverlapBuildResult::Err(msg) => {
                    stack
                        .main_layer()
                        .state
                        .push_error(format!("Could not fork conversation: {msg}"));
                    let _ = reply.send(Ok(()));
                    return;
                }
            };

            // Fresh UiState for the fork; seed status from the main layer so the
            // status line keeps showing model/provider/cwd/git.
            let fork_state = Rc::new(UiState::new(EditorState::default()));
            let main_status = stack.main_layer().state.status.get_clone();
            fork_state.status.set(main_status);
            if let Some(prompt) = &prompt_for_bridge {
                fork_state.push_user(prompt);
                fork_state.busy.set(true);
                fork_state.status.update(|s| s.busy = true);
            }

            // 3. Event pump feeding the forked UiState (identical to overlap).
            let LayerBinding {
                harness,
                cmd_tx,
                event_tx,
                event_rx,
                unsubscribe,
            } = binding;
            let pump_state = Rc::clone(&fork_state);
            use_future(async move {
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

            let layer = Rc::new(ConversationLayer {
                kind: LayerKind::Overlap,
                overlap: Some(OverlapMeta {
                    extension_id: "btw".to_string(),
                    badge: Some("BTW".to_string()),
                    single_instance_key: Some("btw".to_string()),
                    dismissible: true,
                    slash_commands: SlashCommandScope::Disabled,
                }),
                state: Rc::clone(&fork_state),
                harness: Some(harness),
                event_tx,
                unsubscribe: Some(unsubscribe),
                cmd_tx: Some(cmd_tx),
            });

            let submit_layer = Rc::clone(&layer);
            let submit: Rc<dyn Fn(String)> = Rc::new(move |text: String| {
                if submit_layer.state.busy.get() {
                    return;
                }
                submit_layer.state.push_user(&text);
                tracing::info!(
                    target: "flown::prompt",
                    layer = "btw-overlay",
                    text_len = text.len(),
                    "ui prompt submitted"
                );
                match submit_layer.submit_prompt(text) {
                    SubmitOutcome::Queued => {
                        submit_layer.state.busy.set(true);
                        submit_layer.state.status.update(|s| s.busy = true);
                    }
                    SubmitOutcome::NoAgent => {
                        submit_layer
                            .state
                            .push_error("No LLM agent available. Check your config.");
                    }
                    SubmitOutcome::DriverGone => {
                        submit_layer
                            .state
                            .push_error("Layer driver exited. Cannot send prompt.");
                    }
                    SubmitOutcome::ChannelFull => {
                        submit_layer
                            .state
                            .push_error("Layer driver busy (command queue full).");
                    }
                }
            });

            let close: Rc<dyn Fn()> = Rc::new({
                let overlay_stack = Rc::clone(&overlay_stack);
                move || overlay_stack.pop()
            });
            let teardown: Rc<dyn Fn()> = Rc::new({
                let layer = Rc::clone(&layer);
                move || teardown_overlap_layers(vec![Rc::clone(&layer)])
            });
            let content_state = Rc::clone(&fork_state);
            let overlay = crate::tui::overlay_stack::ActiveOverlay {
                geometry: iodilos::OverlayGeometry::FullBleed,
                dismissible: true,
                route_app_keys: false,
                content: Rc::new(move || {
                    crate::tui::components::overlay_conversation::overlay_conversation(
                        crate::tui::components::overlay_conversation::OverlayConversationProps {
                            state: Rc::clone(&content_state),
                            tail_label: Some("btw".to_string()),
                        },
                    )
                }),
                on_key: Some(Rc::new({
                    let state = Rc::clone(&fork_state);
                    let config = config.clone();
                    let submit = Rc::clone(&submit);
                    let close = Rc::clone(&close);
                    move |key| {
                        crate::tui::components::overlay_conversation::handle_overlay_key(
                            key,
                            &state,
                            &config,
                            Rc::clone(&submit),
                            Rc::clone(&close),
                        )
                    }
                })),
                on_close: Some(Rc::clone(&teardown) as Rc<dyn Fn()>),
            };

            if overlay_stack.push(overlay) {
                let _ = reply.send(Ok(()));
            } else {
                teardown_overlap_layers(vec![layer]);
                stack
                    .main_layer()
                    .state
                    .push_system("An overlay is already active. Close it before opening another.");
                let _ = reply.send(Ok(()));
            }
        });
    }

    /// Open the `/model` overlay. The view content and key handler share one
    /// picker state so navigation is local to the overlay.
    pub fn open_model_overlay(
        &self,
        content_factory: std::rc::Rc<dyn Fn() -> View>,
        on_key: std::rc::Rc<dyn Fn(crossterm::event::KeyEvent) -> bool>,
    ) {
        if self.overlay_stack.is_active() {
            self.stack
                .active()
                .state
                .push_system("An overlay is already active. Close it before opening another.");
            return;
        }
        let overlay = crate::tui::overlay_stack::ActiveOverlay {
            geometry: iodilos::OverlayGeometry::Inset { ratio: 0.125 },
            dismissible: true,
            route_app_keys: false,
            content: content_factory,
            on_key: Some(on_key),
            on_close: None,
        };
        self.overlay_stack.push(overlay);
    }

    /// Handle the `OpenModelOverlay` runtime command. The content factory is NOT
    /// invoked here (this runs in the command-pump `spawn_local` task, where
    /// CURRENT_OWNER is unset, so `use_context`/`on_key` would not resolve); it
    /// runs once in App's OverlayLayer render effect under the mount owner.
    pub fn handle_open_model_overlay(
        &self,
        reply: tokio::sync::oneshot::Sender<crate::core::extensions::types::CommandResult>,
    ) {
        let overlay_stack = Rc::clone(&self.overlay_stack);
        let config = self.config.clone();
        let parts = match &self.harness {
            Some(h) => {
                let h = Arc::clone(h);
                let stack = Rc::clone(&overlay_stack);
                crate::tui::components::model_overlay::model_overlay_parts(h, stack, config)
            }
            None => {
                self.stack.active().state.push_error(
                    "No LLM agent available. Cannot open the model picker.".to_string(),
                );
                let _ = reply.send(Ok(()));
                return;
            }
        };
        self.open_model_overlay(parts.content, parts.on_key);
        let _ = reply.send(Ok(()));
    }

    pub fn close_active_overlap(&self) {
        tracing::info!(target: "flown::overlap", "close_active_overlap called");
        let stack = self.stack.clone();
        if stack.active_is_overlap() && !stack.active_overlap_is_dismissible() {
            return;
        }
        let layers = stack.pop_all_overlap_layers();
        if layers.is_empty() {
            tracing::info!(target: "flown::overlap", "close_active_overlap: no active layer; pending overlap cancelled");
            return;
        }
        tracing::info!(target: "flown::overlap", count = layers.len(), "close_active_overlap: layers popped, shutting down drivers");
        teardown_overlap_layers(layers);
    }

    pub fn send_to_active(&self, text: String) {
        let layer = self.stack.active();
        if layer.state.busy.get() {
            return;
        }
        layer.state.push_user(&text);
        if let Some(tx) = &layer.cmd_tx {
            layer.state.busy.set(true);
            layer.state.status.update(|s| s.busy = true);
            // Queue the prompt on the layer's driver; it awaits prompt()
            // directly on its own stack. try_send is non-blocking (capacity 8).
            match tx.try_send(LayerCommand::Prompt(text)) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    layer
                        .state
                        .push_error("Layer driver exited. Cannot send prompt.".to_string());
                    layer.state.busy.set(false);
                    layer.state.status.update(|s| s.busy = false);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    layer
                        .state
                        .push_error("Layer driver busy (command queue full).".to_string());
                    layer.state.busy.set(false);
                    layer.state.status.update(|s| s.busy = false);
                }
            }
        } else {
            layer
                .state
                .push_error("No LLM agent available. Check your config.".to_string());
        }
    }

    pub fn notify_active(&self, text: String) {
        self.stack.active().state.push_system(text);
    }

    pub fn notify_error_active(&self, text: String) {
        self.stack.active().state.push_error(text);
    }

    pub fn clear_active(&self) {
        self.stack.active().state.clear();
    }
}

// Teardown order is load-bearing:
// 1. unsubscribe first so the harness stops emitting into event_tx;
// 2. send Shutdown to the driver so it aborts any in-flight turn on its own
//    stack and exits deterministically;
// 3. drop the layer last, ending the event pump via event_tx drop.
fn teardown_overlap_layers(layers: Vec<Rc<ConversationLayer>>) {
    for layer in layers {
        if let Some(unsub) = &layer.unsubscribe {
            (unsub)();
        }
        if let Some(tx) = &layer.cmd_tx {
            match tx.try_send(LayerCommand::Shutdown) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::warn!(
                        target: "flown::overlap",
                        "close: layer driver already exited"
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        target: "flown::overlap",
                        "close: command channel full; forcing harness abort"
                    );
                    if let Some(h) = &layer.harness {
                        let h = Arc::clone(h);
                        tokio::spawn(async move {
                            let _ = h.abort().await;
                        });
                    }
                }
            }
        }
        drop(layer);
    }
}

/// The result shipped over the overlap build channel: a built harness or an error.
enum OverlapBuildResult {
    Ok(Arc<AgentHarness>),
    Err(String),
}

fn harness_event_name(event: &AgentHarnessEvent) -> &'static str {
    use flown_agent::AgentHarnessEvent;

    match event {
        AgentHarnessEvent::QueueUpdate { .. } => "QueueUpdate",
        AgentHarnessEvent::SavePoint { .. } => "SavePoint",
        AgentHarnessEvent::Abort { .. } => "Abort",
        AgentHarnessEvent::Settled { .. } => "Settled",
        AgentHarnessEvent::BeforeAgentStart { .. } => "BeforeAgentStart",
        AgentHarnessEvent::Context { .. } => "Context",
        AgentHarnessEvent::BeforeProviderRequest { .. } => "BeforeProviderRequest",
        AgentHarnessEvent::BeforeProviderPayload { .. } => "BeforeProviderPayload",
        AgentHarnessEvent::AfterProviderResponse { .. } => "AfterProviderResponse",
        AgentHarnessEvent::ToolCall { .. } => "ToolCall",
        AgentHarnessEvent::ToolResult { .. } => "ToolResult",
        AgentHarnessEvent::AgentStart => "AgentStart",
        AgentHarnessEvent::TurnStart => "TurnStart",
        AgentHarnessEvent::MessageStart { .. } => "MessageStart",
        AgentHarnessEvent::MessageUpdate { .. } => "MessageUpdate",
        AgentHarnessEvent::MessageEnd { .. } => "MessageEnd",
        AgentHarnessEvent::TurnEnd { .. } => "TurnEnd",
        AgentHarnessEvent::AgentEnd { .. } => "AgentEnd",
        AgentHarnessEvent::ToolExecutionStart { .. } => "ToolExecutionStart",
        AgentHarnessEvent::ToolExecutionUpdate { .. } => "ToolExecutionUpdate",
        AgentHarnessEvent::ToolExecutionEnd { .. } => "ToolExecutionEnd",
        AgentHarnessEvent::SessionBeforeCompact { .. } => "SessionBeforeCompact",
        AgentHarnessEvent::SessionCompact { .. } => "SessionCompact",
        AgentHarnessEvent::SessionBeforeTree { .. } => "SessionBeforeTree",
        AgentHarnessEvent::SessionTree { .. } => "SessionTree",
        AgentHarnessEvent::ModelUpdate { .. } => "ModelUpdate",
        AgentHarnessEvent::ThinkingLevelUpdate { .. } => "ThinkingLevelUpdate",
        AgentHarnessEvent::ResourcesUpdate { .. } => "ResourcesUpdate",
        AgentHarnessEvent::ToolsUpdate { .. } => "ToolsUpdate",
    }
}

fn assistant_message_event_name(event: &flown_ai::AssistantMessageEvent) -> &'static str {
    use flown_ai::AssistantMessageEvent;

    match event {
        AssistantMessageEvent::Start { .. } => "Start",
        AssistantMessageEvent::TextStart { .. } => "TextStart",
        AssistantMessageEvent::TextDelta { .. } => "TextDelta",
        AssistantMessageEvent::TextEnd { .. } => "TextEnd",
        AssistantMessageEvent::ThinkingStart { .. } => "ThinkingStart",
        AssistantMessageEvent::ThinkingDelta { .. } => "ThinkingDelta",
        AssistantMessageEvent::ThinkingEnd { .. } => "ThinkingEnd",
        AssistantMessageEvent::ToolCallStart { .. } => "ToolCallStart",
        AssistantMessageEvent::ToolCallDelta { .. } => "ToolCallDelta",
        AssistantMessageEvent::ToolCallEnd { .. } => "ToolCallEnd",
        AssistantMessageEvent::Done { .. } => "Done",
        AssistantMessageEvent::Error { .. } => "Error",
    }
}

fn log_overlap_pump_event(extension_id: &str, event: &AgentHarnessEvent) {
    use flown_agent::AgentHarnessEvent;

    match event {
        AgentHarnessEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => {
            log_overlap_message_update(extension_id, assistant_message_event);
        }
        AgentHarnessEvent::MessageEnd { message } | AgentHarnessEvent::TurnEnd { message, .. } => {
            log_overlap_message_event(extension_id, event, message);
        }
        AgentHarnessEvent::AgentStart
        | AgentHarnessEvent::TurnStart
        | AgentHarnessEvent::AgentEnd { .. }
        | AgentHarnessEvent::Abort { .. }
        | AgentHarnessEvent::Settled { .. }
        | AgentHarnessEvent::ToolExecutionStart { .. }
        | AgentHarnessEvent::ToolExecutionEnd { .. } => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = harness_event_name(event),
                "overlap pump event"
            );
        }
        _ => {}
    }
}

fn log_overlap_message_update(extension_id: &str, event: &flown_ai::AssistantMessageEvent) {
    use flown_ai::AssistantMessageEvent;

    match event {
        AssistantMessageEvent::TextDelta { delta, .. }
        | AssistantMessageEvent::ThinkingDelta { delta, .. }
        | AssistantMessageEvent::ToolCallDelta { delta, .. } => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = "MessageUpdate",
                assistant_event = assistant_message_event_name(event),
                delta_len = delta.len(),
                delta = %log_fragment(delta),
                "overlap pump event"
            );
        }
        AssistantMessageEvent::TextEnd { content, .. }
        | AssistantMessageEvent::ThinkingEnd { content, .. } => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = "MessageUpdate",
                assistant_event = assistant_message_event_name(event),
                content_len = content.len(),
                "overlap pump event"
            );
        }
        AssistantMessageEvent::Done { reason, message } => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = "MessageUpdate",
                assistant_event = "Done",
                reason = ?reason,
                stop_reason = ?message.stop_reason,
                "overlap pump event"
            );
        }
        AssistantMessageEvent::Error { reason, error } => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = "MessageUpdate",
                assistant_event = "Error",
                reason = ?reason,
                stop_reason = ?error.stop_reason,
                "overlap pump event"
            );
        }
        _ => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = "MessageUpdate",
                assistant_event = assistant_message_event_name(event),
                "overlap pump event"
            );
        }
    }
}

fn log_fragment(text: &str) -> String {
    const MAX_CHARS: usize = 512;
    let mut escaped = String::new();
    let mut chars = text.chars();
    for ch in chars.by_ref().take(MAX_CHARS) {
        for escaped_ch in ch.escape_default() {
            escaped.push(escaped_ch);
        }
    }
    if chars.next().is_some() {
        escaped.push_str("...");
    }
    escaped
}

fn log_overlap_message_event(
    extension_id: &str,
    event: &AgentHarnessEvent,
    message: &flown_agent::AgentMessage,
) {
    match message {
        flown_agent::AgentMessage::Assistant(message) => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = harness_event_name(event),
                role = "assistant",
                stop_reason = ?message.stop_reason,
                "overlap pump event"
            );
        }
        flown_agent::AgentMessage::User(_) => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = harness_event_name(event),
                role = "user",
                "overlap pump event"
            );
        }
        flown_agent::AgentMessage::ToolResult(_) => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = harness_event_name(event),
                role = "tool_result",
                "overlap pump event"
            );
        }
        flown_agent::AgentMessage::Custom(message) => {
            tracing::info!(
                target: "flown::overlap",
                extension = %extension_id,
                kind = harness_event_name(event),
                role = "custom",
                custom_type = %message.custom_type,
                "overlap pump event"
            );
        }
    }
}

fn log_overlap_state(
    extension_id: &str,
    phase: &'static str,
    event: &AgentHarnessEvent,
    state: &UiState,
    accumulated_len: usize,
    in_thinking: bool,
) {
    let status = state.status.get_clone();
    tracing::info!(
        target: "flown::overlap",
        extension = %extension_id,
        phase,
        kind = harness_event_name(event),
        state_busy = state.busy.get(),
        status_busy = status.busy,
        frame = status.frame,
        accumulated_len,
        in_thinking,
        "overlap state around translate_event"
    );
}

fn log_overlap_state_after_translate(
    extension_id: &str,
    state: &UiState,
    accumulated_len: usize,
    in_thinking: bool,
) {
    let status = state.status.get_clone();
    tracing::info!(
        target: "flown::overlap",
        extension = %extension_id,
        phase = "after",
        state_busy = state.busy.get(),
        status_busy = status.busy,
        frame = status.frame,
        accumulated_len,
        in_thinking,
        "overlap state around translate_event"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_overlap_meta() -> OverlapMeta {
        OverlapMeta {
            extension_id: "test".to_string(),
            badge: Some("TEST".to_string()),
            single_instance_key: Some("test".to_string()),
            dismissible: true,
            slash_commands: SlashCommandScope::Disabled,
        }
    }

    fn with_root(f: impl FnOnce()) {
        let owner = create_root(f);
        owner.dispose();
    }

    /// A main-only stack reports depth 1 and active == main.
    #[test]
    fn stack_starts_with_main_only() {
        with_root(|| {
            let (tx, _rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            assert_eq!(stack.depth(), 1);
            assert!(!stack.active_is_overlap());
            assert_eq!(stack.active().kind, LayerKind::Main);
        });
    }

    /// pop_active on a main-only stack is a no-op (Main is never popped).
    #[test]
    fn cannot_pop_main() {
        with_root(|| {
            let (tx, _rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            assert!(stack.pop_active().is_none());
            assert_eq!(stack.depth(), 1);
        });
    }

    /// Popping an overlap layer must not expose an intermediate invalid state where
    /// `active_index` still points past the shortened layer stack.
    #[test]
    fn pop_active_switches_index_and_layers_atomically() {
        with_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);

            let (overlap_tx, _overlap_rx) = flume::unbounded();
            stack.push(ConversationLayer {
                kind: LayerKind::Overlap,
                overlap: Some(test_overlap_meta()),
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: overlap_tx,
                unsubscribe: None,
                cmd_tx: None,
            });

            let observed = Rc::new(std::cell::RefCell::new(Vec::new()));
            let observed_for_effect = Rc::clone(&observed);
            let stack_for_effect = Rc::clone(&stack);
            create_effect(move || {
                stack_for_effect.active_index_signal().get();
                observed_for_effect
                    .borrow_mut()
                    .push(stack_for_effect.active().kind);
            });

            let popped = stack.pop_active().expect("overlap layer popped");

            assert_eq!(popped.kind, LayerKind::Overlap);
            assert_eq!(stack.depth(), 1);
            assert_eq!(stack.active().kind, LayerKind::Main);
            let observed = observed.borrow();
            assert_eq!(observed.first(), Some(&LayerKind::Overlap));
            assert_eq!(observed.last(), Some(&LayerKind::Main));
            assert!(observed[1..].iter().all(|kind| *kind == LayerKind::Main));
        });
    }

    #[test]
    fn reserve_overlap_rejects_repeated_enter_while_pending() {
        with_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);

            let token = stack
                .reserve_overlap(Some("test".to_string()))
                .expect("first overlap reserves slot");

            assert!(stack.overlap_is_active_or_pending());
            assert!(stack.reserve_overlap(Some("test".to_string())).is_none());
            assert!(stack.is_overlap_token_current(token));
        });
    }

    #[test]
    fn close_cancels_pending_overlap_before_layer_push() {
        with_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            let token = stack
                .reserve_overlap(Some("test".to_string()))
                .expect("pending overlap reserved");

            let popped = stack.pop_all_overlap_layers();

            assert!(popped.is_empty());
            assert_eq!(stack.depth(), 1);
            assert!(!stack.overlap_is_active_or_pending());
            assert!(!stack.is_overlap_token_current(token));
        });
    }

    #[test]
    fn pop_all_overlap_layers_returns_to_clean_main_stack() {
        with_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            stack
                .reserve_overlap(Some("test".to_string()))
                .expect("overlap reserved");

            for _ in 0..2 {
                let (overlap_tx, _overlap_rx) = flume::unbounded();
                stack.push(ConversationLayer {
                    kind: LayerKind::Overlap,
                    overlap: Some(test_overlap_meta()),
                    state: Rc::new(UiState::new(EditorState::default())),
                    harness: None,
                    event_tx: overlap_tx,
                    unsubscribe: None,
                    cmd_tx: None,
                });
            }

            let popped = stack.pop_all_overlap_layers();

            assert_eq!(popped.len(), 2);
            assert_eq!(stack.depth(), 1);
            assert_eq!(stack.active().kind, LayerKind::Main);
            assert!(!stack.overlap_is_active_or_pending());
        });
    }

    #[test]
    fn full_bleed_overlay_close_runs_overlay_teardown_without_switching_stack() {
        with_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::new(UiState::new(EditorState::default())),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            let overlay_stack = crate::tui::overlay_stack::OverlayStack::new();
            let closed = Rc::new(std::cell::Cell::new(false));
            let closed_for_overlay = Rc::clone(&closed);
            overlay_stack.push(crate::tui::overlay_stack::ActiveOverlay {
                geometry: iodilos::OverlayGeometry::FullBleed,
                dismissible: true,
                route_app_keys: false,
                content: Rc::new(View::new),
                on_key: None,
                on_close: Some(Rc::new(move || closed_for_overlay.set(true))),
            });

            overlay_stack.pop();

            assert!(closed.get());
            assert_eq!(stack.depth(), 1);
            assert_eq!(stack.active().kind, LayerKind::Main);
            assert!(!overlay_stack.is_active());
        });
    }

    #[test]
    fn model_overlay_command_does_not_read_harness_from_context() {
        let owner = create_root(|| {
            let (main_tx, _main_rx) = flume::unbounded();
            let main_state = Rc::new(UiState::new(EditorState::default()));
            let main = ConversationLayer {
                kind: LayerKind::Main,
                overlap: None,
                state: Rc::clone(&main_state),
                harness: None,
                event_tx: main_tx,
                unsubscribe: None,
                cmd_tx: None,
            };
            let stack = ConversationStack::new(main, None);
            let overlay_stack = crate::tui::overlay_stack::OverlayStack::new();
            let runtime_control =
                RuntimeControl::new(Rc::clone(&stack), overlay_stack, None, Config::default());
            let (reply_tx, mut reply_rx) = tokio::sync::oneshot::channel();

            runtime_control.handle_open_model_overlay(reply_tx);

            let reply = reply_rx
                .try_recv()
                .expect("model overlay command should reply synchronously");
            assert!(reply.is_ok());
            assert!(!stack.active().state.entries.get_clone().is_empty());
        });
        owner.dispose();
    }
}
