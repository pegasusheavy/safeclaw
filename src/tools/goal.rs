use async_trait::async_trait;
use tracing::debug;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;
use crate::goals::{GoalManager, GoalStatus, TaskStatus};

/// Tool for the LLM to create, manage, and decompose goals into tasks.
pub struct GoalTool;

impl GoalTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &str {
        "goal"
    }

    fn description(&self) -> &str {
        "Manage background goals and tasks. Actions: create, list, get, add_task, update_status, \
         complete_task, fail_task, cancel, pause, resume. \
         Goals persist across restarts and are worked on autonomously between conversations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "create", "list", "get", "add_task", "update_status",
                        "complete_task", "fail_task", "cancel", "pause", "resume"
                    ],
                    "description": "Goal action to perform"
                },
                "goal_id": {
                    "type": "string",
                    "description": "Goal ID (for get/add_task/cancel/pause/resume)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (for complete_task/fail_task)"
                },
                "title": {
                    "type": "string",
                    "description": "Goal or task title"
                },
                "description": {
                    "type": "string",
                    "description": "Goal or task description"
                },
                "priority": {
                    "type": "integer",
                    "description": "Goal priority (higher = more important, default 0)"
                },
                "parent_goal_id": {
                    "type": "string",
                    "description": "Parent goal ID for sub-goals"
                },
                "tool_call": {
                    "type": "object",
                    "description": "Tool call to execute for this task: { tool, params, reasoning }"
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task IDs this task depends on"
                },
                "result": {
                    "type": "string",
                    "description": "Result text when completing/failing a task"
                },
                "status_filter": {
                    "type": "string",
                    "description": "Filter goals by status (for list): active, paused, completed, failed, cancelled"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or_default();
        let mgr = GoalManager::new(ctx.db.clone());

        match action {
            "create" => {
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or_default();
                let description = params.get("description").and_then(|v| v.as_str()).unwrap_or_default();
                let priority = params.get("priority").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let parent = params.get("parent_goal_id").and_then(|v| v.as_str());

                if title.is_empty() {
                    return Ok(ToolOutput::error("title is required for create"));
                }

                let id = mgr.create_goal(title, description, priority, parent).await?;
                Ok(ToolOutput::ok_with_meta(
                    format!("Created goal: {title}"),
                    serde_json::json!({ "goal_id": id }),
                ))
            }

            "list" => {
                let status_filter = params.get("status_filter").and_then(|v| v.as_str());
                let goals = mgr.list_goals(status_filter, 50, 0).await?;

                if goals.is_empty() {
                    return Ok(ToolOutput::ok("No goals found."));
                }

                let lines: Vec<String> = goals
                    .iter()
                    .map(|gs| {
                        let progress = if gs.total_tasks > 0 {
                            format!(" [{}/{}]", gs.completed_tasks, gs.total_tasks)
                        } else {
                            String::new()
                        };
                        format!(
                            "[{}] {} (priority={}, status={}){} — {}",
                            gs.goal.id,
                            gs.goal.title,
                            gs.goal.priority,
                            gs.goal.status.as_str(),
                            progress,
                            if gs.goal.description.is_empty() {
                                "no description"
                            } else {
                                &gs.goal.description
                            }
                        )
                    })
                    .collect();

                Ok(ToolOutput::ok(lines.join("\n")))
            }

            "get" => {
                let goal_id = params.get("goal_id").and_then(|v| v.as_str()).unwrap_or_default();
                if goal_id.is_empty() {
                    return Ok(ToolOutput::error("goal_id is required for get"));
                }

                let goal = mgr.get_goal(goal_id).await?;
                let tasks = mgr.get_tasks(goal_id).await?;

                let mut out = format!(
                    "Goal: {} ({})\nStatus: {}\nPriority: {}\nDescription: {}\n",
                    goal.title, goal.id, goal.status.as_str(), goal.priority, goal.description,
                );

                if let Some(ref r) = goal.reflection {
                    out.push_str(&format!("Reflection: {r}\n"));
                }

                if tasks.is_empty() {
                    out.push_str("\nNo tasks defined yet.");
                } else {
                    out.push_str(&format!("\nTasks ({}):\n", tasks.len()));
                    for (i, task) in tasks.iter().enumerate() {
                        let deps = if task.depends_on.is_empty() {
                            String::new()
                        } else {
                            format!(" (depends: {})", task.depends_on.join(", "))
                        };
                        out.push_str(&format!(
                            "  {}. [{}] {} — {}{}\n",
                            i + 1,
                            task.status.as_str(),
                            task.title,
                            task.id,
                            deps,
                        ));
                        if let Some(ref result) = task.result {
                            out.push_str(&format!("     Result: {result}\n"));
                        }
                    }
                }

                Ok(ToolOutput::ok(out))
            }

            "add_task" => {
                let goal_id = params.get("goal_id").and_then(|v| v.as_str()).unwrap_or_default();
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or_default();
                let description = params.get("description").and_then(|v| v.as_str()).unwrap_or_default();
                let tool_call = params.get("tool_call").cloned();
                let depends_on: Vec<String> = params
                    .get("depends_on")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                if goal_id.is_empty() || title.is_empty() {
                    return Ok(ToolOutput::error("goal_id and title are required for add_task"));
                }

                // Auto-increment sort order
                let existing = mgr.get_tasks(goal_id).await?;
                let sort_order = existing.len() as i32;

                let id = mgr
                    .add_task(goal_id, title, description, tool_call, &depends_on, sort_order)
                    .await?;

                Ok(ToolOutput::ok_with_meta(
                    format!("Added task: {title}"),
                    serde_json::json!({ "task_id": id }),
                ))
            }

            "complete_task" => {
                let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
                let result = params.get("result").and_then(|v| v.as_str());

                if task_id.is_empty() {
                    return Ok(ToolOutput::error("task_id is required for complete_task"));
                }

                mgr.update_task_status(task_id, TaskStatus::Completed, result).await?;
                debug!(task_id, "task marked completed");
                Ok(ToolOutput::ok(format!("Task {task_id} completed")))
            }

            "fail_task" => {
                let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or_default();
                let result = params.get("result").and_then(|v| v.as_str());

                if task_id.is_empty() {
                    return Ok(ToolOutput::error("task_id is required for fail_task"));
                }

                mgr.update_task_status(task_id, TaskStatus::Failed, result).await?;
                Ok(ToolOutput::ok(format!("Task {task_id} marked as failed")))
            }

            "cancel" | "pause" | "resume" => {
                let goal_id = params.get("goal_id").and_then(|v| v.as_str()).unwrap_or_default();
                if goal_id.is_empty() {
                    return Ok(ToolOutput::error("goal_id is required"));
                }

                let new_status = match action {
                    "cancel" => GoalStatus::Cancelled,
                    "pause" => GoalStatus::Paused,
                    "resume" => GoalStatus::Active,
                    _ => unreachable!(),
                };

                mgr.update_goal_status(goal_id, new_status.clone()).await?;
                Ok(ToolOutput::ok(format!(
                    "Goal {goal_id} status changed to {}",
                    new_status.as_str()
                )))
            }

            other => Ok(ToolOutput::error(format!("unknown goal action: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::messaging::MessagingManager;
    use crate::security::SandboxedFs;
    use crate::trash::TrashManager;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        let base = std::env::temp_dir().join(format!("sa-goaltest-{}", std::process::id()));
        let sandbox_dir = base.join("sandbox");
        let trash_dir = base.join("trash");
        std::fs::create_dir_all(&sandbox_dir).unwrap();
        std::fs::create_dir_all(&trash_dir).unwrap();

        let db = db::test_db();
        let db_read = db.clone();
        ToolContext {
            sandbox: SandboxedFs::new(sandbox_dir).unwrap(),
            db,
            db_read,
            http_client: reqwest::Client::new(),
            messaging: Arc::new(MessagingManager::new()),
            trash: Arc::new(TrashManager::new(&trash_dir).unwrap()),
        }
    }

    #[tokio::test]
    async fn create_and_list_goal() {
        let ctx = test_ctx();
        let tool = GoalTool::new();

        let r = tool
            .execute(
                serde_json::json!({
                    "action": "create",
                    "title": "Learn Rust",
                    "description": "Study the Rust book",
                    "priority": 5
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("Learn Rust"));

        let list = tool
            .execute(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(list.success);
        assert!(list.output.contains("Learn Rust"));
    }

    #[tokio::test]
    async fn create_missing_title() {
        let ctx = test_ctx();
        let tool = GoalTool::new();
        let r = tool
            .execute(serde_json::json!({"action": "create"}), &ctx)
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("title is required"));
    }

    #[tokio::test]
    async fn add_task_and_complete() {
        let ctx = test_ctx();
        let tool = GoalTool::new();

        let create = tool
            .execute(
                serde_json::json!({"action": "create", "title": "Task goal"}),
                &ctx,
            )
            .await
            .unwrap();
        let goal_id = create.metadata.unwrap()["goal_id"].as_str().unwrap().to_string();

        let add = tool
            .execute(
                serde_json::json!({
                    "action": "add_task",
                    "goal_id": goal_id,
                    "title": "Step 1"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(add.success);
        let task_id = add.metadata.unwrap()["task_id"].as_str().unwrap().to_string();

        let complete = tool
            .execute(
                serde_json::json!({
                    "action": "complete_task",
                    "task_id": task_id,
                    "result": "Done!"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(complete.success);

        let get = tool
            .execute(serde_json::json!({"action": "get", "goal_id": goal_id}), &ctx)
            .await
            .unwrap();
        assert!(get.output.contains("completed"));
        assert!(get.output.contains("Done!"));
    }

    #[tokio::test]
    async fn pause_resume_cancel() {
        let ctx = test_ctx();
        let tool = GoalTool::new();

        let create = tool
            .execute(
                serde_json::json!({"action": "create", "title": "Status test"}),
                &ctx,
            )
            .await
            .unwrap();
        let goal_id = create.metadata.unwrap()["goal_id"].as_str().unwrap().to_string();

        let pause = tool
            .execute(serde_json::json!({"action": "pause", "goal_id": goal_id}), &ctx)
            .await
            .unwrap();
        assert!(pause.success);
        assert!(pause.output.contains("paused"));

        let resume = tool
            .execute(serde_json::json!({"action": "resume", "goal_id": goal_id}), &ctx)
            .await
            .unwrap();
        assert!(resume.success);
        assert!(resume.output.contains("active"));

        let cancel = tool
            .execute(serde_json::json!({"action": "cancel", "goal_id": goal_id}), &ctx)
            .await
            .unwrap();
        assert!(cancel.success);
        assert!(cancel.output.contains("cancelled"));
    }

    #[tokio::test]
    async fn unknown_action() {
        let ctx = test_ctx();
        let tool = GoalTool::new();
        let r = tool
            .execute(serde_json::json!({"action": "nope"}), &ctx)
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.output.contains("unknown"));
    }

    #[tokio::test]
    async fn tool_metadata() {
        let tool = GoalTool::new();
        assert_eq!(tool.name(), "goal");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("action")));
    }
}
