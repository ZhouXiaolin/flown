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
//! - `Extension::register` runs on **tokio** and produces command metadata,
//!   async command handlers, one-shot tools, hooks, and `ToolStore`s.
//! - The command table is moved to the **iodilos** side, wrapped in a
//!   [`CommandSide`] that binds a host-owned runtime command proxy.
//! - Command handlers receive an [`ExtensionContext`] facade. They drive UI and
//!   conversation behavior through capabilities (`ctx.ui`, `ctx.conversation`)
//!   instead of owning internal `Rc<UiState>` or raw harness state.
//! - [`ToolHandle`] / [`ToolStore`] stay on **tokio** where
//!   `harness.set_tools` is reachable.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use flown_agent::AgentTool;
use tokio::sync::oneshot;

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

/// Whether an extension-owned overlap participates in global slash commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlashCommandScope {
    #[default]
    Global,
    Disabled,
}

/// Options for opening an extension-owned agent overlap.
#[derive(Debug, Clone)]
pub struct OverlapOptions {
    pub extension_id: String,
    pub badge: Option<String>,
    pub single_instance_key: Option<String>,
    pub dismissible: bool,
    pub slash_commands: SlashCommandScope,
    pub initial_prompt: Option<String>,
}

impl OverlapOptions {
    pub fn new(extension_id: impl Into<String>) -> Self {
        Self {
            extension_id: extension_id.into(),
            badge: None,
            single_instance_key: None,
            dismissible: true,
            slash_commands: SlashCommandScope::Global,
            initial_prompt: None,
        }
    }
}

/// Parsed invocation handed to an extension command handler.
#[derive(Debug, Clone)]
pub struct CommandInvocation {
    pub name: String,
    pub args: String,
}

pub type CommandResult = anyhow::Result<()>;
pub type CommandFuture = Pin<Box<dyn Future<Output = CommandResult> + 'static>>;

/// Handler invoked when the user runs a registered extension command.
///
/// The handler is async and drives host behavior through [`ExtensionContext`].
/// It returns `Result<()>`; user-visible output is explicit (`ctx.ui.notify`,
/// `ctx.ui.notify_error`, etc.) and errors are surfaced by the command runner.
pub type CommandHandler =
    Arc<dyn Fn(CommandInvocation, ExtensionContext) -> CommandFuture + Send + Sync>;

/// A fully-registered command, captured by [`ExtensionApi`] during `register`
/// and moved into the iodilos-side [`super::CommandSide`].
pub struct RegisteredCommand {
    pub name: String,
    pub meta: CommandMeta,
    pub handler: CommandHandler,
}

// ── ExtensionContext + runtime command proxy ────────────────────────────

/// Commands that can be sent through the runtime command proxy.
#[derive(Debug)]
pub enum RuntimeCommand {
    OpenOverlap {
        options: OverlapOptions,
        reply: oneshot::Sender<CommandResult>,
    },
    CloseActiveOverlap {
        reply: oneshot::Sender<CommandResult>,
    },
    SendToActive {
        text: String,
        reply: oneshot::Sender<CommandResult>,
    },
    NotifyActive {
        text: String,
    },
    NotifyErrorActive {
        text: String,
    },
    ClearActive,
    /// Open the `/model` overlay (model + thinking-intensity picker). Pushed
    /// onto the OverlayStack by the iodilos-side RuntimeControl.
    OpenModelOverlay {
        reply: oneshot::Sender<CommandResult>,
    },
    /// Fork the main session into a transient overlay (btw). The prompt, when
    /// present, is submitted to the forked harness immediately.
    ForkConversation {
        prompt: Option<String>,
        reply: oneshot::Sender<CommandResult>,
    },
}

/// Host-owned command proxy used by [`ExtensionContext`] capabilities.
///
/// The proxy is `Send + Sync` and request-response aware: command handlers can
/// await important runtime actions without owning raw `UiState`,
/// `ConversationStack`, or harness internals.
#[derive(Clone)]
pub struct RuntimeCommandProxy {
    tx: flume::Sender<RuntimeCommand>,
}

impl RuntimeCommandProxy {
    pub fn new(tx: flume::Sender<RuntimeCommand>) -> Self {
        Self { tx }
    }

    fn send(&self, cmd: RuntimeCommand) {
        let _ = self.tx.send(cmd);
    }

    async fn request<F>(&self, make: F) -> CommandResult
    where
        F: FnOnce(oneshot::Sender<CommandResult>) -> RuntimeCommand,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| anyhow::anyhow!("runtime command channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("runtime command reply dropped"))?
    }

    pub async fn open_overlap(&self, options: OverlapOptions) -> CommandResult {
        self.request(|reply| RuntimeCommand::OpenOverlap { options, reply })
            .await
    }

    pub async fn close_active_overlap(&self) -> CommandResult {
        self.request(|reply| RuntimeCommand::CloseActiveOverlap { reply })
            .await
    }

    pub async fn send_to_active(&self, text: String) -> CommandResult {
        self.request(|reply| RuntimeCommand::SendToActive { text, reply })
            .await
    }

    /// Ask the iodilos-side runtime to open the `/model` overlay.
    pub async fn open_model_overlay(&self) -> CommandResult {
        self.request(|reply| RuntimeCommand::OpenModelOverlay { reply })
            .await
    }

    /// Ask the iodilos-side runtime to fork the main session into a transient
    /// overlay (btw). `prompt`, when present, is submitted to the fork.
    pub async fn fork_conversation(&self, prompt: Option<String>) -> CommandResult {
        self.request(|reply| RuntimeCommand::ForkConversation { prompt, reply })
            .await
    }

    pub fn notify_active(&self, text: String) {
        self.send(RuntimeCommand::NotifyActive { text });
    }

    pub fn notify_error_active(&self, text: String) {
        self.send(RuntimeCommand::NotifyErrorActive { text });
    }

    pub fn clear_active(&self) {
        self.send(RuntimeCommand::ClearActive);
    }
}

/// Unified context object handed to extension command handlers.
#[derive(Clone)]
pub struct ExtensionContext {
    pub ui: UiCapability,
    pub conversation: ConversationCapability,
}

impl ExtensionContext {
    pub fn new(runtime: Arc<RuntimeCommandProxy>) -> Self {
        Self {
            ui: UiCapability {
                runtime: Arc::clone(&runtime),
            },
            conversation: ConversationCapability { runtime },
        }
    }
}

/// UI capability namespace for extension commands.
#[derive(Clone)]
pub struct UiCapability {
    runtime: Arc<RuntimeCommandProxy>,
}

impl UiCapability {
    /// Fire-and-forget informational notification on the active conversation.
    pub fn notify(&self, text: impl Into<String>) {
        self.runtime.notify_active(text.into());
    }

    /// Fire-and-forget error notification on the active conversation.
    pub fn notify_error(&self, text: impl Into<String>) {
        self.runtime.notify_error_active(text.into());
    }

    /// Clear the active conversation transcript.
    pub fn clear(&self) {
        self.runtime.clear_active();
    }
}

/// Conversation capability namespace for extension commands.
#[derive(Clone)]
pub struct ConversationCapability {
    runtime: Arc<RuntimeCommandProxy>,
}

impl ConversationCapability {
    pub async fn open_overlap(&self, options: OverlapOptions) -> CommandResult {
        self.runtime.open_overlap(options).await
    }

    pub async fn close_active_overlap(&self) -> CommandResult {
        self.runtime.close_active_overlap().await
    }

    pub async fn send_to_active(&self, text: impl Into<String>) -> CommandResult {
        self.runtime.send_to_active(text.into()).await
    }

    /// Open the `/model` overlay (model + thinking-intensity picker).
    pub async fn open_model_overlay(&self) -> CommandResult {
        self.runtime.open_model_overlay().await
    }

    /// Fork the main session into a transient overlay (btw).
    pub async fn fork_conversation(&self, prompt: Option<String>) -> CommandResult {
        self.runtime.fork_conversation(prompt).await
    }
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
    /// Set when any edit occurred since the last reconcile; the runner checks
    /// it when the `dirty_signal` fires.
    dirty: std::sync::atomic::AtomicBool,
    /// Wakeup signal shared with the runner's reconcile loop. One signal is
    /// shared across **all** stores (the runner rebuilds the whole tool set on
    /// any edit, so per-store granularity buys nothing). Fired on every edit
    /// so the runner wakes immediately instead of polling on a timer.
    ///
    /// Field-level dead-code allow: only written by `mark_dirty`, which is part
    /// of the reserved runtime-edit API (see [`ToolHandle::add`]).
    #[allow(dead_code)]
    dirty_signal: Arc<tokio::sync::Notify>,
}

impl ToolStore {
    /// `dirty_signal` is shared across every store the runner owns, so a single
    /// `notified().await` in the reconcile loop covers all of them.
    pub(crate) fn new(dirty_signal: Arc<tokio::sync::Notify>) -> Self {
        Self {
            tools: std::sync::RwLock::new(std::collections::HashMap::new()),
            dirty: std::sync::atomic::AtomicBool::new(false),
            dirty_signal,
        }
    }
}

impl ToolHandle {
    pub(crate) fn from_store(store: Arc<ToolStore>) -> Self {
        Self { store }
    }

    /// Add or replace a tool by name. Staged until the next reconcile.
    ///
    /// Part of the reserved runtime-edit API: no built-in extension calls this
    /// yet (MCP ships a one-shot snapshot at registration), but an MCP server
    /// watcher or future extension will. It fires the shared dirty signal so
    /// the runner's reconcile task wakes immediately.
    #[allow(dead_code)]
    pub fn add(&self, tool: AgentTool) {
        self.store
            .tools
            .write()
            .expect("poisoned tool store")
            .insert(tool.name.clone(), tool);
        self.mark_dirty();
    }

    /// Remove a tool by name. Staged until the next reconcile.
    ///
    /// Reserved runtime-edit API — see [`Self::add`].
    #[allow(dead_code)]
    pub fn remove(&self, name: &str) {
        if self
            .store
            .tools
            .write()
            .expect("poisoned tool store")
            .remove(name)
            .is_some()
        {
            self.mark_dirty();
        }
    }

    /// Replace the entire tool set this handle owns. Staged until reconcile.
    ///
    /// Reserved runtime-edit API — see [`Self::add`].
    #[allow(dead_code)]
    pub fn replace_all(&self, tools: Vec<AgentTool>) {
        let mut map = std::collections::HashMap::new();
        for t in tools {
            map.insert(t.name.clone(), t);
        }
        *self.store.tools.write().expect("poisoned tool store") = map;
        self.mark_dirty();
    }

    /// Flag the store dirty and wake the reconcile loop. Called after every
    /// successful edit. `notify_one` is a no-op when no task is waiting, but a
    /// stored permit guarantees the next `notified().await` resolves — so an
    /// edit that lands between reconcile passes is never lost (reconcile also
    /// re-checks `dirty` on wake).
    ///
    /// Dead-code analysis flags this (and the `dirty_signal` field) because the
    /// only callers are the reserved `add`/`remove`/`replace_all`, which have
    /// no consumer yet — MCP ships a snapshot, not a watcher. The whole chain
    /// goes live the moment an extension calls `api.tool_handle()`.
    #[allow(dead_code)]
    fn mark_dirty(&self) {
        self.store
            .dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.store.dirty_signal.notify_one();
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

/// Reserved for `on(event, handler)`. No built-in extension uses a hook yet, so
/// the signature is intentionally coarse and the chained-aggregation semantics
/// are undecided — both may tighten once the first hook consumer lands.
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
    /// Wakeup signal shared with every [`ToolStore`] this API mints and with
    /// the runner's reconcile loop. One signal for all stores: the runner
    /// rebuilds the whole tool set on any edit, so per-store granularity buys
    /// nothing, and a single `notified().await` avoids the `!Unpin` / pin
    /// gymnastics `select_all` over per-store signals would require.
    pub(crate) shared_dirty_signal: Arc<tokio::sync::Notify>,
}

impl ExtensionApi {
    pub(crate) fn new() -> Self {
        Self {
            commands: Vec::new(),
            one_shot_tools: Vec::new(),
            hooks: Vec::new(),
            tool_stores: Vec::new(),
            shared_dirty_signal: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Register an async slash command (`/mcp`). `name` includes the leading
    /// `/` (e.g. `"/mcp"`). The handler receives an [`ExtensionContext`] and
    /// returns `Result<()>`.
    pub fn register_command(&mut self, name: &str, meta: CommandMeta, handler: CommandHandler) {
        self.commands.push(RegisteredCommand {
            name: name.to_string(),
            meta,
            handler,
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
        let store = Arc::new(ToolStore::new(Arc::clone(&self.shared_dirty_signal)));
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
        Arc<tokio::sync::Notify>,
    ) {
        (
            self.commands,
            self.one_shot_tools,
            self.hooks,
            self.tool_stores,
            self.shared_dirty_signal,
        )
    }
}
