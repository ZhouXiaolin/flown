use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "flown",
    about = "Terminal coding agent powered by LLM",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Model to use (overrides config)
    #[arg(short, long, global = true)]
    pub model: Option<String>,

    /// API provider (overrides config)
    #[arg(short, long, global = true)]
    pub provider: Option<String>,

    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start an interactive coding session (default)
    Chat {
        /// Initial prompt to send
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
    },

    /// Shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,

    /// Set a configuration value
    Set {
        /// Configuration key
        key: String,
        /// Value to set
        value: String,
    },

    /// Open config file in editor
    Edit,
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// List configured MCP servers
    List,

    /// Show MCP server status
    Status,

    /// List available MCP tools
    Tools,

    /// Call an MCP tool via JSON-RPC
    Call {
        /// MCP server name
        server: String,
        /// Tool name to call
        tool: String,
        /// JSON arguments
        #[arg(long, default_value = "{}")]
        args: String,
    },
}
