# Changelog

All notable changes to SafeClaw are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.1] — 2026-03-04

### Added

- **API reference** — comprehensive documentation for all 100+ REST
  endpoints in `docs/api-reference.md`, organized by category with
  request/response examples for every endpoint.

- **Skill authoring guide** — tutorial in `docs/skill-authoring-guide.md`
  covering Python (with venv), Node.js, Rhai, and shell skills. Includes
  `skill.toml` manifest reference, credentials, dependencies, sandboxing,
  versioning/rollback, hot reload, extension routes, dashboard UI panels,
  and a complete RSS monitor example.

## [0.8.0] — 2026-03-04

### Added

- **Skill versioning** — `version` field in `skill.toml`. Create snapshots
  via `POST /api/skills/{name}/snapshot`, list with
  `GET /api/skills/{name}/versions`, and rollback with
  `POST /api/skills/{name}/rollback`. Snapshots are stored under
  `<skill_dir>/.versions/<version>/`, preserving source files while
  excluding `.venv`, `data`, `node_modules`, and logs.

- **Skill dependency management** — `dependencies` array in `skill.toml`
  listing names of other skills that must be running first. Reconciliation
  uses two-pass dependency ordering: skills with unmet deps are deferred
  until their dependencies start.

- **Per-skill sandboxing** — `[sandbox]` section in `skill.toml` with
  `restrict_fs` (restricts HOME/cwd to the skill directory), `block_network`,
  `max_memory_mib` (default 1024), `max_file_size_mib` (default 128), and
  `max_open_files` (default 128). Replaces the one-size-fits-all
  `ProcessLimits::skill()` with per-skill resource limits applied via
  rlimit in pre_exec.

- **Skill health monitoring** — `SkillHealth` struct with `uptime_secs`,
  `restart_count`, `last_error`, and `memory_bytes` (read from
  `/proc/{pid}/statm` on Linux). Health data is included in both the
  `GET /api/skills` list and `GET /api/skills/{name}/detail` responses.
  Restart counts and last errors persist across skill restart cycles.

- **Hot reload** — `hot_reload()` runs during every reconciliation cycle,
  hashing `skill.toml`, the entrypoint file, and `requirements.txt`. When
  hashes differ from the previous check, the skill is stopped and restarted
  automatically without requiring a full agent restart.

### Changed

- `SkillManifest` now includes `version`, `dependencies`, and `sandbox`
  fields (all backward-compatible with defaults).
- `SkillStatus` now includes `version`, `dependencies`, and `health`.
- `RunningSkill` tracks `started_at`, `restart_count`, and `last_error`.
- `SkillManager` tracks cumulative `restart_counts`, `last_errors`, and
  `file_hashes` for hot-reload detection.

## [0.7.0] — 2026-03-04

### Added

- **Session system activation**: Sessions now enabled by default. The tick
  loop processes pending sessions via `process_pending_sessions()`, running
  each through the LLM with persona-specific prompts and auto-approved tool
  execution. Configurable `max_turns` per session (default 10).
- **Specialist personas**: New `personas` database table with CRUD REST API
  (`GET/POST /api/personas`, `PUT/DELETE /api/personas/{id}`). Four default
  personas seeded on first run: coder, researcher, writer, planner. Each
  persona has a personality prompt and optional tool restrictions.
- **`delegate` tool**: Allows the agent to spawn sub-agent sessions with a
  specified persona. Supports synchronous mode (waits for result) and
  asynchronous mode (background processing). Sub-agents run in isolated
  sessions with their own conversation history.
- **`plan` tool**: Multi-persona collaborative planning. Multiple specialist
  personas discuss an objective across configurable rounds (1-5), each
  contributing from their area of expertise. A final synthesis step produces
  an actionable plan with executive summary, action items, risks, and
  success criteria.

### Changed

- `SessionsConfig.enabled` now defaults to `true` (was `false`).
- `SessionsConfig` gains a `max_turns` field (default 10) controlling the
  maximum LLM turns per session run.

## [0.6.0] — 2026-03-04

### Added

- **Email backend** — send and read emails via Gmail API and Microsoft Graph
  using existing OAuth tokens.  New `email` tool with `send`, `inbox`, and
  `search` actions.  OAuth scopes upgraded to include `gmail.send` and
  `Mail.Send` for write access.
- **Slack workspace bot** — full Slack Web API backend for sending messages
  (with Block Kit for rich content) and an Events API endpoint at
  `/api/messaging/slack/events` for receiving messages.  Configured via
  `SLACK_BOT_TOKEN` environment variable and `[slack]` config section.
- **Matrix backend** — connects to any Matrix homeserver via the
  Client-Server API (reqwest).  Supports login with access token or
  username/password.  Background `/sync` long-polling for incoming messages.
  Configured via `MATRIX_ACCESS_TOKEN` or `MATRIX_USER`/`MATRIX_PASSWORD`
  and `[matrix]` config section.
- **Rich messaging infrastructure** — `RichContent` enum (`Image`, `File`,
  `Buttons`, `Card`) with `send_rich()` method on `MessagingBackend`.
  Default implementation falls back to text for platforms without native
  rich message support.
- **Telegram rich messages** — native `send_photo`, `send_document`, and
  `InlineKeyboardMarkup` support.  Cards rendered with HTML formatting.
- **Discord rich messages** — native embed support for images and cards via
  Serenity's `CreateEmbed` and `CreateMessage` builders.
- **Slack rich messages** — Block Kit blocks for images, buttons (actions),
  and structured cards with headers and sections.
- **Config sections** — new `[slack]`, `[matrix]`, and `[email]` config
  sections with `enabled`, authorization, and provider-specific settings.

### Changed

- **`MessagingBackend` trait** — extended with `as_any()` for downcasting,
  `supports_rich_messages()`, and `send_rich()` (with default text fallback).
  Fully backward-compatible via default method implementations.
- **`MessagingManager`** — added `send_rich_all()` for broadcasting rich
  content to all registered backends.
- **OAuth scopes** — Google now includes `gmail.send`; Microsoft now includes
  `Mail.Send` alongside existing read scopes.

## [0.5.0] — 2026-03-04

### Added

- **Generic webhook endpoint** (`POST /api/webhook/{token}`) — accepts any HTTP
  body from any service (GitHub, Bitbucket, Jira, Slack, GitLab, PagerDuty,
  Discord, Linear, etc.), auto-detects the source platform from headers,
  extracts event types, and routes the raw payload through the agent to the
  appropriate skill.  Returns 200 OK immediately; processing is asynchronous.
- **API token management** — SHA-256 hashed tokens stored in SQLite with global
  (`*`) or per-skill scoping.  Dashboard CRUD via `GET/POST /api/tokens` and
  `PUT/DELETE /api/tokens/{id}` (JWT-protected).  Tokens use `sc_` prefix with
  32 random bytes.
- **`api_tokens` database table** — new migration adds token storage with
  `id`, `name`, `token_hash` (unique), `scopes`, `created_at`, `last_used`,
  and `enabled` columns.

## [0.4.0] — 2026-03-05

### Added

- **Self-sandboxing binary** — the process applies kernel-level isolation on
  startup without requiring Docker or any container runtime.
- **Linux: seccomp-bpf syscall filter** — blocks 21 dangerous syscalls (ptrace,
  mount, chroot, reboot, kernel module loading, BPF, perf_event_open, etc.)
  with EPERM.  Inherited by all child processes including skills and exec
  commands.  Uses the `seccompiler` crate from the Firecracker project.
- **Linux: capability dropping** — clears all 42 bounding capabilities on
  startup via `prctl(PR_CAPBSET_DROP)`, preventing privilege escalation even
  when running as root.
- **Linux: `PR_SET_NO_NEW_PRIVS`** — prevents child processes from gaining
  privileges via setuid binaries.
- **macOS: Seatbelt sandbox** — applies a deny-default Sandbox Profile Language
  (SBPL) policy via `sandbox_init(3)` FFI.  Restricts filesystem writes to
  data/config dirs, allows read-only system paths and Homebrew, permits network
  and process fork/exec for skills.
- **`SandboxStatus` struct** — tracks which isolation layers are active and logs
  a structured summary at startup.
- **Platform-dispatched sandbox orchestrator** (`src/security/sandbox.rs`) —
  applies Linux (4 layers) or macOS (Seatbelt) isolation automatically based on
  the target OS.

### Changed

- **Landlock always applied** — removed `NO_JAIL=1` bypass; Landlock filesystem
  restrictions are now unconditional on Linux 5.13+.
- **Dockerfile simplified** — removed chroot jail entrypoint and `SYS_ADMIN`
  capability.  Runs as `USER safeclaw` with direct `ENTRYPOINT ["safeclaw"]`.
  Docker is now optional; the binary provides equivalent isolation natively.
- **docker-compose.yml** — removed `cap_add: SYS_ADMIN`.

### Performance (from 0.3.0)

- **Read-only DB connection** — added a second `SQLITE_OPEN_READ_ONLY`
  connection for all SELECT queries, eliminating mutex contention between reads
  and writes.
- **Skill reconciliation TTL** — 30-second cooldown prevents redundant
  filesystem scans on every tick and message.
- **Cached `auto_approve` HashSet** — built once at startup instead of per
  message.
- **Parallelized dashboard handlers** — `/metrics` and `/security-overview`
  fetch stats, audit, and cost summaries concurrently via `tokio::join!`.
- **Optional `chromiumoxide` and `serenity`** — gated behind `browser` and
  `discord` feature flags, saving ~120s compile time in default builds.
- **Async skill listing** — `list_async()`/`list_data()` avoids blocking the
  tokio runtime during directory scans.

### Fixed

- **`user_profiles` UPSERT** — the `ON CONFLICT(user_id, key)` clause silently
  failed when `user_id` was NULL due to SQL NULL inequality semantics.  Now
  stores empty string for the global profile so the unique constraint fires
  correctly.

## [0.3.0] — 2026-03-04

### Added

- **Daimon framework integration** — optional `daimon` feature flag pulls in the
  [daimon](https://crates.io/crates/daimon) agentic framework (v0.16) from
  crates.io with `macros` and `mcp` features. Enables MCP tool server
  connectivity and bidirectional tool trait bridging.
- **MCP tool server support** — configure external MCP servers (stdio or HTTP
  transport) via `[[mcp.servers]]` in `config.toml`. Tools are auto-discovered
  at startup and registered in SafeClaw's tool registry with `mcp_{server}_{tool}`
  prefixed names, flowing through the approval queue like built-in tools.
- **Bridge module (`src/bridge/`)** — bidirectional adapters between SafeClaw and
  daimon tool traits:
  - `DaimonToolAdapter`: wraps daimon/MCP tools for SafeClaw's `ToolRegistry`
  - `SafeClawToolAdapter`: wraps SafeClaw tools for daimon's agent patterns
- **`McpConfig` / `McpServerEntry`** — new configuration structs for declaring
  MCP servers with name, transport type, command/args (stdio), or URL (http).
- **`SafeAgentError::Tool`** — new error variant for tool execution bridge errors.

### Changed

- **Dockerfile switched to Debian** — builder uses `rust:1.93-bookworm` and
  runtime uses `debian:bookworm-slim` instead of Alpine, for glibc compatibility
  with NVIDIA CUDA libraries.
- **llama-gguf dependency updated** — bumped to v0.11.3 with CUDA tensor naming
  fixes, NVRTC include path auto-detection, and GPU-only inference corrections.
  Vulkan feature removed from default build (CUDA-only in Docker).
- **Local LLM config** — `LlmConfig` now supports `use_gpu` and
  `max_context_len` fields, passed through to llama-gguf's `EngineConfig` for
  GPU acceleration and VRAM-aware context capping.

## [0.1.2] — 2026-02-24

### Changed

- **Rebrand: safe-agent → SafeClaw** — product renamed throughout the
  codebase. Package name, binary, Docker image, localStorage keys,
  service worker cache, PWA manifest, User-Agent header, and all
  display strings updated. Company, GitHub org, and contact info
  unchanged.

## [0.1.0] — 2026-02-17

Initial release of SafeClaw: a sandboxed autonomous AI agent with tool
execution, knowledge graph, multi-interface control, and a full web dashboard.

### Core Agent

- **Multi-backend LLM engine** — supports Claude Code CLI, OpenAI Codex CLI,
  Google Gemini CLI, Aider (multi-provider), OpenRouter API (hundreds of
  models via one key), and local GGUF inference via `llama-gguf`.
- **Structured tool calling** — LLM proposes tool calls via fenced
  `tool_call` blocks; the agent parses, routes through auto-approve or the
  human approval queue, executes, feeds results back, and loops up to
  `max_tool_turns` (default 5).
- **Approval queue** — dangerous tool calls require human approval via the
  dashboard, Telegram, or WhatsApp before execution. Configurable
  `auto_approve_tools` list for safe operations.
- **Tick-based agent loop** — configurable interval (default 120 s) for
  background housekeeping: expiring stale actions, running cron jobs,
  processing background goals, and reconciling skills.

### Autonomy & Planning

- **Goal system** — persistent goals with prioritized task decomposition,
  dependency chains, and automatic execution during the tick loop.
- **Cron scheduler** — enabled cron jobs are evaluated every tick and
  executed via the tool registry. The `cron` tool lets the LLM create,
  list, enable, disable, and remove scheduled jobs.
- **Self-reflection** — after a goal completes or fails, the LLM generates
  a concise self-reflection stored on the goal record.
- **Proactive notifications** — background goal progress and cron results
  are pushed to all configured messaging platforms.

### Memory

- **Core memory** — single-row personality that persists across restarts.
- **Conversation memory** — rolling window of recent messages with
  configurable depth.
- **Archival memory** — long-term storage with SQLite FTS5 full-text search.
- **Knowledge graph** — typed nodes and weighted edges with FTS5 search,
  neighbor traversal, and graph statistics.

### Tools (13 built-in)

| Tool | Description |
|------|-------------|
| `exec` | Sandboxed shell command execution with timeout |
| `read_file` | Read files within the sandbox |
| `write_file` | Write files within the sandbox |
| `edit_file` | Find-and-replace editing within files |
| `delete_file` | Move files to recoverable trash |
| `apply_patch` | Apply unified diffs to files |
| `web_search` | DuckDuckGo web search |
| `web_fetch` | Fetch and convert web pages to markdown |
| `browser` | Headless Chrome automation via CDP |
| `message` | Send messages to Telegram/WhatsApp |
| `cron` | Manage scheduled jobs |
| `goal` | Create and manage background goals and tasks |
| `knowledge_graph` | Query and modify the knowledge graph |
| `memory_search` | Search archival memory |
| `memory_get` | Retrieve archival memory entries |
| `process` | List and manage background processes |
| `image` | Image analysis placeholder |
| `sessions_*` | Multi-agent session coordination (4 tools) |

### Skill System

- **Skill lifecycle management** — auto-discovery from the skills directory,
  process spawning with resource limits, graceful shutdown (SIGTERM then
  SIGKILL), and automatic restart for daemon skills.
- **Skill manifest** (`skill.toml`) — declares name, description, type
  (daemon/oneshot), entrypoint, environment variables, and credential specs.
- **Credential management** — skills declare required credentials; values
  are stored encrypted on disk and injected as environment variables.
- **Rhai extension system** — skills can register HTTP routes and serve
  custom UI panels/pages via `routes.rhai` and a `ui/` directory.
- **Skill import** — import existing skills from Git repositories, archive
  URLs (`.tar.gz`/`.zip`), or local directories via the dashboard or API.
- **Skill deletion** — remove skills with automatic process cleanup and
  credential purging.

### Dashboard (Svelte 5 + Tailwind CSS)

- **Overview tab** — live SSE feed of tool activity with animated
  indicators, pending actions queue, activity log, memory panels, and
  agent stats.
- **Chat tab** — real-time conversation with the agent including
  thinking/executing state indicators.
- **Goals tab** — list goals with status filters, task progress bars,
  expandable detail with self-reflection, and pause/resume/cancel controls.
- **Skills tab** — manage skills with per-skill credential configuration,
  environment variables, log viewer, manifest editor, extension panel, and
  import/delete UI.
- **Knowledge tab** — browse and search the knowledge graph.
- **Tools tab** — view registered tools and their descriptions.
- **Trash tab** — recoverable file deletion with restore and permanent
  delete.
- **Settings tab** — messaging platform configuration.

### Authentication & SSO

- **JWT-based session auth** — password login with HS256 JWT cookies.
- **Multi-provider SSO** — configurable dashboard login via Google, GitHub,
  Microsoft, Discord, Slack, LinkedIn, Spotify, Dropbox, Twitter, Notion,
  Zoom, and Twitch. Allowlist by email address.
- **Multi-account OAuth** — connect multiple accounts per provider for
  calendar, email, and other integrations.

### Messaging Platforms

- **Telegram bot** — full bidirectional messaging with chat ID allowlist,
  typing indicators during tool execution, and proactive notifications.
- **WhatsApp bot** — Baileys-based Node.js bridge with QR code pairing,
  webhook integration, and the same feature set as Telegram.
- **Abstracted messaging layer** — `MessagingManager` with `send_all`,
  `typing_all`, and per-platform routing.

### Multi-User Support

- **User management** — `users` table with UUID IDs, unique usernames,
  display names, emails, and password-based authentication. Three roles:
  `admin` (full access, user management, approval), `user` (chat and tool
  use), `viewer` (read-only dashboard access).
- **Platform identity mapping** — each user can be linked to a Telegram
  user ID and/or WhatsApp number. Incoming messages automatically resolve
  to the correct user for per-user permission enforcement.
- **Per-user conversation memory** — `user_id` column on
  `conversation_history` provides isolated conversation windows per user.
  Also added to `activity_log`, `audit_log`, `goals`, `pending_actions`.
- **UserContext threading** — `handle_message_as()` accepts an optional
  `UserContext` that flows through tool execution, audit logging, and
  memory operations. Viewers are blocked from sending messages.
- **Multi-user dashboard auth** — JWT claims extended with `user_id` and
  `role`. Login supports both legacy single-password and per-user
  username+password modes. SSO callback links emails to existing users.
  Header shows current user identity and role.
- **User management dashboard** — Settings tab includes a full user
  management panel: create users, assign roles, enable/disable accounts,
  link Telegram/WhatsApp IDs, change passwords, and delete users.
- **Backward-compatible** — existing single-user deployments continue to
  work without configuration changes. Multi-user features activate when
  users are created.

### Two-Factor Authentication & Passkeys

- **TOTP 2FA** — users can enable time-based one-time password
  authentication via any standard authenticator app (Google Authenticator,
  Authy, etc.). Setup flow generates a base32 secret with QR code and
  10 single-use recovery codes. TOTP verification uses HMAC-SHA1 (RFC 6238)
  with ±1 time-step tolerance for clock drift.
- **Passkeys (WebAuthn)** — full FIDO2/WebAuthn passkey support via
  `webauthn-rs`. Users can register platform authenticators (Touch ID,
  Windows Hello, Android biometrics) or roaming security keys (YubiKey).
  Registration and authentication use the standard `navigator.credentials`
  browser API with server-side challenge/response verification.
- **Challenge-token login flow** — when 2FA is enabled, password
  authentication returns a short-lived challenge token (5-minute JWT)
  instead of a session. The user must then verify via TOTP code, recovery
  code, or passkey assertion to receive the full session JWT.
- **2FA management API** — `POST /api/auth/2fa/setup`, `/enable`,
  `/disable`, `GET /api/auth/2fa/status`; passkey endpoints at
  `/api/auth/passkey/register/{start,finish}`,
  `/api/auth/passkey/authenticate/{start,finish}`,
  `GET /api/auth/passkeys`, `DELETE /api/auth/passkeys/{id}`.
- **Dashboard 2FA settings panel** — Settings tab includes a dedicated
  Two-Factor Authentication section for managing TOTP setup (with QR code
  scanner), recovery codes, and passkey registration/deletion.
- **Login overlay 2FA flow** — login screen seamlessly transitions to a
  2FA challenge step showing passkey button, TOTP code input, and recovery
  code fallback based on the user's enabled methods.
- **DB schema** — `totp_secret`, `totp_enabled`, `recovery_codes` columns
  on `users` table; new `passkeys` table for WebAuthn credential storage.

### Timezone & Locale Awareness

- **System-level timezone/locale** — configurable via `config.toml` with
  `timezone` (IANA name, e.g. "America/New_York") and `locale` (BCP 47 tag,
  e.g. "en-US") fields. Defaults to UTC / en-US.
- **Per-user overrides** — each user has `timezone` and `locale` columns in the
  `users` table that take precedence over system defaults. Settable via the
  dashboard Settings tab or the `POST /api/timezone` endpoint.
- **Time-aware LLM** — the system prompt now includes the current date/time in
  the user's timezone so the agent can give contextually appropriate responses
  (proper greetings, scheduling, relative time references).
- **Dashboard timezone picker** — Settings tab includes a Timezone & Locale
  panel with browser auto-detection buttons, a searchable IANA timezone input
  with common timezones pre-listed, and a locale text field.
- **Consistent timestamp formatting** — all user-facing timestamps in the
  dashboard (Activity Log, Audit Trail, Chat, Goals, Knowledge Graph, Memory
  panels, OAuth status) are formatted in the browser's local timezone using a
  shared `time.ts` utility that correctly parses both RFC 3339 and SQLite
  `datetime('now')` formats.
- **Timezone API endpoints** — `GET /api/timezone` (current effective
  timezone/locale with formatted time), `POST /api/timezone` (set per-user),
  `GET /api/timezones` (full IANA timezone list), `GET /api/timezone/convert`
  (server-side UTC-to-local conversion with relative day labels).

### Onboarding Wizard

- **First-run setup flow** — a full-screen 4-step wizard appears in the
  dashboard on first launch, guiding the user through initial configuration
  before the normal dashboard UI is shown.
- **Step 1: Agent Identity** — set the agent's display name and core
  personality instructions.
- **Step 2: LLM Backend** — choose from available backends with a "Test
  Connection" button that sends a live test prompt to validate the setup.
- **Step 3: Messaging (optional)** — displays Telegram and WhatsApp status
  with guidance on environment variables; skippable.
- **Step 4: Review & Finish** — summary of all configured settings with a
  "Complete Setup" button.
- **Metadata table** — new `metadata` key-value table in the database stores
  `onboarding_completed` flag; subsequent launches skip the wizard.
- **Config persistence** — agent name, personality, and LLM backend choices
  are written to the TOML config file on disk during onboarding.
- **Auth-exempt endpoints** — all `/api/onboarding/*` routes are placed below
  the auth middleware layer so the wizard works before any user account exists.

### PII Encryption at Rest

- **Auto-generated encryption key** — on first launch the system generates
  a cryptographically random 256-bit AES key and persists it to
  `<data_dir>/encryption.key` with owner-only (0600) permissions. The key
  can also be supplied via the `ENCRYPTION_KEY` environment variable for
  containerized deployments.
- **AES-256-GCM field encryption** — all PII fields in the `users` table
  (`display_name`, `email`, `password_hash`, `telegram_id`, `whatsapp_id`,
  `totp_secret`, `recovery_codes`) are encrypted before storage using
  AES-256-GCM with a unique random 12-byte nonce per value. Encrypted
  values carry an `ENC$` prefix for format identification.
- **HMAC-SHA-256 blind indexes** — lookup fields (`email`, `telegram_id`,
  `whatsapp_id`) have companion `*_blind` columns containing deterministic
  HMAC-SHA-256 hashes computed with a derived key. This allows SQL
  equality lookups without ever storing or querying plaintext values.
- **Automatic migration** — on startup, `migrate_encrypt_pii()` scans all
  existing users and transparently encrypts any plaintext PII fields,
  populating blind indexes. Safe to run repeatedly; already-encrypted
  fields are skipped.
- **Graceful legacy support** — the decrypt function treats values without
  the `ENC$` prefix as plaintext, ensuring backward compatibility during
  migration from older database formats.

### Security & Sandboxing

- **Path jailing** — `SandboxedFs` enforces all file operations stay within
  the agent's data directory; symlink and traversal attacks are blocked.
- **Process resource limits** — skills and exec'd commands run with
  `rlimit` caps on CPU time, memory, file size, open files, and process
  count.
- **Landlock LSM** — on Linux, the agent restricts its own filesystem
  access to the data directory and necessary system paths.
- **Chroot jail** — Docker entrypoint builds a minimal chroot with only
  required binaries and libraries.
- **Non-root execution** — Docker image runs the agent as a dedicated
  `safeclaw` user (configurable UID/GID).
- **URL validation** — blocks requests to private IPs, localhost, and
  metadata endpoints.
- **SQL injection guard** — input sanitization for all user-provided values
  in queries.
- **Recoverable trash** — `delete_file` moves to a trash directory instead
  of permanent deletion; restorable via the dashboard.
- **Capability-based permissions** — fine-grained per-tool operation
  restrictions via `[security.tool_capabilities]` config. Blocks specific
  commands (e.g. allow `ls` but not `rm` for exec) and tool operations.
- **Structured audit trail** — every tool call, approval decision,
  rate-limit hit, PII detection, and 2FA challenge is logged to the
  `audit_log` table with timestamps, reasoning, params, and source.
  Queryable via API with filtering by event type and tool name.
- **LLM cost tracking** — token usage and estimated USD cost per request
  stored in `llm_usage` table. Model-aware pricing for Claude, GPT, Gemini,
  Llama, DeepSeek, etc. Daily cost limit with automatic alerting.
- **Rate limiting** — sliding-window limiter caps tool calls per minute
  (default 30) and per hour (default 300) to prevent runaway loops.
- **PII/sensitive data detection** — scans LLM responses for SSNs, credit
  cards, API keys, private keys, JWT tokens, AWS access keys, and passwords.
  Flags responses with a warning before delivery.
- **Explainability** — "Why did you do that?" drill-down retrieves the
  causal chain of audit events for any action, showing reasoning at each step.
- **2FA for dangerous operations** — configurable tools (default: `exec`)
  require dashboard confirmation before execution. Time-limited challenges
  with 5-minute TTL.
- **Security dashboard tab** — overview with live stats, filterable audit
  trail, cost tracking with budget progress bars, rate-limit gauges, and
  2FA challenge management.

### Deployment & Operations

- **Multi-architecture Docker image** — builds for `linux/amd64` and
  `linux/arm64` on Alpine Linux with pre-installed Python, Node.js, Claude
  Code CLI, and ngrok. Supports Raspberry Pi, Apple Silicon, and Graviton.
- **ACME/Let's Encrypt TLS** — automatic HTTPS certificate provisioning
  with TLS-ALPN-01 challenge.
- **Ngrok tunnel** — optional tunnel for public access and OAuth callbacks.
- **Platform install scripts** — chroot jail scripts for Debian/RPM/Arch
  Linux, macOS sandbox (App Sandbox + TCC), and Windows (PowerShell with
  AppContainer).
- **Health check endpoint** — `/healthz` returns 200 OK (or 503) with
  database, agent, and tool health. Unauthenticated for use with load
  balancers, Docker HEALTHCHECK, and Kubernetes liveness probes.
- **Prometheus metrics** — `/metrics` exposes OpenMetrics text format:
  agent info, paused state, tick/approve/reject counters, audit events,
  LLM cost and token gauges, rate limiter state. Unauthenticated for
  Prometheus/Grafana scraping.
- **Auto-update mechanism** — `/api/update/check` queries GitHub Releases
  API for newer versions; `/api/update/apply` runs `git pull` + `cargo
  build --release` (or signals Docker container restart). Dashboard shows
  current vs. latest version, release notes, and one-click update.
- **Backup & restore** — `/api/backup` exports all agent data (memory,
  activity, goals, cron, stats) as a timestamped JSON file. `/api/restore`
  merges the backup via INSERT OR REPLACE. Dashboard has download/upload
  buttons.
- **Multi-node federation** — `FederationManager` with peer registry,
  heartbeat protocol (configurable interval), asynchronous memory delta
  replication, and distributed task claiming. `[federation]` config
  section with `enabled`, `peers`, `advertise_address`, intervals.
  Federation API endpoints for sync, heartbeat, and task claims.
  Dashboard Operations tab manages peers.
- **LLM backend plugin architecture** — `LlmBackend` async trait with
  `LlmPluginRegistry` for dynamic registration. All built-in backends
  auto-register. `register_plugin()` and `switch_backend()` at runtime.
  `/api/llm/backends` lists available and active backends.
- **Operations dashboard tab** — health check viewer, Prometheus info,
  update checker, backup/restore UI, federation peer management, and
  LLM backend plugin overview.

### CI/CD

- **Test workflow** — Rust (`cargo fmt`, `clippy`, `cargo test --release`)
  and Svelte (`pnpm test`, `pnpm build`) on every push/PR to `main`.
- **CodeQL workflow** — static security analysis for JavaScript/TypeScript.
- **Docker workflow** — multi-arch image build and push to
  `ghcr.io/pegasusheavy/SafeClaw` on push to `main` and version tags.

### Configuration

- **TOML config file** — all settings have sensible defaults; environment
  variables can override most values.
- **Example config** — `config.example.toml` documents every option with
  inline comments, including `[federation]` for multi-node setup.

[0.1.0]: https://github.com/PegasusHeavyIndustries/SafeClaw/releases/tag/v0.1.0
