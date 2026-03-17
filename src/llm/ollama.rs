use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Result, SafeAgentError};
use crate::llm::context::GenerateContext;
use crate::llm::prompts;

const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL: &str = "llama3.1:8b";

/// LLM engine backed by a local or remote Ollama instance.
///
/// Communicates via the Ollama HTTP chat API (`POST /api/chat`).
///
/// Configuration priority (highest -> lowest):
///   1. Environment variables (`OLLAMA_HOST`, `OLLAMA_MODEL`)
///   2. `[llm]` section of `config.toml`
///   3. Built-in defaults
pub struct OllamaEngine {
    client: Client,
    base_url: String,
    model: String,
    personality: String,
    agent_name: String,
    timezone: String,
    locale: String,
    max_tokens: usize,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ChatOptions>,
}

#[derive(Serialize)]
struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: Option<ChatMessage>,
    #[serde(default)]
    _done: bool,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    eval_duration: Option<u64>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
}

impl OllamaEngine {
    pub fn new(config: &Config) -> Result<Self> {
        let base_url = std::env::var("OLLAMA_HOST")
            .ok()
            .or_else(|| {
                if config.llm.ollama_host.is_empty() {
                    None
                } else {
                    Some(config.llm.ollama_host.clone())
                }
            })
            .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string())
            .trim_end_matches('/')
            .to_string();

        let model = std::env::var("OLLAMA_MODEL")
            .ok()
            .or_else(|| {
                if config.llm.ollama_model.is_empty() {
                    None
                } else {
                    Some(config.llm.ollama_model.clone())
                }
            })
            .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());

        let timeout_secs = config.llm.timeout_secs;

        let client = Client::builder()
            .timeout(if timeout_secs > 0 {
                Duration::from_secs(timeout_secs)
            } else {
                Duration::from_secs(600)
            })
            .build()
            .map_err(|e| SafeAgentError::Config(format!("failed to create HTTP client: {e}")))?;

        info!(
            model = %model,
            base_url = %base_url,
            max_tokens = config.llm.max_tokens,
            temperature = config.llm.temperature,
            timeout_secs,
            "Ollama engine initialized"
        );

        Ok(Self {
            client,
            base_url,
            model,
            personality: config.core_personality.clone(),
            agent_name: config.agent_name.clone(),
            timezone: config.timezone.clone(),
            locale: config.locale.clone(),
            max_tokens: config.llm.max_tokens,
            temperature: config.llm.temperature,
        })
    }

    pub async fn generate(&self, ctx: &GenerateContext<'_>) -> Result<String> {
        let system_prompt = prompts::system_prompt(
            &self.personality,
            &self.agent_name,
            ctx.tools,
            Some(&self.timezone),
            Some(&self.locale),
            ctx.prompt_skills,
        );

        let url = format!("{}/api/chat", self.base_url);

        let image_b64s: Vec<String> = ctx.images.iter().map(|img| {
            if img.data_b64.starts_with("data:") {
                img.data_b64.split(',').nth(1).unwrap_or(&img.data_b64).to_string()
            } else {
                img.data_b64.clone()
            }
        }).collect();

        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt,
                    images: Vec::new(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: ctx.message.to_string(),
                    images: image_b64s,
                },
            ],
            stream: false,
            options: Some(ChatOptions {
                temperature: Some(self.temperature),
                num_predict: Some(self.max_tokens),
            }),
        };

        debug!(
            model = %self.model,
            prompt_len = ctx.message.len(),
            max_tokens = self.max_tokens,
            "invoking Ollama API"
        );

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                SafeAgentError::Llm(format!("Ollama request failed: {e}"))
            })?;

        let status = resp.status();

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            warn!(status = %status, error = %error_text, "Ollama API error");
            return Err(SafeAgentError::Llm(format!(
                "Ollama API returned {status}: {error_text}"
            )));
        }

        let chat_resp: ChatResponse = resp.json().await.map_err(|e| {
            SafeAgentError::Llm(format!("failed to parse Ollama response: {e}"))
        })?;

        if let (Some(eval_count), Some(eval_duration)) =
            (chat_resp.eval_count, chat_resp.eval_duration)
        {
            let tok_per_sec = if eval_duration > 0 {
                (eval_count as f64 / eval_duration as f64) * 1_000_000_000.0
            } else {
                0.0
            };
            debug!(
                eval_tokens = eval_count,
                prompt_tokens = chat_resp.prompt_eval_count.unwrap_or(0),
                tok_per_sec = format!("{tok_per_sec:.1}"),
                "Ollama usage"
            );
        }

        let response = chat_resp
            .message
            .map(|m| m.content)
            .unwrap_or_default()
            .trim()
            .to_string();

        info!(
            response_len = response.len(),
            model = %self.model,
            "Ollama response received"
        );

        if response.is_empty() {
            return Err(SafeAgentError::Llm(
                "Ollama returned empty response".into(),
            ));
        }

        Ok(response)
    }
}
