use std::sync::Arc;

use async_trait::async_trait;
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::config::MatrixConfig;
use crate::error::{Result, SafeAgentError};

use super::{split_message, MessagingBackend};

/// Matrix messaging backend using the Client-Server API via reqwest.
///
/// Connects to any Matrix homeserver, authenticates with username/password
/// or access token, sends messages via the send event endpoint, and polls
/// for new messages via the `/sync` endpoint in a background task.
pub struct MatrixBackend {
    homeserver: String,
    access_token: tokio::sync::RwLock<String>,
    http: reqwest::Client,
    txn_counter: std::sync::atomic::AtomicU64,
}

impl MatrixBackend {
    pub fn new(homeserver: String, access_token: String) -> Self {
        let homeserver = homeserver.trim_end_matches('/').to_string();
        Self {
            homeserver,
            access_token: tokio::sync::RwLock::new(access_token),
            http: reqwest::Client::new(),
            txn_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Login with username and password, returning a new MatrixBackend.
    pub async fn login(
        homeserver: &str,
        user: &str,
        password: &str,
    ) -> Result<(Self, String)> {
        let hs = homeserver.trim_end_matches('/');
        let http = reqwest::Client::new();
        let url = format!("{hs}/_matrix/client/v3/login");

        let resp = http
            .post(&url)
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": {
                    "type": "m.id.user",
                    "user": user
                },
                "password": password,
            }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("matrix login failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SafeAgentError::Messaging(format!("matrix login parse: {e}")))?;

        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                SafeAgentError::Messaging(format!("matrix login: {err}"))
            })?
            .to_string();

        let user_id = body
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        info!(user_id, "matrix login successful");
        Ok((Self::new(hs.to_string(), token.clone()), token))
    }

    fn next_txn_id(&self) -> String {
        let n = self
            .txn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("sc_{n}_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis())
    }
}

#[async_trait]
impl MessagingBackend for MatrixBackend {
    fn platform_name(&self) -> &str {
        "matrix"
    }

    fn max_message_length(&self) -> usize {
        65535
    }

    async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        let token = self.access_token.read().await.clone();

        for chunk in split_message(text, self.max_message_length()) {
            let room_id = urlencoding::encode(channel);
            let txn_id = self.next_txn_id();
            let url = format!(
                "{}/_matrix/client/v3/rooms/{room_id}/send/m.room.message/{txn_id}",
                self.homeserver
            );

            let resp = self
                .http
                .put(&url)
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "msgtype": "m.text",
                    "body": chunk,
                }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| SafeAgentError::Messaging(format!("matrix send failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(SafeAgentError::Messaging(format!(
                    "matrix send {status}: {body}"
                )));
            }
        }
        Ok(())
    }

    async fn send_typing(&self, channel: &str) -> Result<()> {
        let token = self.access_token.read().await.clone();
        let room_id = urlencoding::encode(channel);
        let url = format!(
            "{}/_matrix/client/v3/rooms/{room_id}/typing/@me:matrix",
            self.homeserver
        );

        let _ = self
            .http
            .put(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "typing": true,
                "timeout": 5000,
            }))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sync polling (background task for receiving messages)
// ---------------------------------------------------------------------------

/// Start the Matrix /sync polling loop in a background task.
pub fn start_sync(
    backend: Arc<MatrixBackend>,
    config: MatrixConfig,
    agent: Arc<Agent>,
) -> tokio::sync::oneshot::Sender<()> {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut since: Option<String> = None;
        let http = reqwest::Client::new();

        info!("matrix sync polling started");

        loop {
            let token = backend.access_token.read().await.clone();
            let mut url = format!(
                "{}/_matrix/client/v3/sync?timeout=30000",
                backend.homeserver
            );
            if let Some(ref s) = since {
                url.push_str(&format!("&since={s}"));
            } else {
                // On first sync, only get the last few events
                url.push_str("&filter={\"room\":{\"timeline\":{\"limit\":1}}}");
            }

            tokio::select! {
                resp = http.get(&url).bearer_auth(&token).timeout(std::time::Duration::from_secs(60)).send() => {
                    match resp {
                        Ok(r) if r.status().is_success() => {
                            if let Ok(body) = r.json::<serde_json::Value>().await {
                                if let Some(next) = body.get("next_batch").and_then(|v| v.as_str()) {
                                    let is_initial = since.is_none();
                                    since = Some(next.to_string());

                                    // Skip processing on the initial sync to avoid replaying old messages
                                    if !is_initial {
                                        process_sync_response(&body, &config, &agent).await;
                                    }
                                }
                            }
                        }
                        Ok(r) => {
                            warn!(status = %r.status(), "matrix sync non-200");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                        Err(e) => {
                            warn!(err = %e, "matrix sync error");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("matrix sync shutting down");
                    return;
                }
            }
        }
    });

    shutdown_tx
}

async fn process_sync_response(
    body: &serde_json::Value,
    config: &MatrixConfig,
    agent: &Arc<Agent>,
) {
    let rooms = body
        .get("rooms")
        .and_then(|r| r.get("join"))
        .and_then(|j| j.as_object());

    let rooms = match rooms {
        Some(r) => r,
        None => return,
    };

    for (room_id, room_data) in rooms {
        // Authorization: check allowed rooms
        if !config.allowed_room_ids.is_empty() && !config.allowed_room_ids.contains(room_id) {
            continue;
        }

        let events = room_data
            .get("timeline")
            .and_then(|t| t.get("events"))
            .and_then(|e| e.as_array());

        let events = match events {
            Some(e) => e,
            None => continue,
        };

        for event in events {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if event_type != "m.room.message" {
                continue;
            }

            let content = match event.get("content") {
                Some(c) => c,
                None => continue,
            };

            let msgtype = content
                .get("msgtype")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if msgtype != "m.text" {
                continue;
            }

            let sender = event
                .get("sender")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Skip our own messages
            if config.user_id.as_deref() == Some(sender) {
                continue;
            }

            let text = content
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if text.is_empty() {
                continue;
            }

            info!(room_id, sender, "matrix message received");
            let agent = agent.clone();
            tokio::spawn(async move {
                match agent.handle_message_as(&text, None).await {
                    Ok(_reply) => info!("matrix message processed"),
                    Err(e) => error!(err = %e, "matrix message processing failed"),
                }
            });
        }
    }
}
