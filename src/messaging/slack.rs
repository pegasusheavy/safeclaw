use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error, info};

use crate::agent::Agent;
use crate::config::SlackConfig;
use crate::error::{Result, SafeAgentError};

use super::rich::{self, RichContent};
use super::{split_message, MessagingBackend};

/// Slack workspace bot backend using the Slack Web API.
///
/// Sending: `chat.postMessage` with optional Block Kit for rich content.
/// Receiving: Events API — Slack POSTs to `/api/messaging/slack/events`.
pub struct SlackBackend {
    bot_token: String,
    http: reqwest::Client,
}

impl SlackBackend {
    pub fn new(bot_token: String) -> Self {
        Self {
            bot_token,
            http: reqwest::Client::new(),
        }
    }

    /// Post a message with Block Kit blocks for rich content.
    async fn post_blocks(
        &self,
        channel: &str,
        text: &str,
        blocks: serde_json::Value,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "channel": channel,
            "text": text,
            "blocks": blocks,
        });

        let resp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("slack send failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("slack response parse: {e}")))?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(SafeAgentError::Messaging(format!("slack API error: {err}")));
        }

        Ok(())
    }
}

#[async_trait]
impl MessagingBackend for SlackBackend {
    fn platform_name(&self) -> &str {
        "slack"
    }

    fn max_message_length(&self) -> usize {
        4000
    }

    async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        for chunk in split_message(text, self.max_message_length()) {
            let payload = serde_json::json!({
                "channel": channel,
                "text": chunk,
            });

            let resp = self
                .http
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(&self.bot_token)
                .json(&payload)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("slack send failed: {e}")))?;

            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("slack response parse: {e}")))?;

            if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(SafeAgentError::Messaging(format!("slack API error: {err}")));
            }
        }
        Ok(())
    }

    async fn send_typing(&self, _channel: &str) -> Result<()> {
        Ok(())
    }

    fn supports_rich_messages(&self) -> bool {
        true
    }

    async fn send_rich(&self, channel: &str, content: &RichContent) -> Result<()> {
        match content {
            RichContent::Image { url, caption } => {
                let blocks = serde_json::json!([
                    {
                        "type": "image",
                        "image_url": url,
                        "alt_text": caption.as_deref().unwrap_or("image"),
                    }
                ]);
                let text = caption.as_deref().unwrap_or(url);
                self.post_blocks(channel, text, blocks).await
            }
            RichContent::Buttons { text, buttons } => {
                let elements: Vec<serde_json::Value> = buttons
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "type": "button",
                            "text": {"type": "plain_text", "text": b.label},
                            "url": b.data,
                        })
                    })
                    .collect();
                let blocks = serde_json::json!([
                    {"type": "section", "text": {"type": "mrkdwn", "text": text}},
                    {"type": "actions", "elements": elements},
                ]);
                self.post_blocks(channel, text, blocks).await
            }
            RichContent::Card {
                title,
                description,
                image_url,
                url,
            } => {
                let mut blocks = vec![serde_json::json!({
                    "type": "header",
                    "text": {"type": "plain_text", "text": title},
                })];
                if let Some(desc) = description {
                    blocks.push(serde_json::json!({
                        "type": "section",
                        "text": {"type": "mrkdwn", "text": desc},
                    }));
                }
                if let Some(img) = image_url {
                    blocks.push(serde_json::json!({
                        "type": "image",
                        "image_url": img,
                        "alt_text": title,
                    }));
                }
                if let Some(link) = url {
                    blocks.push(serde_json::json!({
                        "type": "section",
                        "text": {"type": "mrkdwn", "text": format!("<{link}|Open>")},
                    }));
                }
                self.post_blocks(channel, title, serde_json::json!(blocks))
                    .await
            }
            other => self.send_message(channel, &other.to_text_fallback()).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Events API handler (called from dashboard routes)
// ---------------------------------------------------------------------------

/// Handle a Slack Events API payload. Returns the response body.
pub async fn handle_slack_event(
    payload: serde_json::Value,
    config: &SlackConfig,
    agent: Arc<Agent>,
) -> serde_json::Value {
    // URL verification challenge
    if payload.get("type").and_then(|v| v.as_str()) == Some("url_verification") {
        let challenge = payload
            .get("challenge")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return serde_json::json!({"challenge": challenge});
    }

    // Event callback
    if payload.get("type").and_then(|v| v.as_str()) == Some("event_callback") {
        if let Some(event) = payload.get("event") {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if event_type == "message" || event_type == "app_mention" {
                let text = event
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let channel = event
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let user = event
                    .get("user")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Skip bot messages
                if event.get("bot_id").is_some()
                    || event.get("subtype").and_then(|v| v.as_str()) == Some("bot_message")
                {
                    return serde_json::json!({"ok": true});
                }

                // Authorization: check allowed channels
                if !config.allowed_channel_ids.is_empty()
                    && !config.allowed_channel_ids.contains(&channel)
                {
                    debug!(channel, "slack message from unauthorized channel");
                    return serde_json::json!({"ok": true});
                }

                info!(channel, user, "slack message received");
                tokio::spawn(async move {
                    match agent.handle_message_as(&text, None).await {
                        Ok(_reply) => {
                            info!("slack message processed");
                        }
                        Err(e) => {
                            error!(err = %e, "slack message processing failed");
                        }
                    }
                });
            }
        }
    }

    serde_json::json!({"ok": true})
}
