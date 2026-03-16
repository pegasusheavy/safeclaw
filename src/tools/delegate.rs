use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::error::Result;

/// Tool that allows the agent to delegate tasks to specialist sub-agents.
pub struct DelegateTool {
    agent_ref: Arc<OnceLock<Weak<Agent>>>,
}

impl DelegateTool {
    pub fn new(agent_ref: Arc<OnceLock<Weak<Agent>>>) -> Self {
        Self { agent_ref }
    }

    fn agent(&self) -> Result<Arc<Agent>> {
        self.agent_ref
            .get()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| crate::error::SafeAgentError::Tool("agent not initialized".into()))
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a task to a specialist sub-agent. Spawns a session with a \
         specific persona (coder, researcher, writer, planner) and runs it \
         to completion. Use this for parallel research, code review, writing, \
         or any task that benefits from a specialist perspective."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task for the sub-agent to complete"
                },
                "persona": {
                    "type": "string",
                    "description": "Persona ID: coder, researcher, writer, planner, or a custom persona ID",
                    "default": "default"
                },
                "label": {
                    "type": "string",
                    "description": "Short label for the session",
                    "default": "delegation"
                },
                "wait": {
                    "type": "boolean",
                    "description": "If true (default), waits for the sub-agent to finish and returns the result. If false, spawns asynchronously and returns the session ID.",
                    "default": true
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let agent = self.agent()?;

        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let persona = params
            .get("persona")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("delegation");
        let wait = params
            .get("wait")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if task.is_empty() {
            return Ok(ToolOutput::error("task is required"));
        }

        let session_id = agent.create_session(label, persona, task).await?;

        if wait {
            let result = agent.run_session(&session_id, persona, 10).await?;

            Ok(ToolOutput {
                output: format!(
                    "Sub-agent ({persona}) completed.\n\
                     Session: {session_id}\n\n\
                     Result:\n{result}"
                ),
                success: true,
                metadata: Some(json!({
                    "session_id": session_id,
                    "persona": persona,
                })),
            })
        } else {
            Ok(ToolOutput {
                output: format!(
                    "Delegated to {persona} sub-agent.\n\
                     Session: {session_id}\n\
                     The task will be processed in the background."
                ),
                success: true,
                metadata: Some(json!({
                    "session_id": session_id,
                    "persona": persona,
                    "async": true,
                })),
            })
        }
    }
}
