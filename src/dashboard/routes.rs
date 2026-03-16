use std::sync::Arc;

use axum::middleware;
use axum::routing::{any, delete, get, post, put};
use axum::Router;
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::agent::Agent;
use crate::config::Config;
use crate::dashboard::authn::PasskeyManager;
use crate::error::{Result, SafeAgentError};
use crate::installer::BinaryInstaller;
use crate::messaging::MessagingManager;
use crate::skills::ExtensionManager;
use crate::trash::TrashManager;

use super::auth;
use super::handlers;
use super::messaging_webhook;
use super::oauth;
use super::skill_ext;
use super::sse;
use super::webhook;

/// State shared across all routes.
#[derive(Clone)]
pub struct DashState {
    pub agent: Arc<Agent>,
    pub config: Config,
    pub db: Arc<Mutex<Connection>>,
    pub db_read: Arc<Mutex<Connection>>,
    /// The password users must provide to access the dashboard.
    pub dashboard_password: String,
    /// Secret bytes used to sign/verify HS256 JWT cookies.
    pub jwt_secret: Vec<u8>,
    /// Extension manager for Rhai-based skill routes and UI.
    pub extension_manager: Arc<Mutex<ExtensionManager>>,
    /// Messaging manager for WhatsApp QR code / status.
    pub messaging: Arc<MessagingManager>,
    /// Trash manager for recoverable file deletion.
    pub trash: Arc<TrashManager>,
    /// WebAuthn/passkey manager (None if origin not configured).
    pub passkey_manager: Option<Arc<PasskeyManager>>,
    /// Binary installer for managing tool binaries via dashboard.
    pub installer: BinaryInstaller,
}

pub fn build(
    agent: Arc<Agent>,
    config: Config,
    db: Arc<Mutex<Connection>>,
    db_read: Arc<Mutex<Connection>>,
    messaging: Arc<MessagingManager>,
    trash: Arc<TrashManager>,
    installer: BinaryInstaller,
) -> Result<Router> {
    let password_required = config.dashboard.password_enabled
        && config.dashboard.sso_providers.is_empty();

    let dashboard_password = std::env::var("DASHBOARD_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if password_required {
                None
            } else {
                // Password not required — use empty string as placeholder
                Some(String::new())
            }
        })
        .ok_or_else(|| {
            SafeAgentError::Config(
                "DASHBOARD_PASSWORD environment variable is required but not set".to_string(),
            )
        })?;

    let jwt_secret_str = std::env::var("JWT_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            SafeAgentError::Config(
                "JWT_SECRET environment variable is required but not set".to_string(),
            )
        })?;

    let jwt_secret = jwt_secret_str.into_bytes();

    tracing::info!("dashboard password protection enabled (JWT auth)");

    // Initialize skill extension manager
    let skills_dir = Config::data_dir().join("skills");
    let db_path = Config::data_dir().join("safeclaw.db");
    let mut ext_mgr = ExtensionManager::new(skills_dir, db_path);
    ext_mgr.discover();

    // Attempt to build a PasskeyManager for WebAuthn support.
    // Requires WEBAUTHN_ORIGIN + WEBAUTHN_RP_ID (or TUNNEL_URL for origin).
    let passkey_manager = {
        let origin = std::env::var("WEBAUTHN_ORIGIN")
            .ok()
            .or_else(|| std::env::var("TUNNEL_URL").ok())
            .unwrap_or_else(|| {
                let bind = config.dashboard_bind.clone();
                if bind.starts_with("http") { bind } else { format!("http://{bind}") }
            });
        let rp_id = std::env::var("WEBAUTHN_RP_ID")
            .ok()
            .unwrap_or_else(|| {
                webauthn_rs::prelude::Url::parse(&origin)
                    .ok()
                    .and_then(|u| u.host_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "localhost".to_string())
            });

        match PasskeyManager::new(db.clone(), &origin, &rp_id) {
            Ok(mgr) => {
                tracing::info!(origin, rp_id, "WebAuthn passkey support enabled");
                Some(Arc::new(mgr))
            }
            Err(e) => {
                tracing::warn!(err = %e, "WebAuthn passkey support disabled");
                None
            }
        }
    };

    let state = DashState {
        agent,
        config,
        db,
        db_read,
        dashboard_password,
        jwt_secret,
        extension_manager: Arc::new(Mutex::new(ext_mgr)),
        messaging,
        trash,
        passkey_manager,
        installer,
    };

    Ok(Router::new()
        // Dashboard UI
        .route("/", get(serve_index))
        .route("/style.css", get(serve_css))
        .route("/app.js", get(serve_js))
        .route("/manifest.json", get(serve_manifest))
        .route("/sw.js", get(serve_sw))
        .route("/manifest-icon-192.png", get(serve_icon_192))
        .route("/manifest-icon-512.png", get(serve_icon_512))
        // Auth
        .route("/api/auth/check", get(auth::check))
        .route("/api/auth/info", get(auth::login_info))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/sso/{provider}/start", get(auth::sso_start))
        .route("/api/auth/sso/{provider}/callback", get(auth::sso_callback))
        // 2FA / Passkey authentication endpoints
        .route("/api/auth/2fa/verify", post(auth::verify_2fa))
        .route("/api/auth/2fa/setup", post(auth::setup_totp))
        .route("/api/auth/2fa/enable", post(auth::enable_totp))
        .route("/api/auth/2fa/disable", post(auth::disable_totp))
        .route("/api/auth/2fa/status", get(auth::totp_status))
        .route("/api/auth/passkey/register/start", post(auth::passkey_register_start))
        .route("/api/auth/passkey/register/finish", post(auth::passkey_register_finish))
        .route("/api/auth/passkey/authenticate/start", post(auth::passkey_auth_start))
        .route("/api/auth/passkey/authenticate/finish", post(auth::passkey_auth_finish))
        .route("/api/auth/passkeys", get(auth::list_passkeys))
        .route("/api/auth/passkeys/{id}", delete(auth::delete_passkey))
        // API — Status & Control
        .route("/api/status", get(handlers::get_status))
        .route("/api/stats", get(handlers::get_stats))
        .route("/api/agent/pause", post(handlers::pause_agent))
        .route("/api/agent/resume", post(handlers::resume_agent))
        .route("/api/agent/tick", post(handlers::force_tick))
        // API — Approval Queue
        .route("/api/pending", get(handlers::get_pending))
        .route("/api/pending/{id}/approve", post(handlers::approve_action))
        .route("/api/pending/{id}/reject", post(handlers::reject_action))
        .route("/api/pending/approve-all", post(handlers::approve_all))
        .route("/api/pending/reject-all", post(handlers::reject_all))
        // API — Activity
        .route("/api/activity", get(handlers::get_activity))
        // API — Memory
        .route("/api/memory/core", get(handlers::get_core_memory))
        .route("/api/memory/conversation", get(handlers::get_conversation_memory))
        .route("/api/memory/archival", get(handlers::search_archival_memory))
        .route("/api/memory/conversation/history", get(handlers::conversation_history))
        // API — Knowledge Graph
        .route("/api/knowledge/nodes", get(handlers::get_knowledge_nodes))
        .route("/api/knowledge/nodes/{id}", get(handlers::get_knowledge_node))
        .route("/api/knowledge/nodes/{id}/neighbors", get(handlers::get_knowledge_neighbors))
        .route("/api/knowledge/search", get(handlers::search_knowledge))
        .route("/api/knowledge/stats", get(handlers::get_knowledge_stats))
        // API — Tools
        .route("/api/tools", get(handlers::list_tools))
        // API — Chat
        .route("/api/chat", post(handlers::send_chat_message))
        // API — Skills & Credentials
        .route("/api/skills", get(handlers::list_skills))
        .route("/api/skills/import", post(handlers::import_skill))
        .route("/api/skills/{name}", delete(handlers::delete_skill))
        .route("/api/skills/{name}/credentials", get(handlers::get_skill_credentials))
        .route("/api/skills/{name}/credentials", put(handlers::set_skill_credential))
        .route("/api/skills/{name}/credentials/{key}", delete(handlers::delete_skill_credential))
        .route("/api/skills/{name}/stop", post(handlers::stop_skill))
        .route("/api/skills/{name}/start", post(handlers::start_skill))
        .route("/api/skills/{name}/restart", post(handlers::restart_skill))
        .route("/api/skills/{name}/detail", get(handlers::get_skill_detail))
        .route("/api/skills/{name}/log", get(handlers::get_skill_log))
        .route("/api/skills/{name}/manifest", put(handlers::update_skill_manifest))
        .route("/api/skills/{name}/enabled", put(handlers::set_skill_enabled))
        .route("/api/skills/{name}/env", put(handlers::set_skill_env_var))
        .route("/api/skills/{name}/env/{key}", delete(handlers::delete_skill_env_var))
        // OAuth — generic multi-provider (start/callback exempt from auth in auth.rs)
        .route("/oauth/{provider}/start", get(oauth::oauth_start))
        .route("/oauth/{provider}/callback", get(oauth::oauth_callback))
        .route("/api/oauth/status", get(oauth::all_oauth_status))
        .route("/api/oauth/providers", get(oauth::list_providers))
        .route("/api/oauth/{provider}/refresh", post(oauth::oauth_refresh))
        .route("/api/oauth/{provider}/disconnect/{account}", post(oauth::oauth_disconnect))
        // API — Skill Extensions (Rhai routes + static files)
        .route("/api/skills/extensions", get(skill_ext::list_extensions))
        .route("/api/skills/{name}/ext/{*path}", any(skill_ext::skill_ext_handler))
        .route("/skills/{name}/ui/{*path}", get(skill_ext::skill_static_file))
        .route("/skills/{name}/page", get(skill_ext::skill_page))
        // API — Messaging (webhook + WhatsApp QR + config)
        .route("/api/messaging/incoming", post(messaging_webhook::incoming))
        .route("/api/messaging/config", get(messaging_webhook::messaging_config))
        .route("/api/messaging/whatsapp/status", get(messaging_webhook::whatsapp_status))
        .route("/api/messaging/whatsapp/qr", get(messaging_webhook::whatsapp_qr))
        .route("/api/messaging/platforms", get(messaging_webhook::list_platforms))
        .route("/api/messaging/twilio/incoming", post(messaging_webhook::twilio_incoming))
        // API — Goals
        .route("/api/goals", get(handlers::list_goals))
        .route("/api/goals/{id}", get(handlers::get_goal))
        .route("/api/goals/{id}/status", put(handlers::update_goal_status))
        // API — Trash
        .route("/api/trash", get(handlers::list_trash))
        .route("/api/trash/stats", get(handlers::trash_stats))
        .route("/api/trash/empty", post(handlers::empty_trash))
        .route("/api/trash/{id}/restore", post(handlers::restore_trash))
        .route("/api/trash/{id}", delete(handlers::permanent_delete_trash))
        // API — Security: Audit Trail
        .route("/api/security/audit", get(handlers::get_audit_log))
        .route("/api/security/audit/summary", get(handlers::get_audit_summary))
        .route("/api/security/audit/{id}/explain", get(handlers::explain_action))
        // API — Security: Cost Tracking
        .route("/api/security/cost", get(handlers::get_cost_summary))
        .route("/api/security/cost/recent", get(handlers::get_cost_recent))
        // API — Security: Rate Limiting
        .route("/api/security/rate-limit", get(handlers::get_rate_limit_status))
        // API — Security: 2FA
        .route("/api/security/2fa", get(handlers::get_2fa_challenges))
        .route("/api/security/2fa/{id}/confirm", post(handlers::confirm_2fa))
        .route("/api/security/2fa/{id}/reject", post(handlers::reject_2fa))
        // API — Security: Overview
        .route("/api/security/overview", get(handlers::get_security_overview))
        // API — Tool Events (streaming progress)
        .route("/api/tool-events", get(handlers::get_tool_events))
        // API — Tunnel
        .route("/api/tunnel/status", get(handlers::tunnel_status))
        // API — Binaries (install/uninstall tool binaries)
        .route("/api/binaries", get(super::binaries::list_binaries))
        .route("/api/binaries/{name}", get(super::binaries::get_binary))
        .route("/api/binaries/{name}", post(super::binaries::install_binary))
        .route("/api/binaries/{name}", delete(super::binaries::uninstall_binary))
        // API — Backup & Restore
        .route("/api/backup", get(handlers::create_backup))
        .route("/api/restore", post(handlers::restore_backup))
        // API — Updates
        .route("/api/update/check", get(handlers::check_update))
        .route("/api/update/apply", post(handlers::trigger_update))
        // API — Users (multi-user management)
        .route("/api/users", get(handlers::list_users))
        .route("/api/users", post(handlers::create_user))
        .route("/api/users/{id}", get(handlers::get_user))
        .route("/api/users/{id}", put(handlers::update_user))
        .route("/api/users/{id}", delete(handlers::delete_user))
        // API — Timezone & Locale
        .route("/api/timezone", get(handlers::get_timezone))
        .route("/api/timezone", post(handlers::set_timezone))
        .route("/api/timezones", get(handlers::list_timezones))
        .route("/api/timezone/convert", get(handlers::convert_time))
        // API — LLM Backends (plugin architecture)
        .route("/api/llm/backends", get(handlers::llm_backends))
        // API — LLM Advisor & Ollama Management
        .route("/api/llm/advisor/system", get(handlers::llm_system_specs))
        .route("/api/llm/advisor/recommend", get(handlers::llm_recommend))
        .route("/api/llm/ollama/status", get(handlers::ollama_status))
        .route("/api/llm/ollama/pull", post(handlers::ollama_pull))
        .route("/api/llm/ollama/models/{tag}", delete(handlers::ollama_delete))
        .route("/api/llm/ollama/configure", post(handlers::ollama_configure))
        // API — Federation
        .route("/api/federation/status", get(handlers::federation_status))
        .route("/api/federation/peers", get(handlers::federation_peers))
        .route("/api/federation/peers", post(handlers::federation_add_peer))
        .route("/api/federation/peers/{id}", delete(handlers::federation_remove_peer))
        // API — Webhook Tokens (management)
        .route("/api/tokens", get(webhook::list_tokens))
        .route("/api/tokens", post(webhook::create_token))
        .route("/api/tokens/{id}", put(webhook::update_token))
        .route("/api/tokens/{id}", delete(webhook::delete_token))
        // SSE
        .route("/api/events", get(sse::events))
        // Auth middleware — applied to all routes above
        .layer(middleware::from_fn_with_state(state.clone(), auth::require_auth))
        // Unauthenticated endpoints — below auth layer
        // Generic webhook (self-authenticating via token in URL path)
        .route("/api/webhook/{token}", post(webhook::webhook_handler))
        .route("/healthz", get(handlers::healthz))
        // Onboarding wizard — exempt from auth so the wizard works before any user exists
        .route("/api/onboarding/status", get(handlers::onboarding_status))
        .route("/api/onboarding/complete", post(handlers::onboarding_complete))
        .route("/api/onboarding/test-llm", post(handlers::onboarding_test_llm))
        .route("/api/onboarding/save-config", post(handlers::onboarding_save_config))
        .route("/api/persona", get(handlers::get_persona))
        .route("/api/persona", put(handlers::update_persona))
        .route("/metrics", get(handlers::metrics))
        .route("/api/federation/sync", post(handlers::federation_receive_sync))
        .route("/api/federation/heartbeat", post(handlers::federation_receive_heartbeat))
        .route("/api/federation/claim", post(handlers::federation_receive_claim))
        .with_state(state))
}

async fn serve_index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("ui/index.html"))
}

async fn serve_css() -> (axum::http::HeaderMap, &'static str) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/css".parse().unwrap(),
    );
    (headers, include_str!("ui/style.css"))
}

async fn serve_js() -> (axum::http::HeaderMap, &'static str) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/javascript".parse().unwrap(),
    );
    (headers, include_str!("ui/app.js"))
}

async fn serve_manifest() -> (axum::http::HeaderMap, &'static str) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/manifest+json".parse().unwrap(),
    );
    (headers, include_str!("pwa/manifest.json"))
}

async fn serve_sw() -> (axum::http::HeaderMap, &'static str) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/javascript".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("service-worker-allowed"),
        "/".parse().unwrap(),
    );
    (headers, include_str!("pwa/sw.js"))
}

async fn serve_icon_192() -> (axum::http::HeaderMap, &'static [u8]) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/png".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "public, max-age=604800".parse().unwrap(),
    );
    (headers, include_bytes!("pwa/icon-192.png"))
}

async fn serve_icon_512() -> (axum::http::HeaderMap, &'static [u8]) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "image/png".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "public, max-age=604800".parse().unwrap(),
    );
    (headers, include_bytes!("pwa/icon-512.png"))
}
