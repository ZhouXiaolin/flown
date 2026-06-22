//! Extension runtime, split along the thread boundary.
//!
//! [`Extension::register`](super::Extension::register) runs once on the
//! **tokio** side and produces pure `Send` data. That data is then split:
//!
//! - [`ToolSide`] stays on **tokio** — owns the harness and the live
//!   [`ToolStore`]s, reconciles dirty handles by calling
//!   `harness.set_tools().await`, and hosts extension background tasks.
//! - [`CommandSide`] moves to the **iodilos** side — owns the command table and
//!   a host runtime command proxy. It dispatches slash commands by constructing
//!   an [`ExtensionContext`] and awaiting the registered async handler.
//!
//! See `docs/adr/0001-async-extension-context-and-command-proxy.md` and the
//! threading-model note in [`super::types`].

use std::sync::Arc;

use flown_agent::{AgentHarness, AgentTool};

use super::types::{
    CommandInvocation, Extension, ExtensionApi, ExtensionContext, RegisteredCommand,
    RuntimeCommandProxy, ToolHandle,
};

/// Run every extension's `register` on the tokio side, then split the result
/// into its tokio-side ([`ToolSide`]) and iodilos-side ([`CommandSide`]) halves.
///
/// `built_in_tools` are the non-extension tools (read/bash/edit/write); they
/// are prepended to every tool set the runner pushes to the harness, so the
/// full-replace `set_tools` call never drops them.
pub fn build(
    harness: Arc<AgentHarness>,
    built_in_tools: Vec<AgentTool>,
    extensions: Vec<Box<dyn Extension>>,
) -> (ToolSide, CommandTable) {
    let mut api = ExtensionApi::new();
    for ext in &extensions {
        ext.register(&mut api);
    }
    let (commands, one_shot_tools, _hooks, tool_stores, dirty_signal) = api.into_parts();

    let tool_side = ToolSide {
        harness,
        built_in_tools,
        one_shot_tools,
        tool_stores,
        dirty_signal,
    };
    let command_side = CommandTable { commands };
    (tool_side, command_side)
}

// ── tokio side ────────────────────────────────────────────────────────────

/// tokio-side runtime: owns the harness and reconciles tool edits.
///
/// The McpExtension's background task (cloned [`ToolHandle`] in hand) calls
/// `handle.add()`/`remove()` from tokio; the runtime's reconcile task
/// ([`Self::run_reconcile`]) wakes on the shared dirty signal and flushes those
/// edits into the harness.
///
/// Cheap to clone (every field is behind an `Arc` or a `Vec`). The runtime
/// keeps one clone for the reconcile loop and lets the iodilos side read the
/// initial tool set from another.
#[derive(Clone)]
pub struct ToolSide {
    harness: Arc<AgentHarness>,
    built_in_tools: Vec<AgentTool>,
    one_shot_tools: Vec<AgentTool>,
    tool_stores: Vec<Arc<super::types::ToolStore>>,
    /// Shared wakeup signal: every [`ToolStore`] fires it on edit, and
    /// [`Self::run_reconcile`] awaits it. One signal for all stores — the
    /// runner rebuilds the whole tool set on any edit.
    dirty_signal: Arc<tokio::sync::Notify>,
}

impl ToolSide {
    /// If any [`ToolHandle`] was dirtied since the last call, rebuild the full
    /// tool set and push it to the harness via `set_tools`. Async because
    /// `set_tools` is; poll this on the tokio side.
    pub async fn reconcile_tools(&self) {
        let any_dirty = self
            .tool_stores
            .iter()
            .any(|s| ToolHandle::from_store(s.clone()).take_dirty());
        if !any_dirty {
            return;
        }
        let all = self.all_tools();
        // Activate every currently-registered tool, while preserving any prior
        // active choice that still applies (e.g. a tool the user had not
        // deactivated). Passing `None` would have kept the pre-reconcile
        // active list, so an MCP server connecting at runtime — or a tool
        // added later via `ToolHandle::add` — would never reach the LLM.
        let previous_active = self.harness.active_tool_names();
        let mut next_active: Vec<String> = previous_active
            .into_iter()
            .filter(|name| all.iter().any(|t| t.name == *name))
            .collect();
        for tool in &all {
            if !next_active.contains(&tool.name) {
                next_active.push(tool.name.clone());
            }
        }
        let _ = self.harness.set_tools(all, Some(next_active)).await;
    }

    /// Drive reconcile in response to tool edits, instead of on a timer.
    ///
    /// Waits for the shared dirty signal, then reconciles. Repeats for the
    /// lifetime of the runtime. Because `Notify::notify_one` stores one permit
    /// when there is no waiter, an edit that lands between iterations is not
    /// lost: the next `notified().await` resolves immediately and
    /// `reconcile_tools` re-checks the dirty flag.
    ///
    /// One signal covers every store (each store fires the same `Notify`), so
    /// this is a single `notified().await` — no `select_all` or pinning needed.
    pub async fn run_reconcile(self) {
        loop {
            // Reconcile any edits already staged before we start waiting, so an
            // edit that happened during bootstrap is not deferred to the first
            // signal.
            self.reconcile_tools().await;
            self.dirty_signal.notified().await;
        }
    }

    /// The full current tool set: built-in + one-shot tools + every live store
    /// snapshot. Built-ins are prepended so they survive every full-replace
    /// `set_tools` (which would otherwise drop them when a reconcile fires).
    fn all_tools(&self) -> Vec<AgentTool> {
        let mut tools: Vec<AgentTool> = self.built_in_tools.clone();
        tools.extend(self.one_shot_tools.clone());
        for store in &self.tool_stores {
            tools.extend(ToolHandle::from_store(store.clone()).snapshot());
        }
        tools
    }

    /// The initial tool set, captured before any runtime edit. Used by agent
    /// bootstrap to seed `harness.set_tools` once.
    pub fn initial_tools(&self) -> Vec<AgentTool> {
        self.all_tools()
    }
}

// ── iodilos side ──────────────────────────────────────────────────────────

/// Pure, `Send` command metadata table. Built on tokio, then moved to iodilos
/// where it's wrapped into a [`CommandSide`] bound to the live `UiState`.
pub struct CommandTable {
    pub commands: Vec<RegisteredCommand>,
}

impl CommandTable {
    /// Bind this table to the host runtime command proxy, producing the
    /// dispatch-capable [`CommandSide`]. Called once from the iodilos mount
    /// closure, after the runtime proxy exists.
    pub fn bind(self, runtime: Arc<RuntimeCommandProxy>) -> CommandSide {
        CommandSide {
            commands: self.commands,
            ctx: ExtensionContext::new(runtime),
        }
    }
}

/// Iodilos-side command dispatcher. NOT `Send` — its context owns a runtime
/// command proxy that may hold `Rc`-based UI state behind the facade.
pub struct CommandSide {
    commands: Vec<RegisteredCommand>,
    ctx: ExtensionContext,
}

impl CommandSide {
    /// All registered commands, in registration order. The editor reads this
    /// to drive autocomplete and `/help`.
    pub fn commands(&self) -> &[RegisteredCommand] {
        &self.commands
    }

    /// Close the active extension overlap, if any. Reaches the bound
    /// runtime proxy. No-op when the active layer is Main.
    pub fn close_active_overlap(&self) {
        let ctx = self.ctx.clone();
        iodilos::prelude::use_future(async move {
            let _ = ctx.conversation.close_active_overlap().await;
        });
    }

    /// Resolve `text` against registered commands. `text` is the full input
    /// (e.g. `/mcp list`). Returns the command + args if the name matches a
    /// registered command or one of its aliases.
    pub fn resolve(&self, text: &str) -> Option<(&RegisteredCommand, String)> {
        let mut parts = text.split_whitespace();
        let name = parts.next()?;
        let args = parts.collect::<Vec<_>>().join(" ");
        let cmd = self
            .commands
            .iter()
            .find(|c| c.name == name || c.meta.aliases.iter().any(|a| a == name))?;
        Some((cmd, args))
    }

    /// Dispatch a command by awaiting the registered async handler. Returns
    /// `true` if a registered command handled it.
    pub fn dispatch(&self, text: &str) -> bool {
        let Some((cmd, args)) = self.resolve(text) else {
            return false;
        };
        let invocation = CommandInvocation {
            name: cmd.name.clone(),
            args,
        };
        let ctx = self.ctx.clone();
        let handler = cmd.handler.clone();
        iodilos::prelude::use_future(async move {
            if let Err(error) = handler(invocation, ctx.clone()).await {
                ctx.ui
                    .notify_error(format!("Extension command failed: {error}"));
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn command_side_with_rx() -> (
        CommandSide,
        flume::Receiver<super::super::types::RuntimeCommand>,
    ) {
        let mut api = ExtensionApi::new();
        super::super::mcp::McpExtension::new(Config::default(), None).register(&mut api);
        let (commands, _, _, _, _) = api.into_parts();
        let (tx, rx) = flume::unbounded();
        let side = CommandTable { commands }.bind(Arc::new(RuntimeCommandProxy::new(tx)));
        (side, rx)
    }

    /// `/mcp list` resolves to the registered command and its async handler
    /// sends a runtime notification command through ExtensionContext.
    #[tokio::test]
    async fn mcp_list_handler_notifies() {
        let (side, rx) = command_side_with_rx();
        let (cmd, args) = side.resolve("/mcp list").expect("/mcp should resolve");
        (cmd.handler)(
            CommandInvocation {
                name: cmd.name.clone(),
                args,
            },
            side.ctx.clone(),
        )
        .await
        .expect("handler should succeed");
        let cmd = rx.try_recv().expect("handler should emit one command");
        let super::super::types::RuntimeCommand::NotifyActive { text } = cmd else {
            panic!("expected notify command");
        };
        assert_eq!(text, "No MCP servers configured.");
    }

    /// A line that doesn't match any registered command returns `false` and
    /// records nothing — the static dispatcher must still get a chance.
    #[test]
    fn resolve_unknown_command_returns_none() {
        let (side, rx) = command_side_with_rx();
        assert!(side.resolve("/help").is_none());
        assert!(rx.is_empty());
    }

    /// `/mcp` with no args (or `help`) shows help text; an unknown subcommand
    /// lands in the error channel.
    #[tokio::test]
    async fn mcp_unknown_subcommand_is_error() {
        let (side, rx) = command_side_with_rx();
        let (cmd, args) = side
            .resolve("/mcp frobnicate")
            .expect("/mcp should resolve");
        (cmd.handler)(
            CommandInvocation {
                name: cmd.name.clone(),
                args,
            },
            side.ctx.clone(),
        )
        .await
        .expect("handler should succeed");
        let cmd = rx.try_recv().expect("handler should emit one command");
        let super::super::types::RuntimeCommand::NotifyErrorActive { text } = cmd else {
            panic!("expected error command");
        };
        assert!(text.contains("frobnicate"));
    }
}
