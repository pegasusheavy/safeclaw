use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use super::routes::DashState;

// ---------------------------------------------------------------------------
// POST /api/messaging/incoming
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct IncomingMessage {
    pub platform: String,
    pub channel: String,
    pub sender: String,
    pub text: String,
    /// Whether this message is from a group chat.
    #[serde(default)]
    pub is_group: bool,
    /// Whether the agent was @mentioned or directly replied to.
    #[serde(default)]
    pub is_mentioned: bool,
}

#[derive(Serialize)]
pub struct IncomingResponse {
    pub reply: Option<String>,
}

pub async fn incoming(
    State(state): State<DashState>,
    Json(body): Json<IncomingMessage>,
) -> (StatusCode, Json<IncomingResponse>) {
    info!(
        platform = %body.platform,
        channel = %body.channel,
        sender = %body.sender,
        is_group = body.is_group,
        is_mentioned = body.is_mentioned,
        "incoming message via webhook"
    );

    // Group message gating: only respond if mentioned or replied to
    if body.is_group && !body.is_mentioned {
        return (
            StatusCode::OK,
            Json(IncomingResponse { reply: None }),
        );
    }

    // Look up user by platform identity for multi-user routing
    let user_ctx = match body.platform.as_str() {
        "whatsapp" => {
            state.agent.user_manager.get_by_whatsapp_id(&body.sender).await
                .map(|u| crate::users::UserContext::from_user(&u, "whatsapp"))
        }
        "telegram" => {
            // sender might be a chat_id string — try parsing as i64
            body.sender.parse::<i64>().ok()
                .and_then(|_id| {
                    // Use a blocking approach since we're already in an async context
                    // but get_by_telegram_id is also async
                    None::<crate::users::User> // Will be resolved below
                })
                .map(|u| crate::users::UserContext::from_user(&u, "telegram"))
        }
        "imessage" => {
            state.agent.user_manager.get_by_imessage_id(&body.sender).await
                .map(|u| crate::users::UserContext::from_user(&u, "imessage"))
        }
        "android_sms" => {
            state.agent.user_manager.get_by_android_sms_id(&body.sender).await
                .map(|u| crate::users::UserContext::from_user(&u, "android_sms"))
        }
        "discord" => {
            state.agent.user_manager.get_by_discord_id(&body.sender).await
                .map(|u| crate::users::UserContext::from_user(&u, "discord"))
        }
        "signal" => {
            state.agent.user_manager.get_by_signal_id(&body.sender).await
                .map(|u| crate::users::UserContext::from_user(&u, "signal"))
        }
        _ => None,
    };

    // For telegram, we need a separate async lookup
    let user_ctx = if user_ctx.is_none() && body.platform == "telegram" {
        if let Ok(tg_id) = body.sender.parse::<i64>() {
            state.agent.user_manager.get_by_telegram_id(tg_id).await
                .map(|u| crate::users::UserContext::from_user(&u, "telegram"))
        } else {
            None
        }
    } else {
        user_ctx
    };

    // Strip @mention prefix so the agent sees clean text
    let clean_text = if body.is_mentioned {
        strip_mention(&body.text)
    } else {
        body.text.clone()
    };

    // Send the message to the agent for processing
    match state.agent.handle_message_as(&clean_text, user_ctx.as_ref()).await {
        Ok(reply) => {
            // Also send the reply back through the platform's backend
            if let Some(backend) = state.messaging.get(&body.platform) {
                if let Err(e) = backend.send_message(&body.channel, &reply).await {
                    error!(platform = %body.platform, err = %e, "failed to relay reply");
                }
            }

            (
                StatusCode::OK,
                Json(IncomingResponse { reply: Some(reply) }),
            )
        }
        Err(e) => {
            error!("agent handle_message failed: {e}");
            let error_msg = format!("⚠️ Error: {e}");

            // Send error back through the platform
            if let Some(backend) = state.messaging.get(&body.platform) {
                let _ = backend.send_message(&body.channel, &error_msg).await;
            }

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(IncomingResponse {
                    reply: Some(error_msg),
                }),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Mention stripping
// ---------------------------------------------------------------------------

/// Remove common @mention patterns from the beginning of a message.
fn strip_mention(text: &str) -> String {
    let text = text.trim();
    // Discord-style <@123456789> mention
    if text.starts_with("<@") {
        if let Some(end) = text.find('>') {
            return text[end + 1..].trim_start().to_string();
        }
    }
    // @username mention — strip first word
    if text.starts_with('@') {
        if let Some(idx) = text.find(char::is_whitespace) {
            return text[idx..].trim_start().to_string();
        }
    }
    text.to_string()
}

// ---------------------------------------------------------------------------
// GET /api/messaging/whatsapp/status
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct WhatsAppStatus {
    pub enabled: bool,
    pub status: String,
    pub qr: Option<String>,
    pub details: Option<serde_json::Value>,
}

pub async fn whatsapp_status(
    State(state): State<DashState>,
) -> Json<WhatsAppStatus> {
    if !state.config.whatsapp.enabled {
        return Json(WhatsAppStatus {
            enabled: false,
            status: "disabled".to_string(),
            qr: None,
            details: None,
        });
    }

    // Try to query the bridge's status endpoint
    let client = reqwest::Client::new();
    let bridge_url = format!(
        "http://127.0.0.1:{}/status",
        state.config.whatsapp.bridge_port
    );

    match client.get(&bridge_url).timeout(std::time::Duration::from_secs(3)).send().await {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                let status_str = body
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let qr = body.get("qr").and_then(|v| v.as_str()).map(|s| s.to_string());

                Json(WhatsAppStatus {
                    enabled: true,
                    status: status_str,
                    qr,
                    details: Some(body),
                })
            } else {
                Json(WhatsAppStatus {
                    enabled: true,
                    status: "error".to_string(),
                    qr: None,
                    details: None,
                })
            }
        }
        Err(_) => Json(WhatsAppStatus {
            enabled: true,
            status: "bridge_unreachable".to_string(),
            qr: None,
            details: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// GET /api/messaging/whatsapp/qr
// ---------------------------------------------------------------------------

pub async fn whatsapp_qr(
    State(state): State<DashState>,
) -> (StatusCode, String) {
    if !state.config.whatsapp.enabled {
        return (StatusCode::NOT_FOUND, "WhatsApp not enabled".to_string());
    }

    let client = reqwest::Client::new();
    let bridge_url = format!(
        "http://127.0.0.1:{}/qr",
        state.config.whatsapp.bridge_port
    );

    match client.get(&bridge_url).timeout(std::time::Duration::from_secs(3)).send().await {
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            (StatusCode::OK, body)
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Bridge unreachable".to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// GET /api/messaging/platforms
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PlatformInfo {
    pub name: String,
    pub connected: bool,
}

pub async fn list_platforms(
    State(state): State<DashState>,
) -> Json<Vec<PlatformInfo>> {
    let mut platforms = Vec::new();

    if state.config.telegram.enabled {
        platforms.push(PlatformInfo {
            name: "telegram".to_string(),
            connected: state.messaging.get("telegram").is_some(),
        });
    }

    if state.config.whatsapp.enabled {
        platforms.push(PlatformInfo {
            name: "whatsapp".to_string(),
            connected: state.messaging.get("whatsapp").is_some(),
        });
    }

    if state.config.imessage.enabled {
        platforms.push(PlatformInfo {
            name: "imessage".to_string(),
            connected: state.messaging.get("imessage").is_some(),
        });
    }

    if state.config.twilio.enabled {
        platforms.push(PlatformInfo {
            name: "twilio".to_string(),
            connected: state.messaging.get("twilio").is_some(),
        });
    }

    if state.config.android_sms.enabled {
        platforms.push(PlatformInfo {
            name: "android_sms".to_string(),
            connected: state.messaging.get("android_sms").is_some(),
        });
    }

    if state.config.discord.enabled {
        platforms.push(PlatformInfo {
            name: "discord".to_string(),
            connected: state.messaging.get("discord").is_some(),
        });
    }

    if state.config.signal.enabled {
        platforms.push(PlatformInfo {
            name: "signal".to_string(),
            connected: state.messaging.get("signal").is_some(),
        });
    }

    Json(platforms)
}

// ---------------------------------------------------------------------------
// GET /api/messaging/config
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TelegramConfigInfo {
    pub enabled: bool,
    pub connected: bool,
    pub has_token: bool,
    pub allowed_chat_ids: Vec<i64>,
    pub primary_channel: Option<String>,
}

#[derive(Serialize)]
pub struct WhatsAppConfigInfo {
    pub enabled: bool,
    pub connected: bool,
    pub bridge_port: u16,
    pub webhook_port: u16,
    pub allowed_numbers: Vec<String>,
    pub primary_channel: Option<String>,
    pub bridge_status: String,
    pub qr: Option<String>,
    pub connected_number: Option<String>,
}

#[derive(Serialize)]
pub struct MessagingConfigResponse {
    pub telegram: TelegramConfigInfo,
    pub whatsapp: WhatsAppConfigInfo,
    pub active_platforms: Vec<String>,
}

pub async fn messaging_config(
    State(state): State<DashState>,
) -> Json<MessagingConfigResponse> {
    let has_token = std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
    let tg_connected = state.messaging.get("telegram").is_some();
    let tg_primary = state.messaging.primary_channel("telegram").map(|s| s.to_string());

    let wa_connected = state.messaging.get("whatsapp").is_some();
    let wa_primary = state.messaging.primary_channel("whatsapp").map(|s| s.to_string());

    // Query bridge status if WhatsApp is enabled
    let (bridge_status, qr, connected_number) = if state.config.whatsapp.enabled {
        let client = reqwest::Client::new();
        let bridge_url = format!(
            "http://127.0.0.1:{}/status",
            state.config.whatsapp.bridge_port
        );
        match client.get(&bridge_url).timeout(std::time::Duration::from_secs(3)).send().await {
            Ok(resp) => {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let st = body.get("state").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                    let q = body.get("qr").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let num = body.get("number").and_then(|v| v.as_str()).map(|s| s.to_string());
                    (st, q, num)
                } else {
                    ("error".to_string(), None, None)
                }
            }
            Err(_) => ("bridge_unreachable".to_string(), None, None),
        }
    } else {
        ("disabled".to_string(), None, None)
    };

    Json(MessagingConfigResponse {
        telegram: TelegramConfigInfo {
            enabled: state.config.telegram.enabled,
            connected: tg_connected,
            has_token,
            allowed_chat_ids: state.config.telegram.allowed_chat_ids.clone(),
            primary_channel: tg_primary,
        },
        whatsapp: WhatsAppConfigInfo {
            enabled: state.config.whatsapp.enabled,
            connected: wa_connected,
            bridge_port: state.config.whatsapp.bridge_port,
            webhook_port: state.config.whatsapp.webhook_port,
            allowed_numbers: state.config.whatsapp.allowed_numbers.clone(),
            primary_channel: wa_primary,
            bridge_status,
            qr,
            connected_number,
        },
        active_platforms: state.messaging.platforms().into_iter().map(|s| s.to_string()).collect(),
    })
}

// ---------------------------------------------------------------------------
// POST /api/messaging/twilio/incoming
// ---------------------------------------------------------------------------

/// Twilio sends incoming SMS as application/x-www-form-urlencoded with
/// fields: From, To, Body, MessageSid, etc.
#[derive(Deserialize)]
pub struct TwilioIncoming {
    #[serde(rename = "From")]
    pub from: String,
    #[serde(rename = "To")]
    pub to: String,
    #[serde(rename = "Body")]
    pub body: String,
}

pub async fn twilio_incoming(
    State(state): State<DashState>,
    axum::extract::Form(form): axum::extract::Form<TwilioIncoming>,
) -> (StatusCode, String) {
    info!(from = %form.from, to = %form.to, "incoming Twilio SMS");

    let user_ctx = state
        .agent
        .user_manager
        .get_by_twilio_number(&form.from)
        .await
        .map(|u| crate::users::UserContext::from_user(&u, "twilio"));

    match state
        .agent
        .handle_message_as(&form.body, user_ctx.as_ref())
        .await
    {
        Ok(reply) => {
            // Send reply back via Twilio backend
            if let Some(backend) = state.messaging.get("twilio") {
                if let Err(e) = backend.send_message(&form.from, &reply).await {
                    error!(err = %e, "failed to relay Twilio reply");
                }
            }

            // Return TwiML empty response (Twilio expects XML)
            (
                StatusCode::OK,
                "<Response></Response>".to_string(),
            )
        }
        Err(e) => {
            error!("agent handle_message failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "<Response></Response>".to_string(),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/messaging/slack/events — Slack Events API
// ---------------------------------------------------------------------------

pub async fn slack_events(
    State(state): State<DashState>,
    Json(payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    let result = crate::messaging::slack::handle_slack_event(
        payload,
        &state.config.slack,
        state.agent.clone(),
    )
    .await;
    (StatusCode::OK, Json(result))
}
