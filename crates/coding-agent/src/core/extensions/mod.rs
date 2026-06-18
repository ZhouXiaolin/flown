//! Extension layer (M2a).
//!
//! Composable capabilities plugged into the agent without modifying the core
//! event loop. An [`Extension`](types::Extension) publishes commands, tools,
//! and hooks via an [`ExtensionApi`](types::ExtensionApi) during a one-time
//! `register` pass; the runtime splits the result along the thread boundary
//! (see [`runner`] and the threading-model doc there).
//!
//! See `docs/adr/0003-extension-layer-m2-refinement.md` for the design and
//! `docs/m2a-extension-api-draft.md` for the per-extension writing convention.
//!
//! M2a ships exactly one extension: [`mcp::McpExtension`] (the `/mcp` command
//! plus MCP tools with runtime add/remove). Application-ontology behavior
//! (built-in tools read/bash/edit/write, `/help` `/clear` `/quit` `/skills`
//! `/skill:xxx`, …) stays in its existing modules and does **not** go through
//! this layer.

pub mod btw;
pub mod mcp;
pub mod runner;
pub mod types;

use std::sync::Arc;

use flown_agent::{AgentHarness, AgentTool};

use crate::config::Config;

pub use runner::{CommandSide, CommandSink, CommandTable, ToolSide, build};
pub use types::{ControlRuntime, Extension, OverlapOptions, SlashCommandScope};

/// Run every extension's `register` on the tokio side and split the result.
///
/// Returns the tokio-side [`ToolSide`] (owns the harness, reconciles tools)
/// and the iodilos-side [`CommandTable`] (pure metadata, to be `bind`-ed to
/// the `UiState` sink once iodilos has mounted).
///
/// `built_in_tools` are the non-extension tools (read/bash/edit/write) that
/// live outside this layer. They are held by [`ToolSide`] so every
/// `harness.set_tools` call carries the **full** set — `set_tools` is full-replace
/// (`harness.rs:528`), so omitting them would wipe the built-ins on reconcile.
pub fn build_runner(
    harness: Arc<AgentHarness>,
    _config: Config,
    built_in_tools: Vec<AgentTool>,
    mcp: Option<Arc<tokio::sync::Mutex<crate::core::mcp::McpManager>>>,
) -> (ToolSide, CommandTable) {
    let extensions: Vec<Box<dyn Extension>> = vec![
        Box::new(mcp::McpExtension::new(_config.clone(), mcp)),
        Box::new(btw::BtwExtension::new()),
    ];
    build(harness, built_in_tools, extensions)
}
