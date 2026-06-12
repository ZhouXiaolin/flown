use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::mcp::McpServerConfig;

/// A provider configuration (API key based).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub key: String,
}

/// Agent default model binding (provider/model_name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefault {
    pub model: String,
}

/// Top-level configuration loaded from ~/.flown/config.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default model to use (e.g. "openrouter/openrouter/free")
    pub model: String,

    /// API provider (e.g. "anthropic", "openai")
    pub provider: String,

    /// Environment variable name containing the API key
    pub api_key_env: String,

    /// Theme name
    pub theme: String,

    /// Directory containing skill definitions
    pub skills_dir: PathBuf,

    /// Directory for session storage
    pub sessions_dir: PathBuf,

    /// Directory for workflow files
    pub workflows_dir: PathBuf,

    /// MCP server configurations
    #[serde(rename = "mcpServers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,

    /// Provider configurations (name -> ProviderConfig)
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    /// Agent configurations (name -> AgentDefault)
    #[serde(default)]
    pub agent: BTreeMap<String, AgentDefault>,

    /// Additional system prompt text
    pub system_prompt_extra: String,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let flown_dir = home.join(".flown");

        Self {
            model: "deepseek/deepseek-v4-flash".into(),
            provider: "deepseek".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            theme: "arctic".into(),
            skills_dir: flown_dir.join("skills"),
            sessions_dir: flown_dir.join("sessions"),
            workflows_dir: flown_dir.join("workflows"),
            mcp_servers: BTreeMap::new(),
            providers: BTreeMap::new(),
            agent: BTreeMap::new(),
            system_prompt_extra: String::new(),
        }
    }
}

impl Config {
    /// Load config from ~/.flown/config.json, creating default if missing.
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: Config = serde_json::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    /// Save config to ~/.flown/config.json.
    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::config_path();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&config_path, format!("{content}\n"))?;
        Ok(())
    }

    /// Path to the config file.
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".flown")
            .join("config.json")
    }

    /// Resolve the default model string from agent.default config.
    /// Falls back to the `model` field if agent.default is not set.
    pub fn resolve_default_model(&self) -> String {
        self.agent
            .get("default")
            .map(|d| d.model.clone())
            .unwrap_or_else(|| self.model.clone())
    }

    /// Resolve provider name and API key for a model string like "provider/model".
    /// Returns (provider_name, api_key).
    pub fn resolve_provider_and_key(&self, model_str: &str) -> (String, Option<String>) {
        let provider_name = model_str
            .split('/')
            .next()
            .unwrap_or(model_str)
            .to_string();

        let api_key = self.providers.get(&provider_name).map(|p| p.key.clone());

        (provider_name, api_key)
    }

    /// Get the API key from the environment.
    pub fn api_key(&self) -> Option<String> {
        std::env::var(&self.api_key_env).ok()
    }
}
