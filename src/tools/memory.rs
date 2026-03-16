use async_trait::async_trait;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

/// Search archival memory via full-text search.
pub struct MemorySearchTool;

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search the agent's archival memory using full-text search. Returns matching entries with category and timestamp."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 10)"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or_default();
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);

        if query.is_empty() {
            return Ok(ToolOutput::error("query is required"));
        }

        let db = ctx.db.lock().await;
        let mut stmt = db.prepare(
            "SELECT am.id, am.content, am.category, am.created_at
             FROM archival_memory_fts fts
             JOIN archival_memory am ON am.id = fts.rowid
             WHERE archival_memory_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;

        let entries: Vec<String> = stmt
            .query_map(rusqlite::params![query, limit], |row| {
                Ok(format!(
                    "[{}] [{}] {}",
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(1)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        if entries.is_empty() {
            Ok(ToolOutput::ok("No matching memories found."))
        } else {
            Ok(ToolOutput::ok(entries.join("\n")))
        }
    }
}

/// Get a specific archival memory entry by ID.
pub struct MemoryGetTool;

#[async_trait]
impl Tool for MemoryGetTool {
    fn name(&self) -> &str {
        "memory_get"
    }

    fn description(&self) -> &str {
        "Retrieve a specific archival memory entry by ID."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "integer",
                    "description": "Memory entry ID"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let id = params.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if id == 0 {
            return Ok(ToolOutput::error("id is required"));
        }

        let db = ctx.db.lock().await;
        let result = db.query_row(
            "SELECT id, content, category, created_at FROM archival_memory WHERE id = ?1",
            [id],
            |row| {
                Ok(format!(
                    "[{}] [{}] {}",
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(1)?,
                ))
            },
        );

        match result {
            Ok(entry) => Ok(ToolOutput::ok(entry)),
            Err(_) => Ok(ToolOutput::error(format!("Memory entry {id} not found"))),
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
        let base = std::env::temp_dir().join(format!("sa-memtest-{}", std::process::id()));
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
    async fn memory_search_empty_query() {
        let ctx = test_ctx();
        let result = MemorySearchTool.execute(serde_json::json!({"query": ""}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("query is required"));
    }

    #[tokio::test]
    async fn memory_search_no_results() {
        let ctx = test_ctx();
        let result = MemorySearchTool.execute(
            serde_json::json!({"query": "nonexistent"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No matching"));
    }

    #[tokio::test]
    async fn memory_search_with_results() {
        let ctx = test_ctx();
        {
            let db = ctx.db.lock().await;
            db.execute(
                "INSERT INTO archival_memory (content, category) VALUES (?1, ?2)",
                rusqlite::params!["The quick brown fox jumps", "test"],
            ).unwrap();
        }
        let result = MemorySearchTool.execute(
            serde_json::json!({"query": "quick brown fox"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("quick brown fox"));
    }

    #[tokio::test]
    async fn memory_get_missing_id() {
        let ctx = test_ctx();
        let result = MemoryGetTool.execute(serde_json::json!({"id": 0}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("id is required"));
    }

    #[tokio::test]
    async fn memory_get_not_found() {
        let ctx = test_ctx();
        let result = MemoryGetTool.execute(serde_json::json!({"id": 9999}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn memory_get_existing() {
        let ctx = test_ctx();
        let id: i64;
        {
            let db = ctx.db.lock().await;
            db.execute(
                "INSERT INTO archival_memory (content, category) VALUES (?1, ?2)",
                rusqlite::params!["stored info", "notes"],
            ).unwrap();
            id = db.last_insert_rowid();
        }
        let result = MemoryGetTool.execute(serde_json::json!({"id": id}), &ctx).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("stored info"));
        assert!(result.output.contains("notes"));
    }

    #[tokio::test]
    async fn tool_metadata() {
        assert_eq!(MemorySearchTool.name(), "memory_search");
        assert_eq!(MemoryGetTool.name(), "memory_get");
        assert!(!MemorySearchTool.description().is_empty());
        assert!(!MemoryGetTool.description().is_empty());
        let schema = MemorySearchTool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::json!("query")));
    }
}
