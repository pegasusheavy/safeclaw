pub mod actions;
pub mod cron_runner;
pub mod personas;
pub mod reasoning;
pub mod sessions;
pub mod tick;
pub mod tool_parse;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, error, info, warn};

use crate::approval::ApprovalQueue;
use crate::config::Config;
use crate::error::Result;
use crate::llm::LlmEngine;
use crate::memory::MemoryManager;
use crate::messaging::MessagingManager;
use crate::security::audit::AuditLogger;
use crate::security::capabilities::CapabilityChecker;
use crate::security::cost_tracker::CostTracker;
use crate::security::pii::PiiScanner;
use crate::security::rate_limiter::RateLimiter;
use crate::security::twofa::TwoFactorManager;
use crate::skills::{PluginRegistry, PromptSkill, SkillManager};
use crate::tools::{ToolContext, ToolRegistry};
use crate::trash::TrashManager;
use crate::tunnel::TunnelUrl;
use crate::federation::FederationManager;
use crate::security::SandboxedFs;
use crate::crypto::FieldEncryptor;
use crate::users::{UserContext, UserManager};

pub struct Agent {
    pub config: Config,
    /// Cached HashSet of auto-approve tool names (avoids rebuilding on every message).
    auto_approve: std::collections::HashSet<String>,
    pub memory: MemoryManager,
    pub approval_queue: ApprovalQueue,
    pub tools: ToolRegistry,
    pub llm: LlmEngine,
    pub ctx: ToolContext,
    pub skill_manager: Mutex<SkillManager>,
    /// Prompt skills loaded from all plugins at startup.  Read-only after
    /// construction — trigger matching borrows a filtered slice per message.
    pub prompt_skills: Vec<PromptSkill>,
    /// Pre-filtered subset of `prompt_skills` with no triggers (always-on).
    /// Used by background LLM calls (goals, self-reflection, approved actions)
    /// to avoid re-filtering and cloning every tick.
    always_on_skills: Vec<PromptSkill>,
    pub audit: AuditLogger,
    pub cost_tracker: CostTracker,
    pub rate_limiter: RateLimiter,
    pub capability_checker: CapabilityChecker,
    pub pii_scanner: PiiScanner,
    pub twofa: TwoFactorManager,
    pub federation: FederationManager,
    pub user_manager: UserManager,
    paused: AtomicBool,
    sse_tx: broadcast::Sender<String>,
    /// In-memory ring buffer of recent tool progress events for hydrating the
    /// dashboard on page reload.
    recent_events: Mutex<Vec<serde_json::Value>>,
}

const MAX_BUFFERED_EVENTS: usize = 50;

impl Agent {
    pub async fn new(
        config: Config,
        db: Arc<Mutex<Connection>>,
        db_read: Arc<Mutex<Connection>>,
        sandbox: SandboxedFs,
        tools: ToolRegistry,
        messaging: Arc<MessagingManager>,
        trash: Arc<TrashManager>,
        encryptor: Arc<FieldEncryptor>,
    ) -> Result<Self> {
        // Initialize memory (with optional embedding engine)
        let mut memory = MemoryManager::new(db.clone(), db_read.clone(), config.conversation_window);
        memory.init(&config.core_personality).await?;

        let embed_host = if config.memory.embedding_host.is_empty() {
            &config.llm.ollama_host
        } else {
            &config.memory.embedding_host
        };
        memory.init_embeddings(embed_host, &config.memory.embedding_model);

        // Initialize approval queue
        let approval_queue = ApprovalQueue::new(db.clone(), config.approval_expiry_secs);

        // Initialize LLM engine (Claude CLI or local GGUF)
        let llm = LlmEngine::new(&config)?;

        // Build tool context
        let http_client = reqwest::Client::builder()
            .user_agent("SafeClaw/0.1.2")
            .build()
            .unwrap_or_default();

        let ctx = ToolContext {
            sandbox: sandbox.clone(),
            db: db.clone(),
            db_read: db_read.clone(),
            http_client,
            messaging: messaging.clone(),
            trash,
        };

        // Initialize skill manager
        let skills_dir = sandbox.root().join("skills");
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
        let telegram_chat_id = messaging
            .primary_channel("telegram")
            .and_then(|s| s.parse::<i64>().ok());
        let mut skill_manager = SkillManager::new(skills_dir, bot_token, telegram_chat_id);

        // Initialize plugin registry and load prompt skills + subprocess dirs
        let prompt_skills = {
            let mut registry = PluginRegistry::new(config.plugins.disabled.clone());

            let global_plugins_dir = if config.plugins.global_dir.is_empty() {
                dirs::config_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from(".config"))
                    .join("safeclaw")
                    .join("plugins")
            } else {
                std::path::PathBuf::from(&config.plugins.global_dir)
            };

            let project_plugins_dir = if config.plugins.project_dir.is_empty() {
                std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                    .join(".safeclaw")
                    .join("plugins")
            } else {
                std::path::PathBuf::from(&config.plugins.project_dir)
            };

            let loaded_global = registry.scan_dir(&global_plugins_dir).unwrap_or_else(|e| {
                warn!("failed to scan global plugin dir: {e}");
                0
            });
            let loaded_project = registry.scan_dir(&project_plugins_dir).unwrap_or_else(|e| {
                warn!("failed to scan project plugin dir: {e}");
                0
            });

            info!(
                global = loaded_global,
                project = loaded_project,
                total = registry.len(),
                prompt_skills = registry.all_prompt_skills().len(),
                subprocess_dirs = registry.all_subprocess_skill_dirs().len(),
                "plugins loaded"
            );

            // Wire subprocess skill dirs into SkillManager
            for dir in registry.all_subprocess_skill_dirs() {
                skill_manager.add_skill_dir(dir.to_path_buf());
            }

            // Extract prompt skills as owned values (registry is consumed)
            registry
                .all_prompt_skills()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };

        // Pre-compute always-on skills once to avoid per-tick cloning
        let always_on_skills: Vec<PromptSkill> = crate::skills::always_on_skills(&prompt_skills)
            .into_iter()
            .cloned()
            .collect();

        // Security subsystems
        let audit = AuditLogger::new(db.clone());
        let cost_tracker = CostTracker::new(db.clone(), config.security.daily_cost_limit_usd);
        let rate_limiter = RateLimiter::new(
            config.security.rate_limit_per_minute,
            config.security.rate_limit_per_hour,
        );
        let capability_checker = CapabilityChecker::new(&config.security);
        let pii_scanner = PiiScanner::new(config.security.pii_detection);
        let twofa = TwoFactorManager::new(config.security.require_2fa.clone());

        // SSE broadcast channel
        let (sse_tx, _) = broadcast::channel(64);

        // Federation
        let fed_name = if config.federation.node_name.is_empty() {
            &config.agent_name
        } else {
            &config.federation.node_name
        };
        let fed_addr = if config.federation.advertise_address.is_empty() {
            format!("http://{}", config.dashboard_bind)
        } else {
            config.federation.advertise_address.clone()
        };
        let federation = FederationManager::new(fed_name, &fed_addr, config.federation.enabled);

        // User management
        let user_manager = UserManager::new(db.clone(), encryptor);

        // Cache auto_approve tools as HashSet for O(1) lookup per message
        let auto_approve: std::collections::HashSet<String> = config
            .auto_approve_tools
            .iter()
            .cloned()
            .collect();

        Ok(Self {
            config,
            auto_approve,
            memory,
            approval_queue,
            tools,
            llm,
            ctx,
            skill_manager: Mutex::new(skill_manager),
            prompt_skills,
            always_on_skills,
            audit,
            cost_tracker,
            rate_limiter,
            capability_checker,
            pii_scanner,
            twofa,
            federation,
            user_manager,
            paused: AtomicBool::new(false),
            sse_tx,
            recent_events: Mutex::new(Vec::with_capacity(MAX_BUFFERED_EVENTS)),
        })
    }

    /// Run the agent loop until shutdown.
    pub async fn run(&self, mut shutdown: broadcast::Receiver<()>) {
        let tick_interval = tokio::time::Duration::from_secs(self.config.tick_interval_secs);

        info!(interval_secs = self.config.tick_interval_secs, "agent loop starting");

        // Initial skill reconciliation on startup
        {
            let mut sm = self.skill_manager.lock().await;
            if let Err(e) = sm.reconcile().await {
                error!("initial skill reconciliation failed: {e}");
            }
        }

        loop {
            // Execute any approved actions first
            if let Err(e) = self.execute_approved().await {
                error!("error executing approved actions: {e}");
            }

            // Run a tick if not paused
            if !self.is_paused() {
                if let Err(e) = self.tick().await {
                    error!("tick error: {e}");
                    self.memory
                        .log_activity("tick", "tick failed", Some(&e.to_string()), "error")
                        .await
                        .ok();
                }
            }

            // Reconcile skills every tick
            {
                let mut sm = self.skill_manager.lock().await;
                if let Err(e) = sm.reconcile().await {
                    error!("skill reconciliation failed: {e}");
                }
            }

            // Wait for tick interval or shutdown
            tokio::select! {
                _ = tokio::time::sleep(tick_interval) => {}
                _ = shutdown.recv() => {
                    info!("agent loop shutting down");
                    break;
                }
            }
        }

        // Shut down all skills
        {
            let mut sm = self.skill_manager.lock().await;
            sm.shutdown().await;
        }
    }

    /// Force an immediate tick (from dashboard or Telegram).
    pub async fn force_tick(&self) -> Result<()> {
        self.tick().await
    }

    /// Handle a message with an explicit user context (multi-user mode).
    /// If `user_ctx` is None, the message is treated as coming from the
    /// default/system user (backward-compatible single-user mode).
    pub async fn handle_message_as(&self, user_message: &str, user_ctx: Option<&UserContext>) -> Result<String> {
        // Permission check: viewers cannot send messages
        if let Some(ctx) = user_ctx {
            if !ctx.role.can_chat() {
                return Err(crate::error::SafeAgentError::PermissionDenied(
                    format!("user '{}' (role: {}) is not allowed to send messages", ctx.username, ctx.role),
                ));
            }
            // Update last_seen_at
            self.user_manager.touch(&ctx.user_id).await;
        }

        let user_id = user_ctx.map(|c| c.user_id.as_str());

        // Store the user message in conversation history
        self.memory
            .conversation
            .append_with_user("user", user_message, user_id)
            .await?;

        let max_turns = self.config.max_tool_turns;

        // Build the initial context: the user's message plus recent conversation
        let mut context = self.build_llm_context(user_message).await;
        let mut final_text = String::new();

        // Resolve which prompt skills to inject for this user message.
        // Skills without triggers are always-on; others match by phrase.
        let active_skills: Vec<PromptSkill> = crate::skills::resolve_skills(&self.prompt_skills, user_message)
            .into_iter()
            .cloned()
            .collect();

        for turn in 0..max_turns {
            debug!(turn, "tool-call loop iteration");

            // Emit "thinking" event — LLM is generating
            self.emit_event(serde_json::json!({
                "type": "thinking",
                "turn": turn,
                "max_turns": max_turns,
            }));

            // Send typing indicators to messaging platforms
            self.ctx.messaging.typing_all().await;

            // Call the LLM with tool schemas and active prompt skills
            let gen_ctx = crate::llm::GenerateContext {
                message: &context,
                tools: Some(&self.tools),
                prompt_skills: &active_skills,
            };
            let raw_response = self.llm.generate(&gen_ctx).await?;

            // Parse tool_call blocks from the response
            let parsed = tool_parse::parse_llm_response(&raw_response);

            // If no tool calls, this is the final reply
            if parsed.tool_calls.is_empty() {
                final_text = parsed.text;
                self.emit_event(serde_json::json!({
                    "type": "turn_complete",
                    "turn": turn,
                    "turns_used": turn + 1,
                    "has_reply": true,
                    "tool_calls_total": 0,
                }));
                break;
            }

            info!(
                turn,
                num_tool_calls = parsed.tool_calls.len(),
                "LLM proposed tool calls"
            );

            // Collect results from auto-approved tools
            let mut tool_results: Vec<String> = Vec::new();
            let mut pending_approvals: Vec<String> = Vec::new();

            for call in &parsed.tool_calls {
                // --- Security gate: blocked tools / capability check ---
                if self.capability_checker.is_blocked(&call.tool) {
                    let msg = format!("tool '{}' is blocked by security policy", call.tool);
                    self.audit.log_permission_denied(&call.tool, &msg, "agent").await;
                    tool_results.push(format!(
                        "[Tool result: {} (blocked)]\n{}",
                        call.tool, msg
                    ));
                    self.emit_event(serde_json::json!({
                        "type": "tool_blocked",
                        "tool": call.tool,
                        "reason": msg,
                        "turn": turn,
                    }));
                    continue;
                }

                if let Err(e) = self.capability_checker.check_or_error(&call.tool, &call.params) {
                    let msg = e.to_string();
                    self.audit.log_permission_denied(&call.tool, &msg, "agent").await;
                    tool_results.push(format!(
                        "[Tool result: {} (capability denied)]\n{}",
                        call.tool, msg
                    ));
                    continue;
                }

                // --- Security gate: rate limiter ---
                if let Err(e) = self.rate_limiter.check_and_record() {
                    let msg = e.to_string();
                    self.audit.log_rate_limit(&call.tool, "agent").await;
                    tool_results.push(format!(
                        "[Tool result: {} (rate limited)]\n{}",
                        call.tool, msg
                    ));
                    self.emit_event(serde_json::json!({
                        "type": "rate_limited",
                        "tool": call.tool,
                        "turn": turn,
                    }));
                    continue;
                }

                if self.auto_approve.contains(call.tool.as_str()) {
                    // --- Security gate: 2FA for dangerous auto-approved tools ---
                    if self.twofa.requires_2fa(&call.tool) {
                        use crate::security::twofa::TwoFactorVerdict;
                        match self.twofa.check(&call.tool, &call.params, &call.reasoning, "agent") {
                            TwoFactorVerdict::NotRequired => {
                                // Should not happen since we checked requires_2fa above
                            }
                            TwoFactorVerdict::ChallengeCreated(id) => {
                                self.audit.log_2fa(&call.tool, "challenge_created", "agent").await;
                                pending_approvals.push(format!(
                                    "{} (2FA required, challenge {}): {}",
                                    call.tool, id, call.reasoning
                                ));
                                self.emit_event(serde_json::json!({
                                    "type": "2fa_challenge",
                                    "tool": call.tool,
                                    "challenge_id": id,
                                    "reasoning": call.reasoning,
                                    "turn": turn,
                                }));
                                continue;
                            }
                            TwoFactorVerdict::Confirmed => {
                                self.audit.log_2fa(&call.tool, "confirmed", "agent").await;
                                // Fall through to execute
                            }
                        }
                    }

                    // Emit "tool_start" event
                    self.emit_event(serde_json::json!({
                        "type": "tool_start",
                        "tool": call.tool,
                        "reasoning": call.reasoning,
                        "auto_approved": true,
                        "turn": turn,
                    }));

                    // Send typing indicator while executing
                    self.ctx.messaging.typing_all().await;

                    // Auto-approve: execute immediately
                    debug!(tool = %call.tool, "auto-executing tool call");
                    match actions::execute_tool_call(&self.tools, &self.ctx, call).await {
                        Ok(output) => {
                            let status = if output.success { "success" } else { "error" };
                            let preview = truncate_preview(&output.output, 200);

                            // Audit trail
                            self.audit.log_tool_call(
                                &call.tool, &call.params, &preview, output.success,
                                "agent", &call.reasoning, user_message,
                            ).await;

                            tool_results.push(format!(
                                "[Tool result: {} ({})]\n{}",
                                call.tool, status, output.output
                            ));
                            info!(
                                tool = %call.tool,
                                success = output.success,
                                output_len = output.output.len(),
                                "auto-executed tool call"
                            );

                            // Emit "tool_result" event
                            self.emit_event(serde_json::json!({
                                "type": "tool_result",
                                "tool": call.tool,
                                "success": output.success,
                                "output_preview": preview,
                                "turn": turn,
                            }));
                        }
                        Err(e) => {
                            let err_str = e.to_string();
                            let preview = truncate_preview(&err_str, 200);
                            self.audit.log_tool_call(
                                &call.tool, &call.params, &preview, false,
                                "agent", &call.reasoning, user_message,
                            ).await;

                            tool_results.push(format!(
                                "[Tool result: {} (error)]\n{}",
                                call.tool, err_str
                            ));
                            warn!(tool = %call.tool, err = %err_str, "auto-executed tool call failed");

                            // Emit "tool_result" event with error
                            self.emit_event(serde_json::json!({
                                "type": "tool_result",
                                "tool": call.tool,
                                "success": false,
                                "output_preview": preview,
                                "turn": turn,
                            }));
                        }
                    }
                } else {
                    // Needs human approval: propose to queue
                    let action_json = serde_json::json!({
                        "tool": call.tool,
                        "params": call.params,
                        "reasoning": call.reasoning,
                    });
                    match self
                        .approval_queue
                        .propose(action_json, &call.reasoning, user_message)
                        .await
                    {
                        Ok(id) => {
                            self.audit.log_approval(&call.tool, "propose", &call.reasoning, "agent").await;
                            info!(tool = %call.tool, id = %id, "proposed tool call for approval");
                            pending_approvals.push(format!(
                                "{} ({}): {}",
                                call.tool, id, call.reasoning
                            ));

                            // Emit "approval_needed" event
                            self.emit_event(serde_json::json!({
                                "type": "approval_needed",
                                "tool": call.tool,
                                "id": id,
                                "reasoning": call.reasoning,
                                "turn": turn,
                            }));
                        }
                        Err(e) => {
                            error!(tool = %call.tool, err = %e, "failed to propose tool call");
                        }
                    }
                }
            }

            // If we got tool results from auto-executed calls, feed them back
            if !tool_results.is_empty() {
                let results_block = tool_results.join("\n\n");
                context = format!(
                    "{context}\n\nAssistant: {text}\n\n{results_block}\n\nContinue with the results above. \
                     Give the user a final natural-language answer.",
                    text = parsed.text,
                );

                // If there are also pending approvals, include that info
                if !pending_approvals.is_empty() {
                    let approval_note = format!(
                        "\n\nNote: The following tool calls are awaiting human approval:\n{}",
                        pending_approvals.join("\n")
                    );
                    context.push_str(&approval_note);
                }

                // Continue the loop — the LLM gets another turn with results
                continue;
            }

            // All tool calls need approval — return now with a partial reply
            let mut reply_parts = Vec::new();
            if !parsed.text.is_empty() {
                reply_parts.push(parsed.text);
            }
            reply_parts.push(format!(
                "I need approval to proceed. The following actions are waiting for your approval:\n{}",
                pending_approvals.join("\n")
            ));
            final_text = reply_parts.join("\n\n");

            self.emit_event(serde_json::json!({
                "type": "turn_complete",
                "turn": turn,
                "turns_used": turn + 1,
                "has_reply": true,
                "pending_approvals": pending_approvals.len(),
                "tool_calls_total": parsed.tool_calls.len(),
            }));
            break;
        }

        // If we exhausted max_turns without a clean finish, use whatever we have
        if final_text.is_empty() {
            final_text = "I ran out of tool-call turns. Here's what I have so far — please let me know if you need more.".to_string();
            self.emit_event(serde_json::json!({
                "type": "turn_complete",
                "turns_used": max_turns,
                "has_reply": false,
                "exhausted": true,
                "tool_calls_total": 0,
            }));
        }

        // PII detection: scan the final response before sending
        let pii_detections = self.pii_scanner.scan(&final_text);
        if !pii_detections.is_empty() {
            let categories: Vec<String> = pii_detections.iter().map(|d| d.category.to_string()).collect();
            warn!(
                count = pii_detections.len(),
                categories = %categories.join(", "),
                "PII detected in LLM response — flagging"
            );
            self.audit.log_pii_detected(
                &format!("{} sensitive item(s): {}", pii_detections.len(), categories.join(", ")),
                "flag",
                "agent",
            ).await;

            // Prepend a warning to the response
            final_text = format!(
                "⚠️ **Sensitive data warning**: This response may contain {}. \
                 Please review before sharing.\n\n{}",
                categories.join(", "),
                final_text,
            );
        }

        // Store the assistant reply
        self.memory
            .conversation
            .append("assistant", &final_text)
            .await?;

        // Reconcile skills after every message so newly created or deleted
        // skills are picked up immediately instead of waiting for the next tick.
        {
            let mut sm = self.skill_manager.lock().await;
            if let Err(e) = sm.reconcile().await {
                error!("skill reconciliation after message failed: {e}");
            }
        }

        // Record the action
        self.memory.record_action().await?;
        self.memory
            .log_activity("message", "llm reply", Some(&final_text), "ok")
            .await?;
        self.notify_update();

        // Post-conversation memory enrichment:
        // - Record episodic memory (always)
        // - Run LLM extraction pipeline (if auto_extract is enabled)
        {
            let episode_summary = truncate_preview(&context, 200);
            if let Err(e) = self.memory.episodic.record(
                "user_message",
                &episode_summary,
                &[],
                "completed",
                user_id,
            ).await {
                warn!(err = %e, "failed to record episode");
            }
        }

        if self.config.memory.auto_extract {
            let extraction_fut = crate::memory::extraction::extract_from_conversation(
                self.memory.db(),
                &self.llm,
                &context,
                user_id,
                &[],
            );
            // Timeout prevents a slow LLM from blocking indefinitely
            if tokio::time::timeout(
                std::time::Duration::from_secs(30),
                extraction_fut,
            ).await.is_err() {
                warn!("memory extraction timed out (30s)");
            }
        }

        Ok(final_text)
    }

    /// Build the context string sent to the LLM.
    ///
    /// Includes: user profile, relevant archival memories, recent conversation,
    /// and the current message.
    async fn build_llm_context(&self, user_message: &str) -> String {
        let mut ctx = String::new();

        // Inject user profile if available
        if let Ok(profile) = self.memory.user_model.as_context_string(None).await {
            if !profile.is_empty() {
                ctx.push_str(&profile);
                ctx.push('\n');
            }
        }

        // Inject relevant archival memories (semantic search if embeddings available)
        if let Ok(memories) = self.memory.semantic_search_archival(user_message, 3).await {
            if !memories.is_empty() {
                ctx.push_str("== RELEVANT MEMORIES ==\n");
                for mem in &memories {
                    ctx.push_str(&format!("- {}\n", mem.content));
                }
                ctx.push('\n');
            }
        }

        // Recent conversation history
        if let Ok(messages) = self.memory.conversation.recent().await {
            if !messages.is_empty() {
                for msg in &messages {
                    ctx.push_str(&format!("{}: {}\n", capitalize(&msg.role), msg.content));
                }
            }
        }

        ctx.push_str(&format!("User: {}", user_message));
        ctx
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
        info!("agent paused");
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
        info!("agent resumed");
    }

    /// Subscribe to SSE updates.
    pub fn subscribe_sse(&self) -> broadcast::Receiver<String> {
        self.sse_tx.subscribe()
    }

    /// Notify SSE subscribers of a generic update (backward-compatible).
    pub fn notify_update(&self) {
        let _ = self.sse_tx.send("update".to_string());
    }

    /// Emit a structured JSON event to SSE subscribers.
    ///
    /// Events have a `type` field and a `timestamp`, plus type-specific data.
    /// The dashboard parses these for the real-time activity feed.
    /// Also buffers the event in memory for REST hydration on page reload.
    pub fn emit_event(&self, event: serde_json::Value) {
        let mut evt = event;
        if let Some(obj) = evt.as_object_mut() {
            obj.entry("timestamp")
                .or_insert_with(|| serde_json::Value::String(chrono::Utc::now().to_rfc3339()));
        }

        // Buffer the event for REST hydration
        if let Ok(mut buf) = self.recent_events.try_lock() {
            buf.push(evt.clone());
            if buf.len() > MAX_BUFFERED_EVENTS {
                let excess = buf.len() - MAX_BUFFERED_EVENTS;
                buf.drain(0..excess);
            }
        }

        let _ = self.sse_tx.send(evt.to_string());
    }

    /// Return the last N buffered tool progress events (newest last).
    pub async fn recent_tool_events(&self, limit: usize) -> Vec<serde_json::Value> {
        let buf = self.recent_events.lock().await;
        let start = buf.len().saturating_sub(limit);
        buf[start..].to_vec()
    }

    /// Provide the ngrok tunnel URL to the skill manager so it can inject
    /// `TUNNEL_URL` / `PUBLIC_URL` into every skill's environment.
    pub async fn set_tunnel_url(&self, url: TunnelUrl) {
        let mut mgr = self.skill_manager.lock().await;
        mgr.set_tunnel_url(url);
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

/// Truncate a string to `max_len` chars, appending "…" if truncated.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
