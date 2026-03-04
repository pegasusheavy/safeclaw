mod acme;
mod agent;
mod approval;
mod bridge;
mod config;
mod crypto;
mod dashboard;
mod db;
mod error;
mod federation;
mod goals;
mod installer;
mod llm;
mod memory;
mod messaging;
mod security;
mod skills;
mod tools;
mod trash;
mod tunnel;
mod users;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::config::Config;
use crate::security::SandboxedFs;
use crate::tools::ToolRegistry;

#[tokio::main]
async fn main() {
    // Load .env file (if present) before anything reads env vars
    dotenvy::dotenv().ok();

    // Parse CLI arguments
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    if args.iter().any(|a| a == "--default-config") {
        print!("{}", Config::default_config_contents());
        return;
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load config
    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let config = match Config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config: {e}");
            return;
        }
    };

    info!(
        agent_name = %config.agent_name,
        dashboard = %config.dashboard_bind,
        tick_interval = config.tick_interval_secs,
        "safeclaw starting"
    );

    // Set up sandboxed filesystem
    let data_dir = Config::data_dir();
    let sandbox = match SandboxedFs::new(data_dir.clone()) {
        Ok(s) => s,
        Err(e) => {
            error!("failed to initialize sandbox: {e}");
            return;
        }
    };
    info!(root = %sandbox.root().display(), "sandbox initialized");

    // Apply kernel-level Landlock filesystem sandbox (Linux 5.13+).
    // Skipped when NO_JAIL=1 — the container/deployment already provides
    // isolation so the extra restriction just blocks legitimate binaries.
    if std::env::var("NO_JAIL").as_deref() == Ok("1") {
        info!("landlock sandbox skipped (NO_JAIL=1)");
    } else {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".config"))
            .join("safeclaw");
        match crate::security::apply_landlock(&data_dir, &config_dir) {
            Ok(()) => {}
            Err(e) => warn!("landlock sandbox not applied: {e}"),
        }
    }

    // Initialize trash system
    let trash = match trash::TrashManager::new(&data_dir) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            error!("failed to initialize trash system: {e}");
            return;
        }
    };
    info!(bin_dir = %trash.bin_dir().display(), "trash system initialized");

    // Set up binary installer (user-space tool management via dashboard)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/safeclaw".to_string());
    let local_bin = std::path::PathBuf::from(&home).join(".local/bin");
    let installer = installer::BinaryInstaller::new(local_bin.clone(), &data_dir);

    // Ensure ~/.local/bin exists and prepend it to PATH so user-installed
    // binaries are found by tunnel providers and tool execution.
    if let Err(e) = installer.ensure_install_dir() {
        warn!("could not create ~/.local/bin: {e}");
    } else {
        let current_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{current_path}", local_bin.display());
        // SAFETY: Called during single-threaded startup before any tool
        // spawning or tunnel processes.
        unsafe { std::env::set_var("PATH", &new_path); }
        info!(path = %local_bin.display(), "prepended ~/.local/bin to PATH");
    }

    // Ensure CLAUDE.md exists in the data directory for the Claude CLI backend.
    // This writes/updates the system-managed rules section while preserving any
    // user-added rules below the end marker.
    ensure_claude_md(&data_dir);

    // Open database
    let db_path = sandbox.root().join("safeclaw.db");
    let db = match db::open(&db_path) {
        Ok(d) => d,
        Err(e) => {
            error!("failed to open database: {e}");
            return;
        }
    };
    let db = Arc::new(Mutex::new(db));

    // Handle --check
    if args.iter().any(|a| a == "--check") {
        run_checks(&config, &sandbox).await;
        return;
    }

    // Build the tool registry
    #[allow(unused_mut)]
    let mut tool_registry = build_tool_registry(&config, &data_dir);

    // Connect to MCP servers (daimon bridge)
    #[cfg(feature = "daimon")]
    {
        if !config.mcp.servers.is_empty() {
            info!(
                servers = config.mcp.servers.len(),
                "connecting to MCP servers via daimon bridge"
            );
            let mcp_mgr =
                bridge::McpManager::connect_all(&config.mcp.servers).await;
            info!(
                servers = mcp_mgr.server_count(),
                tools = mcp_mgr.total_tools(),
                "MCP bridge ready"
            );
            mcp_mgr.register_tools(&mut tool_registry);
        }
    }

    info!(tools = tool_registry.len(), "tool registry initialized");

    // Shutdown signal
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // ----- Build the MessagingManager -----
    let mut msg_manager = messaging::MessagingManager::new();

    // Register Telegram backend (if enabled)
    let telegram_backend: Option<Arc<messaging::telegram::TelegramBackend>> =
        if config.telegram.enabled {
            match config::Config::telegram_bot_token() {
                Ok(token) => {
                    let bot = teloxide::Bot::new(token);
                    let backend = Arc::new(messaging::telegram::TelegramBackend::new(bot));
                    let primary_channel = config
                        .telegram
                        .allowed_chat_ids
                        .first()
                        .map(|id| id.to_string())
                        .unwrap_or_default();
                    msg_manager.register(backend.clone(), primary_channel);
                    Some(backend)
                }
                Err(e) => {
                    error!("TELEGRAM_BOT_TOKEN not set: {e}");
                    None
                }
            }
        } else {
            None
        };

    // Register WhatsApp backend (if enabled)
    let whatsapp_backend: Option<Arc<messaging::whatsapp::WhatsAppBackend>> =
        if config.whatsapp.enabled {
            let backend = Arc::new(messaging::whatsapp::WhatsAppBackend::new(
                config.whatsapp.clone(),
            ));
            let primary_channel = config
                .whatsapp
                .allowed_numbers
                .first()
                .cloned()
                .unwrap_or_default();
            msg_manager.register(backend.clone(), primary_channel);
            Some(backend)
        } else {
            None
        };

    // Register iMessage bridge backend (if enabled)
    if config.imessage.enabled {
        let backend = Arc::new(messaging::bridge::BridgeBackend::new(
            "imessage".to_string(),
            config.imessage.bridge_url.clone(),
            20_000, // iMessage supports long messages
        ));
        let primary_channel = config
            .imessage
            .allowed_ids
            .first()
            .cloned()
            .unwrap_or_default();
        msg_manager.register(backend, primary_channel);
        info!("iMessage bridge backend registered");
    }

    // Register Twilio SMS backend (if enabled)
    if config.twilio.enabled {
        match config::Config::twilio_credentials() {
            Ok((sid, token)) => {
                let backend = Arc::new(messaging::twilio::TwilioBackend::new(
                    sid,
                    token,
                    config.twilio.from_number.clone(),
                ));
                let primary_channel = config
                    .twilio
                    .allowed_numbers
                    .first()
                    .cloned()
                    .unwrap_or_default();
                msg_manager.register(backend, primary_channel);
                info!("Twilio SMS backend registered");
            }
            Err(e) => {
                error!("Twilio credentials not set: {e}");
            }
        }
    }

    // Register Android SMS bridge backend (if enabled)
    if config.android_sms.enabled {
        let backend = Arc::new(messaging::bridge::BridgeBackend::new(
            "android_sms".to_string(),
            config.android_sms.bridge_url.clone(),
            160, // Standard SMS length
        ));
        let primary_channel = config
            .android_sms
            .allowed_ids
            .first()
            .cloned()
            .unwrap_or_default();
        msg_manager.register(backend, primary_channel);
        info!("Android SMS bridge backend registered");
    }

    // Register Discord backend (if enabled)
    if config.discord.enabled {
        match std::env::var("DISCORD_BOT_TOKEN") {
            Ok(token) => {
                let http = Arc::new(serenity::all::Http::new(&token));
                let backend = Arc::new(messaging::discord::DiscordBackend::new(http));
                let primary_channel = config
                    .discord
                    .allowed_channel_ids
                    .first()
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                msg_manager.register(backend, primary_channel);
                info!("Discord backend registered");
            }
            Err(_) => {
                error!("DISCORD_BOT_TOKEN not set but discord.enabled = true");
            }
        }
    }

    // Register Signal bridge backend (if enabled)
    if config.signal.enabled {
        let backend = Arc::new(messaging::signal::SignalBackend::new(
            config.signal.bridge_url.clone(),
        ));
        let primary_channel = config
            .signal
            .allowed_numbers
            .first()
            .cloned()
            .unwrap_or_default();
        msg_manager.register(backend, primary_channel);
        info!("Signal bridge backend registered");
    }

    let messaging = Arc::new(msg_manager);

    // Initialize PII encryption key (generated on first launch)
    let encryptor = match crypto::FieldEncryptor::ensure_key(&data_dir) {
        Ok(e) => e,
        Err(e) => {
            error!("failed to initialize PII encryption: {e}");
            return;
        }
    };

    // Build the agent
    let agent = match Agent::new(
        config.clone(),
        db.clone(),
        sandbox,
        tool_registry,
        messaging.clone(),
        trash.clone(),
        encryptor,
    )
    .await
    {
        Ok(a) => Arc::new(a),
        Err(e) => {
            error!("failed to initialize agent: {e}");
            return;
        }
    };

    // Migrate any existing plaintext PII to encrypted form
    if let Err(e) = agent.user_manager.migrate_encrypt_pii().await {
        warn!("PII migration warning: {e}");
    }

    // Start Telegram dispatcher (if enabled)
    let _telegram_shutdown = if let Some(ref tg_backend) = telegram_backend {
        match messaging::telegram::start(
            db.clone(),
            config.telegram.clone(),
            agent.clone(),
            tg_backend.clone(),
        )
        .await
        {
            Ok(tx) => {
                info!("telegram bot started");
                Some(tx)
            }
            Err(e) => {
                error!("failed to start telegram bot: {e}");
                None
            }
        }
    } else {
        None
    };

    // Start Discord gateway (if enabled)
    let _discord_shutdown = if config.discord.enabled
        && std::env::var("DISCORD_BOT_TOKEN").is_ok()
    {
        match messaging::discord::start(config.discord.clone(), agent.clone()).await {
            Ok(tx) => {
                info!("discord bot started");
                Some(tx)
            }
            Err(e) => {
                error!("failed to start discord bot: {e}");
                None
            }
        }
    } else {
        None
    };

    // Start WhatsApp bridge (if enabled)
    if let Some(ref wa_backend) = whatsapp_backend {
        let data_dir = config::Config::data_dir();
        if let Err(e) = wa_backend.start_bridge(data_dir).await {
            error!("failed to start whatsapp bridge: {e}");
        } else {
            info!("whatsapp bridge started");
        }
    }

    // Start ngrok tunnel (if enabled)
    let tunnel_url = if config.tunnel.enabled
        || std::env::var("NGROK_AUTHTOKEN").is_ok()
    {
        let dash_port = config
            .dashboard_bind
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(3030);

        let mgr = tunnel::TunnelManager::start(&config.tunnel, dash_port).await;
        let url = tunnel::shared_url(&mgr);

        // Set TUNNEL_URL in the current process so skills inherit it.
        // Also store the manager so it lives for the program's lifetime.
        let url_clone = url.clone();
        tokio::spawn(async move {
            let mut rx = mgr.url_receiver();
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                if let Some(ref u) = *rx.borrow() {
                    // SAFETY: We set these env vars before any skill
                    // processes are spawned and only from this single
                    // task, so there are no concurrent readers.
                    unsafe {
                        std::env::set_var("TUNNEL_URL", u);
                        std::env::set_var("PUBLIC_URL", u);
                    }
                    info!(public_url = %u, "TUNNEL_URL set");
                }
            }
            // Keep mgr alive so ngrok doesn't exit.
            drop(mgr);
        });

        Some(url_clone)
    } else {
        None
    };

    // Inject tunnel URL receiver into the agent so the skill manager can
    // forward it to skill environments.
    if let Some(ref turl) = tunnel_url {
        agent.set_tunnel_url(turl.clone()).await;
    }

    // Resolve ACME / TLS configuration.
    // If ACME is enabled, validate that the required fields are present.
    // If validation fails the process aborts — this is intentional so the
    // Docker container restarts with a clear error instead of running
    // without TLS silently.
    let tls_config = {
        let tls = acme::resolve_tls_config(&config);
        if tls.acme_enabled {
            if let Err(e) = acme::validate_acme_config(&tls) {
                error!("ACME configuration invalid — aborting: {e}");
                std::process::exit(1);
            }
            info!(
                domains = ?tls.acme_domains,
                production = tls.acme_production,
                port = tls.acme_port,
                "ACME TLS enabled"
            );
            Some(tls)
        } else {
            None
        }
    };

    // Start the dashboard
    let dashboard_handle = {
        let agent = agent.clone();
        let config = config.clone();
        let db = db.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let tls = tls_config.clone();
        let messaging_clone = messaging.clone();
        let trash_clone = trash.clone();
        let installer = installer.clone();
        tokio::spawn(async move {
            if let Err(e) = dashboard::serve(config, agent, db, shutdown_rx, tls, messaging_clone, trash_clone, installer).await {
                error!("dashboard error: {e}");
                // If the dashboard (ACME cert acquisition) fails, kill the
                // entire process so the container restarts.
                std::process::exit(1);
            }
        })
    };

    // Start the agent loop
    let agent_handle = {
        let agent = agent.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            agent.run(shutdown_rx).await;
        })
    };

    info!("safeclaw is running — press Ctrl+C to stop");

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl+c");

    info!("shutdown signal received, stopping...");
    let _ = shutdown_tx.send(());

    // Wait for tasks to finish
    let _ = tokio::join!(dashboard_handle, agent_handle);
    info!("safeclaw stopped");
}

/// Build the tool registry from config.
fn build_tool_registry(config: &Config, data_dir: &std::path::Path) -> ToolRegistry {
    use crate::tools::*;

    let mut registry = ToolRegistry::new();

    // Always register core tools
    if config.tools.exec.enabled {
        registry.register(Box::new(exec::ExecTool::new(config.tools.exec.timeout_secs)));
    }

    registry.register(Box::new(process::ProcessTool::new()));
    registry.register(Box::new(file::ReadFileTool));
    registry.register(Box::new(file::WriteFileTool));
    registry.register(Box::new(file::EditFileTool));
    registry.register(Box::new(file::DeleteFileTool));
    registry.register(Box::new(file::ApplyPatchTool));

    if config.tools.web.enabled {
        registry.register(Box::new(web::WebSearchTool::new(config.tools.web.max_results)));
        registry.register(Box::new(web::WebFetchTool));
    }

    if config.tools.browser.enabled {
        registry.register(Box::new(browser::BrowserTool::new(
            config.tools.browser.headless,
            data_dir.to_path_buf(),
        )));
    }

    if config.tools.message.enabled {
        registry.register(Box::new(message::MessageTool::new()));
    }

    if config.sessions.enabled {
        registry.register(Box::new(sessions::SessionsListTool));
        registry.register(Box::new(sessions::SessionsHistoryTool));
        registry.register(Box::new(sessions::SessionsSendTool));
        registry.register(Box::new(sessions::SessionsSpawnTool));
    }

    if config.tools.cron.enabled {
        registry.register(Box::new(cron::CronTool::new()));
    }

    registry.register(Box::new(goal::GoalTool::new()));
    registry.register(Box::new(image::ImageTool::new()));
    registry.register(Box::new(memory::MemorySearchTool));
    registry.register(Box::new(memory::MemoryGetTool));
    registry.register(Box::new(knowledge::KnowledgeGraphTool::new()));

    registry
}

async fn run_checks(config: &Config, _sandbox: &SandboxedFs) {
    info!("running pre-flight checks...");

    let backend = std::env::var("LLM_BACKEND")
        .unwrap_or_else(|_| config.llm.backend.clone());

    info!("config: OK");
    info!("  agent_name: {}", config.agent_name);
    info!("  dashboard_bind: {}", config.dashboard_bind);
    info!("  llm_backend: {}", backend);

    match backend.as_str() {
        "claude" => {
            info!("  model: {}", config.llm.model);

            // Check claude CLI is reachable
            match tokio::process::Command::new(&config.llm.claude_bin)
                .arg("--version")
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    let ver = String::from_utf8_lossy(&out.stdout);
                    info!("claude CLI: OK ({})", ver.trim());
                }
                Ok(out) => {
                    error!("claude CLI: exited with {}", out.status);
                }
                Err(e) => {
                    error!("claude CLI: NOT FOUND ({}): {e}", config.llm.claude_bin);
                }
            }
        }
        "codex" => {
            let codex_bin = std::env::var("CODEX_BIN")
                .unwrap_or_else(|_| config.llm.codex_bin.clone());

            match tokio::process::Command::new(&codex_bin)
                .arg("--version")
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    let ver = String::from_utf8_lossy(&out.stdout);
                    info!("codex CLI: OK ({})", ver.trim());
                }
                Ok(out) => {
                    error!("codex CLI: exited with {}", out.status);
                }
                Err(e) => {
                    error!("codex CLI: NOT FOUND ({}): {e}", codex_bin);
                }
            }

            match std::env::var("CODEX_API_KEY") {
                Ok(_) => info!("CODEX_API_KEY: set"),
                Err(_) => info!("CODEX_API_KEY: not set (will use saved auth)"),
            }
        }
        "gemini" => {
            let gemini_bin = std::env::var("GEMINI_BIN")
                .unwrap_or_else(|_| config.llm.gemini_bin.clone());

            match tokio::process::Command::new(&gemini_bin)
                .arg("--version")
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    let ver = String::from_utf8_lossy(&out.stdout);
                    info!("gemini CLI: OK ({})", ver.trim());
                }
                Ok(out) => {
                    error!("gemini CLI: exited with {}", out.status);
                }
                Err(e) => {
                    error!("gemini CLI: NOT FOUND ({}): {e}", gemini_bin);
                }
            }

            match std::env::var("GEMINI_API_KEY").or(std::env::var("GOOGLE_API_KEY")) {
                Ok(_) => info!("GEMINI_API_KEY / GOOGLE_API_KEY: set"),
                Err(_) => info!("GEMINI_API_KEY: not set (will use saved auth)"),
            }
        }
        "aider" => {
            let aider_bin = std::env::var("AIDER_BIN")
                .unwrap_or_else(|_| config.llm.aider_bin.clone());

            match tokio::process::Command::new(&aider_bin)
                .arg("--version")
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    let ver = String::from_utf8_lossy(&out.stdout);
                    info!("aider: OK ({})", ver.trim());
                }
                Ok(out) => {
                    error!("aider: exited with {}", out.status);
                }
                Err(e) => {
                    error!("aider: NOT FOUND ({}): {e}", aider_bin);
                }
            }
        }
        "local" => {
            let model_path = std::env::var("MODEL_PATH")
                .unwrap_or_else(|_| config.llm.model_path.clone());

            if model_path.is_empty() {
                error!("MODEL_PATH: NOT SET (required for local backend)");
            } else if std::path::Path::new(&model_path).exists() {
                info!("model file: OK ({})", model_path);
            } else {
                error!("model file: NOT FOUND ({})", model_path);
            }

            #[cfg(not(feature = "local"))]
            error!("binary compiled WITHOUT `local` feature — local backend unavailable");
            #[cfg(feature = "local")]
            info!("local feature: enabled");
        }
        other => {
            error!("unknown LLM backend: {other}");
        }
    }

    if config.telegram.enabled {
        match Config::telegram_bot_token() {
            Ok(_) => info!("TELEGRAM_BOT_TOKEN: set"),
            Err(_) => error!("TELEGRAM_BOT_TOKEN: NOT SET (telegram enabled)"),
        }
    }

    // Tunnel check
    let tunnel_enabled = config.tunnel.enabled || std::env::var("NGROK_AUTHTOKEN").is_ok();
    if tunnel_enabled {
        let provider = config.tunnel.provider.as_str();
        info!("tunnel provider: {provider}");

        match provider {
            "ngrok" => {
                let ngrok_bin = std::env::var("NGROK_BIN")
                    .unwrap_or_else(|_| config.tunnel.ngrok.bin.clone());

                match tokio::process::Command::new(&ngrok_bin)
                    .arg("version")
                    .output()
                    .await
                {
                    Ok(out) if out.status.success() => {
                        let ver = String::from_utf8_lossy(&out.stdout);
                        info!("ngrok: OK ({})", ver.trim());
                    }
                    Ok(out) => {
                        error!("ngrok: exited with {}", out.status);
                    }
                    Err(e) => {
                        error!("ngrok: NOT FOUND ({}): {e}", ngrok_bin);
                    }
                }

                match std::env::var("NGROK_AUTHTOKEN") {
                    Ok(_) => info!("NGROK_AUTHTOKEN: set"),
                    Err(_) => info!("NGROK_AUTHTOKEN: not set (ngrok will use saved auth)"),
                }
            }
            "cloudflare" => {
                let static_url = std::env::var("CLOUDFLARE_TUNNEL_URL").ok()
                    .filter(|u| !u.is_empty())
                    .or_else(|| {
                        if config.tunnel.cloudflare.url.is_empty() { None }
                        else { Some(config.tunnel.cloudflare.url.clone()) }
                    });

                if let Some(url) = static_url {
                    info!("cloudflare: static URL configured ({})", url);
                } else {
                    let cf_bin = std::env::var("CLOUDFLARED_BIN")
                        .unwrap_or_else(|_| config.tunnel.cloudflare.bin.clone());

                    match tokio::process::Command::new(&cf_bin)
                        .arg("version")
                        .output()
                        .await
                    {
                        Ok(out) if out.status.success() => {
                            let ver_stderr = String::from_utf8_lossy(&out.stderr);
                            let ver_stdout = String::from_utf8_lossy(&out.stdout);
                            let ver = if ver_stderr.trim().is_empty() {
                                ver_stdout
                            } else {
                                ver_stderr
                            };
                            info!("cloudflared: OK ({})", ver.trim());
                        }
                        Ok(out) => {
                            error!("cloudflared: exited with {}", out.status);
                        }
                        Err(e) => {
                            error!("cloudflared: NOT FOUND ({}): {e}", cf_bin);
                        }
                    }
                }
            }
            "tailscale" => {
                let static_url = std::env::var("TAILSCALE_TUNNEL_URL").ok()
                    .filter(|u| !u.is_empty())
                    .or_else(|| {
                        if config.tunnel.tailscale.url.is_empty() { None }
                        else { Some(config.tunnel.tailscale.url.clone()) }
                    });

                if let Some(url) = static_url {
                    info!("tailscale: static URL configured ({})", url);
                } else {
                    let ts_bin = std::env::var("TAILSCALE_BIN")
                        .unwrap_or_else(|_| config.tunnel.tailscale.bin.clone());

                    match tokio::process::Command::new(&ts_bin)
                        .arg("version")
                        .output()
                        .await
                    {
                        Ok(out) if out.status.success() => {
                            let ver = String::from_utf8_lossy(&out.stdout);
                            info!("tailscale: OK ({})", ver.trim());
                        }
                        Ok(out) => {
                            error!("tailscale: exited with {}", out.status);
                        }
                        Err(e) => {
                            error!("tailscale: NOT FOUND ({}): {e}", ts_bin);
                        }
                    }

                    info!("tailscale mode: {}", config.tunnel.tailscale.mode);
                }
            }
            other => {
                error!("unknown tunnel provider: {other}");
            }
        }
    } else {
        info!("tunnel: disabled");
    }

    // ACME check
    let tls = acme::resolve_tls_config(config);
    if tls.acme_enabled {
        info!("ACME TLS: enabled");
        info!("  domains: {:?}", tls.acme_domains);
        info!("  email: {}", tls.acme_email);
        info!("  environment: {}", if tls.acme_production { "production" } else { "staging" });
        info!("  port: {}", tls.acme_port);
        match acme::validate_acme_config(&tls) {
            Ok(()) => info!("ACME config: OK"),
            Err(e) => error!("ACME config: INVALID — {e}"),
        }
    } else {
        info!("ACME TLS: disabled");
    }

}

// ---------------------------------------------------------------------------
// CLAUDE.md management
// ---------------------------------------------------------------------------

/// Default system rules embedded from the repo's config/CLAUDE.md.
const CLAUDE_MD_SYSTEM_RULES: &str = include_str!("../config/CLAUDE.md");

const CLAUDE_MD_BEGIN: &str = "<!-- SAFE-AGENT SYSTEM RULES - DO NOT EDIT BELOW THIS LINE -->";
const CLAUDE_MD_END: &str = "<!-- END SAFE-AGENT SYSTEM RULES -->";

/// Write or update the CLAUDE.md in `data_dir`.
///
/// The file is split into two sections separated by markers:
///   1. **System rules** (between the markers) — always overwritten from the
///      embedded template so that new releases can add rules.
///   2. **User rules** (everything after the end marker) — preserved across
///      updates so operators can append custom instructions.
///
/// If the file does not exist yet, it is created with the system section only.
fn ensure_claude_md(data_dir: &std::path::Path) {
    let path = data_dir.join("CLAUDE.md");

    // Preserve any user-added content after the end marker.
    let user_section = if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(existing) => {
                if let Some(pos) = existing.find(CLAUDE_MD_END) {
                    let after = &existing[pos + CLAUDE_MD_END.len()..];
                    let trimmed = after.trim_start_matches('\n');
                    if trimmed.is_empty() {
                        String::new()
                    } else {
                        format!("\n{trimmed}")
                    }
                } else {
                    // No markers found — treat the entire existing file as user rules
                    // so we don't destroy manually written content.
                    format!("\n{}", existing.trim())
                }
            }
            Err(e) => {
                warn!("failed to read existing CLAUDE.md: {e}");
                String::new()
            }
        }
    } else {
        String::new()
    };

    let content = format!(
        "{CLAUDE_MD_BEGIN}\n{CLAUDE_MD_SYSTEM_RULES}\n{CLAUDE_MD_END}\n{user_section}"
    );

    match std::fs::write(&path, content.trim_end().to_owned() + "\n") {
        Ok(()) => info!(path = %path.display(), "CLAUDE.md updated"),
        Err(e) => warn!("failed to write CLAUDE.md: {e}"),
    }
}

fn print_usage() {
    println!(
        "safeclaw — sandboxed autonomous AI agent with tool execution

USAGE:
    safeclaw [OPTIONS]

OPTIONS:
    --config <PATH>     Path to config file (default: ~/.config/safeclaw/config.toml)
    --default-config    Print default config to stdout and exit
    --check             Validate config and connectivity, then exit
    -h, --help          Print this help message

LLM BACKEND:
    LLM_BACKEND           \"claude\" (default), \"codex\", \"gemini\", \"aider\", or \"local\"
    CLAUDE_BIN            Path to claude CLI binary (claude backend)
    CLAUDE_CONFIG_DIR     Claude profile directory (claude backend)
    CLAUDE_MODEL          Model name: sonnet, opus, haiku (claude backend)
    CODEX_BIN             Path to codex CLI binary (codex backend)
    CODEX_MODEL           Model name: gpt-5-codex, o3, etc. (codex backend)
    CODEX_PROFILE         Codex config profile name (codex backend)
    CODEX_API_KEY         OpenAI API key for Codex (codex backend, optional)
    GEMINI_BIN            Path to gemini CLI binary (gemini backend)
    GEMINI_MODEL          Model name: gemini-2.5-pro, etc. (gemini backend)
    GEMINI_API_KEY        Google AI Studio API key (gemini backend, optional)
    AIDER_BIN             Path to aider binary (aider backend)
    AIDER_MODEL           Model string: gpt-4o, claude-3.5-sonnet (aider backend)
    MODEL_PATH            Path to .gguf model file (local backend)

TLS / ACME (LET'S ENCRYPT):
    ACME_ENABLED          Set to \"true\" to enable automatic HTTPS certificates
    ACME_DOMAIN           Comma-separated domain(s) for the certificate
    ACME_EMAIL            Contact email for Let's Encrypt (required)
    ACME_PRODUCTION       \"true\" for production CA, \"false\" for staging (default)
    ACME_CACHE_DIR        Directory to cache certs (default: $data_dir/acme-cache)
    ACME_PORT             HTTPS listen port (default: 443)

TUNNEL:
    NGROK_AUTHTOKEN       Auth token from ngrok dashboard (auto-enables tunnel)
    NGROK_BIN             Path to ngrok binary (default: ngrok)
    NGROK_PORT            Override local port to tunnel (default: dashboard port)
    NGROK_DOMAIN          Static domain (e.g. myapp.ngrok-free.app)
    CLOUDFLARED_BIN       Path to cloudflared binary (default: cloudflared)
    CLOUDFLARE_TUNNEL_URL Static URL (skip spawning cloudflared)
    TAILSCALE_BIN         Path to tailscale binary (default: tailscale)
    TAILSCALE_TUNNEL_URL  Static URL (skip spawning tailscale)

ENVIRONMENT:
    DASHBOARD_PASSWORD    Required. Dashboard login password.
    JWT_SECRET            Required. Secret for signing dashboard JWT cookies.
    TELEGRAM_BOT_TOKEN    Required if Telegram is enabled.
    RUST_LOG              Optional. Tracing filter (default: info).
"
    );
}
