use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::error::Result;
use crate::llm::GenerateContext;

/// Tool for collaborative planning where multiple personas discuss and
/// refine a plan before execution.
pub struct PlanTool {
    agent_ref: Arc<OnceLock<Weak<Agent>>>,
}

impl PlanTool {
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
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "plan"
    }

    fn description(&self) -> &str {
        "Run a multi-persona collaborative planning session. Multiple \
         specialist personas (e.g. coder, researcher, planner) take turns \
         discussing an objective, then a final synthesis produces an \
         actionable plan. Use this for complex tasks that benefit from \
         multiple perspectives before execution."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "The objective to plan for"
                },
                "personas": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Persona IDs to include (e.g. [\"coder\", \"researcher\", \"planner\"]). Default: [\"coder\", \"researcher\", \"planner\"]"
                },
                "rounds": {
                    "type": "integer",
                    "description": "Number of discussion rounds (1-5). Default: 2",
                    "minimum": 1,
                    "maximum": 5
                },
                "context": {
                    "type": "string",
                    "description": "Additional context or constraints for the planning session"
                }
            },
            "required": ["objective"]
        })
    }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        let agent = self.agent()?;

        let objective = params
            .get("objective")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let context = params
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let rounds = params
            .get("rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(2)
            .min(5) as usize;

        if objective.is_empty() {
            return Ok(ToolOutput::error("objective is required"));
        }

        let persona_ids: Vec<String> = params
            .get("personas")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "coder".to_string(),
                    "researcher".to_string(),
                    "planner".to_string(),
                ]
            });

        if persona_ids.is_empty() {
            return Ok(ToolOutput::error("at least one persona is required"));
        }

        // Load persona details
        let mut personas = Vec::new();
        for id in &persona_ids {
            let p = crate::agent::personas::get_persona(
                &agent.ctx.db,
                id,
                &agent.config.core_personality,
            )
            .await;
            personas.push(p);
        }

        let mut discussion = Vec::new();
        let mut full_transcript = String::new();

        let context_block = if context.is_empty() {
            String::new()
        } else {
            format!("\n\nAdditional context:\n{context}")
        };

        // Discussion rounds
        for round in 1..=rounds {
            for persona in &personas {
                let prev_discussion = if discussion.is_empty() {
                    String::from("(No prior discussion yet — you are starting.)")
                } else {
                    discussion.join("\n\n")
                };

                let prompt = format!(
                    "[System]\n\
                     You are {name}, a specialist with this expertise:\n{personality}\n\n\
                     You are in a collaborative planning session (round {round}/{rounds}).\n\n\
                     Objective: {objective}{context_block}\n\n\
                     Discussion so far:\n{prev_discussion}\n\n\
                     Contribute your perspective. Be specific and actionable. \
                     Build on others' points, add what's missing, flag risks \
                     from your area of expertise. Keep your response focused \
                     (2-4 paragraphs).",
                    name = persona.name,
                    personality = persona.personality,
                );

                let gen_ctx = GenerateContext {
                    message: &prompt,
                    tools: None,
                    prompt_skills: &[],
                };

                match agent.llm.generate(&gen_ctx).await {
                    Ok(response) => {
                        let entry = format!("**{} (round {round}):**\n{response}", persona.name);
                        discussion.push(entry.clone());
                        full_transcript.push_str(&entry);
                        full_transcript.push_str("\n\n---\n\n");
                    }
                    Err(e) => {
                        let entry = format!(
                            "**{} (round {round}):** [Error: {e}]",
                            persona.name
                        );
                        discussion.push(entry);
                    }
                }
            }
        }

        // Final synthesis
        let synthesis_prompt = format!(
            "[System]\n\
             You are a strategic planner synthesizing a collaborative discussion \
             into a concrete, actionable plan.\n\n\
             Objective: {objective}{context_block}\n\n\
             Full discussion:\n{transcript}\n\n\
             Synthesize the discussion into a clear, actionable plan with:\n\
             1. Executive summary (2-3 sentences)\n\
             2. Numbered action items with owners/roles\n\
             3. Key risks and mitigations\n\
             4. Success criteria\n\n\
             Be specific and practical.",
            transcript = discussion.join("\n\n"),
        );

        let gen_ctx = GenerateContext {
            message: &synthesis_prompt,
            tools: None,
            prompt_skills: &[],
        };

        let plan = match agent.llm.generate(&gen_ctx).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolOutput {
                    output: format!(
                        "Planning discussion completed but synthesis failed: {e}\n\n\
                         Raw discussion:\n{full_transcript}"
                    ),
                    success: false,
                    metadata: None,
                });
            }
        };

        full_transcript.push_str("**Final Plan:**\n");
        full_transcript.push_str(&plan);

        let persona_names: Vec<&str> = personas.iter().map(|p| p.name.as_str()).collect();

        Ok(ToolOutput {
            output: format!(
                "Collaborative plan ({rounds} rounds, {} personas: {}):\n\n{plan}",
                personas.len(),
                persona_names.join(", ")
            ),
            success: true,
            metadata: Some(json!({
                "rounds": rounds,
                "personas": persona_ids,
                "full_transcript_length": full_transcript.len(),
            })),
        })
    }
}
