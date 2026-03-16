use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{error, info};

use super::routes::DashState;

const MAX_BODY_SIZE: usize = 1024 * 1024; // 1 MB
const MAX_PROMPT_PAYLOAD: usize = 8 * 1024; // 8 KB before truncation
const TOKEN_PREFIX: &str = "sc_";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    pub scopes: String,
    pub created_at: String,
    pub last_used: Option<String>,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct CreateTokenBody {
    pub name: String,
    #[serde(default = "default_scope")]
    pub scopes: String,
}

fn default_scope() -> String {
    "*".to_string()
}

#[derive(Deserialize)]
pub struct UpdateTokenBody {
    pub name: Option<String>,
    pub scopes: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Serialize)]
pub struct CreateTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String,
    pub scopes: String,
}

// ---------------------------------------------------------------------------
// POST /api/webhook/:token — generic webhook receiver
// ---------------------------------------------------------------------------

pub async fn webhook_handler(
    State(state): State<DashState>,
    Path(raw_token): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if body.len() > MAX_BODY_SIZE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "payload too large"})),
        );
    }

    let token = match validate_api_token(&state, &raw_token).await {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid token"})),
            );
        }
    };

    // Update last_used
    {
        let db = state.db.lock().await;
        let _ = db.execute(
            "UPDATE api_tokens SET last_used = datetime('now') WHERE id = ?1",
            [&token.id],
        );
    }

    let body_str = String::from_utf8_lossy(&body).into_owned();
    let platform = detect_platform(&headers);
    let event_type = extract_event_type(&headers);
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let prompt = build_webhook_prompt(
        &token.name,
        &platform,
        event_type.as_deref(),
        &token.scopes,
        &content_type,
        &body_str,
    );

    let webhook_id = uuid::Uuid::new_v4().to_string();
    let token_name = token.name.clone();

    info!(
        webhook_id = %webhook_id,
        token = %token.name,
        platform = %platform,
        event = event_type.as_deref().unwrap_or("unknown"),
        body_bytes = body.len(),
        "webhook received"
    );

    let agent = state.agent.clone();
    let wh_id = webhook_id.clone();
    tokio::spawn(async move {
        match agent.handle_message_as(&prompt, None).await {
            Ok(reply) => {
                info!(webhook_id = %wh_id, reply_len = reply.len(), "webhook processed");
            }
            Err(e) => {
                error!(webhook_id = %wh_id, err = %e, "webhook processing failed");
            }
        }
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "webhook_id": webhook_id,
            "token_name": token_name
        })),
    )
}

// ---------------------------------------------------------------------------
// Token validation
// ---------------------------------------------------------------------------

async fn validate_api_token(state: &DashState, raw_token: &str) -> Option<ApiToken> {
    let hash = hash_token(raw_token);
    let db = state.db.lock().await;
    db.query_row(
        "SELECT id, name, scopes, created_at, last_used, enabled
         FROM api_tokens WHERE token_hash = ?1 AND enabled = 1",
        [&hash],
        |row| {
            Ok(ApiToken {
                id: row.get(0)?,
                name: row.get(1)?,
                scopes: row.get(2)?,
                created_at: row.get(3)?,
                last_used: row.get(4)?,
                enabled: row.get::<_, i32>(5)? != 0,
            })
        },
    )
    .ok()
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}

// ---------------------------------------------------------------------------
// Platform detection from headers
// ---------------------------------------------------------------------------

fn detect_platform(headers: &HeaderMap) -> String {
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if headers.contains_key("x-github-event") || ua.starts_with("GitHub-Hookshot") {
        return "GitHub".to_string();
    }
    if headers.contains_key("x-event-key") || ua.starts_with("Bitbucket") {
        return "Bitbucket".to_string();
    }
    if headers.contains_key("x-atlassian-webhook-identifier") || ua.contains("Atlassian") {
        return "Jira".to_string();
    }
    if headers.contains_key("x-slack-signature") || ua.contains("Slackbot") {
        return "Slack".to_string();
    }
    if headers.contains_key("x-gitlab-event") {
        return "GitLab".to_string();
    }
    if headers.contains_key("x-pagerduty-signature") {
        return "PagerDuty".to_string();
    }
    if headers.contains_key("x-discord-signature") {
        return "Discord".to_string();
    }
    if ua.contains("Linear") {
        return "Linear".to_string();
    }

    if !ua.is_empty() {
        return format!("Unknown (UA: {})", &ua[..ua.len().min(60)]);
    }

    "Unknown".to_string()
}

fn extract_event_type(headers: &HeaderMap) -> Option<String> {
    // GitHub
    if let Some(v) = headers.get("x-github-event").and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }
    // Bitbucket
    if let Some(v) = headers.get("x-event-key").and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }
    // GitLab
    if let Some(v) = headers.get("x-gitlab-event").and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }
    // Jira sends webhookEvent in the body, not headers — handled by the LLM
    None
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

fn build_webhook_prompt(
    token_name: &str,
    platform: &str,
    event: Option<&str>,
    scopes: &str,
    content_type: &str,
    body: &str,
) -> String {
    let truncated = if body.len() > MAX_PROMPT_PAYLOAD {
        format!(
            "{}\n\n[... truncated, {} bytes total]",
            &body[..MAX_PROMPT_PAYLOAD],
            body.len()
        )
    } else {
        body.to_string()
    };

    let scope_line = if scopes == "*" {
        "Scoped skills: all (global token)".to_string()
    } else {
        format!("Scoped skills: {scopes}")
    };

    let event_line = event
        .map(|e| format!("Event: {e}\n"))
        .unwrap_or_default();

    format!(
        "[Webhook received]\n\
         Token: {token_name}\n\
         Platform: {platform}\n\
         {event_line}\
         {scope_line}\n\
         Content-Type: {content_type}\n\n\
         Payload:\n{truncated}\n\n\
         Process this webhook payload and take appropriate action."
    )
}

// ---------------------------------------------------------------------------
// Token CRUD (JWT-authenticated, dashboard-only)
// ---------------------------------------------------------------------------

pub async fn create_token(
    State(state): State<DashState>,
    Json(body): Json<CreateTokenBody>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), StatusCode> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let raw_token = generate_token();
    let hash = hash_token(&raw_token);

    let db = state.db.lock().await;
    db.execute(
        "INSERT INTO api_tokens (id, name, token_hash, scopes) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, name, hash, body.scopes],
    )
    .map_err(|e| {
        error!("create token: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    info!(id = %id, name = %name, scopes = %body.scopes, "api token created");

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id,
            name,
            token: raw_token,
            scopes: body.scopes,
        }),
    ))
}

pub async fn list_tokens(
    State(state): State<DashState>,
) -> Result<Json<Vec<ApiToken>>, StatusCode> {
    let db = state.db.lock().await;
    let mut stmt = db
        .prepare(
            "SELECT id, name, scopes, created_at, last_used, enabled
             FROM api_tokens ORDER BY created_at DESC",
        )
        .map_err(|e| {
            error!("list tokens: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let tokens: Vec<ApiToken> = stmt
        .query_map([], |row| {
            Ok(ApiToken {
                id: row.get(0)?,
                name: row.get(1)?,
                scopes: row.get(2)?,
                created_at: row.get(3)?,
                last_used: row.get(4)?,
                enabled: row.get::<_, i32>(5)? != 0,
            })
        })
        .map_err(|e| {
            error!("list tokens query: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(Json(tokens))
}

pub async fn delete_token(
    State(state): State<DashState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;
    let changed = db
        .execute("DELETE FROM api_tokens WHERE id = ?1", [&id])
        .map_err(|e| {
            error!("delete token: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if changed == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    info!(id = %id, "api token deleted");
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn update_token(
    State(state): State<DashState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateTokenBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let db = state.db.lock().await;

    if let Some(ref name) = body.name {
        db.execute(
            "UPDATE api_tokens SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(ref scopes) = body.scopes {
        db.execute(
            "UPDATE api_tokens SET scopes = ?1 WHERE id = ?2",
            rusqlite::params![scopes, id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(enabled) = body.enabled {
        db.execute(
            "UPDATE api_tokens SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![enabled as i32, id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    info!(id = %id, "api token updated");
    Ok(Json(serde_json::json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// Token generation
// ---------------------------------------------------------------------------

fn generate_token() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    format!("{TOKEN_PREFIX}{}", data_encoding::HEXLOWER.encode(&bytes))
}
