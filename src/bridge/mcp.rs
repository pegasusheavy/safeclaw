//! MCP client manager — connects to configured MCP servers and bridges
//! their tools into safeclaw's [`ToolRegistry`].

use std::sync::Arc;

use daimon::mcp::{HttpTransport, McpClient, StdioTransport};
use daimon::tool::ErasedTool;
use tracing::{error, info, warn};

use crate::config::McpServerEntry;
use crate::tools::ToolRegistry;

use super::DaimonToolAdapter;

/// Manages connections to MCP tool servers.
pub struct McpManager {
    clients: Vec<(String, McpClient)>,
}

impl McpManager {
    /// Connects to all configured MCP servers and returns a manager.
    ///
    /// Servers that fail to connect are logged and skipped — a single
    /// bad server won't prevent the agent from starting.
    pub async fn connect_all(servers: &[McpServerEntry]) -> Self {
        let mut clients = Vec::new();

        for entry in servers {
            match Self::connect_one(entry).await {
                Ok(client) => {
                    let n = client.tool_infos().len();
                    info!(
                        server = %entry.name,
                        tools = n,
                        "MCP server connected"
                    );
                    clients.push((entry.name.clone(), client));
                }
                Err(e) => {
                    error!(
                        server = %entry.name,
                        error = %e,
                        "failed to connect to MCP server, skipping"
                    );
                }
            }
        }

        Self { clients }
    }

    /// Registers all discovered MCP tools into the given registry.
    ///
    /// Tool names are prefixed with `mcp_{server}_{tool}` to avoid
    /// collisions with built-in safeclaw tools.
    pub fn register_tools(self, registry: &mut ToolRegistry) {
        for (server_name, client) in &self.clients {
            let daimon_tools = client.tools();
            for tool in daimon_tools {
                let prefix = format!("mcp_{server_name}");
                let tool_name = tool.name().to_string();
                let shared: Arc<dyn ErasedTool> = Arc::new(tool);
                let adapter = DaimonToolAdapter::with_prefix(shared, &prefix);
                info!(
                    tool = %format!("{prefix}_{tool_name}"),
                    server = %server_name,
                    "registered MCP tool"
                );
                registry.register(Box::new(adapter));
            }
        }
    }

    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    pub fn total_tools(&self) -> usize {
        self.clients
            .iter()
            .map(|(_, c)| c.tool_infos().len())
            .sum()
    }

    async fn connect_one(entry: &McpServerEntry) -> crate::error::Result<McpClient> {
        match entry.transport.as_str() {
            "stdio" => {
                if entry.command.is_empty() {
                    return Err(crate::error::SafeAgentError::Config(format!(
                        "MCP server '{}': stdio transport requires `command`",
                        entry.name
                    )));
                }
                let transport = StdioTransport::new(&entry.command, &entry.args)
                    .await
                    .map_err(|e| {
                        crate::error::SafeAgentError::Config(format!(
                            "MCP server '{}' stdio spawn failed: {e}",
                            entry.name
                        ))
                    })?;
                McpClient::connect(transport).await.map_err(|e| {
                    crate::error::SafeAgentError::Config(format!(
                        "MCP server '{}' handshake failed: {e}",
                        entry.name
                    ))
                })
            }
            "http" | "sse" => {
                if entry.url.is_empty() {
                    return Err(crate::error::SafeAgentError::Config(format!(
                        "MCP server '{}': http transport requires `url`",
                        entry.name
                    )));
                }
                let transport = HttpTransport::new(&entry.url);
                McpClient::connect(transport).await.map_err(|e| {
                    crate::error::SafeAgentError::Config(format!(
                        "MCP server '{}' handshake failed: {e}",
                        entry.name
                    ))
                })
            }
            other => {
                warn!(
                    server = %entry.name,
                    transport = %other,
                    "unknown MCP transport type, skipping"
                );
                Err(crate::error::SafeAgentError::Config(format!(
                    "MCP server '{}': unknown transport '{other}'",
                    entry.name
                )))
            }
        }
    }
}
