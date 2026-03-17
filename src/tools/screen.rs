use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use tracing::{debug, warn};

use super::{Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::error::Result;
use crate::llm::context::ImageAttachment;

/// Screen/clipboard awareness tool — captures the current screen or
/// clipboard contents and optionally analyzes them with the vision model.
///
/// Requires a display server (X11/Wayland) for screenshots.
/// Clipboard reading works via xclip/xsel/wl-clipboard.
pub struct ScreenTool {
    agent_ref: Arc<OnceLock<Weak<Agent>>>,
}

impl ScreenTool {
    pub fn new(agent_ref: Arc<OnceLock<Weak<Agent>>>) -> Self {
        Self { agent_ref }
    }
}

#[async_trait]
impl Tool for ScreenTool {
    fn name(&self) -> &str {
        "screen"
    }

    fn description(&self) -> &str {
        "Capture the screen or clipboard. Actions: 'screenshot' (capture display), 'clipboard' (read clipboard text), 'clipboard_image' (read clipboard image). Screenshots can be analyzed with an optional prompt."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["screenshot", "clipboard", "clipboard_image"],
                    "description": "What to capture"
                },
                "prompt": {
                    "type": "string",
                    "description": "Vision analysis prompt for screenshots/images (default: 'Describe what is on screen.')"
                },
                "output": {
                    "type": "string",
                    "description": "Output filename for screenshot (default: 'screenshot.png')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or_default();
        let prompt = params.get("prompt").and_then(|v| v.as_str())
            .unwrap_or("Describe what is on screen.");
        let output = params.get("output").and_then(|v| v.as_str())
            .unwrap_or("screenshot.png");

        match action {
            "screenshot" => self.take_screenshot(ctx, prompt, output).await,
            "clipboard" => self.read_clipboard_text().await,
            "clipboard_image" => self.read_clipboard_image(ctx, prompt).await,
            _ => Ok(ToolOutput::error(format!("unknown action: {action}"))),
        }
    }
}

impl ScreenTool {
    async fn take_screenshot(&self, ctx: &ToolContext, prompt: &str, output: &str) -> Result<ToolOutput> {
        // Try multiple screenshot tools in order
        let result = tokio::process::Command::new("grim")
            .arg("-")
            .output()
            .await;

        let png_bytes = match result {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
            _ => {
                // Fallback to scrot (X11)
                let result = tokio::process::Command::new("scrot")
                    .arg("-o")
                    .arg("-")
                    .output()
                    .await;
                match result {
                    Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
                    _ => {
                        // Fallback to import (ImageMagick)
                        let result = tokio::process::Command::new("import")
                            .args(["-window", "root", "png:-"])
                            .output()
                            .await;
                        match result {
                            Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
                            _ => return Ok(ToolOutput::error(
                                "no screenshot tool available (install grim, scrot, or imagemagick)"
                            )),
                        }
                    }
                }
            }
        };

        debug!(bytes = png_bytes.len(), "screenshot captured");

        // Save to sandbox
        let _ = ctx.sandbox.write(std::path::Path::new(output), &png_bytes);

        // Analyze with vision if we have an agent
        let agent = self
            .agent_ref
            .get()
            .and_then(|w| w.upgrade());

        if let Some(agent) = agent {
            let b64 = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &png_bytes,
            );
            let gen_ctx = crate::llm::GenerateContext {
                message: prompt,
                tools: None,
                prompt_skills: &[],
                images: vec![ImageAttachment {
                    data_b64: b64,
                    mime_type: "image/png".to_string(),
                }],
            };

            match agent.llm.generate(&gen_ctx).await {
                Ok(analysis) => Ok(ToolOutput::ok_with_meta(
                    analysis,
                    serde_json::json!({ "screenshot_saved": output }),
                )),
                Err(e) => {
                    warn!(err = %e, "vision analysis of screenshot failed");
                    Ok(ToolOutput::ok(format!("Screenshot saved to {output} ({} bytes), but vision analysis failed: {e}", png_bytes.len())))
                }
            }
        } else {
            Ok(ToolOutput::ok(format!("Screenshot saved to {output} ({} bytes)", png_bytes.len())))
        }
    }

    async fn read_clipboard_text(&self) -> Result<ToolOutput> {
        // Try wl-paste (Wayland), then xclip (X11), then xsel
        let result = tokio::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .await;

        let text = match result {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).to_string()
            }
            _ => {
                let result = tokio::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-o"])
                    .output()
                    .await;
                match result {
                    Ok(out) if out.status.success() => {
                        String::from_utf8_lossy(&out.stdout).to_string()
                    }
                    _ => {
                        let result = tokio::process::Command::new("xsel")
                            .args(["--clipboard", "--output"])
                            .output()
                            .await;
                        match result {
                            Ok(out) if out.status.success() => {
                                String::from_utf8_lossy(&out.stdout).to_string()
                            }
                            _ => return Ok(ToolOutput::error(
                                "no clipboard tool available (install wl-clipboard, xclip, or xsel)"
                            )),
                        }
                    }
                }
            }
        };

        if text.is_empty() {
            Ok(ToolOutput::ok("(clipboard is empty)".to_string()))
        } else {
            Ok(ToolOutput::ok(text))
        }
    }

    async fn read_clipboard_image(&self, _ctx: &ToolContext, prompt: &str) -> Result<ToolOutput> {
        // Try wl-paste for image, then xclip
        let result = tokio::process::Command::new("wl-paste")
            .args(["--type", "image/png"])
            .output()
            .await;

        let png_bytes = match result {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
            _ => {
                let result = tokio::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                    .output()
                    .await;
                match result {
                    Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
                    _ => return Ok(ToolOutput::error(
                        "no image in clipboard or clipboard tool not available"
                    )),
                }
            }
        };

        let agent = self
            .agent_ref
            .get()
            .and_then(|w| w.upgrade());

        if let Some(agent) = agent {
            let b64 = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &png_bytes,
            );
            let gen_ctx = crate::llm::GenerateContext {
                message: prompt,
                tools: None,
                prompt_skills: &[],
                images: vec![ImageAttachment {
                    data_b64: b64,
                    mime_type: "image/png".to_string(),
                }],
            };

            match agent.llm.generate(&gen_ctx).await {
                Ok(analysis) => Ok(ToolOutput::ok(analysis)),
                Err(e) => Ok(ToolOutput::error(format!("clipboard image analysis failed: {e}"))),
            }
        } else {
            Ok(ToolOutput::ok(format!("Clipboard contains an image ({} bytes)", png_bytes.len())))
        }
    }
}
