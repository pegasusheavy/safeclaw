# SafeClaw API Reference

All endpoints are served by the Axum web server embedded in the SafeClaw
binary. The base URL is the dashboard bind address (default `0.0.0.0:3030`).

## Authentication

Most endpoints require a valid JWT cookie. Obtain one by posting to
`/api/auth/login`. The cookie is `HttpOnly; SameSite=Strict` with a 7-day
expiry. Endpoints below the auth middleware layer (OAuth callbacks, webhooks,
health checks, onboarding, and static assets) are exempt.

Endpoints marked **Auth: No** can be called without a JWT cookie.

### Common response types

**ActionResponse** — returned by most mutating endpoints:

```json
{
  "ok": true,
  "message": "human-readable result",
  "count": 3
}
```

`message` and `count` are optional.

---

## 1. Auth

### `POST /api/auth/login`

Authenticate with password (and optionally username for multi-user mode).

**Request body:**
```json
{ "password": "string", "username": "string (optional)" }
```

**Response:** Sets a JWT cookie. Returns `{ "ok": true, "user": {...} }`.
If 2FA is enabled, returns `{ "ok": false, "requires_2fa": true, "challenge_token": "...", "methods": ["totp", "passkey"] }`.

### `POST /api/auth/logout`

Clear the JWT cookie.

### `GET /api/auth/check`

**Auth: No.** Check whether the current request has a valid JWT.

**Response:**
```json
{
  "required": true,
  "authenticated": true,
  "subject": "admin",
  "method": "password",
  "user_id": "abc123",
  "role": "admin"
}
```

### `GET /api/auth/info`

**Auth: No.** Login page configuration.

**Response:**
```json
{
  "password_enabled": true,
  "sso_providers": ["google", "github"],
  "multi_user": false,
  "passkeys_available": true
}
```

### `GET /api/auth/sso/{provider}/start`

**Auth: No.** Redirect the browser to an SSO provider's authorization page.

**Path params:** `provider` — SSO provider id (e.g. `google`, `github`).

### `GET /api/auth/sso/{provider}/callback`

**Auth: No.** OAuth callback handler. Sets JWT cookie on success.

---

## 2. Two-Factor Authentication

### `POST /api/auth/2fa/verify`

**Auth: No.** Complete a 2FA challenge after initial login.

**Request body:**
```json
{ "challenge_token": "string", "totp_code": "123456", "recovery_code": "string (optional)" }
```

### `POST /api/auth/2fa/setup`

Generate a new TOTP secret and recovery codes.

**Response:**
```json
{
  "ok": true,
  "secret": "base32-encoded",
  "otpauth_uri": "otpauth://totp/...",
  "recovery_codes": ["code1", "code2", "..."]
}
```

### `POST /api/auth/2fa/enable`

Enable TOTP by verifying a code.

**Request body:** `{ "code": "123456" }`

### `POST /api/auth/2fa/disable`

Disable TOTP by verifying a code.

**Request body:** `{ "code": "123456" }`

### `GET /api/auth/2fa/status`

**Response:**
```json
{ "totp_enabled": true, "passkey_count": 2, "passkeys_available": true }
```

---

## 3. Passkeys (WebAuthn)

Requires `WEBAUTHN_ORIGIN` or `TUNNEL_URL` to be set.

### `POST /api/auth/passkey/register/start`

Start passkey registration (returns WebAuthn creation options).

### `POST /api/auth/passkey/register/finish`

Complete passkey registration.

**Request body:** `{ "credential": {...}, "name": "My YubiKey" }`

### `POST /api/auth/passkey/authenticate/start`

**Auth: No.** Start passkey authentication during 2FA flow.

**Request body:** `{ "challenge_token": "string" }`

### `POST /api/auth/passkey/authenticate/finish`

**Auth: No.** Complete passkey authentication.

**Request body:** `{ "challenge_token": "string", "credential": {...} }`

### `GET /api/auth/passkeys`

List registered passkeys.

### `DELETE /api/auth/passkeys/{id}`

Delete a passkey by ID.

---

## 4. Status & Control

### `GET /api/status`

**Response:**
```json
{
  "running": true,
  "paused": false,
  "agent_name": "safeclaw",
  "dashboard_bind": "0.0.0.0:3030",
  "tick_interval_secs": 60,
  "tools_count": 15
}
```

### `GET /api/stats`

Memory and performance statistics.

### `POST /api/agent/pause`

Pause the agent tick loop.

### `POST /api/agent/resume`

Resume the agent tick loop.

### `POST /api/agent/tick`

Force an immediate tick cycle.

---

## 5. Approval Queue

### `GET /api/pending`

List pending tool-call actions awaiting operator approval.

### `POST /api/pending/{id}/approve`

Approve a pending action.

### `POST /api/pending/{id}/reject`

Reject a pending action.

### `POST /api/pending/approve-all`

Approve all pending actions. Returns `{ "ok": true, "count": N }`.

### `POST /api/pending/reject-all`

Reject all pending actions. Returns `{ "ok": true, "count": N }`.

---

## 6. Activity

### `GET /api/activity`

Recent activity feed.

**Query params:** `limit` (default 50), `offset` (default 0).

---

## 7. Chat

### `POST /api/chat`

Send a message to the agent and receive a reply.

**Request body:**
```json
{ "message": "Hello, agent!", "user_id": "optional-user-id" }
```

**Response:**
```json
{ "reply": "Agent's response text", "timestamp": "2026-03-04T12:00:00Z" }
```

---

## 8. Memory

### `GET /api/memory/core`

**Response:** `{ "personality": "..." }`

### `GET /api/memory/conversation`

Recent conversation messages.

### `GET /api/memory/archival`

Search or list archival memory entries.

**Query params:** `q` (search query, optional).

### `GET /api/memory/conversation/history`

Full conversation history with pagination and search.

**Query params:** `q`, `limit`, `offset`.

**Response:** `{ "messages": [...], "total": 42 }`

---

## 9. Knowledge Graph

### `GET /api/knowledge/nodes`

List knowledge graph nodes.

**Query params:** `limit`, `offset`.

### `GET /api/knowledge/nodes/{id}`

Get a single node by ID.

### `GET /api/knowledge/nodes/{id}/neighbors`

Get neighboring nodes and edges.

**Response:** `[{ "edge": {...}, "node": {...} }]`

### `GET /api/knowledge/search`

Full-text search over knowledge graph nodes.

**Query params:** `q` (required).

### `GET /api/knowledge/stats`

**Response:** `{ "nodes": 150, "edges": 320 }`

---

## 10. Tools

### `GET /api/tools`

List all registered tools.

**Response:** `[{ "name": "exec", "description": "..." }, ...]`

---

## 11. Skills

### `GET /api/skills`

List all discovered skills with status, health, and dependency info.

**Response:** Array of `SkillStatus`:
```json
[{
  "name": "my-skill",
  "version": "1.0.0",
  "description": "What it does",
  "skill_type": "daemon",
  "enabled": true,
  "running": true,
  "pid": 12345,
  "manually_stopped": false,
  "has_venv": true,
  "credentials": [{ "name": "API_KEY", "label": "API Key", "description": "...", "required": true, "configured": true }],
  "dependencies": ["other-skill"],
  "health": {
    "uptime_secs": 3600,
    "restart_count": 0,
    "last_error": null,
    "memory_bytes": 52428800
  }
}]
```

### `POST /api/skills/import`

Import a skill from a git repo, local path, or URL archive.

**Request body:**
```json
{ "source": "git|path|url", "location": "https://github.com/...", "name": "optional-override" }
```

### `DELETE /api/skills/{name}`

Delete a skill (stops it first if running).

### `GET /api/skills/{name}/detail`

Full skill detail including manifest, env, log tail, and health.

### `GET /api/skills/{name}/log`

Tail the skill's log file.

**Query params:** `lines` (default 100).

### `POST /api/skills/{name}/start`

Start a skill (clears manual-stop flag).

### `POST /api/skills/{name}/stop`

Stop a skill and prevent auto-restart until explicitly started.

### `POST /api/skills/{name}/restart`

Stop then start a skill.

### `PUT /api/skills/{name}/manifest`

Replace the skill's `skill.toml`.

**Request body:** `{ "toml": "name = \"my-skill\"\n..." }`

### `PUT /api/skills/{name}/enabled`

Enable or disable a skill.

**Request body:** `{ "enabled": true }`

### `PUT /api/skills/{name}/env`

Set an environment variable in the skill manifest.

**Request body:** `{ "key": "VAR_NAME", "value": "var_value" }`

### `DELETE /api/skills/{name}/env/{key}`

Remove an environment variable from the manifest.

### `GET /api/skills/{name}/credentials`

List declared credentials and whether each is configured.

### `PUT /api/skills/{name}/credentials`

Set a credential value.

**Request body:** `{ "key": "API_KEY", "value": "sk-..." }`

### `DELETE /api/skills/{name}/credentials/{key}`

Delete a stored credential.

### `GET /api/skills/{name}/versions`

List available version snapshots.

**Response:** `{ "versions": ["1.0.0", "1.1.0", "snap-1709582400"] }`

### `POST /api/skills/{name}/snapshot`

Create a version snapshot of the current skill state.

**Response:** `{ "ok": true, "version": "1.0.0" }`

### `POST /api/skills/{name}/rollback`

Rollback to a previous version snapshot (auto-snapshots current state first).

**Request body:** `{ "version": "1.0.0" }`

---

## 12. Skill Extensions (Rhai)

### `GET /api/skills/extensions`

List all skill extensions with their routes and UI config.

### `ANY /api/skills/{name}/ext/{*path}`

Dispatch to a Rhai route handler registered by the skill.

### `GET /skills/{name}/ui/{*path}`

**Auth: No.** Serve static files from a skill's `ui/` directory.

### `GET /skills/{name}/page`

**Auth: No.** Serve a skill's full-page HTML.

---

## 13. OAuth

### `GET /oauth/{provider}/start`

**Auth: No.** Redirect to an OAuth provider for authorization.

### `GET /oauth/{provider}/callback`

**Auth: No.** OAuth callback. Stores tokens and shows success/error page.

### `GET /api/oauth/status`

Status of all configured OAuth providers and their connected accounts.

**Response:**
```json
{
  "providers": [{
    "id": "google",
    "name": "Google",
    "icon": "google",
    "configured": true,
    "authorize_url": "/oauth/google/start",
    "accounts": [{
      "account": "user@gmail.com",
      "email": "user@gmail.com",
      "scopes": "calendar gmail drive",
      "expires_at": "2026-03-04T13:00:00Z"
    }]
  }]
}
```

### `GET /api/oauth/providers`

List provider info without account details.

### `POST /api/oauth/{provider}/refresh`

Refresh OAuth tokens.

**Query params:** `account` (optional — refresh specific account or all).

### `POST /api/oauth/{provider}/disconnect/{account}`

Disconnect an OAuth account.

---

## 14. Messaging

### `POST /api/messaging/incoming`

**Auth: No.** Receive an incoming message from any platform.

**Request body:**
```json
{
  "platform": "telegram|whatsapp|slack|discord|matrix|signal|sms|imessage|custom",
  "channel": "chat-id",
  "sender": "user-name",
  "text": "Hello!",
  "is_group": false,
  "is_mentioned": false
}
```

**Response:** `{ "reply": "Agent's response" }`

### `GET /api/messaging/config`

Platform configuration overview.

### `GET /api/messaging/whatsapp/status`

WhatsApp connection status and QR code availability.

### `GET /api/messaging/whatsapp/qr`

Raw QR code string for WhatsApp pairing.

### `GET /api/messaging/platforms`

List messaging platforms and connection status.

### `POST /api/messaging/twilio/incoming`

Twilio SMS/MMS webhook handler (form-encoded).

### `POST /api/messaging/slack/events`

**Auth: No.** Slack Events API handler (verified by signing secret).

---

## 15. Goals

### `GET /api/goals`

List agent goals.

**Query params:** `status`, `limit`, `offset`.

### `GET /api/goals/{id}`

Get a goal and its tasks.

### `PUT /api/goals/{id}/status`

Update a goal's status.

**Request body:** `{ "status": "active|completed|cancelled" }`

---

## 16. Trash

### `GET /api/trash`

List trashed items with stats.

### `GET /api/trash/stats`

Trash statistics (count, total size).

### `POST /api/trash/empty`

Permanently delete all trashed items.

### `POST /api/trash/{id}/restore`

Restore a trashed item to its original location.

### `DELETE /api/trash/{id}`

Permanently delete a single trashed item.

---

## 17. Security

### `GET /api/security/overview`

Combined security dashboard data.

**Response:**
```json
{
  "audit": { "total_events": 500, "recent": [...] },
  "cost": { "total_usd": 12.50, "today_usd": 0.30 },
  "rate_limit": { "calls_last_minute": 5, "is_limited": false },
  "twofa_pending": 0,
  "blocked_tools": ["exec"],
  "pii_detection_enabled": false
}
```

### `GET /api/security/audit`

Query the audit trail.

**Query params:** `limit`, `offset`, `event_type`, `tool`.

### `GET /api/security/audit/summary`

Aggregated audit statistics.

### `GET /api/security/audit/{id}/explain`

Trace the action chain for a specific audit entry.

### `GET /api/security/cost`

LLM cost summary.

### `GET /api/security/cost/recent`

Recent cost records.

**Query params:** `limit`, `offset`.

### `GET /api/security/rate-limit`

Current rate limit status.

**Response:**
```json
{
  "calls_last_minute": 5,
  "calls_last_hour": 120,
  "limit_per_minute": 30,
  "limit_per_hour": 500,
  "is_limited": false
}
```

### `GET /api/security/2fa`

List pending 2FA challenges for high-risk actions.

### `POST /api/security/2fa/{id}/confirm`

Confirm a 2FA challenge.

### `POST /api/security/2fa/{id}/reject`

Reject a 2FA challenge.

---

## 18. Users

### `GET /api/users`

List all users.

### `POST /api/users`

Create a new user.

**Request body:**
```json
{
  "username": "alice",
  "display_name": "Alice",
  "role": "admin|operator|viewer",
  "password": "secret",
  "email": "alice@example.com",
  "telegram_id": 123456,
  "whatsapp_id": "1234567890"
}
```

### `GET /api/users/{id}`

Get a user by ID.

### `PUT /api/users/{id}`

Update a user.

**Request body:**
```json
{
  "display_name": "Alice B.",
  "role": "operator",
  "email": "new@example.com",
  "enabled": true,
  "password": "new-password",
  "telegram_id": null,
  "whatsapp_id": null
}
```

### `DELETE /api/users/{id}`

Delete a user.

---

## 19. Timezone & Locale

### `GET /api/timezone`

Get current timezone settings.

**Query params:** `user_id` (optional).

**Response:**
```json
{
  "system_timezone": "UTC",
  "system_locale": "en-US",
  "user_timezone": "America/New_York",
  "user_locale": "en-US",
  "effective_timezone": "America/New_York",
  "effective_locale": "en-US",
  "current_time": "2026-03-04T12:00:00Z",
  "current_time_formatted": "March 4, 2026 7:00 AM EST"
}
```

### `POST /api/timezone`

Set timezone/locale for a user.

**Request body:** `{ "user_id": "abc", "timezone": "America/New_York", "locale": "en-US" }`

### `GET /api/timezones`

List all available timezone identifiers.

### `GET /api/timezone/convert`

Convert a UTC time to a local timezone.

**Query params:** `utc` (ISO 8601), `timezone` (optional, defaults to effective).

---

## 20. LLM Backends

### `GET /api/llm/backends`

List available LLM backends and the active one.

**Response:**
```json
{
  "active": "openrouter",
  "active_info": { "name": "OpenRouter", "model": "anthropic/claude-sonnet-4" },
  "available": [
    { "id": "claude", "name": "Claude Code CLI", "configured": true },
    { "id": "openrouter", "name": "OpenRouter", "configured": true }
  ]
}
```

### `GET /api/llm/advisor/system`

System hardware specs for model recommendations.

### `GET /api/llm/advisor/recommend`

AI-generated model recommendations.

**Query params:** `use_case`, `limit`.

### `GET /api/llm/ollama/status`

Ollama installation and model status.

### `POST /api/llm/ollama/pull`

Pull an Ollama model.

**Request body:** `{ "tag": "llama3:8b" }`

### `DELETE /api/llm/ollama/models/{tag}`

Delete an Ollama model.

### `POST /api/llm/ollama/configure`

Configure SafeClaw to use a specific Ollama model.

**Request body:** `{ "model": "llama3:8b" }`

---

## 21. Federation

### `GET /api/federation/status`

Federation status and connected peers.

### `GET /api/federation/peers`

List federation peers.

### `POST /api/federation/peers`

Add a federation peer.

**Request body:** `{ "address": "https://peer.example.com" }`

### `DELETE /api/federation/peers/{id}`

Remove a federation peer.

### `POST /api/federation/sync`

**Auth: No.** Receive sync deltas from a peer.

### `POST /api/federation/heartbeat`

**Auth: No.** Receive a heartbeat from a peer.

### `POST /api/federation/claim`

**Auth: No.** Receive a task claim from a peer.

---

## 22. Webhook Tokens

### `GET /api/tokens`

List API tokens.

**Response:**
```json
[{
  "id": "tok_abc123",
  "name": "CI Pipeline",
  "scopes": "chat,exec",
  "created_at": "2026-03-01T00:00:00Z",
  "last_used": "2026-03-04T12:00:00Z",
  "enabled": true
}]
```

### `POST /api/tokens`

Create a new API token (the token value is only shown once).

**Request body:** `{ "name": "CI Pipeline", "scopes": "chat,exec" }`

**Response (201):**
```json
{ "id": "tok_abc123", "name": "CI Pipeline", "token": "sc_live_...", "scopes": "chat,exec" }
```

### `PUT /api/tokens/{id}`

Update a token's name, scopes, or enabled status.

**Request body:** `{ "name": "New Name", "scopes": "chat", "enabled": false }`

### `DELETE /api/tokens/{id}`

Revoke and delete a token.

### `POST /api/webhook/{token}`

**Auth: No.** Generic webhook endpoint (authenticated by token in URL).

Accepts any JSON body (max 1 MiB). The body is logged and can trigger
agent actions based on token scopes.

**Response:** `{ "ok": true, "webhook_id": "wh_...", "token_name": "CI Pipeline" }`

---

## 23. Persona

### `GET /api/persona`

**Auth: No.** Get the agent's core personality.

### `PUT /api/persona`

**Auth: No.** Update the agent's core personality.

**Request body:** `{ "personality": "You are a helpful assistant..." }`

### `GET /api/personas`

**Auth: No.** List specialist personas.

### `POST /api/personas`

**Auth: No.** Create a specialist persona.

**Request body:** `{ "id": "coder", "name": "Coder", "personality": "...", "tools": "exec,read_file" }`

### `PUT /api/personas/{id}`

**Auth: No.** Update a specialist persona.

### `DELETE /api/personas/{id}`

**Auth: No.** Delete a specialist persona.

---

## 24. Backup & Restore

### `GET /api/backup`

Download a JSON backup of the database.

### `POST /api/restore`

Restore data from a backup.

**Request body:** `{ "tables": { "archival_memory": [...], "knowledge_nodes": [...] } }`

---

## 25. Updates

### `GET /api/update/check`

Check for a newer release on GitHub.

**Response:**
```json
{
  "current_version": "0.8.0",
  "latest_version": "0.8.1",
  "update_available": true,
  "release_url": "https://github.com/...",
  "release_notes": "...",
  "published_at": "2026-03-04"
}
```

### `POST /api/update/apply`

Trigger a self-update (downloads and replaces the binary).

---

## 26. Binaries

### `GET /api/binaries`

List installable tool binaries and their status.

### `GET /api/binaries/{name}`

Get info about a specific binary.

### `POST /api/binaries/{name}`

Install a binary (returns 202 Accepted, installs asynchronously).

### `DELETE /api/binaries/{name}`

Uninstall a binary.

---

## 27. Onboarding

### `GET /api/onboarding/status`

**Auth: No.** Check whether the setup wizard has been completed.

### `POST /api/onboarding/complete`

**Auth: No.** Mark onboarding as complete.

**Request body:** `{ "agent_name": "safeclaw", "core_personality": "..." }`

### `POST /api/onboarding/test-llm`

**Auth: No.** Send a test prompt to the configured LLM.

### `POST /api/onboarding/save-config`

**Auth: No.** Save configuration during onboarding.

**Request body:** `{ "agent_name": "safeclaw", "core_personality": "...", "llm_backend": "openrouter" }`

---

## 28. Tool Events

### `GET /api/tool-events`

Recent tool execution events with streaming progress data.

**Query params:** `limit`, `offset`.

---

## 29. Tunnel

### `GET /api/tunnel/status`

Ngrok tunnel status.

**Response:** `{ "enabled": true, "url": "https://abc.ngrok-free.app" }`

---

## 30. Server-Sent Events

### `GET /api/events`

SSE stream for real-time dashboard updates. Events include approval
queue changes, activity log entries, skill status changes, and chat
messages.

---

## 31. Health & Metrics

### `GET /healthz`

**Auth: No.** Health check endpoint.

**Response (200 or 503):**
```json
{
  "status": "healthy",
  "version": "0.8.0",
  "checks": { "db": true, "llm": true },
  "uptime_secs": 86400
}
```

### `GET /metrics`

**Auth: No.** Prometheus/OpenMetrics text format metrics.
