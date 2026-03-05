use async_trait::async_trait;
use tracing::{debug, info};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

// -- ReadFile ------------------------------------------------------------

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file from the sandboxed data directory. Returns the file contents as text."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path within the sandbox"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if path.is_empty() {
            return Ok(ToolOutput::error("path is required"));
        }

        let rel = std::path::Path::new(path);
        debug!(?rel, "reading file");

        match ctx.sandbox.read_to_string(rel) {
            Ok(contents) => Ok(ToolOutput::ok(contents)),
            Err(e) => Ok(ToolOutput::error(format!("failed to read: {e}"))),
        }
    }
}

// -- WriteFile -----------------------------------------------------------

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file in the sandboxed data directory. Creates parent directories as needed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path within the sandbox"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if path.is_empty() {
            return Ok(ToolOutput::error("path is required"));
        }

        let rel = std::path::Path::new(path);
        debug!(?rel, bytes = content.len(), "writing file");

        match ctx.sandbox.write(rel, content.as_bytes()) {
            Ok(()) => Ok(ToolOutput::ok(format!("Wrote {} bytes to {path}", content.len()))),
            Err(e) => Ok(ToolOutput::error(format!("failed to write: {e}"))),
        }
    }
}

// -- EditFile ------------------------------------------------------------

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace a specific string in a file within the sandbox. Use for targeted edits."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path within the sandbox"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or_default();
        let old = params.get("old_string").and_then(|v| v.as_str()).unwrap_or_default();
        let new = params.get("new_string").and_then(|v| v.as_str()).unwrap_or_default();

        if path.is_empty() || old.is_empty() {
            return Ok(ToolOutput::error("path and old_string are required"));
        }

        let rel = std::path::Path::new(path);
        let contents = match ctx.sandbox.read_to_string(rel) {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("failed to read: {e}"))),
        };

        let count = contents.matches(old).count();
        if count == 0 {
            return Ok(ToolOutput::error("old_string not found in file"));
        }

        let updated = contents.replacen(old, new, 1);
        match ctx.sandbox.write(rel, updated.as_bytes()) {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "Replaced 1 of {count} occurrence(s) in {path}"
            ))),
            Err(e) => Ok(ToolOutput::error(format!("failed to write: {e}"))),
        }
    }
}

// -- DeleteFile ----------------------------------------------------------

pub struct DeleteFileTool;

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Delete a file or directory from the sandbox. Items are moved to trash and can be recovered from the dashboard."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path within the sandbox"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if path.is_empty() {
            return Ok(ToolOutput::error("path is required"));
        }

        let rel = std::path::Path::new(path);
        let abs = ctx.sandbox.resolve(rel)?;

        if !abs.exists() {
            return Ok(ToolOutput::error(format!("not found: {path}")));
        }

        debug!(?abs, "deleting file (moving to trash)");

        match ctx.trash.trash(&abs, "tool:delete_file") {
            Ok(entry) => {
                info!(id = %entry.id, path = %path, "file moved to trash");
                Ok(ToolOutput::ok(format!(
                    "Moved '{}' to trash (ID: {}). Can be restored from the dashboard.",
                    path, entry.id
                )))
            }
            Err(e) => Ok(ToolOutput::error(format!("failed to trash: {e}"))),
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

    fn test_ctx(base: &std::path::Path) -> ToolContext {
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
    async fn read_file_success() {
        let base = std::env::temp_dir().join(format!("sa-test-read-{}", std::process::id()));
        let ctx = test_ctx(&base);
        ctx.sandbox.write(std::path::Path::new("hello.txt"), b"world").unwrap();
        let result = ReadFileTool.execute(serde_json::json!({"path": "hello.txt"}), &ctx).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "world");
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let base = std::env::temp_dir().join(format!("sa-test-readnf-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let result = ReadFileTool.execute(serde_json::json!({"path": "nope.txt"}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("failed to read"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn read_file_empty_path() {
        let base = std::env::temp_dir().join(format!("sa-test-readep-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let result = ReadFileTool.execute(serde_json::json!({"path": ""}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("path is required"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn write_file_success() {
        let base = std::env::temp_dir().join(format!("sa-test-write-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let result = WriteFileTool.execute(
            serde_json::json!({"path": "out.txt", "content": "hello"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("5 bytes"));
        let read = ctx.sandbox.read_to_string(std::path::Path::new("out.txt")).unwrap();
        assert_eq!(read, "hello");
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn write_file_empty_path() {
        let base = std::env::temp_dir().join(format!("sa-test-writeep-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let result = WriteFileTool.execute(
            serde_json::json!({"path": "", "content": "data"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("path is required"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn edit_file_replaces_string() {
        let base = std::env::temp_dir().join(format!("sa-test-edit-{}", std::process::id()));
        let ctx = test_ctx(&base);
        ctx.sandbox.write(std::path::Path::new("doc.txt"), b"foo bar baz").unwrap();
        let result = EditFileTool.execute(
            serde_json::json!({"path": "doc.txt", "old_string": "bar", "new_string": "qux"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Replaced 1"));
        let read = ctx.sandbox.read_to_string(std::path::Path::new("doc.txt")).unwrap();
        assert_eq!(read, "foo qux baz");
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn edit_file_string_not_found() {
        let base = std::env::temp_dir().join(format!("sa-test-editnf-{}", std::process::id()));
        let ctx = test_ctx(&base);
        ctx.sandbox.write(std::path::Path::new("doc.txt"), b"hello").unwrap();
        let result = EditFileTool.execute(
            serde_json::json!({"path": "doc.txt", "old_string": "xyz", "new_string": "abc"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not found"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn edit_file_empty_path_or_old() {
        let base = std::env::temp_dir().join(format!("sa-test-editep-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let r1 = EditFileTool.execute(
            serde_json::json!({"path": "", "old_string": "x", "new_string": "y"}),
            &ctx,
        ).await.unwrap();
        assert!(!r1.success);
        let r2 = EditFileTool.execute(
            serde_json::json!({"path": "x", "old_string": "", "new_string": "y"}),
            &ctx,
        ).await.unwrap();
        assert!(!r2.success);
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn delete_file_moves_to_trash() {
        let base = std::env::temp_dir().join(format!("sa-test-del-{}", std::process::id()));
        let ctx = test_ctx(&base);
        ctx.sandbox.write(std::path::Path::new("delete-me.txt"), b"bye").unwrap();
        let result = DeleteFileTool.execute(
            serde_json::json!({"path": "delete-me.txt"}),
            &ctx,
        ).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("trash"));
        assert!(!ctx.sandbox.resolve(std::path::Path::new("delete-me.txt")).unwrap().exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn delete_file_not_found() {
        let base = std::env::temp_dir().join(format!("sa-test-delnf-{}", std::process::id()));
        let ctx = test_ctx(&base);
        let result = DeleteFileTool.execute(
            serde_json::json!({"path": "nope.txt"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.success);
        assert!(result.output.contains("not found"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn tool_names_and_schemas() {
        assert_eq!(ReadFileTool.name(), "read_file");
        assert_eq!(WriteFileTool.name(), "write_file");
        assert_eq!(EditFileTool.name(), "edit_file");
        assert_eq!(DeleteFileTool.name(), "delete_file");
        assert!(!ReadFileTool.description().is_empty());
        assert!(!WriteFileTool.description().is_empty());
        assert!(!EditFileTool.description().is_empty());
        assert!(!DeleteFileTool.description().is_empty());
        let schema = ReadFileTool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::json!("path")));
    }
}

// -- ApplyPatch ----------------------------------------------------------

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff patch to files in the sandbox."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch content"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let patch = params
            .get("patch")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if patch.is_empty() {
            return Ok(ToolOutput::error("patch content is required"));
        }

        // Write patch to temp file and apply with `patch` command
        let patch_path = ctx.sandbox.resolve(std::path::Path::new(".tmp_patch"))?;
        std::fs::write(&patch_path, patch)?;

        let output = tokio::process::Command::new("patch")
            .arg("-p1")
            .arg("-i")
            .arg(&patch_path)
            .current_dir(ctx.sandbox.root())
            .output()
            .await;

        let _ = std::fs::remove_file(&patch_path);

        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let err = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    Ok(ToolOutput::ok(format!("{text}{err}")))
                } else {
                    Ok(ToolOutput::error(format!("patch failed: {text}{err}")))
                }
            }
            Err(e) => Ok(ToolOutput::error(format!("failed to run patch: {e}"))),
        }
    }
}
