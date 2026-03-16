use tracing::{debug, error, info};

use crate::error::Result;
use crate::llm::GenerateContext;

use super::Agent;

const MAX_SESSION_CONTEXT_MESSAGES: usize = 50;

impl Agent {
    /// Process all active sessions that have pending (unprocessed) messages.
    /// Called from the tick loop.
    pub async fn process_pending_sessions(&self) -> Result<()> {
        let sessions = self.find_pending_sessions().await?;
        if sessions.is_empty() {
            return Ok(());
        }

        debug!(count = sessions.len(), "processing pending sessions");

        let max_turns = self.config.sessions.max_turns;
        for (session_id, agent_id) in sessions {
            if let Err(e) = self.run_session(&session_id, &agent_id, max_turns).await {
                error!(session_id, err = %e, "session processing failed");
                self.store_session_message(&session_id, "system", &format!("Error: {e}"))
                    .await
                    .ok();
            }
        }

        Ok(())
    }

    /// Run a session to completion (or max_turns). Returns the final response.
    pub async fn run_session(
        &self,
        session_id: &str,
        agent_id: &str,
        max_turns: usize,
    ) -> Result<String> {
        let persona = super::personas::get_persona(
            &self.ctx.db,
            agent_id,
            &self.config.core_personality,
        )
        .await;

        info!(
            session_id,
            persona = %persona.id,
            persona_name = %persona.name,
            "running session"
        );

        let mut last_response = String::new();

        for turn in 0..max_turns {
            let messages = self.get_session_messages(session_id).await?;
            if messages.is_empty() {
                break;
            }

            // Check if the last message is from the assistant (nothing to process)
            if let Some(last) = messages.last() {
                if last.0 == "assistant" {
                    break;
                }
            }

            let prompt = build_session_prompt(&persona, &messages);

            let gen_ctx = GenerateContext {
                message: &prompt,
                tools: Some(&self.tools),
                prompt_skills: &self.always_on_skills,
            };

            let response = self.llm.generate(&gen_ctx).await?;

            // Store assistant response
            self.store_session_message(session_id, "assistant", &response)
                .await?;

            // Parse tool calls
            let parsed = super::tool_parse::parse_llm_response(&response);

            if parsed.tool_calls.is_empty() {
                last_response = parsed.text;
                // No tool calls — session turn complete
                self.mark_session_completed(session_id).await.ok();
                break;
            }

            // Execute tool calls (auto-approved for sub-agents, skip others)
            let mut results = Vec::new();
            for call in &parsed.tool_calls {
                // Check if the persona restricts tool access
                if let Some(ref allowed) = persona.allowed_tools() {
                    if !allowed.contains(&call.tool.as_str()) {
                        results.push(format!(
                            "[{} skipped] not in persona's allowed tools",
                            call.tool
                        ));
                        continue;
                    }
                }

                if self.auto_approve.contains(call.tool.as_str()) {
                    match super::actions::execute_tool_call(&self.tools, &self.ctx, call).await {
                        Ok(output) => {
                            results.push(format!("[{}] {}", call.tool, output.output));
                        }
                        Err(e) => {
                            results.push(format!("[{} error] {}", call.tool, e));
                        }
                    }
                } else {
                    results.push(format!(
                        "[{} skipped] requires approval — not auto-approved for sub-agent execution",
                        call.tool
                    ));
                }
            }

            // Store tool results
            if !results.is_empty() {
                let tool_msg = results.join("\n");
                self.store_session_message(session_id, "tool", &tool_msg)
                    .await?;
            }

            last_response = parsed.text;

            debug!(
                session_id,
                turn,
                tool_calls = parsed.tool_calls.len(),
                "session turn complete"
            );
        }

        Ok(last_response)
    }

    /// Find sessions with pending messages (last message is not from assistant).
    async fn find_pending_sessions(&self) -> Result<Vec<(String, String)>> {
        let db = self.ctx.db.lock().await;
        let mut stmt = db.prepare(
            "SELECT s.id, s.agent_id
             FROM sessions s
             WHERE s.status = 'active'
             AND EXISTS (
                 SELECT 1 FROM session_messages sm
                 WHERE sm.session_id = s.id
                 AND sm.role IN ('user', 'system', 'tool')
                 AND sm.id = (SELECT MAX(id) FROM session_messages WHERE session_id = s.id)
             )
             ORDER BY s.updated_at ASC
             LIMIT 5",
        )?;

        let sessions = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(sessions)
    }

    /// Get session message history.
    async fn get_session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let db = self.ctx.db_read.lock().await;
        let mut stmt = db.prepare(
            "SELECT role, content FROM session_messages
             WHERE session_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;

        let mut messages: Vec<(String, String)> = stmt
            .query_map(
                rusqlite::params![session_id, MAX_SESSION_CONTEXT_MESSAGES],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .filter_map(|r| r.ok())
            .collect();

        messages.reverse();
        Ok(messages)
    }

    /// Store a message in a session.
    pub async fn store_session_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<()> {
        let db = self.ctx.db.lock().await;
        db.execute(
            "INSERT INTO session_messages (session_id, role, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, role, content],
        )?;

        db.execute(
            "UPDATE sessions SET updated_at = datetime('now') WHERE id = ?1",
            [session_id],
        )?;

        Ok(())
    }

    /// Mark a session as completed.
    async fn mark_session_completed(&self, session_id: &str) -> Result<()> {
        let db = self.ctx.db.lock().await;
        db.execute(
            "UPDATE sessions SET status = 'completed', updated_at = datetime('now') WHERE id = ?1",
            [session_id],
        )?;
        Ok(())
    }

    /// Create a new session and return its ID.
    pub async fn create_session(
        &self,
        label: &str,
        agent_id: &str,
        initial_message: &str,
    ) -> Result<String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let db = self.ctx.db.lock().await;

        db.execute(
            "INSERT INTO sessions (id, label, agent_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, label, agent_id],
        )?;

        db.execute(
            "INSERT INTO session_messages (session_id, role, content) VALUES (?1, 'system', ?2)",
            rusqlite::params![session_id, initial_message],
        )?;

        info!(session_id, label, agent_id, "session created");
        Ok(session_id)
    }
}

/// Build a prompt string from persona and session messages.
fn build_session_prompt(
    persona: &super::personas::Persona,
    messages: &[(String, String)],
) -> String {
    let mut prompt = format!(
        "[System]\nYou are {}, a specialist agent.\n\n{}\n\n\
         You are working in a delegated sub-session. Complete the task \
         efficiently and provide a clear, concise result. Use tools when \
         needed.\n\n",
        persona.name, persona.personality
    );

    for (role, content) in messages {
        let label = match role.as_str() {
            "system" => "[Task]",
            "user" => "[User]",
            "assistant" => "[Assistant]",
            "tool" => "[Tool Results]",
            _ => "[Message]",
        };
        prompt.push_str(&format!("{label}\n{content}\n\n"));
    }

    prompt
}
