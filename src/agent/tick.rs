use tracing::{debug, error, info, warn};

use crate::error::Result;
use crate::goals::{GoalManager, GoalStatus, TaskStatus};
use crate::llm::GenerateContext;
use crate::tools::ToolCall;

use super::{truncate_preview, Agent};

impl Agent {
    /// Maintenance tick: expire stale actions, run cron jobs, process goals.
    pub async fn tick(&self) -> Result<()> {
        // Expire stale pending actions
        let expired = self.approval_queue.expire_stale().await?;
        if expired > 0 {
            info!(count = expired, "expired stale actions");
        }

        // Run due cron jobs
        if let Err(e) = self.run_due_cron_jobs().await {
            error!(err = %e, "cron job execution failed");
        }

        // Process background goals
        if let Err(e) = self.process_background_goals().await {
            error!(err = %e, "background goal processing failed");
        }

        // Process pending sessions (multi-agent)
        if self.config.sessions.enabled {
            if let Err(e) = self.process_pending_sessions().await {
                error!(err = %e, "session processing failed");
            }
        }

        // Memory consolidation: periodically summarize old memories
        if let Err(e) = self.consolidate_memories().await {
            error!(err = %e, "memory consolidation failed");
        }

        // Record tick
        self.memory.record_tick().await?;

        Ok(())
    }

    /// Run memory consolidation: summarize old archival memories to keep context manageable.
    async fn consolidate_memories(&self) -> crate::error::Result<()> {
        let age_days = self.config.memory.consolidation_age_days;
        let batch = self.config.memory.consolidation_batch_size;

        let pending = crate::memory::consolidation::pending_consolidation_count(
            self.memory.db(),
            age_days,
        ).await?;

        if pending == 0 {
            return Ok(());
        }

        debug!(pending, age_days, "old memories pending consolidation");

        let consolidated = crate::memory::consolidation::consolidate_old_memories(
            self.memory.db(),
            &self.llm,
            age_days,
            batch,
        ).await?;

        if consolidated > 0 {
            info!(consolidated, "archival memories consolidated");
        }

        Ok(())
    }

    /// Process background goals: find the next actionable task and execute it.
    ///
    /// Called every tick. Only processes one task per tick to avoid monopolizing
    /// the agent's time. The agent works through goals incrementally.
    async fn process_background_goals(&self) -> Result<()> {
        let goal_mgr = GoalManager::new(self.ctx.db.clone());

        let active_count = goal_mgr.active_goal_count().await?;
        if active_count == 0 {
            return Ok(());
        }

        debug!(active_goals = active_count, "checking for actionable goal tasks");

        // Find the highest-priority actionable task
        let actionable = goal_mgr.next_actionable_task().await?;
        let (goal, task) = match actionable {
            Some(pair) => pair,
            None => return Ok(()),
        };

        info!(
            goal = %goal.title,
            goal_id = %goal.id,
            task = %task.title,
            task_id = %task.id,
            "processing background goal task"
        );

        // Mark the task as in-progress
        goal_mgr
            .update_task_status(&task.id, TaskStatus::InProgress, None)
            .await?;

        // Emit event for the dashboard
        self.emit_event(serde_json::json!({
            "type": "thinking",
            "context": "background_goal",
            "goal": goal.title,
            "task": task.title,
            "turn": 0,
            "max_turns": 1,
        }));

        // Execute the task
        let (success, result_text) = if let Some(ref tc_json) = task.tool_call {
            // Task has a specific tool call — execute it directly
            self.execute_goal_tool_call(tc_json).await
        } else {
            // Task is a free-form objective — ask the LLM to handle it
            self.execute_goal_via_llm(&goal, &task).await
        };

        // Update the task
        let new_status = if success {
            TaskStatus::Completed
        } else {
            TaskStatus::Failed
        };

        goal_mgr
            .update_task_status(&task.id, new_status.clone(), Some(&result_text))
            .await?;

        let status_str = new_status.as_str();
        info!(
            goal = %goal.title,
            task = %task.title,
            status = status_str,
            "background goal task completed"
        );

        // Log to activity
        self.memory
            .log_activity(
                "goal_task",
                &format!("[{}] {} — {}", goal.title, task.title, status_str),
                Some(&truncate_preview(&result_text, 500)),
                if success { "ok" } else { "error" },
            )
            .await
            .ok();

        // Emit result event
        self.emit_event(serde_json::json!({
            "type": "tool_result",
            "tool": "goal_task",
            "success": success,
            "output_preview": truncate_preview(&result_text, 200),
            "context": "background_goal",
            "goal": goal.title,
            "task": task.title,
        }));

        // Check if the goal is now fully completed (all tasks done)
        // next_actionable_task() has a side effect of auto-completing goals
        let _ = goal_mgr.next_actionable_task().await;

        // Check if this goal just completed so we can run self-reflection
        let updated_goal = goal_mgr.get_goal(&goal.id).await?;
        if updated_goal.status == GoalStatus::Completed
            || updated_goal.status == GoalStatus::Failed
        {
            self.run_self_reflection(&goal_mgr, &updated_goal).await;
        }

        // Send proactive notification about progress
        self.send_goal_progress_notification(&goal, &task, success, &result_text)
            .await;

        self.notify_update();

        Ok(())
    }

    /// Execute a tool call specified in the task's `tool_call` JSON field.
    async fn execute_goal_tool_call(
        &self,
        tc_json: &serde_json::Value,
    ) -> (bool, String) {
        let tool = tc_json
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let params = tc_json
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let reasoning = tc_json
            .get("reasoning")
            .and_then(|v| v.as_str())
            .unwrap_or("background goal task")
            .to_string();

        let tc = ToolCall {
            tool: tool.clone(),
            params,
            reasoning,
        };

        match super::actions::execute_tool_call(&self.tools, &self.ctx, &tc).await {
            Ok(output) => (output.success, output.output),
            Err(e) => (false, format!("Tool execution error: {e}")),
        }
    }

    /// Ask the LLM to work on a free-form goal task.
    async fn execute_goal_via_llm(
        &self,
        goal: &crate::goals::Goal,
        task: &crate::goals::GoalTask,
    ) -> (bool, String) {
        let prompt = format!(
            "You are working on a background goal autonomously.\n\n\
             Goal: {}\nDescription: {}\n\n\
             Current task: {}\nTask description: {}\n\n\
             Complete this task. Use tools if needed. \
             Provide a concise result summary.",
            goal.title,
            goal.description,
            task.title,
            task.description,
        );

        // Send typing indicators
        self.ctx.messaging.typing_all().await;

        let gen_ctx = GenerateContext {
            message: &prompt,
            tools: Some(&self.tools),
            prompt_skills: &self.always_on_skills,
        };

        match self.llm.generate(&gen_ctx).await {
            Ok(reply) => {
                // Parse for tool calls and execute them
                let parsed = super::tool_parse::parse_llm_response(&reply);

                if parsed.tool_calls.is_empty() {
                    return (true, parsed.text);
                }

                // Execute auto-approved tool calls
                let mut results = Vec::new();
                let mut all_success = true;

                for call in &parsed.tool_calls {
                    if self.auto_approve.contains(call.tool.as_str()) {
                        match super::actions::execute_tool_call(&self.tools, &self.ctx, call).await
                        {
                            Ok(output) => {
                                if !output.success {
                                    all_success = false;
                                }
                                results.push(format!("[{}] {}", call.tool, output.output));
                            }
                            Err(e) => {
                                all_success = false;
                                results.push(format!("[{} error] {}", call.tool, e));
                            }
                        }
                    } else {
                        // Non-auto-approved tools in background goals are logged but skipped
                        results.push(format!(
                            "[{} skipped] requires approval — not auto-approved for background execution",
                            call.tool
                        ));
                    }
                }

                let mut full_result = parsed.text;
                if !results.is_empty() {
                    full_result.push_str("\n\nTool results:\n");
                    full_result.push_str(&results.join("\n"));
                }

                (all_success, full_result)
            }
            Err(e) => (false, format!("LLM error: {e}")),
        }
    }

    /// After a goal completes or fails, ask the LLM to reflect on the result.
    async fn run_self_reflection(
        &self,
        goal_mgr: &GoalManager,
        goal: &crate::goals::Goal,
    ) {
        let tasks = match goal_mgr.get_tasks(&goal.id).await {
            Ok(t) => t,
            Err(_) => return,
        };

        let task_summary: Vec<String> = tasks
            .iter()
            .map(|t| {
                let result = t.result.as_deref().unwrap_or("no result");
                format!(
                    "- {} ({}): {}",
                    t.title,
                    t.status.as_str(),
                    truncate_preview(result, 200),
                )
            })
            .collect();

        let prompt = format!(
            "Reflect on the completed goal below. Was the goal achieved? \
             What went well? What could improve next time? Be concise (2-3 sentences).\n\n\
             Goal: {} (status: {})\nDescription: {}\n\nTasks:\n{}",
            goal.title,
            goal.status.as_str(),
            goal.description,
            task_summary.join("\n"),
        );

        let gen_ctx = GenerateContext {
            message: &prompt,
            tools: None,
            prompt_skills: &self.always_on_skills,
        };

        match self.llm.generate(&gen_ctx).await {
            Ok(reflection) => {
                info!(
                    goal_id = %goal.id,
                    "self-reflection generated"
                );
                if let Err(e) = goal_mgr.set_reflection(&goal.id, &reflection).await {
                    warn!(err = %e, "failed to save reflection");
                }

                // Log the reflection
                self.memory
                    .log_activity(
                        "goal_reflection",
                        &format!("[{}] {}", goal.title, goal.status.as_str()),
                        Some(&reflection),
                        "ok",
                    )
                    .await
                    .ok();

                // Notify the user
                let msg = format!(
                    "Goal \"{}\" {}.\n\nReflection: {}",
                    goal.title,
                    goal.status.as_str(),
                    reflection,
                );
                self.ctx.messaging.send_all(&msg).await;
            }
            Err(e) => {
                warn!(err = %e, "failed to generate self-reflection");
            }
        }
    }

    /// Send a proactive notification about goal task progress.
    async fn send_goal_progress_notification(
        &self,
        goal: &crate::goals::Goal,
        task: &crate::goals::GoalTask,
        success: bool,
        result: &str,
    ) {
        let status = if success { "completed" } else { "failed" };
        let preview = truncate_preview(result, 300);

        let msg = format!(
            "Background goal progress:\n\
             Goal: {}\n\
             Task: {} ({})\n\
             Result: {}",
            goal.title, task.title, status, preview,
        );

        self.ctx.messaging.send_all(&msg).await;
    }

    /// Drain and execute all approved tool calls from the approval queue.
    ///
    /// After executing each tool call, stores the result in conversation
    /// history so the LLM has context for follow-up interactions.  If at
    /// least one tool was executed, triggers a follow-up LLM call so the
    /// user gets a complete natural-language response via their messaging
    /// platform.
    pub async fn execute_approved(&self) -> Result<()> {
        let mut executed_any = false;
        let mut result_summaries: Vec<String> = Vec::new();

        while let Some(action) = self.approval_queue.next_approved().await? {
            let call = super::actions::parse_tool_call(&action.action)?;

            // Emit tool_start event for approved execution
            self.emit_event(serde_json::json!({
                "type": "tool_start",
                "tool": call.tool,
                "reasoning": call.reasoning,
                "auto_approved": false,
                "approved": true,
                "approval_id": action.id,
            }));

            // Send typing indicator while executing
            self.ctx.messaging.typing_all().await;

            match super::actions::execute_tool_call(&self.tools, &self.ctx, &call).await {
                Ok(output) => {
                    self.approval_queue
                        .mark_executed(&action.id, true)
                        .await?;

                    let status = if output.success { "success" } else { "error" };
                    let preview = truncate_preview(&output.output, 200);
                    let summary = format!(
                        "[Approved tool result: {} ({})]\n{}",
                        call.tool, status, output.output
                    );

                    // Store the result in conversation history
                    self.memory
                        .conversation
                        .append("system", &summary)
                        .await?;

                    result_summaries.push(summary);
                    executed_any = true;

                    // Emit tool_result event
                    self.emit_event(serde_json::json!({
                        "type": "tool_result",
                        "tool": call.tool,
                        "success": output.success,
                        "output_preview": preview,
                        "approved": true,
                        "approval_id": action.id,
                    }));

                    info!(
                        tool = %call.tool,
                        id = %action.id,
                        output_len = output.output.len(),
                        "executed approved tool call"
                    );
                }
                Err(e) => {
                    self.approval_queue
                        .mark_executed(&action.id, false)
                        .await?;

                    let summary = format!(
                        "[Approved tool result: {} (error)]\n{}",
                        call.tool, e
                    );
                    self.memory
                        .conversation
                        .append("system", &summary)
                        .await?;

                    result_summaries.push(summary);
                    executed_any = true;

                    // Emit tool_result event for error
                    self.emit_event(serde_json::json!({
                        "type": "tool_result",
                        "tool": call.tool,
                        "success": false,
                        "output_preview": truncate_preview(&e.to_string(), 200),
                        "approved": true,
                        "approval_id": action.id,
                    }));

                    error!(
                        tool = %call.tool,
                        id = %action.id,
                        err = %e,
                        "tool call failed"
                    );
                }
            }
        }

        // If we executed approved tools, generate a follow-up LLM response
        // so the user gets a complete answer.
        if executed_any {
            // Emit thinking event for the follow-up LLM call
            self.emit_event(serde_json::json!({
                "type": "thinking",
                "context": "follow_up_after_approval",
                "turn": 0,
                "max_turns": 1,
            }));
            self.ctx.messaging.typing_all().await;

            let context = format!(
                "The following previously-requested tool calls have now been approved and executed. \
                 Summarize the results for the user in a concise, natural-language reply.\n\n{}",
                result_summaries.join("\n\n")
            );

            let gen_ctx = GenerateContext {
                message: &context,
                tools: Some(&self.tools),
                prompt_skills: &self.always_on_skills,
            };

            match self.llm.generate(&gen_ctx).await {
                Ok(reply) => {
                    self.memory
                        .conversation
                        .append("assistant", &reply)
                        .await?;

                    // Send the follow-up reply to the user via messaging
                    self.ctx.messaging.send_all(&reply).await;

                    self.emit_event(serde_json::json!({
                        "type": "turn_complete",
                        "context": "follow_up_after_approval",
                        "has_reply": true,
                        "turns_used": 1,
                        "tool_calls_total": result_summaries.len(),
                    }));

                    info!(
                        reply_len = reply.len(),
                        "sent follow-up reply after approved tool execution"
                    );
                }
                Err(e) => {
                    error!(err = %e, "failed to generate follow-up after approved tools");
                    // Still notify with raw results
                    let fallback = result_summaries.join("\n\n");
                    self.ctx.messaging.send_all(&fallback).await;

                    self.emit_event(serde_json::json!({
                        "type": "error",
                        "message": format!("LLM follow-up failed: {e}"),
                        "context": "follow_up_after_approval",
                    }));
                }
            }

            self.notify_update();
        }

        Ok(())
    }
}
