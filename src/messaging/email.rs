use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::{Result, SafeAgentError};

use super::MessagingBackend;

/// Email messaging backend using Gmail API and Microsoft Graph.
///
/// Uses OAuth tokens from safeclaw's token store. The "channel" for email
/// is the recipient email address. The primary channel is the operator's
/// own email (for self-notifications).
pub struct EmailBackend {
    http: reqwest::Client,
    provider: EmailProvider,
    access_token: tokio::sync::RwLock<String>,
    refresh_token: String,
    client_id: String,
    client_secret: String,
    token_url: String,
    from_email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailProvider {
    Gmail,
    Microsoft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Email {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub snippet: String,
    pub date: String,
    pub is_read: bool,
}

impl EmailBackend {
    pub fn new(
        provider: EmailProvider,
        access_token: String,
        refresh_token: String,
        client_id: String,
        client_secret: String,
        token_url: String,
        from_email: String,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            provider,
            access_token: tokio::sync::RwLock::new(access_token),
            refresh_token,
            client_id,
            client_secret,
            token_url,
            from_email,
        }
    }

    /// Refresh the OAuth access token.
    async fn refresh_access_token(&self) -> Result<()> {
        let resp = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("token refresh failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("token refresh parse: {e}")))?;

        if let Some(token) = body.get("access_token").and_then(|v| v.as_str()) {
            *self.access_token.write().await = token.to_string();
            debug!("email access token refreshed");
            Ok(())
        } else {
            Err(SafeAgentError::Messaging(format!(
                "token refresh: no access_token in response: {body}"
            )))
        }
    }

    /// Make an authenticated GET request, refreshing the token on 401.
    async fn authed_get(&self, url: &str) -> Result<serde_json::Value> {
        let token = self.access_token.read().await.clone();
        let resp = self
            .http
            .get(url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("email GET failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.refresh_access_token().await?;
            let new_token = self.access_token.read().await.clone();
            let resp = self
                .http
                .get(url)
                .bearer_auth(&new_token)
                .send()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("email GET retry: {e}")))?;
            resp.json()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("email GET parse: {e}")))
        } else {
            resp.json()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("email GET parse: {e}")))
        }
    }

    /// Send an email via Gmail API.
    async fn gmail_send(&self, to: &str, subject: &str, body_text: &str) -> Result<()> {
        let raw = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}",
            self.from_email, to, subject, body_text
        );
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());

        let token = self.access_token.read().await.clone();
        let resp = self
            .http
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(&token)
            .json(&serde_json::json!({"raw": encoded}))
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("gmail send failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.refresh_access_token().await?;
            let new_token = self.access_token.read().await.clone();
            let resp = self
                .http
                .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
                .bearer_auth(&new_token)
                .json(&serde_json::json!({"raw": encoded}))
                .send()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("gmail send retry: {e}")))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(SafeAgentError::Messaging(format!("gmail send: {body}")));
            }
        } else if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SafeAgentError::Messaging(format!("gmail send: {body}")));
        }

        info!(to, subject, "email sent via Gmail");
        Ok(())
    }

    /// Send an email via Microsoft Graph.
    async fn graph_send(&self, to: &str, subject: &str, body_text: &str) -> Result<()> {
        let payload = serde_json::json!({
            "message": {
                "subject": subject,
                "body": {
                    "contentType": "Text",
                    "content": body_text
                },
                "toRecipients": [{
                    "emailAddress": {"address": to}
                }]
            }
        });

        let token = self.access_token.read().await.clone();
        let resp = self
            .http
            .post("https://graph.microsoft.com/v1.0/me/sendMail")
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("graph send failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.refresh_access_token().await?;
            let new_token = self.access_token.read().await.clone();
            let resp = self
                .http
                .post("https://graph.microsoft.com/v1.0/me/sendMail")
                .bearer_auth(&new_token)
                .json(&payload)
                .send()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("graph send retry: {e}")))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(SafeAgentError::Messaging(format!("graph send: {body}")));
            }
        } else if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SafeAgentError::Messaging(format!("graph send: {body}")));
        }

        info!(to, subject, "email sent via Microsoft Graph");
        Ok(())
    }

    /// Send an email (auto-dispatches to the correct provider).
    pub async fn send_email(&self, to: &str, subject: &str, body: &str) -> Result<()> {
        match self.provider {
            EmailProvider::Gmail => self.gmail_send(to, subject, body).await,
            EmailProvider::Microsoft => self.graph_send(to, subject, body).await,
        }
    }

    /// List recent emails from the inbox.
    pub async fn list_inbox(&self, count: usize) -> Result<Vec<Email>> {
        match self.provider {
            EmailProvider::Gmail => self.gmail_list_inbox(count).await,
            EmailProvider::Microsoft => self.graph_list_inbox(count).await,
        }
    }

    async fn gmail_list_inbox(&self, count: usize) -> Result<Vec<Email>> {
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults={count}&q=in:inbox"
        );
        let list = self.authed_get(&url).await?;

        let mut emails = Vec::new();
        let messages = list
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for msg_ref in messages.iter().take(count) {
            let id = msg_ref
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let detail_url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Subject&metadataHeaders=Date"
            );
            if let Ok(detail) = self.authed_get(&detail_url).await {
                emails.push(parse_gmail_message(&detail));
            }
        }
        Ok(emails)
    }

    async fn graph_list_inbox(&self, count: usize) -> Result<Vec<Email>> {
        let url = format!(
            "https://graph.microsoft.com/v1.0/me/messages?$top={count}&$orderby=receivedDateTime desc&$select=id,from,toRecipients,subject,bodyPreview,receivedDateTime,isRead"
        );
        let data = self.authed_get(&url).await?;
        let values = data
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(values.iter().map(parse_graph_message).collect())
    }

    pub fn from_email(&self) -> &str {
        &self.from_email
    }

    pub fn provider(&self) -> EmailProvider {
        self.provider
    }
}

#[async_trait]
impl MessagingBackend for EmailBackend {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn platform_name(&self) -> &str {
        match self.provider {
            EmailProvider::Gmail => "email_gmail",
            EmailProvider::Microsoft => "email_microsoft",
        }
    }

    fn max_message_length(&self) -> usize {
        100_000
    }

    async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        let subject = text
            .lines()
            .next()
            .unwrap_or("Message from SafeClaw")
            .chars()
            .take(78)
            .collect::<String>();
        self.send_email(channel, &subject, text).await
    }

    async fn send_typing(&self, _channel: &str) -> Result<()> {
        Ok(())
    }
}

fn parse_gmail_message(detail: &serde_json::Value) -> Email {
    let headers = detail
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array());

    let get_header = |name: &str| -> String {
        headers
            .and_then(|hs| {
                hs.iter().find(|h| {
                    h.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.eq_ignore_ascii_case(name))
                        .unwrap_or(false)
                })
            })
            .and_then(|h| h.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let label_ids = detail
        .get("labelIds")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let is_read = !label_ids
        .iter()
        .any(|l| l.as_str() == Some("UNREAD"));

    Email {
        id: detail
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        from: get_header("From"),
        to: get_header("To"),
        subject: get_header("Subject"),
        snippet: detail
            .get("snippet")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        date: get_header("Date"),
        is_read,
    }
}

fn parse_graph_message(msg: &serde_json::Value) -> Email {
    Email {
        id: msg
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        from: msg
            .get("from")
            .and_then(|f| f.get("emailAddress"))
            .and_then(|e| e.get("address"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        to: msg
            .get("toRecipients")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|r| r.get("emailAddress"))
            .and_then(|e| e.get("address"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        subject: msg
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        snippet: msg
            .get("bodyPreview")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        date: msg
            .get("receivedDateTime")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        is_read: msg
            .get("isRead")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    }
}
