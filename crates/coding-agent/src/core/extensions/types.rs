//! Extension layer type contracts (M2a).
//!
//! Defines the [`Extension`] trait and the registration-phase [`ExtensionApi`].
//!
//! # Threading model (the load-bearing design constraint)
//!
//! The agent straddles two threads with different `Send` regimes:
//!
//! - **tokio side** — owns `Arc<AgentHarness>` (fully `Send`). `register` runs
//!   here at startup. Tool reconcile (`harness.set_tools().await`) and the
//!   McpExtension's MCP-server watcher task live here.
//! - **iodilos side** — single-threaded, owns `Rc<UiState>` (NOT `Send`).
//!   Command dispatch runs here (via `on_key` → `handle_app_key`).
//!
//! The split:
//! - `Extension::register` runs on **tokio** and produces pure `Send` data: a
//!   command table, one-shot tools, and `ToolStore`s.
//! - The command table is moved to the **iodilos** side, wrapped in a
//!   [`CommandSide`] that owns the `Rc<UiState>` sink and *interprets* each
//!   handler's returned [`CommandEffect`] into `UiState` operations.
//! - [`ToolHandle`] / [`ToolStore`] stay on **tokio** where
//!   `harness.set_tools` is reachable.
//!
//! Handlers never receive `&CommandContext` — they take `args` and return a
//! `CommandEffect`. This keeps `CommandHandler` `Send + Sync` (made on tokio,
//! moved to iodilos) without forcing `CommandContext` to be `Sync`, which is
//! impossible while it carries `Rc<UiState>`. An effect is plain `Send` data;
//! iodilos interprets it. This is also more testable (effects are values) and
//! accommodates future btw commands as new `CommandEffect` variants.

use std::sync::Arc;

use flown_agent::types::AgentTool;


// ── Extension trait ─────────────────────────────────────────────────────

/// A composable capability plugged into the agent.
///
/// `register` runs once at startup, on the **tokio** side. The extension is
/// `Send + Sync` so it can live there and be iterated from the builder.
pub trait Extension: Send + Sync {
    /// Stable identifier for logs / diagnostics.
    fn name(&self) -> &'static str;
    /// Publish commands/tools/hooks. Implementations capture any config they
    /// need by clone (e.g. `self.config.clone()`) — handlers receive only
    /// `args`, not a runtime context.
    fn register(&self, api: &mut ExtensionApi);
}

// ── Command metadata ────────────────────────────────────────────────────

/// Metadata for a slash command registered by an extension. Drives
/// autocomplete, `/help`, and dispatch.
#[derive(Debug, Clone)]
pub struct CommandMeta {
    /// One-line description shown in the popup and `/help`.
    pub description: String,
    /// Alternate invocation names (e.g. `["/h"]`).
    pub aliases: Vec<String>,
    /// Named sub-arguments (e.g. `/mcp list`). When non-empty the editor opens
    /// a second-level completion popup after the command name + space.
    pub subcommands: Vec<SubcommandDef>,
}

impl CommandMeta {
    /// Shorthand for a command with no aliases and no subcommands.
    pub fn simple(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            aliases: Vec::new(),
            subcommands: Vec::new(),
        }
    }
}

/// A named subcommand of an extension command.
#[derive(Debug, Clone)]
pub struct SubcommandDef {
    pub name: String,
    pub description: String,
}

// ── CommandEffect + handler ─────────────────────────────────────────────

/// What a command handler wants the runtime to do, returned as plain `Send`
/// data so the handler itself can be `Send + Sync` (made on tokio, invoked on
/// iodilos). The iodilos-side dispatcher interprets each variant.
///
/// Variants are added when a real extension needs them — never preemptively.
/// M2a needs only the notify pair. Future btw (M3) will add e.g.
/// `PushTranscript { prompt }`.
#[derive(Debug, Clone)]
pub enum CommandEffect {
    /// Push an informational line into the transcript. Does not trigger a turn.
    Notify(String),
    /// Push an error line into the transcript.
    NotifyError(String),
    /// Clear the transcript.
    ClearTranscript,
}

/// Handler invoked when the user runs a registered extension command.
///
/// `args` is everything after the command name (trimmed, re-joined). The
/// handler returns a [`CommandEffect`] rather than touching the runtime
/// directly, which keeps it `Send + Sync` regardless of the iodilos thread's
/// `Rc`-based state.
pub type CommandHandler = Arc<dyn Fn(&str) -> CommandEffect + Send + Sync>;

/// A fully-registered command, captured by [`ExtensionApi`] during `register`
/// and moved into the iodilos-side [`super::CommandSide`].
pub struct RegisteredCommand {
    pub name: String,
    pub meta: CommandMeta,
    pub handler: CommandHandler,
    /// When `true`, the command needs the iodilos-side [`ControlRuntime`] to
    /// drive the conversation stack (e.g. `/btw`). The effect `handler` is
    /// ignored for such commands; [`super::CommandSide`] looks up a
    /// [`ControlHandler`] bound at mount by name and dispatches through it.
    /// Effect-only commands (`/mcp`) leave this `false` and use `handler`.
    pub needs_control: bool,
}

// ── Control-runtime (iodilos-side capability, for commands like /btw) ────

/// Iodilos-side capability handed to *control* commands (those registered with
/// [`ExtensionApi::register_control_command`]). Lets a command drive the
/// conversation stack — enter/exit a btw layer, submit a turn, notify —
/// without returning a plain [`CommandEffect`], because "enter btw then
/// optionally send a message" is sequential logic over live runtime state.
///
/// This mirrors pi-mono's `ExtensionCommandContext` (handler receives a
/// stateful `ctx`, not a pure effect). Implementations live on the iodilos
/// thread and hold `Rc`-based state, so this trait is **not** `Send` and is
/// never constructed during `register` (which runs on tokio). `CommandSide`
/// builds the impl at mount and binds it to the command by name.
pub trait ControlRuntime {
    /// Enter a btw layer, forking the active session's history. If `prompt` is
    /// `Some`, submit it as a turn on the new layer immediately.
    fn enter_btw(&self, prompt: Option<String>);
    /// Exit the active btw layer (aborts it, discards it). No-op on Main.
    fn exit_btw(&self);
    /// Submit `text` as a user turn on the active layer's harness.
    fn send_to_active(&self, text: String);
    /// Push an informational line into the active layer's transcript.
    fn notify_active(&self, text: String);
    /// Push an error line into the active layer's transcript.
    fn notify_error_active(&self, text: String);
    /// Clear the active layer's transcript.
    fn clear_active(&self);
    /// Whether the active layer is a btw layer.
    fn active_is_btw(&self) -> bool;
}

// ── ToolHandle (runtime add/remove) ─────────────────────────────────────

/// Handle for adding/removing tools after `register` has returned. Lives on
/// the **tokio** side — `add`/`remove` stage edits, and the runner reconciles
/// them by calling `harness.set_tools(...).await`.
///
/// Cheap to clone (inner is `Arc`). An extension takes one in `register` and
/// clones it into a spawned task (decision A1'): the task watches for the
/// condition that changes the tool set (e.g. an MCP server connecting) and
/// calls [`ToolHandle::add`] / [`ToolHandle::remove`].
#[derive(Clone)]
pub struct ToolHandle {
    pub(crate) store: Arc<ToolStore>,
}

pub(crate) struct ToolStore {
    /// The full set of tools this extension manages (by name). The runner
    /// merges every extension's store before calling `harness.set_tools`.
    tools: std::sync::RwLock<std::collections::HashMap<String, AgentTool>>,
    /// Set when any edit occurred since the last reconcile; the runner polls it.
    dirty: std::sync::atomic::AtomicBool,
}

impl ToolStore {
    pub(crate) fn new() -> Self {
        Self {
            tools: std::sync::RwLock::new(std::collections::HashMap::new()),
            dirty: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl ToolHandle {
    pub(crate) fn from_store(store: Arc<ToolStore>) -> Self {
        Self { store }
    }

    /// Add or replace a tool by name. Staged until the next reconcile.
    pub fn add(&self, tool: AgentTool) {
        self.store
            .tools
            .write()
            .expect("poisoned tool store")
            .insert(tool.name.clone(), tool);
        self.store
            .dirty
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Remove a tool by name. Staged until the next reconcile.
    pub fn remove(&self, name: &str) {
        if self
            .store
            .tools
            .write()
            .expect("poisoned tool store")
            .remove(name)
            .is_some()
        {
            self.store
                .dirty
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Replace the entire tool set this handle owns. Staged until reconcile.
    pub fn replace_all(&self, tools: Vec<AgentTool>) {
        let mut map = std::collections::HashMap::new();
        for t in tools {
            map.insert(t.name.clone(), t);
        }
        *self.store.tools.write().expect("poisoned tool store") = map;
        self.store
            .dirty
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// A snapshot of the current tools (for the runner's reconcile pass).
    pub(crate) fn snapshot(&self) -> Vec<AgentTool> {
        self.store
            .tools
            .read()
            .expect("poisoned tool store")
            .values()
            .cloned()
            .collect()
    }

    /// Whether any add/remove happened since the last call to this method.
    pub(crate) fn take_dirty(&self) -> bool {
        self.store
            .dirty
            .swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

// ── Hook handler (reserved, M2a unused) ──────────────────────────────────

/// Reserved for `on(event, handler)`. The runner aggregates these with the
/// chained semantics specified in ADR-0003 D1. No built-in extension uses a
/// hook in M2a, so the signature is intentionally coarse and may tighten.
pub type HookHandler = Arc<dyn Fn(serde_json::Value) -> serde_json::Value + Send + Sync>;

/// A hook collected during `register`.
pub(crate) struct RegisteredHook {
    pub event: String,
    pub handler: HookHandler,
}

// ── ExtensionApi (registration phase) ───────────────────────────────────

/// The registration-phase API handed to [`Extension::register`].
///
/// Collects commands, one-time tools, hooks, and mints [`ToolHandle`]s for
/// runtime tool edits. Once every extension's `register` has run, the runner
/// consumes the collected state.
pub struct ExtensionApi {
    pub(crate) commands: Vec<RegisteredCommand>,
    pub(crate) one_shot_tools: Vec<AgentTool>,
    pub(crate) hooks: Vec<RegisteredHook>,
    pub(crate) tool_stores: Vec<Arc<ToolStore>>,
}

impl ExtensionApi {
    pub(crate) fn new() -> Self {
        Self {
            commands: Vec::new(),
            one_shot_tools: Vec::new(),
            hooks: Vec::new(),
            tool_stores: Vec::new(),
        }
    }

    /// Register an effect-only slash command (`/mcp`). `name` includes the
    /// leading `/` (e.g. `"/mcp"`). The handler returns a [`CommandEffect`].
    pub fn register_command(&mut self, name: &str, meta: CommandMeta, handler: CommandHandler) {
        self.commands.push(RegisteredCommand {
            name: name.to_string(),
            meta,
            handler,
            needs_control: false,
        });
    }

    /// Register a control slash command (`/btw`) that needs the iodilos-side
    /// [`ControlRuntime`] to drive the conversation stack. Only the metadata
    /// (`name`/`meta`) is captured here; the actual handler is an
    /// iodilos-side closure bound at mount (see [`super::CommandSide`]),
    /// because it holds `Rc`-based state and cannot be `Send`. The placeholder
    /// `handler` is a no-op used only so the `Send + Sync` shape is uniform.
    pub fn register_control_command(
        &mut self,
        name: &str,
        meta: CommandMeta,
        placeholder: CommandHandler,
    ) {
        self.commands.push(RegisteredCommand {
            name: name.to_string(),
            meta,
            handler: placeholder,
            needs_control: true,
        });
    }

    /// Register a tool that is fixed at startup (no runtime add/remove needed).
    /// For tools that change at runtime, use [`Self::tool_handle`] instead.
    pub fn register_tool(&mut self, tool: AgentTool) {
        self.one_shot_tools.push(tool);
    }

    /// Obtain a handle for runtime tool add/remove. Each call returns a fresh
    /// handle backed by its own store; the runner merges all stores on
    /// reconcile. An extension typically calls this once in `register` and
    /// clones the returned handle into a spawned task.
    pub fn tool_handle(&mut self) -> ToolHandle {
        let store = Arc::new(ToolStore::new());
        self.tool_stores.push(store.clone());
        ToolHandle::from_store(store)
    }

    /// Register a typed event hook (reserved — see [`HookHandler`]).
    pub fn on(&mut self, event: &str, handler: HookHandler) {
        self.hooks.push(RegisteredHook {
            event: event.to_string(),
            handler,
        });
    }

    /// Consume the collected state. Called by the tokio-side builder.
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<RegisteredCommand>,
        Vec<AgentTool>,
        Vec<RegisteredHook>,
        Vec<Arc<ToolStore>>,
    ) {
        (
            self.commands,
            self.one_shot_tools,
            self.hooks,
            self.tool_stores,
        )
    }
}
