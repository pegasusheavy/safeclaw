use async_trait::async_trait;
use tokio::process::Command;
use tracing::debug;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::Result;

pub struct ExecTool {
    timeout_secs: u64,
}

impl ExecTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. Commands run in a sandboxed environment and require operator approval."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory (relative to sandbox root)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Override timeout in seconds"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if command.is_empty() {
            return Ok(ToolOutput::error("command is required"));
        }

        let timeout = params
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);

        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(std::path::Path::new);

        let work_dir = if let Some(rel) = cwd {
            ctx.sandbox.resolve(rel)?
        } else {
            ctx.sandbox.root().to_path_buf()
        };

        debug!(command, ?work_dir, timeout, "executing command");

        let mut cmd = build_sandboxed_command(command, &work_dir, &ctx.trash);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            cmd.output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let code = output.status.code().unwrap_or(-1);

                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("[stderr] ");
                    text.push_str(&stderr);
                }

                let meta = serde_json::json!({ "exit_code": code });

                if output.status.success() {
                    Ok(ToolOutput::ok_with_meta(text, meta))
                } else {
                    Ok(ToolOutput {
                        success: false,
                        output: format!("exit code {code}\n{text}"),
                        metadata: Some(meta),
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolOutput::error(format!("failed to run: {e}"))),
            Err(_) => Ok(ToolOutput::error(format!(
                "command timed out after {timeout}s"
            ))),
        }
    }
}

/// Build a Command with platform-appropriate shell, trash-aware PATH, and
/// resource limits.
fn build_sandboxed_command(
    shell_cmd: &str,
    work_dir: &std::path::Path,
    trash: &crate::trash::TrashManager,
) -> Command {
    // On Unix, prepend the trash bin dir so `rm` / `rmdir` invocations in
    // shell commands are intercepted by our wrapper scripts.
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(shell_cmd);

        // Prepend trash wrappers to PATH
        let trash_bin = trash.bin_dir().to_string_lossy().to_string();
        let current_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{trash_bin}:{current_path}");
        c.env("PATH", new_path);

        c
    };

    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(shell_cmd);
        let _ = trash; // not used on Windows
        c
    };

    cmd.current_dir(work_dir);

    // Apply resource limits on Unix via pre_exec
    #[cfg(unix)]
    {
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        let limits = crate::security::ProcessLimits::default();
        unsafe {
            cmd.pre_exec(move || crate::security::apply_process_limits(&limits));
        }
    }

    cmd
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
        let base = std::env::temp_dir().join(format!("sa-exectest-{}", std::process::id()));
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
    async fn exec_empty_command() {
        let ctx = test_ctx();
        let tool = ExecTool::new(30);
        let r = tool.execute(serde_json::json!({"command": ""}), &ctx).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("command is required"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_echo() {
        let ctx = test_ctx();
        let tool = ExecTool::new(10);
        let r = tool.execute(
            serde_json::json!({"command": "echo hello"}),
            &ctx,
        ).await.unwrap();
        assert!(r.success, "output: {}", r.output);
        assert!(r.output.contains("hello"));
        assert_eq!(r.metadata.unwrap()["exit_code"], 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_failing_command() {
        let ctx = test_ctx();
        let tool = ExecTool::new(10);
        let r = tool.execute(
            serde_json::json!({"command": "false"}),
            &ctx,
        ).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("exit code"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_timeout() {
        let ctx = test_ctx();
        let tool = ExecTool::new(1);
        let r = tool.execute(
            serde_json::json!({"command": "sleep 30", "timeout_secs": 1}),
            &ctx,
        ).await.unwrap();
        assert!(!r.success);
        assert!(r.output.contains("timed out"));
    }

    #[test]
    fn tool_metadata() {
        let tool = ExecTool::new(30);
        assert_eq!(tool.name(), "exec");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().contains(&serde_json::json!("command")));
    }
}
