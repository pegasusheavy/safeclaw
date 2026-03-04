use std::sync::{Arc, Mutex};

use llama_gguf::{ChatEngine, Engine, EngineConfig};
use tracing::info;

use crate::config::Config;
use crate::error::{Result, SafeAgentError};
use crate::llm::context::GenerateContext;
use crate::llm::prompts;

/// LLM engine backed by a local GGUF model via llama-gguf.
///
/// Loads the model once at startup and keeps a `ChatEngine` that accumulates
/// conversation history in its KV cache.  Generation is CPU/GPU bound, so
/// every call is dispatched to Tokio's blocking thread pool.
pub struct LocalEngine {
    chat: Arc<Mutex<ChatEngine>>,
    model_path: String,
    personality: String,
    agent_name: String,
    timezone: String,
    locale: String,
}

impl LocalEngine {
    pub fn new(config: &Config) -> Result<Self> {
        let model_path = std::env::var("MODEL_PATH")
            .unwrap_or_else(|_| config.llm.model_path.clone());

        if model_path.is_empty() {
            return Err(SafeAgentError::Config(
                "LLM backend \"local\" requires a model path.  Set `llm.model_path` \
                 in config.toml or the `MODEL_PATH` environment variable."
                    .into(),
            ));
        }

        let engine_config = EngineConfig {
            model_path: model_path.clone(),
            temperature: config.llm.temperature,
            top_p: config.llm.top_p,
            max_tokens: config.llm.max_tokens,
            use_gpu: config.llm.use_gpu,
            max_context_len: config.llm.max_context_len,
            ..Default::default()
        };

        info!(
            model = %model_path,
            temperature = config.llm.temperature,
            top_p = config.llm.top_p,
            max_tokens = config.llm.max_tokens,
            "loading local GGUF model"
        );

        let engine = Engine::load(engine_config).map_err(|e| {
            SafeAgentError::Llm(format!("failed to load GGUF model: {e}"))
        })?;

        let base_system_prompt = prompts::system_prompt(
            &config.core_personality,
            &config.agent_name,
            None,
            Some(&config.timezone),
            Some(&config.locale),
            &[],
        );

        info!(
            chat_template = ?engine.chat_template(),
            vocab_size = engine.model_config().vocab_size,
            max_seq_len = engine.model_config().max_seq_len,
            "local model loaded"
        );

        let chat = ChatEngine::new(engine, Some(base_system_prompt));

        Ok(Self {
            chat: Arc::new(Mutex::new(chat)),
            model_path,
            personality: config.core_personality.clone(),
            agent_name: config.agent_name.clone(),
            timezone: config.timezone.clone(),
            locale: config.locale.clone(),
        })
    }

    /// Generate a response by running inference on the blocking thread pool.
    ///
    /// NOTE: The local engine's ChatEngine is initialized with the base system
    /// prompt (without tools or prompt skills).  Neither tool schemas nor
    /// dynamic prompt skills are injected into the KV cache — the local
    /// backend is primarily for simple chat.
    pub async fn generate(&self, ctx: &GenerateContext<'_>) -> Result<String> {
        let chat = Arc::clone(&self.chat);
        let msg = ctx.message.to_string();

        let response = tokio::task::spawn_blocking(move || {
            let mut engine = chat.lock().map_err(|e| {
                SafeAgentError::Llm(format!("chat engine lock poisoned: {e}"))
            })?;
            engine.chat(&msg).map_err(|e| {
                SafeAgentError::Llm(format!("local inference failed: {e}"))
            })
        })
        .await
        .map_err(|e| SafeAgentError::Llm(format!("blocking task join error: {e}")))??;

        if response.is_empty() {
            return Err(SafeAgentError::Llm(
                "local model returned empty response".into(),
            ));
        }

        info!(
            model = %self.model_path,
            response_len = response.len(),
            "local model response received"
        );

        Ok(response)
    }
}
