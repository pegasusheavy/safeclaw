use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, Json, Redirect};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::routes::DashState;

// ---------------------------------------------------------------------------
// Provider registry
// ---------------------------------------------------------------------------

/// Describes how to interact with a specific OAuth 2.0 provider.
#[derive(Clone)]
pub struct OAuthProvider {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub userinfo_url: &'static str,
    pub default_scopes: &'static str,
    /// Environment variable names for client credentials.
    pub client_id_env: &'static str,
    pub client_secret_env: &'static str,
    /// Skill credential store key names.
    pub client_id_key: &'static str,
    pub client_secret_key: &'static str,
    /// How to extract the user's email/identity from the userinfo response.
    pub email_json_path: EmailPath,
    /// Extra query params to add to the auth URL.
    pub extra_auth_params: &'static [(&'static str, &'static str)],
    /// Token exchange quirks.
    pub token_exchange: TokenExchangeStyle,
    /// Userinfo request quirks.
    pub userinfo_method: UserInfoMethod,
}

#[derive(Clone, Copy)]
pub enum EmailPath {
    /// Top-level field name (e.g. "email")
    Field(&'static str),
    /// Nested: first try field A, then fall back to field B
    FieldOrFallback(&'static str, &'static str),
}

#[derive(Clone, Copy)]
pub enum TokenExchangeStyle {
    /// Standard form POST with client_id/client_secret in body.
    Standard,
    /// GitHub: needs Accept: application/json header.
    GitHubStyle,
    /// Notion: uses HTTP Basic auth (client_id:client_secret).
    BasicAuth,
}

#[derive(Clone, Copy)]
pub enum UserInfoMethod {
    /// GET with Bearer token.
    BearerGet,
    /// POST with Bearer token and empty body (Dropbox).
    BearerPost,
    /// GET with token in query param (Slack uses a different field).
    SlackStyle,
}

/// All supported OAuth providers.
pub static PROVIDERS: &[OAuthProvider] = &[
    OAuthProvider {
        id: "google",
        name: "Google",
        icon: "fa-brands fa-google",
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        userinfo_url: "https://www.googleapis.com/oauth2/v2/userinfo",
        default_scopes: "https://www.googleapis.com/auth/calendar https://www.googleapis.com/auth/gmail.send https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/userinfo.email",
        client_id_env: "GOOGLE_CLIENT_ID",
        client_secret_env: "GOOGLE_CLIENT_SECRET",
        client_id_key: "GOOGLE_CLIENT_ID",
        client_secret_key: "GOOGLE_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[("access_type", "offline"), ("prompt", "consent")],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "microsoft",
        name: "Microsoft",
        icon: "fa-brands fa-microsoft",
        auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        userinfo_url: "https://graph.microsoft.com/v1.0/me",
        default_scopes: "User.Read Mail.Read Mail.Send Calendars.Read Files.Read openid email profile offline_access",
        client_id_env: "MICROSOFT_CLIENT_ID",
        client_secret_env: "MICROSOFT_CLIENT_SECRET",
        client_id_key: "MICROSOFT_CLIENT_ID",
        client_secret_key: "MICROSOFT_CLIENT_SECRET",
        email_json_path: EmailPath::FieldOrFallback("mail", "userPrincipalName"),
        extra_auth_params: &[("prompt", "consent")],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "github",
        name: "GitHub",
        icon: "fa-brands fa-github",
        auth_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        userinfo_url: "https://api.github.com/user",
        default_scopes: "user:email repo read:org",
        client_id_env: "GITHUB_CLIENT_ID",
        client_secret_env: "GITHUB_CLIENT_SECRET",
        client_id_key: "GITHUB_CLIENT_ID",
        client_secret_key: "GITHUB_CLIENT_SECRET",
        email_json_path: EmailPath::FieldOrFallback("email", "login"),
        extra_auth_params: &[],
        token_exchange: TokenExchangeStyle::GitHubStyle,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "slack",
        name: "Slack",
        icon: "fa-brands fa-slack",
        auth_url: "https://slack.com/oauth/v2/authorize",
        token_url: "https://slack.com/api/oauth.v2.access",
        userinfo_url: "https://slack.com/api/users.identity",
        default_scopes: "users:read channels:read chat:write",
        client_id_env: "SLACK_CLIENT_ID",
        client_secret_env: "SLACK_CLIENT_SECRET",
        client_id_key: "SLACK_CLIENT_ID",
        client_secret_key: "SLACK_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::SlackStyle,
    },
    OAuthProvider {
        id: "discord",
        name: "Discord",
        icon: "fa-brands fa-discord",
        auth_url: "https://discord.com/api/oauth2/authorize",
        token_url: "https://discord.com/api/oauth2/token",
        userinfo_url: "https://discord.com/api/users/@me",
        default_scopes: "identify email guilds",
        client_id_env: "DISCORD_CLIENT_ID",
        client_secret_env: "DISCORD_CLIENT_SECRET",
        client_id_key: "DISCORD_CLIENT_ID",
        client_secret_key: "DISCORD_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "spotify",
        name: "Spotify",
        icon: "fa-brands fa-spotify",
        auth_url: "https://accounts.spotify.com/authorize",
        token_url: "https://accounts.spotify.com/api/token",
        userinfo_url: "https://api.spotify.com/v1/me",
        default_scopes: "user-read-email user-read-private playlist-read-private",
        client_id_env: "SPOTIFY_CLIENT_ID",
        client_secret_env: "SPOTIFY_CLIENT_SECRET",
        client_id_key: "SPOTIFY_CLIENT_ID",
        client_secret_key: "SPOTIFY_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "dropbox",
        name: "Dropbox",
        icon: "fa-brands fa-dropbox",
        auth_url: "https://www.dropbox.com/oauth2/authorize",
        token_url: "https://api.dropboxapi.com/oauth2/token",
        userinfo_url: "https://api.dropboxapi.com/2/users/get_current_account",
        default_scopes: "",
        client_id_env: "DROPBOX_CLIENT_ID",
        client_secret_env: "DROPBOX_CLIENT_SECRET",
        client_id_key: "DROPBOX_CLIENT_ID",
        client_secret_key: "DROPBOX_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[("token_access_type", "offline")],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerPost,
    },
    OAuthProvider {
        id: "twitter",
        name: "Twitter / X",
        icon: "fa-brands fa-x-twitter",
        auth_url: "https://twitter.com/i/oauth2/authorize",
        token_url: "https://api.twitter.com/2/oauth2/token",
        userinfo_url: "https://api.twitter.com/2/users/me?user.fields=username",
        default_scopes: "tweet.read users.read offline.access",
        client_id_env: "TWITTER_CLIENT_ID",
        client_secret_env: "TWITTER_CLIENT_SECRET",
        client_id_key: "TWITTER_CLIENT_ID",
        client_secret_key: "TWITTER_CLIENT_SECRET",
        email_json_path: EmailPath::Field("username"),
        extra_auth_params: &[("code_challenge_method", "plain")],
        token_exchange: TokenExchangeStyle::BasicAuth,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "linkedin",
        name: "LinkedIn",
        icon: "fa-brands fa-linkedin",
        auth_url: "https://www.linkedin.com/oauth/v2/authorization",
        token_url: "https://www.linkedin.com/oauth/v2/accessToken",
        userinfo_url: "https://api.linkedin.com/v2/userinfo",
        default_scopes: "openid profile email",
        client_id_env: "LINKEDIN_CLIENT_ID",
        client_secret_env: "LINKEDIN_CLIENT_SECRET",
        client_id_key: "LINKEDIN_CLIENT_ID",
        client_secret_key: "LINKEDIN_CLIENT_SECRET",
        email_json_path: EmailPath::Field("email"),
        extra_auth_params: &[],
        token_exchange: TokenExchangeStyle::Standard,
        userinfo_method: UserInfoMethod::BearerGet,
    },
    OAuthProvider {
        id: "notion",
        name: "Notion",
        icon: "fa-solid fa-n",
        auth_url: "https://api.notion.com/v1/oauth/authorize",
        token_url: "https://api.notion.com/v1/oauth/token",
        userinfo_url: "",
        default_scopes: "",
        client_id_env: "NOTION_CLIENT_ID",
        client_secret_env: "NOTION_CLIENT_SECRET",
        client_id_key: "NOTION_CLIENT_ID",
        client_secret_key: "NOTION_CLIENT_SECRET",
        email_json_path: EmailPath::Field("owner"),
        extra_auth_params: &[("owner", "user")],
        token_exchange: TokenExchangeStyle::BasicAuth,
        userinfo_method: UserInfoMethod::BearerGet,
    },
];

pub fn find_provider(id: &str) -> Option<&'static OAuthProvider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

// ---------------------------------------------------------------------------
// Credential resolution
// ---------------------------------------------------------------------------

fn provider_client_credentials(state: &DashState, provider: &OAuthProvider) -> Option<(String, String)> {
    // Try env vars first
    if let (Ok(id), Ok(secret)) = (
        std::env::var(provider.client_id_env),
        std::env::var(provider.client_secret_env),
    ) {
        if !id.is_empty() && !secret.is_empty() {
            return Some((id, secret));
        }
    }

    // Fall back to skill credential store (use provider id as skill name)
    let skill_name = format!("{}-oauth", provider.id);
    let sm = state.agent.skill_manager.try_lock().ok()?;
    let creds = sm.get_credentials(&skill_name);
    let id = creds.get(provider.client_id_key)?.clone();
    let secret = creds.get(provider.client_secret_key)?.clone();
    if id.is_empty() || secret.is_empty() {
        // Also try the google-oauth skill name for backwards compat
        if provider.id == "google" {
            let creds2 = sm.get_credentials("google-oauth");
            let id2 = creds2.get("GOOGLE_CLIENT_ID")?.clone();
            let secret2 = creds2.get("GOOGLE_CLIENT_SECRET")?.clone();
            if !id2.is_empty() && !secret2.is_empty() {
                return Some((id2, secret2));
            }
        }
        return None;
    }
    Some((id, secret))
}

fn callback_url(provider_id: &str) -> String {
    if let Ok(tunnel) = std::env::var("TUNNEL_URL") {
        if !tunnel.is_empty() {
            return format!("{tunnel}/oauth/{provider_id}/callback");
        }
    }
    let bind = std::env::var("DASHBOARD_BIND").unwrap_or_else(|_| "http://localhost:3031".into());
    format!("{bind}/oauth/{provider_id}/callback")
}

// ---------------------------------------------------------------------------
// Userinfo fetching
// ---------------------------------------------------------------------------

async fn fetch_user_identity(provider: &OAuthProvider, access_token: &str) -> Option<String> {
    if provider.userinfo_url.is_empty() {
        // Notion: extract from token response instead
        return None;
    }

    let client = reqwest::Client::new();
    let resp = match provider.userinfo_method {
        UserInfoMethod::BearerGet => {
            client.get(provider.userinfo_url)
                .bearer_auth(access_token)
                .header("User-Agent", "safeclaw/1.0")
                .send().await.ok()?
        }
        UserInfoMethod::BearerPost => {
            client.post(provider.userinfo_url)
                .bearer_auth(access_token)
                .header("User-Agent", "safeclaw/1.0")
                .send().await.ok()?
        }
        UserInfoMethod::SlackStyle => {
            client.get(provider.userinfo_url)
                .bearer_auth(access_token)
                .header("User-Agent", "safeclaw/1.0")
                .send().await.ok()?
        }
    };

    if !resp.status().is_success() {
        warn!(provider = provider.id, status = %resp.status(), "userinfo request failed");
        return None;
    }

    let json: serde_json::Value = resp.json().await.ok()?;

    // Slack nests user info under "user"
    let root = if provider.id == "slack" {
        json.get("user").unwrap_or(&json)
    } else if provider.id == "twitter" {
        json.get("data").unwrap_or(&json)
    } else {
        &json
    };

    match provider.email_json_path {
        EmailPath::Field(key) => root.get(key).and_then(|v| v.as_str()).map(|s| s.to_string()),
        EmailPath::FieldOrFallback(a, b) => {
            root.get(a).and_then(|v| v.as_str())
                .or_else(|| root.get(b).and_then(|v| v.as_str()))
                .map(|s| s.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

async fn exchange_code(
    provider: &OAuthProvider,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, String> {
    let client = reqwest::Client::new();

    let mut form = vec![
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    let req = match provider.token_exchange {
        TokenExchangeStyle::Standard => {
            form.push(("client_id", client_id));
            form.push(("client_secret", client_secret));
            client.post(provider.token_url).form(&form)
        }
        TokenExchangeStyle::GitHubStyle => {
            form.push(("client_id", client_id));
            form.push(("client_secret", client_secret));
            client.post(provider.token_url)
                .form(&form)
                .header("Accept", "application/json")
        }
        TokenExchangeStyle::BasicAuth => {
            client.post(provider.token_url)
                .form(&form)
                .basic_auth(client_id, Some(client_secret))
        }
    };

    // Twitter PKCE: include code_verifier if we used plain challenge
    let req = if provider.id == "twitter" {
        req.query(&[("code_verifier", "challenge")])
    } else {
        req
    };

    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token endpoint error: {body}"));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| format!("parse error: {e}"))?;

    // Notion puts token in "access_token" at top level but identity in "owner.user.person.email"
    let email_from_token = if provider.id == "notion" {
        json.pointer("/owner/user/person/email")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    Ok(TokenResponse {
        access_token: json.get("access_token").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        refresh_token: json.get("refresh_token").and_then(|v| v.as_str()).map(|s| s.to_string()),
        expires_in: json.get("expires_in").and_then(|v| v.as_u64()),
        email_hint: email_from_token,
    })
}

struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    email_hint: Option<String>,
}

// ---------------------------------------------------------------------------
// GET /oauth/{provider}/start
// ---------------------------------------------------------------------------

pub async fn oauth_start(
    State(state): State<DashState>,
    Path(provider_id): Path<String>,
) -> Result<Redirect, (StatusCode, Html<String>)> {
    let provider = find_provider(&provider_id).ok_or_else(|| {
        (StatusCode::NOT_FOUND, Html(format!("<h2>Unknown provider: {provider_id}</h2>")))
    })?;

    let (client_id, _) = provider_client_credentials(&state, provider).ok_or_else(|| {
        (
            StatusCode::PRECONDITION_FAILED,
            Html(format!(
                "<h2>{} OAuth not configured</h2><p>Set {} and {} in environment variables or skill credentials.</p>",
                provider.name, provider.client_id_env, provider.client_secret_env,
            )),
        )
    })?;

    let redirect_uri = callback_url(&provider_id);

    let mut url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}",
        provider.auth_url,
        urlencoding(&client_id),
        urlencoding(&redirect_uri),
        urlencoding(provider.default_scopes),
    );

    for (key, value) in provider.extra_auth_params {
        // Twitter PKCE: code_challenge = "challenge" with method "plain"
        if *key == "code_challenge_method" {
            url.push_str(&format!("&code_challenge_method={value}&code_challenge=challenge"));
            continue;
        }
        url.push_str(&format!("&{key}={}", urlencoding(value)));
    }

    info!(provider = provider.id, redirect_uri = %redirect_uri, "starting OAuth flow");
    Ok(Redirect::temporary(&url))
}

// ---------------------------------------------------------------------------
// GET /oauth/{provider}/callback
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CallbackParams {
    code: Option<String>,
    error: Option<String>,
}

pub async fn oauth_callback(
    State(state): State<DashState>,
    Path(provider_id): Path<String>,
    Query(params): Query<CallbackParams>,
) -> Result<Html<String>, (StatusCode, Html<String>)> {
    let provider = find_provider(&provider_id).ok_or_else(|| {
        (StatusCode::NOT_FOUND, Html(format!("<h2>Unknown provider: {provider_id}</h2>")))
    })?;

    if let Some(err) = params.error {
        warn!(provider = provider.id, error = %err, "OAuth error");
        return Ok(Html(format!(
            "<h2>{} OAuth Error</h2><p>{err}</p><p><a href=\"/\">Back to dashboard</a></p>",
            provider.name,
        )));
    }

    let code = params.code.ok_or_else(|| {
        (StatusCode::BAD_REQUEST, Html("<h2>Missing authorization code</h2>".into()))
    })?;

    let (client_id, client_secret) = provider_client_credentials(&state, provider).ok_or_else(|| {
        (StatusCode::PRECONDITION_FAILED, Html(format!("<h2>{} OAuth not configured</h2>", provider.name)))
    })?;

    let redirect_uri = callback_url(&provider_id);

    let token_data = exchange_code(provider, &client_id, &client_secret, &code, &redirect_uri)
        .await
        .map_err(|e| {
            error!(provider = provider.id, err = %e, "token exchange failed");
            (StatusCode::BAD_GATEWAY, Html(format!("<h2>Token exchange error</h2><pre>{e}</pre>")))
        })?;

    if token_data.access_token.is_empty() {
        return Ok(Html(format!(
            "<h2>{} OAuth Error</h2><p>No access token received</p><p><a href=\"/\">Back</a></p>",
            provider.name,
        )));
    }

    // Identify the account
    let email = token_data.email_hint
        .or_else(|| {
            // Block on async in sync context — we're already in an async fn
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(
                    fetch_user_identity(provider, &token_data.access_token)
                )
            })
        })
        .unwrap_or_else(|| "unknown".to_string());

    let account_id = sanitize_account_id(&email);

    info!(provider = provider.id, email = %email, account = %account_id, "OAuth tokens received");

    // Store in database
    {
        let db = state.db.lock().await;
        let expires_at = token_data.expires_in.map(|secs| {
            (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        });

        if let Err(e) = db.execute(
            "INSERT OR REPLACE INTO oauth_tokens (provider, account, email, access_token, refresh_token, expires_at, scopes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
            rusqlite::params![
                provider.id,
                &account_id,
                &email,
                token_data.access_token,
                token_data.refresh_token,
                expires_at,
                provider.default_scopes,
            ],
        ) {
            error!(err = %e, "failed to store OAuth tokens");
        }
    }

    // Write credential files for skills and regenerate the discovery manifest
    let _ = write_provider_credentials(
        provider,
        &account_id,
        &client_id,
        &client_secret,
        &token_data.access_token,
        token_data.refresh_token.as_deref(),
        provider.default_scopes,
    );

    Ok(Html(format!(
        r#"<!DOCTYPE html>
<html><head><title>{name} OAuth Success</title>
<style>body{{font-family:system-ui;background:#1a1a1a;color:#e0e0e0;display:flex;justify-content:center;align-items:center;height:100vh;margin:0}}
.card{{background:#2a2a2a;border-radius:12px;padding:2rem 3rem;text-align:center;box-shadow:0 4px 20px rgba(0,0,0,.5)}}
h2{{color:#4caf50}}a{{color:#ff9800;text-decoration:none}}.email{{color:#64b5f6;font-weight:bold}}</style></head>
<body><div class="card">
<h2>{name} Account Connected</h2>
<p>Account <span class="email">{email}</span> authorized successfully.</p>
<p><a href="/">Back to Dashboard</a></p>
</div></body></html>"#,
        name = provider.name,
        email = email,
    )))
}

// ---------------------------------------------------------------------------
// GET /api/oauth/status — all providers, all accounts
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AllOAuthStatus {
    pub providers: Vec<ProviderStatus>,
}

#[derive(Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub configured: bool,
    pub authorize_url: String,
    pub accounts: Vec<OAuthAccount>,
}

#[derive(Serialize)]
pub struct OAuthAccount {
    pub account: String,
    pub email: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
    pub updated_at: Option<String>,
    pub has_refresh_token: bool,
}

pub async fn all_oauth_status(
    State(state): State<DashState>,
) -> Json<AllOAuthStatus> {
    let db = state.db.lock().await;

    let mut providers = Vec::new();

    for p in PROVIDERS {
        let configured = provider_client_credentials(&state, p).is_some();

        let accounts: Vec<OAuthAccount> = match db.prepare(
            "SELECT account, email, refresh_token, expires_at, scopes, updated_at
             FROM oauth_tokens WHERE provider = ?1 ORDER BY email",
        ) {
            Ok(mut stmt) => {
                match stmt.query_map(rusqlite::params![p.id], |row| {
                    Ok(OAuthAccount {
                        account: row.get(0)?,
                        email: row.get(1)?,
                        has_refresh_token: row.get::<_, Option<String>>(2)?.is_some(),
                        expires_at: row.get(3)?,
                        scopes: row.get::<_, String>(4)?
                            .split(' ')
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect(),
                        updated_at: row.get(5)?,
                    })
                }) {
                    Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                    Err(_) => vec![],
                }
            }
            Err(_) => vec![],
        };

        providers.push(ProviderStatus {
            id: p.id.to_string(),
            name: p.name.to_string(),
            icon: p.icon.to_string(),
            configured,
            authorize_url: format!("/oauth/{}/start", p.id),
            accounts,
        });
    }

    Json(AllOAuthStatus { providers })
}

// ---------------------------------------------------------------------------
// POST /api/oauth/{provider}/refresh
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RefreshQuery {
    pub account: Option<String>,
}

pub async fn oauth_refresh(
    State(state): State<DashState>,
    Path(provider_id): Path<String>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let provider = find_provider(&provider_id).ok_or_else(|| {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "Unknown provider"})))
    })?;

    let (client_id, client_secret) = provider_client_credentials(&state, provider).ok_or_else(|| {
        (StatusCode::PRECONDITION_FAILED, Json(serde_json::json!({"error": "Not configured"})))
    })?;

    let accounts: Vec<(String, String)> = {
        let db = state.db.lock().await;
        let mut stmt = db.prepare(
            "SELECT account, refresh_token FROM oauth_tokens WHERE provider = ?1 AND refresh_token IS NOT NULL",
        ).map_err(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "DB error"})))
        })?;

        stmt.query_map(rusqlite::params![provider.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Query error"})))
        })?
        .filter_map(|r| r.ok())
        .filter(|(acct, _)| query.account.as_ref().map_or(true, |a| a == acct))
        .collect()
    };

    if accounts.is_empty() {
        return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "No accounts with refresh tokens"}))));
    }

    let client = reqwest::Client::new();
    let mut refreshed = 0u32;
    let mut errors = Vec::new();

    for (account_id, refresh_token) in &accounts {
        let mut form = vec![
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];

        let req = match provider.token_exchange {
            TokenExchangeStyle::Standard | TokenExchangeStyle::GitHubStyle => {
                form.push(("client_id", client_id.as_str()));
                form.push(("client_secret", client_secret.as_str()));
                client.post(provider.token_url).form(&form)
            }
            TokenExchangeStyle::BasicAuth => {
                client.post(provider.token_url)
                    .form(&form)
                    .basic_auth(&client_id, Some(&client_secret))
            }
        };

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => { errors.push(format!("{account_id}: {e}")); continue; }
        };

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            errors.push(format!("{account_id}: {body}"));
            continue;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => { errors.push(format!("{account_id}: parse: {e}")); continue; }
        };

        let access_token = json.get("access_token").and_then(|v| v.as_str()).unwrap_or("");
        let expires_in = json.get("expires_in").and_then(|v| v.as_u64());
        let expires_at = expires_in.map(|secs| {
            (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        });

        {
            let db = state.db.lock().await;
            let _ = db.execute(
                "UPDATE oauth_tokens SET access_token = ?1, expires_at = ?2, updated_at = datetime('now') WHERE provider = ?3 AND account = ?4",
                rusqlite::params![access_token, expires_at, provider.id, account_id],
            );
        }

        let _ = write_provider_credentials(
            provider,
            account_id,
            &client_id,
            &client_secret,
            access_token,
            Some(refresh_token),
            provider.default_scopes,
        );

        refreshed += 1;
        info!(provider = provider.id, account = %account_id, "token refreshed");
    }

    Ok(Json(serde_json::json!({ "ok": errors.is_empty(), "refreshed": refreshed, "errors": errors })))
}

// ---------------------------------------------------------------------------
// POST /api/oauth/{provider}/disconnect/{account}
// ---------------------------------------------------------------------------

pub async fn oauth_disconnect(
    State(state): State<DashState>,
    Path((provider_id, account)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let db = state.db.lock().await;
    let _ = db.execute(
        "DELETE FROM oauth_tokens WHERE provider = ?1 AND account = ?2",
        rusqlite::params![provider_id, account],
    );
    drop(db);

    remove_provider_credentials(&provider_id, &account);

    info!(provider = %provider_id, account = %account, "OAuth account disconnected");
    Json(serde_json::json!({"ok": true, "provider": provider_id, "account": account}))
}

// ---------------------------------------------------------------------------
// GET /api/oauth/providers — lightweight list for UI
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub configured: bool,
}

pub async fn list_providers(
    State(state): State<DashState>,
) -> Json<Vec<ProviderInfo>> {
    Json(PROVIDERS.iter().map(|p| {
        ProviderInfo {
            id: p.id.to_string(),
            name: p.name.to_string(),
            icon: p.icon.to_string(),
            configured: provider_client_credentials(&state, p).is_some(),
        }
    }).collect())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn sanitize_account_id(email: &str) -> String {
    email
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '@' { c } else { '_' })
        .collect()
}

/// Write provider-agnostic OAuth credential files and update the discovery manifest.
///
/// Stores credentials under `$DATA_DIR/oauth/{provider}/{account}.json` in a
/// uniform JSON format.  For Google, also writes the `authorized_user` format
/// to the legacy skill directories for backward compatibility.
fn write_provider_credentials(
    provider: &OAuthProvider,
    account_id: &str,
    client_id: &str,
    client_secret: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    scopes: &str,
) -> std::io::Result<()> {
    let data_dir = crate::config::Config::data_dir();
    let filename = format!("{account_id}.json");

    // --- 1. Universal format: $DATA_DIR/oauth/{provider}/{account}.json ---
    let provider_dir = data_dir.join("oauth").join(provider.id);
    std::fs::create_dir_all(&provider_dir)?;

    let universal_json = serde_json::json!({
        "provider": provider.id,
        "account": account_id,
        "access_token": access_token,
        "refresh_token": refresh_token.unwrap_or(""),
        "client_id": client_id,
        "client_secret": client_secret,
        "token_url": provider.token_url,
        "scopes": scopes,
    });

    let json_str = serde_json::to_string_pretty(&universal_json)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(provider_dir.join(&filename), &json_str)?;

    // --- 2. Google backward-compat: write `authorized_user` format ---
    if provider.id == "google" {
        let compat_json = serde_json::json!({
            "type": "authorized_user",
            "client_id": client_id,
            "client_secret": client_secret,
            "refresh_token": refresh_token.unwrap_or(""),
            "token": access_token,
            "token_uri": "https://oauth2.googleapis.com/token",
            "account": account_id,
        });
        let compat_str = serde_json::to_string_pretty(&compat_json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let skills_dir = data_dir.join("skills");

        let oauth_accounts = skills_dir.join("google-oauth/data/accounts");
        std::fs::create_dir_all(&oauth_accounts)?;
        std::fs::write(oauth_accounts.join(&filename), &compat_str)?;

        let cal_creds = skills_dir.join("calendar-reminder/data/credentials");
        std::fs::create_dir_all(&cal_creds)?;
        std::fs::write(cal_creds.join(&filename), &compat_str)?;
    }

    // --- 3. Regenerate the discovery manifest ---
    let _ = regenerate_oauth_manifest(&data_dir);

    info!(provider = provider.id, account = %account_id, "wrote OAuth credentials");
    Ok(())
}

/// Remove credential files for a disconnected account.
fn remove_provider_credentials(provider_id: &str, account_id: &str) {
    let data_dir = crate::config::Config::data_dir();
    let filename = format!("{account_id}.json");

    // Remove universal token file
    let _ = std::fs::remove_file(data_dir.join("oauth").join(provider_id).join(&filename));

    // Remove Google legacy files
    if provider_id == "google" {
        let skills_dir = data_dir.join("skills");
        let _ = std::fs::remove_file(skills_dir.join("google-oauth/data/accounts").join(&filename));
        let _ = std::fs::remove_file(skills_dir.join("calendar-reminder/data/credentials").join(&filename));
    }

    // Regenerate manifest
    let _ = regenerate_oauth_manifest(&data_dir);
}

/// Map OAuth scopes to human-readable capabilities.
fn scopes_to_capabilities(provider_id: &str, scopes: &str) -> Vec<String> {
    let mut caps = Vec::new();
    let s = scopes.to_lowercase();

    match provider_id {
        "google" => {
            if s.contains("calendar") { caps.push("calendar".into()); }
            if s.contains("gmail") || s.contains("mail") { caps.push("email".into()); }
            if s.contains("drive") { caps.push("files".into()); }
            if s.contains("contacts") { caps.push("contacts".into()); }
        }
        "microsoft" => {
            if s.contains("calendars") { caps.push("calendar".into()); }
            if s.contains("mail") { caps.push("email".into()); }
            if s.contains("files") { caps.push("files".into()); }
            if s.contains("contacts") { caps.push("contacts".into()); }
        }
        "github" => {
            if s.contains("repo") { caps.push("repositories".into()); }
            if s.contains("org") { caps.push("organizations".into()); }
        }
        "slack" => {
            if s.contains("chat") { caps.push("messaging".into()); }
            if s.contains("channels") { caps.push("channels".into()); }
        }
        "discord" => {
            if s.contains("guilds") { caps.push("servers".into()); }
        }
        "spotify" => {
            if s.contains("playlist") { caps.push("playlists".into()); }
            if s.contains("user-read") { caps.push("profile".into()); }
        }
        "dropbox" => {
            caps.push("files".into());
        }
        "notion" => {
            caps.push("pages".into());
            caps.push("databases".into());
        }
        _ => {}
    }

    if caps.is_empty() {
        caps.push("auth".into());
    }
    caps
}

/// Rebuild `$DATA_DIR/oauth/manifest.json` from the per-provider token files.
fn regenerate_oauth_manifest(data_dir: &std::path::Path) -> std::io::Result<()> {
    let oauth_dir = data_dir.join("oauth");
    if !oauth_dir.exists() {
        return Ok(());
    }

    let mut accounts = Vec::new();

    if let Ok(providers) = std::fs::read_dir(&oauth_dir) {
        for entry in providers.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue; // skip manifest.json itself
            }
            let provider_id = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            if let Ok(files) = std::fs::read_dir(&path) {
                for file_entry in files.flatten() {
                    let fp = file_entry.path();
                    if fp.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(&fp) {
                        if let Ok(token) = serde_json::from_str::<serde_json::Value>(&content) {
                            let account = token.get("account")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let scopes = token.get("scopes")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let capabilities = scopes_to_capabilities(&provider_id, &scopes);

                            accounts.push(serde_json::json!({
                                "provider": provider_id,
                                "account": account,
                                "scopes": scopes,
                                "capabilities": capabilities,
                                "token_file": fp.to_string_lossy(),
                            }));
                        }
                    }
                }
            }
        }
    }

    let manifest = serde_json::json!({
        "description": "Connected OAuth accounts — read by the agent to discover available services",
        "accounts": accounts,
    });

    let manifest_str = serde_json::to_string_pretty(&manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(oauth_dir.join("manifest.json"), manifest_str)?;

    info!(count = accounts.len(), "regenerated OAuth manifest");
    Ok(())
}

fn urlencoding(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace(':', "%3A")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('@', "%40")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_provider_known_ids() {
        let known = ["google", "microsoft", "github", "slack", "discord",
                      "spotify", "dropbox", "twitter", "linkedin", "notion"];
        for id in known {
            let p = find_provider(id);
            assert!(p.is_some(), "provider '{}' should exist", id);
            assert_eq!(p.unwrap().id, id);
        }
    }

    #[test]
    fn find_provider_unknown() {
        assert!(find_provider("nonexistent").is_none());
        assert!(find_provider("").is_none());
    }

    #[test]
    fn provider_fields_non_empty() {
        for p in PROVIDERS.iter() {
            assert!(!p.id.is_empty());
            assert!(!p.name.is_empty());
            assert!(!p.auth_url.is_empty());
            assert!(!p.token_url.is_empty());
            assert!(!p.client_id_env.is_empty());
            assert!(!p.client_secret_env.is_empty());
        }
    }

    #[test]
    fn providers_have_unique_ids() {
        let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), PROVIDERS.len(), "provider IDs must be unique");
    }

    #[test]
    fn urlencoding_special_chars() {
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoding("https://x.com"), "https%3A%2F%2Fx.com");
    }

    #[test]
    fn urlencoding_empty() {
        assert_eq!(urlencoding(""), "");
    }

    #[test]
    fn email_path_field() {
        let path = EmailPath::Field("email");
        match path {
            EmailPath::Field(f) => assert_eq!(f, "email"),
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn email_path_field_or_fallback() {
        let path = EmailPath::FieldOrFallback("email", "mail");
        match path {
            EmailPath::FieldOrFallback(a, b) => {
                assert_eq!(a, "email");
                assert_eq!(b, "mail");
            }
            _ => panic!("expected FieldOrFallback"),
        }
    }
}
