#[cfg(feature = "browser")]
pub mod browser;
pub mod cron;
pub mod exec;
pub mod file;
pub mod goal;
pub mod image;
pub mod knowledge;
pub mod memory;
pub mod message;
pub mod process;
pub mod sessions;
pub mod web;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::error::{Result, SafeAgentError};
use crate::messaging::MessagingManager;
use crate::security::SandboxedFs;
use crate::trash::TrashManager;

/// Output from a tool execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ToolOutput {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            metadata: None,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            metadata: None,
        }
    }

    pub fn ok_with_meta(output: impl Into<String>, meta: serde_json::Value) -> Self {
        Self {
            success: true,
            output: output.into(),
            metadata: Some(meta),
        }
    }
}

/// A tool call proposed by the LLM.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub params: serde_json::Value,
    pub reasoning: String,
}

/// Shared context passed to tools during execution.
pub struct ToolContext {
    pub sandbox: SandboxedFs,
    pub db: Arc<Mutex<Connection>>,
    /// Read-only connection for SELECT queries (reduces mutex contention).
    pub db_read: Arc<Mutex<Connection>>,
    pub http_client: reqwest::Client,
    pub messaging: Arc<MessagingManager>,
    pub trash: Arc<TrashManager>,
}

/// The trait all tools implement.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name of the tool (e.g. "exec", "web_search").
    fn name(&self) -> &str;

    /// Human-readable description for the LLM prompt.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given parameters.
    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput>;
}

/// Registry of all available tools.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Panics on duplicate names.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        assert!(
            !self.tools.contains_key(&name),
            "duplicate tool name: {name}"
        );
        self.tools.insert(name, tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// List all registered tools as (name, description) pairs.
    pub fn list(&self) -> Vec<(&str, &str)> {
        let mut items: Vec<_> = self
            .tools
            .values()
            .map(|t| (t.name(), t.description()))
            .collect();
        items.sort_by_key(|(name, _)| *name);
        items
    }

    /// Execute a tool by name.
    pub async fn execute(
        &self,
        name: &str,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| SafeAgentError::ToolNotFound(name.to_string()))?;
        tool.execute(params, ctx).await
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messaging::MessagingManager;
    use crate::trash::TrashManager;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct MockTool {
        name: &'static str,
        description: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            self.description
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "input": { "type": "string" } }
            })
        }

        async fn execute(
            &self,
            params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput> {
            let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("default");
            Ok(ToolOutput::ok(format!("mock: {}", input)))
        }
    }

    fn make_test_context() -> ToolContext {
        let tmp = std::env::temp_dir().join("safeclaw-tools-test");
        std::fs::create_dir_all(&tmp).unwrap();
        let sandbox = SandboxedFs::new(tmp.clone()).unwrap();
        let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        let db_read = db.clone(); // In-memory: use same connection for tests
        let http_client = reqwest::Client::new();
        let messaging = Arc::new(MessagingManager::new());
        let trash = Arc::new(TrashManager::new(Path::new(&tmp)).unwrap());

        ToolContext {
            sandbox,
            db,
            db_read,
            http_client,
            messaging,
            trash,
        }
    }

    #[test]
    fn test_tool_output_ok() {
        let out = ToolOutput::ok("success");
        assert!(out.success);
        assert_eq!(out.output, "success");
        assert!(out.metadata.is_none());
    }

    #[test]
    fn test_tool_output_error() {
        let out = ToolOutput::error("failed");
        assert!(!out.success);
        assert_eq!(out.output, "failed");
        assert!(out.metadata.is_none());
    }

    #[test]
    fn test_tool_output_ok_with_meta() {
        let meta = serde_json::json!({"count": 42});
        let out = ToolOutput::ok_with_meta("done", meta.clone());
        assert!(out.success);
        assert_eq!(out.output, "done");
        assert_eq!(out.metadata, Some(meta));
    }

    #[test]
    fn test_tool_registry_new() {
        let reg = ToolRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn test_tool_registry_register_get_list_len() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(MockTool {
            name: "mock_a",
            description: "First mock",
        }));
        reg.register(Box::new(MockTool {
            name: "mock_b",
            description: "Second mock",
        }));

        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 2);

        let tool = reg.get("mock_a").unwrap();
        assert_eq!(tool.name(), "mock_a");
        assert_eq!(tool.description(), "First mock");

        assert!(reg.get("nonexistent").is_none());

        let list = reg.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, "mock_a");
        assert_eq!(list[1].0, "mock_b");
    }

    #[tokio::test]
    async fn test_tool_registry_execute_unknown_tool() {
        let reg = ToolRegistry::new();
        let ctx = make_test_context();

        let result = reg
            .execute("unknown_tool", serde_json::json!({}), &ctx)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("tool not found"));
        assert!(err.to_string().contains("unknown_tool"));
    }

    #[test]
    fn test_tool_output_serde_roundtrip() {
        let o = ToolOutput::ok("hello");
        let json = serde_json::to_string(&o).unwrap();
        let deser: ToolOutput = serde_json::from_str(&json).unwrap();
        assert!(deser.success);
        assert_eq!(deser.output, "hello");
    }

    #[test]
    fn test_tool_output_metadata_skip_when_none() {
        let o = ToolOutput::ok("hi");
        let json = serde_json::to_string(&o).unwrap();
        assert!(!json.contains("metadata"));
    }

    #[test]
    fn test_tool_call_serde() {
        let call = ToolCall {
            tool: "exec".into(),
            params: serde_json::json!({"cmd": "ls"}),
            reasoning: "list".into(),
        };
        let json = serde_json::to_string(&call).unwrap();
        let deser: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.tool, "exec");
        assert_eq!(deser.reasoning, "list");
    }

    #[test]
    #[should_panic(expected = "duplicate tool name")]
    fn test_tool_registry_duplicate_panics() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(MockTool { name: "dup", description: "a" }));
        reg.register(Box::new(MockTool { name: "dup", description: "b" }));
    }

    #[tokio::test]
    async fn test_tool_registry_execute_mock_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(MockTool {
            name: "mock",
            description: "Mock tool",
        }));
        let ctx = make_test_context();

        let result = reg
            .execute("mock", serde_json::json!({"input": "hello"}), &ctx)
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "mock: hello");
    }
}
