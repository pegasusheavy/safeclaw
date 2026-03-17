use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use tracing::{debug, warn};

use super::{Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::error::Result;
use crate::llm::context::ImageAttachment;

/// Image analysis tool — sends images to a vision-capable LLM.
pub struct ImageTool {
    agent_ref: Arc<OnceLock<Weak<Agent>>>,
}

impl ImageTool {
    pub fn new(agent_ref: Arc<OnceLock<Weak<Agent>>>) -> Self {
        Self { agent_ref }
    }
}

#[async_trait]
impl Tool for ImageTool {
    fn name(&self) -> &str {
        "image"
    }

    fn description(&self) -> &str {
        "Analyze an image and return a description. Provide either a file path (relative to sandbox) or a URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["image"],
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Path to image file (sandbox-relative) or URL"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to analyze (default: 'Describe the image.')"
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let image = params.get("image").and_then(|v| v.as_str()).unwrap_or_default();
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe the image.");

        if image.is_empty() {
            return Ok(ToolOutput::error("image path or URL is required"));
        }

        let (data_b64, mime_type) = if image.starts_with("http://") || image.starts_with("https://") {
            match fetch_image_as_base64(&ctx.http_client, image).await {
                Ok(pair) => pair,
                Err(e) => return Ok(ToolOutput::error(format!("failed to fetch image: {e}"))),
            }
        } else {
            match ctx.sandbox.read_binary(image) {
                Ok(bytes) => {
                    let mime = guess_mime_type(image);
                    (base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes), mime)
                }
                Err(e) => return Ok(ToolOutput::error(format!("failed to read image: {e}"))),
            }
        };

        debug!(image, prompt, "invoking vision model");

        let agent = self
            .agent_ref
            .get()
            .and_then(|w| w.upgrade())
            .ok_or_else(|| crate::error::SafeAgentError::Tool("agent not initialized".into()))?;

        let gen_ctx = crate::llm::GenerateContext {
            message: prompt,
            tools: None,
            prompt_skills: &[],
            images: vec![ImageAttachment { data_b64, mime_type }],
        };

        match agent.llm.generate(&gen_ctx).await {
            Ok(analysis) => Ok(ToolOutput::ok(analysis)),
            Err(e) => {
                warn!(err = %e, "vision analysis failed");
                Ok(ToolOutput::error(format!("vision analysis failed: {e}")))
            }
        }
    }
}

async fn fetch_image_as_base64(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<(String, String), String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/png")
        .split(';')
        .next()
        .unwrap_or("image/png")
        .to_string();

    let bytes = resp.bytes().await.map_err(|e| format!("read error: {e}"))?;

    if bytes.len() > 20 * 1024 * 1024 {
        return Err("image too large (>20 MiB)".into());
    }

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok((b64, content_type))
}

fn guess_mime_type(path: &str) -> String {
    match path.rsplit('.').next().map(|s| s.to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        _ => "image/png",
    }
    .to_string()
}
