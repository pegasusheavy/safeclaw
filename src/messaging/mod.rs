pub mod bridge;
pub mod commands;
#[cfg(feature = "discord")]
pub mod discord;
pub mod email;
pub mod matrix;
pub mod rich;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod twilio;
pub mod whatsapp;

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{error, info};

use crate::error::Result;

pub use rich::RichContent;

// ---------------------------------------------------------------------------
// Messaging backend trait
// ---------------------------------------------------------------------------

/// Platform-agnostic messaging interface. Telegram, WhatsApp, and future
/// platforms all implement this trait.
#[async_trait]
pub trait MessagingBackend: Send + Sync {
    /// Downcast support for tools that need provider-specific APIs.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Platform identifier (e.g. "telegram", "whatsapp").
    fn platform_name(&self) -> &str;

    /// Maximum message length before splitting is required.
    fn max_message_length(&self) -> usize;

    /// Send a text message to the given channel/chat.
    async fn send_message(&self, channel: &str, text: &str) -> Result<()>;

    /// Send a typing/composing indicator. Backends that don't support
    /// typing indicators should return Ok(()) silently.
    async fn send_typing(&self, channel: &str) -> Result<()>;

    /// Whether this backend supports rich content (images, files, buttons).
    fn supports_rich_messages(&self) -> bool {
        false
    }

    /// Send rich content. Falls back to text on backends that don't override.
    async fn send_rich(&self, channel: &str, content: &RichContent) -> Result<()> {
        self.send_message(channel, &content.to_text_fallback()).await
    }
}

// ---------------------------------------------------------------------------
// Messaging manager
// ---------------------------------------------------------------------------

/// Holds all active messaging backends and provides convenience methods
/// for sending to one, some, or all of them.
pub struct MessagingManager {
    backends: Vec<Arc<dyn MessagingBackend>>,
    /// Primary channel per backend: platform_name -> channel_id.
    /// Used by the message tool and notifications.
    primary_channels: std::collections::HashMap<String, String>,
}

impl MessagingManager {
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            primary_channels: std::collections::HashMap::new(),
        }
    }

    /// Register a backend with its primary channel (e.g. telegram chat id,
    /// whatsapp phone number).
    pub fn register(&mut self, backend: Arc<dyn MessagingBackend>, primary_channel: String) {
        info!(
            platform = backend.platform_name(),
            channel = %primary_channel,
            "registered messaging backend"
        );
        self.primary_channels
            .insert(backend.platform_name().to_string(), primary_channel);
        self.backends.push(backend);
    }

    /// Get a specific backend by platform name.
    pub fn get(&self, platform: &str) -> Option<&Arc<dyn MessagingBackend>> {
        self.backends.iter().find(|b| b.platform_name() == platform)
    }

    /// Get the primary channel for a given platform.
    pub fn primary_channel(&self, platform: &str) -> Option<&str> {
        self.primary_channels.get(platform).map(|s| s.as_str())
    }

    /// Get the first backend's primary channel.
    pub fn default_channel(&self) -> Option<(&Arc<dyn MessagingBackend>, &str)> {
        let backend = self.backends.first()?;
        let channel = self.primary_channels.get(backend.platform_name())?;
        Some((backend, channel.as_str()))
    }

    /// Send a message to the primary channel of every registered backend.
    pub async fn send_all(&self, text: &str) {
        for backend in &self.backends {
            let platform = backend.platform_name();
            if let Some(channel) = self.primary_channels.get(platform) {
                if let Err(e) = backend.send_message(channel, text).await {
                    error!(platform, err = %e, "failed to send to messaging backend");
                }
            }
        }
    }

    /// Send a typing indicator to the primary channel of every registered backend.
    pub async fn typing_all(&self) {
        for backend in &self.backends {
            let platform = backend.platform_name();
            if let Some(channel) = self.primary_channels.get(platform) {
                if let Err(e) = backend.send_typing(channel).await {
                    error!(platform, err = %e, "failed to send typing indicator");
                }
            }
        }
    }

    /// Send rich content to the primary channel of every registered backend.
    pub async fn send_rich_all(&self, content: &RichContent) {
        for backend in &self.backends {
            let platform = backend.platform_name();
            if let Some(channel) = self.primary_channels.get(platform) {
                if let Err(e) = backend.send_rich(channel, content).await {
                    error!(platform, err = %e, "failed to send rich message");
                }
            }
        }
    }

    /// List all registered platform names.
    pub fn platforms(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.platform_name()).collect()
    }

    /// Whether any backends are registered.
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Message splitting utility (shared by all backends)
// ---------------------------------------------------------------------------

/// Split a long message into chunks that fit within the given character limit.
/// Tries to break at newlines near the end of each chunk for readability.
pub fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if max_len == 0 || text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        let break_at = if end < text.len() {
            let chunk_len = end - start;
            text[start..end]
                .rfind('\n')
                .filter(|&pos| pos > chunk_len.saturating_sub(200))
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // A mock backend that records calls.
    struct MockBackend {
        name: &'static str,
        sent: Arc<StdMutex<Vec<(String, String)>>>,
        typed: Arc<StdMutex<Vec<String>>>,
    }

    impl MockBackend {
        fn new(name: &'static str) -> (Arc<Self>, Arc<StdMutex<Vec<(String, String)>>>, Arc<StdMutex<Vec<String>>>) {
            let sent = Arc::new(StdMutex::new(Vec::new()));
            let typed = Arc::new(StdMutex::new(Vec::new()));
            let backend = Arc::new(Self { name, sent: sent.clone(), typed: typed.clone() });
            (backend, sent, typed)
        }
    }

    #[async_trait]
    impl MessagingBackend for MockBackend {
        fn platform_name(&self) -> &str { self.name }
        fn max_message_length(&self) -> usize { 4096 }
        async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
            self.sent.lock().unwrap().push((channel.to_string(), text.to_string()));
            Ok(())
        }
        async fn send_typing(&self, channel: &str) -> Result<()> {
            self.typed.lock().unwrap().push(channel.to_string());
            Ok(())
        }
    }

    #[test]
    fn manager_new_is_empty() {
        let mgr = MessagingManager::new();
        assert!(mgr.is_empty());
        assert!(mgr.platforms().is_empty());
        assert!(mgr.default_channel().is_none());
    }

    #[test]
    fn manager_register_and_lookup() {
        let (backend, _, _) = MockBackend::new("test");
        let mut mgr = MessagingManager::new();
        mgr.register(backend, "chan123".into());
        assert!(!mgr.is_empty());
        assert_eq!(mgr.platforms(), vec!["test"]);
        assert!(mgr.get("test").is_some());
        assert!(mgr.get("other").is_none());
        assert_eq!(mgr.primary_channel("test"), Some("chan123"));
        assert!(mgr.primary_channel("other").is_none());
    }

    #[test]
    fn manager_default_channel() {
        let (b, _, _) = MockBackend::new("tg");
        let mut mgr = MessagingManager::new();
        mgr.register(b, "42".into());
        let (backend, ch) = mgr.default_channel().unwrap();
        assert_eq!(backend.platform_name(), "tg");
        assert_eq!(ch, "42");
    }

    #[tokio::test]
    async fn manager_send_all() {
        let (b1, sent1, _) = MockBackend::new("p1");
        let (b2, sent2, _) = MockBackend::new("p2");
        let mut mgr = MessagingManager::new();
        mgr.register(b1, "ch1".into());
        mgr.register(b2, "ch2".into());
        mgr.send_all("hello").await;
        assert_eq!(sent1.lock().unwrap().len(), 1);
        assert_eq!(sent1.lock().unwrap()[0], ("ch1".to_string(), "hello".to_string()));
        assert_eq!(sent2.lock().unwrap().len(), 1);
        assert_eq!(sent2.lock().unwrap()[0], ("ch2".to_string(), "hello".to_string()));
    }

    #[tokio::test]
    async fn manager_typing_all() {
        let (b1, _, typed1) = MockBackend::new("p1");
        let mut mgr = MessagingManager::new();
        mgr.register(b1, "ch1".into());
        mgr.typing_all().await;
        assert_eq!(typed1.lock().unwrap().len(), 1);
        assert_eq!(typed1.lock().unwrap()[0], "ch1");
    }

    #[test]
    fn test_split_message_short() {
        let text = "Hello world";
        let chunks = split_message(text, 100);
        assert_eq!(chunks, vec!["Hello world"]);
    }

    #[test]
    fn test_split_message_exact_length() {
        let text = "Exactly 12 chars";
        assert_eq!(text.len(), 16);
        let chunks = split_message(text, 16);
        assert_eq!(chunks, vec!["Exactly 12 chars"]);
    }

    #[test]
    fn test_split_message_just_under_limit() {
        let text = "Fifteen chars!!";
        let chunks = split_message(text, 20);
        assert_eq!(chunks, vec!["Fifteen chars!!"]);
    }

    #[test]
    fn test_split_message_long_splits() {
        let text = "a".repeat(250);
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[1].len(), 100);
        assert_eq!(chunks[2].len(), 50);
    }

    #[test]
    fn test_split_message_with_newlines_breaks_at_newline() {
        let text = "Line one\nLine two\nLine three\nLine four";
        let chunks = split_message(text, 30);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn test_split_message_long_no_newlines() {
        let text = "a".repeat(500);
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 5);
        for (i, chunk) in chunks.iter().enumerate() {
            if i < 4 {
                assert_eq!(chunk.len(), 100, "chunk {} should be 100 chars", i);
            } else {
                assert_eq!(chunk.len(), 100, "last chunk");
            }
        }
    }

    #[test]
    fn test_split_message_empty() {
        let chunks = split_message("", 100);
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_split_message_max_len_zero() {
        let text = "hello";
        let chunks = split_message(text, 0);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn test_split_message_max_len_one() {
        let text = "ab";
        let chunks = split_message(text, 1);
        assert_eq!(chunks, vec!["a", "b"]);
    }

    #[test]
    fn test_split_message_newline_near_end_preferred() {
        let text = format!("{}mid\n{}", "Start of message ".repeat(5), "end ".repeat(10));
        let chunks = split_message(&text, 50);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn test_split_message_single_char_repeated() {
        let text = "x".repeat(10);
        let chunks = split_message(&text, 3);
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0], "xxx");
        assert_eq!(chunks[1], "xxx");
        assert_eq!(chunks[2], "xxx");
        assert_eq!(chunks[3], "x");
    }
}
