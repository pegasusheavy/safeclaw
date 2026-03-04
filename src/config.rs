use serde::Deserialize;
use std::path::{Path, PathBuf};
use tracing::info;

use crate::error::{Result, SafeAgentError};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_agent_name")]
    pub agent_name: String,

    #[serde(default)]
    pub core_personality: String,

    /// Default timezone for the system (IANA name, e.g. "America/New_York").
    /// Per-user overrides take precedence.  Defaults to "UTC".
    #[serde(default = "default_timezone")]
    pub timezone: String,

    /// Default locale for date/number formatting (BCP 47 tag, e.g. "en-US").
    /// Per-user overrides take precedence.  Defaults to "en-US".
    #[serde(default = "default_locale")]
    pub locale: String,

    #[serde(default = "default_dashboard_bind")]
    pub dashboard_bind: String,

    #[serde(default = "default_tick_interval_secs")]
    pub tick_interval_secs: u64,

    #[serde(default = "default_conversation_window")]
    pub conversation_window: usize,

    #[serde(default = "default_approval_expiry_secs")]
    pub approval_expiry_secs: u64,

    #[serde(default = "default_auto_approve_tools")]
    pub auto_approve_tools: Vec<String>,

    /// Maximum number of tool-call round-trips per user message before the
    /// agent returns whatever it has.  Prevents infinite tool-call loops.
    #[serde(default = "default_max_tool_turns")]
    pub max_tool_turns: usize,

    #[serde(default)]
    pub llm: LlmConfig,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub telegram: TelegramConfig,

    #[serde(default)]
    pub whatsapp: WhatsAppConfig,

    #[serde(default)]
    pub imessage: IMessageConfig,

    #[serde(default)]
    pub twilio: TwilioConfig,

    #[serde(default)]
    pub android_sms: AndroidSmsConfig,

    #[serde(default)]
    pub discord: DiscordConfig,

    #[serde(default)]
    pub signal: SignalConfig,

    #[serde(default)]
    pub sessions: SessionsConfig,

    #[serde(default)]
    pub tunnel: TunnelConfig,

    #[serde(default)]
    pub tls: TlsConfig,

    #[serde(default)]
    pub security: SecurityConfig,

    #[serde(default)]
    pub federation: FederationConfig,

    #[serde(default)]
    pub plugins: PluginsConfig,

    #[serde(default)]
    pub memory: MemoryConfig,

    #[serde(default)]
    pub mcp: McpConfig,
}

// -- MCP ---------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
        }
    }
}

/// A single MCP server entry in the config.
///
/// ```toml
/// [[mcp.servers]]
/// name = "filesystem"
/// transport = "stdio"
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
///
/// [[mcp.servers]]
/// name = "remote-tools"
/// transport = "http"
/// url = "http://localhost:8080/mcp"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerEntry {
    pub name: String,

    #[serde(default = "default_mcp_transport")]
    pub transport: String,

    /// For stdio transport: the command to spawn.
    #[serde(default)]
    pub command: String,

    /// For stdio transport: arguments to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// For http transport: the URL of the MCP server.
    #[serde(default)]
    pub url: String,

    /// Optional prefix for tool names (defaults to server name).
    #[serde(default)]
    pub tool_prefix: Option<String>,
}

fn default_mcp_transport() -> String {
    "stdio".into()
}

// -- Federation --------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct FederationConfig {
    /// Enable multi-node federation.
    #[serde(default)]
    pub enabled: bool,

    /// Display name for this node (defaults to agent_name).
    #[serde(default)]
    pub node_name: String,

    /// Advertised address of this node (e.g. "http://host:3031").
    /// Peers use this to connect back.
    #[serde(default)]
    pub advertise_address: String,

}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_name: String::new(),
            advertise_address: String::new(),
        }
    }
}

// -- Security ----------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    /// Tools that are completely blocked (never executable).
    #[serde(default)]
    pub blocked_tools: Vec<String>,

    /// Tools that require 2FA (confirmation on a second channel) before execution.
    #[serde(default = "default_2fa_tools")]
    pub require_2fa: Vec<String>,

    /// Maximum tool calls per minute (0 = unlimited).
    #[serde(default = "default_rate_limit_per_minute")]
    pub rate_limit_per_minute: u32,

    /// Maximum tool calls per hour (0 = unlimited).
    #[serde(default = "default_rate_limit_per_hour")]
    pub rate_limit_per_hour: u32,

    /// Maximum estimated LLM cost per day in USD (0.0 = unlimited).
    #[serde(default)]
    pub daily_cost_limit_usd: f64,

    /// Enable PII/sensitive data detection in LLM responses.
    #[serde(default = "default_true")]
    pub pii_detection: bool,

    /// Capability restrictions per tool. Keys are tool names, values are
    /// lists of allowed operations/capabilities.
    /// e.g. { "exec" = ["echo", "ls", "cat"], "file" = ["read"] }
    #[serde(default)]
    pub tool_capabilities: std::collections::HashMap<String, Vec<String>>,
}

// -- LLM -----------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    /// Backend to use: "claude" (default), "cline", "codex", "gemini",
    /// "aider", "openrouter", "ollama", or "local".
    /// Can be overridden with the `LLM_BACKEND` env var.
    #[serde(default = "default_backend")]
    pub backend: String,

    /// Ordered list of backend keys to try on failure.
    /// e.g. ["claude", "openrouter", "gemini"]
    /// If empty (default), uses the single `backend` field.
    #[serde(default)]
    pub failover_chain: Vec<String>,

    // -- Claude CLI settings (backend = "claude") --

    /// Path to the `claude` binary (default: "claude").
    /// Can be overridden with the `CLAUDE_BIN` env var.
    #[serde(default = "default_claude_bin")]
    pub claude_bin: String,

    /// Claude Code config directory for profile selection.
    /// Can be overridden with the `CLAUDE_CONFIG_DIR` env var.
    #[serde(default)]
    pub claude_config_dir: String,

    /// Model to use (e.g. "sonnet", "opus", "haiku").
    /// Can be overridden with the `CLAUDE_MODEL` env var.
    #[serde(default = "default_model")]
    pub model: String,

    /// Maximum tool-use turns per Claude CLI invocation.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Process timeout in seconds (0 = no timeout).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    // -- Cline CLI settings (backend = "cline") --

    /// Path to the `cline` binary (default: "cline").
    /// Can be overridden with the `CLINE_BIN` env var.
    #[serde(default = "default_cline_bin")]
    pub cline_bin: String,

    /// Cline model override (e.g. "claude-sonnet-4-20250514").
    /// Can be overridden with the `CLINE_MODEL` env var.
    #[serde(default)]
    pub cline_model: String,

    // -- Codex CLI settings (backend = "codex") --

    /// Path to the `codex` binary (default: "codex").
    /// Can be overridden with the `CODEX_BIN` env var.
    #[serde(default = "default_codex_bin")]
    pub codex_bin: String,

    /// Codex model override (e.g. "gpt-5-codex", "o3").
    /// Can be overridden with the `CODEX_MODEL` env var.
    #[serde(default)]
    pub codex_model: String,

    /// Codex config profile name (from `~/.codex/config.toml`).
    /// Can be overridden with the `CODEX_PROFILE` env var.
    #[serde(default)]
    pub codex_profile: String,

    // -- Gemini CLI settings (backend = "gemini") --

    /// Path to the `gemini` binary (default: "gemini").
    /// Can be overridden with the `GEMINI_BIN` env var.
    #[serde(default = "default_gemini_bin")]
    pub gemini_bin: String,

    /// Gemini model override (e.g. "gemini-2.5-pro").
    /// Can be overridden with the `GEMINI_MODEL` env var.
    #[serde(default)]
    pub gemini_model: String,

    // -- Aider settings (backend = "aider") --

    /// Path to the `aider` binary (default: "aider").
    /// Can be overridden with the `AIDER_BIN` env var.
    #[serde(default = "default_aider_bin")]
    pub aider_bin: String,

    /// Aider model string (e.g. "gpt-4o", "claude-3.5-sonnet").
    /// Can be overridden with the `AIDER_MODEL` env var.
    #[serde(default)]
    pub aider_model: String,

    // -- OpenRouter settings (backend = "openrouter") --

    /// OpenRouter API key.
    /// Can be overridden with `OPENROUTER_API_KEY` env var.
    #[serde(default)]
    pub openrouter_api_key: String,

    /// OpenRouter model identifier (e.g. "anthropic/claude-sonnet-4",
    /// "openai/gpt-4o", "google/gemini-2.5-pro", "meta-llama/llama-4-maverick").
    /// Can be overridden with `OPENROUTER_MODEL` env var.
    #[serde(default)]
    pub openrouter_model: String,

    /// OpenRouter API base URL (default: "https://openrouter.ai/api/v1").
    /// Can be overridden with `OPENROUTER_BASE_URL` env var.
    #[serde(default)]
    pub openrouter_base_url: String,

    /// Max tokens for OpenRouter completions (0 = use general max_tokens).
    #[serde(default)]
    pub openrouter_max_tokens: usize,

    /// Site URL sent as HTTP-Referer for OpenRouter analytics.
    /// Can be overridden with `OPENROUTER_SITE_URL` env var.
    #[serde(default)]
    pub openrouter_site_url: String,

    /// App name sent as X-Title for OpenRouter dashboard identification.
    /// Can be overridden with `OPENROUTER_APP_NAME` env var.
    #[serde(default)]
    pub openrouter_app_name: String,

    // -- Ollama settings (backend = "ollama") --

    /// Ollama API base URL (default: "http://localhost:11434").
    /// Can be overridden with `OLLAMA_HOST` env var.
    #[serde(default)]
    pub ollama_host: String,

    /// Ollama model tag (e.g. "llama3.1:8b", "qwen2.5-coder:14b").
    /// Can be overridden with `OLLAMA_MODEL` env var.
    #[serde(default)]
    pub ollama_model: String,

    // -- Local model settings (backend = "local") --

    /// Path to a GGUF model file for local inference.
    /// Can be overridden with the `MODEL_PATH` env var.
    #[serde(default)]
    pub model_path: String,

    /// Temperature for local sampling (0.0 = greedy).
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Top-P (nucleus) sampling for local model.
    #[serde(default = "default_top_p")]
    pub top_p: f32,

    /// Maximum tokens to generate per response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,

    /// Use GPU acceleration for local inference (requires CUDA).
    #[serde(default)]
    pub use_gpu: bool,

    /// Maximum context length for local inference.
    /// Caps the model's KV cache size to reduce VRAM usage.
    /// If unset, uses the model's native context length.
    #[serde(default)]
    pub max_context_len: Option<usize>,

}

// -- Tools ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub exec: ExecToolConfig,

    #[serde(default)]
    pub web: WebToolConfig,

    #[serde(default)]
    pub browser: BrowserToolConfig,

    #[serde(default)]
    pub message: MessageToolConfig,

    #[serde(default)]
    pub cron: CronToolConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_exec_timeout")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_web_max_results")]
    pub max_results: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrowserToolConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub headless: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CronToolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

// -- Dashboard -----------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DashboardConfig {
    /// Whether password-based login is enabled (default: true).
    /// Set to false to require SSO-only login.
    #[serde(default = "default_true")]
    pub password_enabled: bool,

    /// SSO providers enabled for dashboard login.
    /// Use provider IDs from the OAuth registry: "google", "github",
    /// "microsoft", "discord", etc.
    #[serde(default)]
    pub sso_providers: Vec<String>,

    /// Email addresses allowed to sign in via SSO.
    /// Empty means any authenticated SSO user is allowed.
    #[serde(default)]
    pub sso_allowed_emails: Vec<String>,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            password_enabled: true,
            sso_providers: Vec::new(),
            sso_allowed_emails: Vec::new(),
        }
    }
}

// -- Telegram ------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
}

// -- WhatsApp ------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_whatsapp_bridge_port")]
    pub bridge_port: u16,

    /// The dashboard port used to construct the webhook URL that the
    /// bridge POSTs incoming messages to.
    #[serde(default = "default_whatsapp_webhook_port")]
    pub webhook_port: u16,

    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

fn default_whatsapp_bridge_port() -> u16 {
    3033
}

fn default_whatsapp_webhook_port() -> u16 {
    3030
}

impl Default for WhatsAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bridge_port: default_whatsapp_bridge_port(),
            webhook_port: default_whatsapp_webhook_port(),
            allowed_numbers: Vec::new(),
        }
    }
}

// -- iMessage ------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct IMessageConfig {
    #[serde(default)]
    pub enabled: bool,

    /// URL of the iMessage AppleScript bridge HTTP server.
    #[serde(default = "default_imessage_bridge_url")]
    pub bridge_url: String,

    /// Allowed phone numbers and/or iCloud email addresses.
    #[serde(default)]
    pub allowed_ids: Vec<String>,
}

fn default_imessage_bridge_url() -> String {
    "http://127.0.0.1:3040".to_string()
}

impl Default for IMessageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bridge_url: default_imessage_bridge_url(),
            allowed_ids: Vec::new(),
        }
    }
}

// -- Twilio --------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TwilioConfig {
    #[serde(default)]
    pub enabled: bool,

    /// The Twilio phone number to send from (e.g. "+15559876543").
    #[serde(default)]
    pub from_number: String,

    /// Allowed destination phone numbers.
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

impl Default for TwilioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            from_number: String::new(),
            allowed_numbers: Vec::new(),
        }
    }
}

// -- Android SMS ---------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AndroidSmsConfig {
    #[serde(default)]
    pub enabled: bool,

    /// URL of the Android Termux bridge HTTP server.
    #[serde(default = "default_android_sms_bridge_url")]
    pub bridge_url: String,

    /// Allowed phone numbers.
    #[serde(default)]
    pub allowed_ids: Vec<String>,
}

fn default_android_sms_bridge_url() -> String {
    "http://127.0.0.1:3041".to_string()
}

impl Default for AndroidSmsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bridge_url: default_android_sms_bridge_url(),
            allowed_ids: Vec::new(),
        }
    }
}

// -- Discord -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_guild_ids: Vec<u64>,

    #[serde(default)]
    pub allowed_channel_ids: Vec<u64>,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_guild_ids: Vec::new(),
            allowed_channel_ids: Vec::new(),
        }
    }
}

// -- Signal --------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SignalConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub allowed_numbers: Vec<String>,

    /// URL of the signal-cli-rest-api bridge.
    #[serde(default = "default_signal_bridge_url")]
    pub bridge_url: String,
}

fn default_signal_bridge_url() -> String {
    "http://127.0.0.1:3042".to_string()
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_numbers: Vec::new(),
            bridge_url: default_signal_bridge_url(),
        }
    }
}

// -- Sessions ------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SessionsConfig {
    #[serde(default)]
    pub enabled: bool,
}

// -- Plugins -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct PluginsConfig {
    /// Global plugin directory (default: ~/.config/safeclaw/plugins).
    /// Empty string means use the default path.
    #[serde(default)]
    pub global_dir: String,

    /// Project-local plugin directory (default: .safeclaw/plugins).
    /// Relative to the working directory. Empty means use default.
    #[serde(default)]
    pub project_dir: String,

    /// Plugin names to explicitly disable.
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            global_dir: String::new(),
            project_dir: String::new(),
            disabled: Vec::new(),
        }
    }
}

// -- TLS / ACME ----------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    /// Enable ACME (Let's Encrypt) automatic certificate management.
    /// When enabled, `acme_domains` and `acme_email` are required.
    /// Can be overridden with `ACME_ENABLED=true`.
    #[serde(default)]
    pub acme_enabled: bool,

    /// Domain name(s) for the certificate.
    /// Can be overridden with `ACME_DOMAIN`.
    #[serde(default)]
    pub acme_domains: Vec<String>,

    /// Contact email for Let's Encrypt (e.g. "mailto:admin@example.com").
    /// Can be overridden with `ACME_EMAIL`.
    #[serde(default)]
    pub acme_email: String,

    /// Use the Let's Encrypt production CA (true) or staging (false).
    /// Staging is useful for testing — it doesn't enforce rate limits.
    /// Can be overridden with `ACME_PRODUCTION=true`.
    #[serde(default)]
    pub acme_production: bool,

    /// Directory to cache ACME account keys and certificates.
    /// Defaults to `$XDG_DATA_HOME/safeclaw/acme-cache`.
    #[serde(default)]
    pub acme_cache_dir: String,

    /// Port for the HTTPS listener (default: 443).
    /// Can be overridden with `ACME_PORT`.
    #[serde(default = "default_acme_port")]
    pub acme_port: u16,
}

// -- Tunnel (multi-provider) ------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TunnelConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_tunnel_provider")]
    pub provider: String,

    #[serde(default)]
    pub ngrok: NgrokConfig,

    #[serde(default)]
    pub cloudflare: CloudflareConfig,

    #[serde(default)]
    pub tailscale: TailscaleConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NgrokConfig {
    #[serde(default)]
    pub authtoken: String,

    #[serde(default)]
    pub domain: String,

    #[serde(default = "default_ngrok_bin")]
    pub bin: String,

    #[serde(default = "default_ngrok_inspect_port")]
    pub inspect_port: u16,

    #[serde(default = "default_ngrok_poll_interval")]
    pub poll_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudflareConfig {
    #[serde(default)]
    pub tunnel_id: String,

    #[serde(default)]
    pub credentials_file: String,

    #[serde(default = "default_cloudflared_bin")]
    pub bin: String,

    #[serde(default)]
    pub hostname: String,

    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TailscaleConfig {
    #[serde(default = "default_tailscale_mode")]
    pub mode: String,

    #[serde(default = "default_tailscale_bin")]
    pub bin: String,

    #[serde(default)]
    pub hostname: String,

    #[serde(default)]
    pub url: String,
}

// -- Memory --------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    /// Ollama model used for generating embeddings (default: "nomic-embed-text").
    /// Set to empty string to disable embeddings and fall back to FTS5.
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,

    /// Ollama host used for embedding requests (defaults to the same as llm.ollama_host).
    /// Can be overridden with `EMBEDDING_OLLAMA_HOST` env var.
    #[serde(default)]
    pub embedding_host: String,

    /// Automatically extract facts, preferences, and entities after each conversation.
    #[serde(default = "default_true")]
    pub auto_extract: bool,

    /// Consolidate archival memories older than this many days.
    #[serde(default = "default_consolidation_age_days")]
    pub consolidation_age_days: u32,

    /// Maximum number of old memories to consolidate per tick.
    #[serde(default = "default_consolidation_batch")]
    pub consolidation_batch_size: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            embedding_model: default_embedding_model(),
            embedding_host: String::new(),
            auto_extract: true,
            consolidation_age_days: default_consolidation_age_days(),
            consolidation_batch_size: default_consolidation_batch(),
        }
    }
}

// -- Defaults ------------------------------------------------------------

fn default_agent_name() -> String {
    "safeclaw".to_string()
}
fn default_timezone() -> String {
    "UTC".to_string()
}
fn default_locale() -> String {
    "en-US".to_string()
}
fn default_dashboard_bind() -> String {
    "127.0.0.1:3030".to_string()
}
fn default_tick_interval_secs() -> u64 {
    120
}
fn default_conversation_window() -> usize {
    5
}
fn default_approval_expiry_secs() -> u64 {
    3600
}
fn default_auto_approve_tools() -> Vec<String> {
    vec![
        "message".to_string(),
        "memory_search".to_string(),
        "memory_get".to_string(),
        "goal".to_string(),
    ]
}
fn default_max_tool_turns() -> usize {
    5
}
fn default_backend() -> String {
    "claude".to_string()
}
fn default_claude_bin() -> String {
    "claude".to_string()
}
fn default_cline_bin() -> String {
    "cline".to_string()
}
fn default_codex_bin() -> String {
    "codex".to_string()
}
fn default_gemini_bin() -> String {
    "gemini".to_string()
}
fn default_aider_bin() -> String {
    "aider".to_string()
}
fn default_model() -> String {
    "sonnet".to_string()
}
fn default_max_turns() -> u32 {
    10
}
fn default_timeout_secs() -> u64 {
    120
}
fn default_temperature() -> f32 {
    0.7
}
fn default_top_p() -> f32 {
    0.95
}
fn default_max_tokens() -> usize {
    2048
}
fn default_true() -> bool {
    true
}
fn default_exec_timeout() -> u64 {
    30
}
fn default_web_max_results() -> usize {
    10
}
fn default_acme_port() -> u16 {
    443
}
fn default_ngrok_bin() -> String {
    "ngrok".to_string()
}
fn default_ngrok_inspect_port() -> u16 {
    4040
}
fn default_ngrok_poll_interval() -> u64 {
    15
}
fn default_tunnel_provider() -> String {
    "ngrok".to_string()
}
fn default_cloudflared_bin() -> String {
    "cloudflared".to_string()
}
fn default_tailscale_bin() -> String {
    "tailscale".to_string()
}
fn default_tailscale_mode() -> String {
    "funnel".to_string()
}
fn default_embedding_model() -> String {
    "nomic-embed-text".to_string()
}
fn default_consolidation_age_days() -> u32 {
    30
}
fn default_consolidation_batch() -> usize {
    20
}
fn default_2fa_tools() -> Vec<String> {
    vec![
        "exec".to_string(),
    ]
}
fn default_rate_limit_per_minute() -> u32 {
    30
}
fn default_rate_limit_per_hour() -> u32 {
    300
}

// -- Default impls -------------------------------------------------------

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            failover_chain: Vec::new(),
            claude_bin: default_claude_bin(),
            claude_config_dir: String::new(),
            model: default_model(),
            max_turns: default_max_turns(),
            timeout_secs: default_timeout_secs(),
            cline_bin: default_cline_bin(),
            cline_model: String::new(),
            codex_bin: default_codex_bin(),
            codex_model: String::new(),
            codex_profile: String::new(),
            gemini_bin: default_gemini_bin(),
            gemini_model: String::new(),
            aider_bin: default_aider_bin(),
            aider_model: String::new(),
            openrouter_api_key: String::new(),
            openrouter_model: String::new(),
            openrouter_base_url: String::new(),
            openrouter_max_tokens: 0,
            openrouter_site_url: String::new(),
            openrouter_app_name: String::new(),
            ollama_host: String::new(),
            ollama_model: String::new(),
            model_path: String::new(),
            temperature: default_temperature(),
            top_p: default_top_p(),
            max_tokens: default_max_tokens(),
            use_gpu: false,
            max_context_len: None,
        }
    }
}

impl Default for ExecToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: default_exec_timeout(),
        }
    }
}

impl Default for WebToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_results: default_web_max_results(),
        }
    }
}

impl Default for BrowserToolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            headless: true,
        }
    }
}

impl Default for MessageToolConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

impl Default for CronToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
        }
    }
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_chat_ids: Vec::new(),
        }
    }
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
        }
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            acme_enabled: false,
            acme_domains: Vec::new(),
            acme_email: String::new(),
            acme_production: false,
            acme_cache_dir: String::new(),
            acme_port: default_acme_port(),
        }
    }
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_tunnel_provider(),
            ngrok: NgrokConfig::default(),
            cloudflare: CloudflareConfig::default(),
            tailscale: TailscaleConfig::default(),
        }
    }
}

impl Default for NgrokConfig {
    fn default() -> Self {
        Self {
            authtoken: String::new(),
            domain: String::new(),
            bin: default_ngrok_bin(),
            inspect_port: default_ngrok_inspect_port(),
            poll_interval_secs: default_ngrok_poll_interval(),
        }
    }
}

impl Default for CloudflareConfig {
    fn default() -> Self {
        Self {
            tunnel_id: String::new(),
            credentials_file: String::new(),
            bin: default_cloudflared_bin(),
            hostname: String::new(),
            url: String::new(),
        }
    }
}

impl Default for TailscaleConfig {
    fn default() -> Self {
        Self {
            mode: default_tailscale_mode(),
            bin: default_tailscale_bin(),
            hostname: String::new(),
            url: String::new(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            blocked_tools: Vec::new(),
            require_2fa: default_2fa_tools(),
            rate_limit_per_minute: default_rate_limit_per_minute(),
            rate_limit_per_hour: default_rate_limit_per_hour(),
            daily_cost_limit_usd: 0.0,
            pii_detection: true,
            tool_capabilities: std::collections::HashMap::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent_name: default_agent_name(),
            core_personality: String::new(),
            timezone: default_timezone(),
            locale: default_locale(),
            dashboard_bind: default_dashboard_bind(),
            tick_interval_secs: default_tick_interval_secs(),
            conversation_window: default_conversation_window(),
            approval_expiry_secs: default_approval_expiry_secs(),
            auto_approve_tools: default_auto_approve_tools(),
            max_tool_turns: default_max_tool_turns(),
            llm: LlmConfig::default(),
            tools: ToolsConfig::default(),
            dashboard: DashboardConfig::default(),
            telegram: TelegramConfig::default(),
            whatsapp: WhatsAppConfig::default(),
            imessage: IMessageConfig::default(),
            twilio: TwilioConfig::default(),
            android_sms: AndroidSmsConfig::default(),
            discord: DiscordConfig::default(),
            signal: SignalConfig::default(),
            sessions: SessionsConfig::default(),
            tunnel: TunnelConfig::default(),
            tls: TlsConfig::default(),
            security: SecurityConfig::default(),
            federation: FederationConfig::default(),
            plugins: PluginsConfig::default(),
            memory: MemoryConfig::default(),
            mcp: McpConfig::default(),
        }
    }
}

// -- Config impl ---------------------------------------------------------

impl Config {
    /// Load config from the given path, or the default XDG config location.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path(),
        };

        let config = if config_path.exists() {
            info!("loading config from {}", config_path.display());
            let contents = std::fs::read_to_string(&config_path).map_err(SafeAgentError::Io)?;
            toml::from_str(&contents)
                .map_err(|e| SafeAgentError::Config(format!("parse error: {e}")))?
        } else {
            info!("no config file found, using defaults");
            Config::default()
        };

        Ok(config)
    }

    /// Returns the default config file path: `$XDG_CONFIG_HOME/safeclaw/config.toml`
    pub fn default_config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("safeclaw")
            .join("config.toml")
    }

    /// Returns the data directory: `$XDG_DATA_HOME/safeclaw/`
    pub fn data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from(".local/share"))
            .join("safeclaw")
    }

    /// Get the Telegram bot token from the environment.
    pub fn telegram_bot_token() -> Result<String> {
        std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| SafeAgentError::Config("TELEGRAM_BOT_TOKEN environment variable not set".into()))
    }

    /// Read Twilio credentials from environment variables.
    pub fn twilio_credentials() -> Result<(String, String)> {
        let sid = std::env::var("TWILIO_ACCOUNT_SID")
            .map_err(|_| SafeAgentError::Config("TWILIO_ACCOUNT_SID not set".into()))?;
        let token = std::env::var("TWILIO_AUTH_TOKEN")
            .map_err(|_| SafeAgentError::Config("TWILIO_AUTH_TOKEN not set".into()))?;
        Ok((sid, token))
    }

    /// Generate the default config file contents.
    pub fn default_config_contents() -> &'static str {
        include_str!("../config.example.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let c = Config::default();
        assert_eq!(c.agent_name, "safeclaw");
        assert_eq!(c.dashboard_bind, "127.0.0.1:3030");
        assert_eq!(c.tick_interval_secs, 120);
        assert_eq!(c.conversation_window, 5);
        assert_eq!(c.approval_expiry_secs, 3600);
        assert_eq!(c.max_tool_turns, 5);
        assert!(c.core_personality.is_empty());
    }

    #[test]
    fn test_default_auto_approve_tools() {
        let c = Config::default();
        assert!(c.auto_approve_tools.contains(&"message".to_string()));
        assert!(c.auto_approve_tools.contains(&"memory_search".to_string()));
        assert!(c.auto_approve_tools.contains(&"memory_get".to_string()));
        assert!(c.auto_approve_tools.contains(&"goal".to_string()));
        assert_eq!(c.auto_approve_tools.len(), 4);
    }

    #[test]
    fn default_llm_config() {
        let llm = LlmConfig::default();
        assert_eq!(llm.backend, "claude");
        assert_eq!(llm.claude_bin, "claude");
        assert_eq!(llm.model, "sonnet");
        assert_eq!(llm.max_turns, 10);
        assert_eq!(llm.timeout_secs, 120);
        assert!((llm.temperature - 0.7).abs() < 0.001);
        assert!((llm.top_p - 0.95).abs() < 0.001);
        assert_eq!(llm.max_tokens, 2048);
    }

    #[test]
    fn default_tools_config() {
        let tools = ToolsConfig::default();
        assert!(tools.exec.enabled);
        assert_eq!(tools.exec.timeout_secs, 30);
        assert!(tools.web.enabled);
        assert_eq!(tools.web.max_results, 10);
        assert!(!tools.browser.enabled);
        assert!(tools.browser.headless);
        assert!(!tools.message.enabled);
        assert!(tools.cron.enabled);
    }

    #[test]
    fn default_dashboard_config() {
        let d = DashboardConfig::default();
        assert!(d.password_enabled);
        assert!(d.sso_providers.is_empty());
        assert!(d.sso_allowed_emails.is_empty());
    }

    #[test]
    fn default_telegram_config() {
        let t = TelegramConfig::default();
        assert!(!t.enabled);
        assert!(t.allowed_chat_ids.is_empty());
    }

    #[test]
    fn default_whatsapp_config() {
        let w = WhatsAppConfig::default();
        assert!(!w.enabled);
        assert_eq!(w.bridge_port, 3033);
        assert_eq!(w.webhook_port, 3030);
        assert!(w.allowed_numbers.is_empty());
    }

    #[test]
    fn default_sessions_config() {
        let s = SessionsConfig::default();
        assert!(!s.enabled);
    }

    #[test]
    fn default_tls_config() {
        let t = TlsConfig::default();
        assert!(!t.acme_enabled);
        assert!(t.acme_domains.is_empty());
        assert!(t.acme_email.is_empty());
        assert!(!t.acme_production);
        assert_eq!(t.acme_port, 443);
    }

    #[test]
    fn default_tunnel_config() {
        let t = TunnelConfig::default();
        assert!(!t.enabled);
        assert_eq!(t.provider, "ngrok");
        // NgrokConfig defaults
        assert_eq!(t.ngrok.bin, "ngrok");
        assert!(t.ngrok.authtoken.is_empty());
        assert!(t.ngrok.domain.is_empty());
        assert_eq!(t.ngrok.inspect_port, 4040);
        assert_eq!(t.ngrok.poll_interval_secs, 15);
        // CloudflareConfig defaults
        assert_eq!(t.cloudflare.bin, "cloudflared");
        assert!(t.cloudflare.tunnel_id.is_empty());
        assert!(t.cloudflare.hostname.is_empty());
        // TailscaleConfig defaults
        assert_eq!(t.tailscale.bin, "tailscale");
        assert_eq!(t.tailscale.mode, "funnel");
        assert!(t.tailscale.hostname.is_empty());
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"agent_name = "TestBot""#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.agent_name, "TestBot");
        assert_eq!(c.dashboard_bind, "127.0.0.1:3030");
        assert_eq!(c.max_tool_turns, 5);
    }

    #[test]
    fn parse_llm_section() {
        let toml_str = r#"
        [llm]
        backend = "openrouter"
        model = "opus"
        max_turns = 20
        temperature = 0.5
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.llm.backend, "openrouter");
        assert_eq!(c.llm.model, "opus");
        assert_eq!(c.llm.max_turns, 20);
        assert!((c.llm.temperature - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_tools_section() {
        let toml_str = r#"
        [tools.exec]
        enabled = false
        timeout_secs = 60
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(!c.tools.exec.enabled);
        assert_eq!(c.tools.exec.timeout_secs, 60);
    }

    #[test]
    fn parse_dashboard_sso() {
        let toml_str = r#"
        [dashboard]
        password_enabled = false
        sso_providers = ["google", "github"]
        sso_allowed_emails = ["admin@example.com"]
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert!(!c.dashboard.password_enabled);
        assert_eq!(c.dashboard.sso_providers, vec!["google", "github"]);
        assert_eq!(c.dashboard.sso_allowed_emails, vec!["admin@example.com"]);
    }

    #[test]
    fn load_nonexistent_returns_defaults() {
        let c = Config::load(Some(Path::new("/tmp/nonexistent-safeclaw-test.toml"))).unwrap();
        assert_eq!(c.agent_name, "safeclaw");
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let path = std::env::temp_dir().join("bad-safeclaw.toml");
        std::fs::write(&path, "this is not valid %%% toml").unwrap();
        let result = Config::load(Some(&path));
        assert!(result.is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_config_path_has_safeclaw() {
        let path = Config::default_config_path();
        assert!(path.to_string_lossy().contains("safeclaw"));
        assert!(path.to_string_lossy().contains("config.toml"));
    }

    #[test]
    fn data_dir_has_safeclaw() {
        let path = Config::data_dir();
        assert!(path.to_string_lossy().contains("safeclaw"));
    }

    #[test]
    fn telegram_bot_token_without_env_var_errors() {
        unsafe { std::env::remove_var("TELEGRAM_BOT_TOKEN"); }
        assert!(Config::telegram_bot_token().is_err());
    }

    #[test]
    fn default_config_contents_is_non_empty() {
        let contents = Config::default_config_contents();
        assert!(!contents.is_empty());
    }

    #[test]
    fn default_plugins_config() {
        let c = Config::default();
        assert!(c.plugins.global_dir.is_empty());
        assert!(c.plugins.project_dir.is_empty());
        assert!(c.plugins.disabled.is_empty());
    }

    #[test]
    fn parse_plugins_section() {
        let toml_str = r#"
        [plugins]
        global_dir = "/home/user/.config/safeclaw/plugins"
        project_dir = ".safeclaw/plugins"
        disabled = ["broken-plugin"]
        "#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.plugins.global_dir, "/home/user/.config/safeclaw/plugins");
        assert_eq!(c.plugins.project_dir, ".safeclaw/plugins");
        assert_eq!(c.plugins.disabled, vec!["broken-plugin"]);
    }

    #[test]
    fn default_imessage_config() {
        let c = IMessageConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.bridge_url, "http://127.0.0.1:3040");
        assert!(c.allowed_ids.is_empty());
    }

    #[test]
    fn default_twilio_config() {
        let c = TwilioConfig::default();
        assert!(!c.enabled);
        assert!(c.from_number.is_empty());
        assert!(c.allowed_numbers.is_empty());
    }

    #[test]
    fn default_android_sms_config() {
        let c = AndroidSmsConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.bridge_url, "http://127.0.0.1:3041");
        assert!(c.allowed_ids.is_empty());
    }
}
