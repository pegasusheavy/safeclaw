//! Bidirectional adapters between safeclaw and daimon tool traits.

use std::sync::Arc;

use async_trait::async_trait;
use daimon::tool::SharedTool;

use crate::error::{Result, SafeAgentError};
use crate::tools::{Tool as SafeClawTool, ToolContext, ToolOutput as SafeClawToolOutput};

// ---------------------------------------------------------------------------
// Daimon → SafeClaw
// ---------------------------------------------------------------------------

/// Wraps a daimon [`SharedTool`] so it can be registered in safeclaw's
/// [`ToolRegistry`](crate::tools::ToolRegistry).
///
/// The adapter ignores safeclaw's `ToolContext` since daimon tools are
/// self-contained (they carry their own transport / state).
pub struct DaimonToolAdapter {
    inner: SharedTool,
    prefixed_name: String,
}

impl DaimonToolAdapter {
    pub fn new(tool: SharedTool) -> Self {
        let prefixed_name = format!("mcp_{}", tool.name());
        Self {
            inner: tool,
            prefixed_name,
        }
    }

    pub fn with_prefix(tool: SharedTool, prefix: &str) -> Self {
        let prefixed_name = format!("{}_{}", prefix, tool.name());
        Self {
            inner: tool,
            prefixed_name,
        }
    }
}

#[async_trait]
impl SafeClawTool for DaimonToolAdapter {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<SafeClawToolOutput> {
        match self.inner.execute_erased(&params).await {
            Ok(out) => {
                if out.is_error {
                    Ok(SafeClawToolOutput::error(out.content))
                } else {
                    Ok(SafeClawToolOutput::ok(out.content))
                }
            }
            Err(e) => Err(SafeAgentError::Tool(format!("daimon tool error: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// SafeClaw → Daimon
// ---------------------------------------------------------------------------

/// Wraps a safeclaw tool (behind `Arc`) so it can be used in daimon's agent
/// patterns (e.g. `AgentTool`, `Supervisor`).
///
/// Because safeclaw tools require a [`ToolContext`], the adapter captures one
/// at construction time.
pub struct SafeClawToolAdapter {
    inner: Arc<dyn SafeClawTool>,
    ctx: Arc<ToolContext>,
}

impl SafeClawToolAdapter {
    pub fn new(tool: Arc<dyn SafeClawTool>, ctx: Arc<ToolContext>) -> Self {
        Self { inner: tool, ctx }
    }
}

impl daimon::tool::Tool for SafeClawToolAdapter {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(
        &self,
        input: &serde_json::Value,
    ) -> std::result::Result<daimon::tool::ToolOutput, daimon::error::DaimonError> {
        match self.inner.execute(input.clone(), &self.ctx).await {
            Ok(out) => {
                if out.success {
                    Ok(daimon::tool::ToolOutput::text(out.output))
                } else {
                    Ok(daimon::tool::ToolOutput::error(out.output))
                }
            }
            Err(e) => Ok(daimon::tool::ToolOutput::error(format!(
                "safeclaw error: {e}"
            ))),
        }
    }
}
