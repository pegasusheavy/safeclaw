use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Result, SafeAgentError};
use crate::llm::context::GenerateContext;
use crate::llm::prompts;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// LLM engine backed by the OpenRouter API.
///
/// OpenRouter provides an OpenAI-compatible chat completions endpoint that
/// routes to hundreds of models (Claude, GPT, Gemini, Llama, Mistral, etc.)
/// via a single API key.
///
/// Configuration priority (highest → lowest):
///   1. Environment variables (`OPENROUTER_API_KEY`, `OPENROUTER_MODEL`, …)
///   2. `[llm]` section of `config.toml`
///   3. Built-in defaults
pub struct OpenRouterEngine {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    personality: String,
    agent_name: String,
    timezone: String,
    locale: String,
    max_tokens: usize,
    temperature: f32,
    top_p: f32,
    /// Optional site URL sent as `HTTP-Referer` for OpenRouter analytics.
    site_url: Option<String>,
    /// Optional app name sent as `X-Title` for OpenRouter dashboard.
    app_name: Option<String>,
}

// -- OpenAI-compatible request/response types ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: MessageContent,
}

/// OpenAI-compatible content: either a plain string or a multimodal array.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize, Deserialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

impl OpenRouterEngine {
    pub fn new(config: &Config) -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .or_else(|| {
                if config.llm.openrouter_api_key.is_empty() {
                    None
                } else {
                    Some(config.llm.openrouter_api_key.clone())
                }
            })
            .ok_or_else(|| {
                SafeAgentError::Config(
                    "OpenRouter API key required: set OPENROUTER_API_KEY env var \
                     or openrouter_api_key in config"
                        .into(),
                )
            })?;

        let base_url = std::env::var("OPENROUTER_BASE_URL")
            .ok()
            .or_else(|| {
                if config.llm.openrouter_base_url.is_empty() {
                    None
                } else {
                    Some(config.llm.openrouter_base_url.clone())
                }
            })
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let model = std::env::var("OPENROUTER_MODEL")
            .ok()
            .or_else(|| {
                if config.llm.openrouter_model.is_empty() {
                    None
                } else {
                    Some(config.llm.openrouter_model.clone())
                }
            })
            .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

        let max_tokens = if config.llm.openrouter_max_tokens > 0 {
            config.llm.openrouter_max_tokens
        } else {
            config.llm.max_tokens
        };

        let temperature = config.llm.temperature;
        let top_p = config.llm.top_p;
        let timeout_secs = config.llm.timeout_secs;

        let site_url = std::env::var("OPENROUTER_SITE_URL").ok().or_else(|| {
            if config.llm.openrouter_site_url.is_empty() {
                None
            } else {
                Some(config.llm.openrouter_site_url.clone())
            }
        });

        let app_name = std::env::var("OPENROUTER_APP_NAME").ok().or_else(|| {
            if config.llm.openrouter_app_name.is_empty() {
                None
            } else {
                Some(config.llm.openrouter_app_name.clone())
            }
        });

        let client = Client::builder()
            .timeout(if timeout_secs > 0 {
                Duration::from_secs(timeout_secs)
            } else {
                Duration::from_secs(300)
            })
            .build()
            .map_err(|e| SafeAgentError::Config(format!("failed to create HTTP client: {e}")))?;

        info!(
            model = %model,
            base_url = %base_url,
            max_tokens,
            temperature,
            timeout_secs,
            app_name = ?app_name,
            "OpenRouter engine initialized"
        );

        Ok(Self {
            client,
            api_key,
            base_url,
            model,
            personality: config.core_personality.clone(),
            agent_name: config.agent_name.clone(),
            timezone: config.timezone.clone(),
            locale: config.locale.clone(),
            max_tokens,
            temperature,
            top_p,
            site_url,
            app_name,
        })
    }

    /// Send a message to OpenRouter and return the plain-text response.
    pub async fn generate(&self, ctx: &GenerateContext<'_>) -> Result<String> {
        let system_prompt = prompts::system_prompt(&self.personality, &self.agent_name, ctx.tools, Some(&self.timezone), Some(&self.locale), ctx.prompt_skills);
        let url = format!("{}/chat/completions", self.base_url);

        let user_content = if ctx.images.is_empty() {
            MessageContent::Text(ctx.message.to_string())
        } else {
            let mut parts = vec![ContentPart::Text {
                text: ctx.message.to_string(),
            }];
            for img in &ctx.images {
                let data_uri = if img.data_b64.starts_with("data:") {
                    img.data_b64.clone()
                } else {
                    format!("data:{};base64,{}", img.mime_type, img.data_b64)
                };
                parts.push(ContentPart::ImageUrl {
                    image_url: ImageUrl { url: data_uri },
                });
            }
            MessageContent::Parts(parts)
        };

        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text(system_prompt),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_content,
                },
            ],
            max_tokens: Some(self.max_tokens),
            temperature: Some(self.temperature),
            top_p: Some(self.top_p),
        };

        debug!(
            model = %self.model,
            prompt_len = ctx.message.len(),
            max_tokens = self.max_tokens,
            "invoking OpenRouter API"
        );

        let mut req = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json");

        if let Some(ref site_url) = self.site_url {
            req = req.header("HTTP-Referer", site_url.as_str());
        }
        if let Some(ref app_name) = self.app_name {
            req = req.header("X-Title", app_name.as_str());
        }

        let resp = req.json(&body).send().await.map_err(|e| {
            SafeAgentError::Llm(format!("OpenRouter request failed: {e}"))
        })?;

        let status = resp.status();

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            let error_msg = if let Ok(err_resp) = serde_json::from_str::<ErrorResponse>(&error_text)
            {
                err_resp
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| error_text.clone())
            } else {
                error_text
            };

            warn!(
                status = %status,
                error = %error_msg,
                "OpenRouter API error"
            );

            return Err(SafeAgentError::Llm(format!(
                "OpenRouter API returned {status}: {error_msg}"
            )));
        }

        let chat_resp: ChatResponse = resp.json().await.map_err(|e| {
            SafeAgentError::Llm(format!("failed to parse OpenRouter response: {e}"))
        })?;

        if let Some(ref usage) = chat_resp.usage {
            debug!(
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_tokens = usage.total_tokens,
                "OpenRouter usage"
            );
        }

        let response = chat_resp
            .choices
            .into_iter()
            .next()
            .map(|c| match c.message.content {
                MessageContent::Text(s) => s,
                MessageContent::Parts(parts) => parts
                    .into_iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .unwrap_or_default()
            .trim()
            .to_string();

        info!(
            response_len = response.len(),
            model = %self.model,
            "OpenRouter response received"
        );

        if response.is_empty() {
            return Err(SafeAgentError::Llm(
                "OpenRouter returned empty response".into(),
            ));
        }

        Ok(response)
    }
}
