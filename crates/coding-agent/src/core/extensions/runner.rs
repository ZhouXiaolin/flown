//! Extension runtime, split along the thread boundary.
//!
//! [`Extension::register`](super::Extension::register) runs once on the
//! **tokio** side and produces pure `Send` data. That data is then split:
//!
//! - [`ToolSide`] stays on **tokio** — owns the harness and the live
//!   [`ToolStore`]s, reconciles dirty handles by calling
//!   `harness.set_tools().await`, and hosts extension background tasks.
//! - [`CommandSide`] moves to the **iodilos** side — owns the command table and
//!   the `Rc<UiState>` sink, dispatches slash commands, and interprets the
//!   returned [`CommandEffect`] into `UiState` operations.
//!
//! See `docs/m2a-extension-api-draft.md` and the threading-model note in
//! [`super::types`].

use std::rc::Rc;
use std::sync::Arc;

use flown_agent::harness::AgentHarness;
use flown_agent::types::AgentTool;

use super::types::{
    CommandEffect, CommandHandler, Extension, ExtensionApi, RegisteredCommand, ToolHandle,
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
    let (commands, one_shot_tools, _hooks, tool_stores) = api.into_parts();

    let tool_side = ToolSide {
        harness,
        built_in_tools,
        one_shot_tools,
        tool_stores,
    };
    let command_side = CommandTable { commands };
    (tool_side, command_side)
}

// ── tokio side ────────────────────────────────────────────────────────────

/// tokio-side runtime: owns the harness and reconciles tool edits.
///
/// The McpExtension's background task (cloned [`ToolHandle`] in hand) calls
/// `handle.add()`/`remove()` from tokio; the runtime periodically calls
/// [`Self::reconcile_tools`] to flush those edits into the harness.
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
}

impl ToolSide {
    /// If any [`ToolHandle`] was dirtied since the last call, rebuild the full
    /// tool set and push it to the harness via `set_tools`. Async because
    /// `set_tools` is; poll this on the tokio side.
    pub async fn reconcile_tools(&self) {
        let any_dirty = self
            .tool_stores
            .iter()
            .map(|s| ToolHandle::from_store(s.clone()).take_dirty())
            .any(|d| d);
        if !any_dirty {
            return;
        }
        let all = self.all_tools();
        let _ = self.harness.set_tools(all, None).await;
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
    /// Bind this table to the iodilos-side transcript sink, producing the
    /// dispatch-capable [`CommandSide`]. Called once from the iodilos mount
    /// closure, after `Rc<UiState>` exists.
    pub fn bind(self, sink: Rc<CommandSink>) -> CommandSide {
        CommandSide {
            commands: self.commands,
            sink,
        }
    }
}

/// iodilos-side runtime: dispatches extension slash commands and interprets
/// their [`CommandEffect`]s into `UiState` operations. NOT `Send` — it owns the
/// `Rc` transcript sink.
pub struct CommandSide {
    commands: Vec<RegisteredCommand>,
    sink: Rc<CommandSink>,
}

/// Sink the iodilos side uses to apply [`CommandEffect`]s. Holds the live
/// `Rc<UiState>` via closures captured at mount. Kept opaque so the extension
/// contract doesn't depend on TUI internals.
pub struct CommandSink {
    pub notify: Box<dyn Fn(String)>,
    pub notify_error: Box<dyn Fn(String)>,
    pub clear: Box<dyn Fn()>,
}

impl CommandSide {
    /// All registered commands, in registration order. The editor reads this
    /// to drive autocomplete and `/help`.
    pub fn commands(&self) -> &[RegisteredCommand] {
        &self.commands
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

    /// Dispatch a command: call its handler with the args and apply the
    /// returned [`CommandEffect`] to the transcript. Returns `true` if a
    /// registered command handled it.
    pub fn dispatch(&self, text: &str) -> bool {
        let Some((cmd, args)) = self.resolve(text) else {
            return false;
        };
        let effect = (cmd.handler)(&args);
        self.apply(effect);
        true
    }

    /// Interpret a [`CommandEffect`] into `UiState` operations.
    fn apply(&self, effect: CommandEffect) {
        match effect {
            CommandEffect::Notify(s) => (self.sink.notify)(s),
            CommandEffect::NotifyError(s) => (self.sink.notify_error)(s),
            CommandEffect::ClearTranscript => (self.sink.clear)(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn mcp_command_side_with_events(
    ) -> (CommandSide, Rc<RefCell<Vec<(&'static str, String)>>>) {
        let mut api = ExtensionApi::new();
        super::super::mcp::McpExtension::new(Config::default(), None).register(&mut api);
        let (commands, _, _, _) = api.into_parts();
        let events: Rc<RefCell<Vec<(&'static str, String)>>> = Rc::default();
        let sink = Rc::new(CommandSink {
            notify: {
                let e = events.clone();
                Box::new(move |text| e.borrow_mut().push(("notify", text)))
            },
            notify_error: {
                let e = events.clone();
                Box::new(move |text| e.borrow_mut().push(("error", text)))
            },
            clear: {
                let e = events.clone();
                Box::new(move || e.borrow_mut().push(("clear", String::new())))
            },
        });
        let side = CommandTable { commands }.bind(sink);
        (side, events)
    }

    /// `/mcp list` with no servers routes through dispatch → handler → sink
    /// and lands as a single `notify`. This is the end-to-end wiring test for
    /// the extension command path (ADR-0003 verification gate).
    #[test]
    fn dispatch_mcp_list_produces_notify() {
        let (side, events) = mcp_command_side_with_events();
        assert!(side.dispatch("/mcp list"));
        let recorded = events.borrow();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "notify");
        assert_eq!(recorded[0].1, "No MCP servers configured.");
    }

    /// A line that doesn't match any registered command returns `false` and
    /// records nothing — the static dispatcher must still get a chance.
    #[test]
    fn dispatch_unknown_command_returns_false() {
        let (side, events) = mcp_command_side_with_events();
        assert!(!side.dispatch("/help"));
        assert!(events.borrow().is_empty());
    }

    /// `/mcp` with no args (or `help`) shows help text; an unknown subcommand
    /// lands in the error channel.
    #[test]
    fn dispatch_mcp_unknown_subcommand_is_error() {
        let (side, events) = mcp_command_side_with_events();
        assert!(side.dispatch("/mcp frobnicate"));
        let recorded = events.borrow();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "error");
        assert!(recorded[0].1.contains("frobnicate"));
    }
}
