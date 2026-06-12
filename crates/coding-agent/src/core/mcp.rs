use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::types::{McpServerInfo, McpServerStatus, ToolInfo};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub r#type: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("mcp server '{0}' not configured")]
    ServerNotConfigured(String),
    #[error("mcp server '{0}' is disabled")]
    ServerDisabled(String),
    #[error("mcp server '{0}' closed stdout")]
    ServerClosed(String),
    #[error("mcp server '{server}' returned error for {method}: {error}")]
    JsonRpc {
        server: String,
        method: String,
        error: Value,
    },
    #[error("mcp response for {method} from '{server}' did not include result")]
    MissingResult { server: String, method: String },
    #[error("invalid mcp tool name: {0}")]
    InvalidToolName(String),
    #[error("mcp server '{0}' is not connected")]
    ServerNotConnected(String),
}

#[derive(Debug)]
pub struct McpManager {
    configs: BTreeMap<String, McpServerConfig>,
    clients: HashMap<String, McpClient>,
    errors: HashMap<String, String>,
}

impl McpManager {
    pub fn new(configs: BTreeMap<String, McpServerConfig>) -> Self {
        Self {
            configs,
            clients: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    pub fn from_config(config: McpConfig) -> Self {
        Self::new(config.mcp_servers)
    }

    pub fn configs(&self) -> &BTreeMap<String, McpServerConfig> {
        &self.configs
    }

    pub fn is_connected(&self, name: &str) -> bool {
        self.clients.contains_key(name)
    }

    pub async fn connect(&mut self, name: &str) -> Result<(), McpError> {
        let config = self
            .configs
            .get(name)
            .cloned()
            .ok_or_else(|| McpError::ServerNotConfigured(name.to_string()))?;
        if config.disabled {
            return Err(McpError::ServerDisabled(name.to_string()));
        }

        let client = McpClient::connect(name, &config).await?;
        self.errors.remove(name);
        self.clients.insert(name.to_string(), client);
        Ok(())
    }

    pub async fn connect_all(&mut self) {
        let names = self
            .configs
            .iter()
            .filter(|(_, config)| !config.disabled)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();

        for name in names {
            if let Err(error) = self.connect(&name).await {
                self.errors.insert(name, error.to_string());
            }
        }
    }

    pub fn server_info(&self) -> Vec<McpServerInfo> {
        self.configs
            .iter()
            .map(|(name, config)| {
                if let Some(client) = self.clients.get(name) {
                    return McpServerInfo {
                        name: name.clone(),
                        status: McpServerStatus::Connected,
                        command: config.command.clone(),
                        args: config.args.clone(),
                        tool_count: client.tools.len(),
                        error: None,
                    };
                }
                if let Some(error) = self.errors.get(name) {
                    return McpServerInfo {
                        name: name.clone(),
                        status: McpServerStatus::Error,
                        command: config.command.clone(),
                        args: config.args.clone(),
                        tool_count: 0,
                        error: Some(error.clone()),
                    };
                }
                McpServerInfo {
                    name: name.clone(),
                    status: McpServerStatus::Disconnected,
                    command: config.command.clone(),
                    args: config.args.clone(),
                    tool_count: 0,
                    error: None,
                }
            })
            .collect()
    }

    pub fn tool_infos(&self) -> Vec<ToolInfo> {
        let mut tools = Vec::new();
        for (server_name, client) in &self.clients {
            for tool in &client.tools {
                let name = format!("mcp__{server_name}__{}", tool.name);
                tools.push(ToolInfo {
                    name: name.clone(),
                    label: name,
                    description: tool.description.clone().unwrap_or_default(),
                    input_schema: tool.input_schema.clone(),
                    source: Some(format!("mcp:{server_name}")),
                });
            }
        }
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        tools
    }

    pub async fn call_tool(
        &mut self,
        full_name: &str,
        arguments: Value,
    ) -> Result<String, McpError> {
        let Some(rest) = full_name.strip_prefix("mcp__") else {
            return Err(McpError::InvalidToolName(full_name.to_string()));
        };
        let Some((server_name, tool_name)) = rest.split_once("__") else {
            return Err(McpError::InvalidToolName(full_name.to_string()));
        };
        let client = self
            .clients
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotConnected(server_name.to_string()))?;
        client.call_tool(tool_name, arguments).await
    }
}

#[derive(Debug)]
struct McpClient {
    name: String,
    _child: Child,
    stdin: ChildStdin,
    reader: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    tools: Vec<McpTool>,
}

impl McpClient {
    async fn connect(name: &str, config: &McpServerConfig) -> Result<Self, McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("mcp stdin was not captured");
        let stdout = child.stdout.take().expect("mcp stdout was not captured");
        let reader = BufReader::new(stdout).lines();
        let mut client = Self {
            name: name.to_string(),
            _child: child,
            stdin,
            reader,
            next_id: 1,
            tools: Vec::new(),
        };

        client.initialize().await?;
        client.tools = client.fetch_tools().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<(), McpError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "flown-coding-agent",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn fetch_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(serde_json::from_value(
            result.get("tools").cloned().unwrap_or_else(|| json!([])),
        )?)
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<String, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await?;
        Ok(mcp_tool_result_text(&result))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(request).await?;

        loop {
            let Some(line) = self.reader.next_line().await? else {
                return Err(McpError::ServerClosed(self.name.clone()));
            };
            if line.trim().is_empty() {
                continue;
            }
            let response: Value = serde_json::from_str(&line)?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(McpError::JsonRpc {
                    server: self.name.clone(),
                    method: method.to_string(),
                    error: error.clone(),
                });
            }
            return response
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::MissingResult {
                    server: self.name.clone(),
                    method: method.to_string(),
                });
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.write_message(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_message(&mut self, message: Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(&message)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

fn mcp_tool_result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .filter(|texts| !texts.is_empty())
        .map(|texts| texts.join("\n"))
        .unwrap_or_else(|| result.to_string())
}
