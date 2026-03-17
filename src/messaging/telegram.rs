use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, InputFile, ParseMode};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::config::TelegramConfig;
use crate::error::Result;

use super::commands::{handle_bot_command, CommandPrefix, CommandResult};
use super::{split_message, MessagingBackend};

// ---------------------------------------------------------------------------
// MessagingBackend implementation
// ---------------------------------------------------------------------------

pub struct TelegramBackend {
    bot: Bot,
}

impl TelegramBackend {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }

    pub fn bot(&self) -> &Bot {
        &self.bot
    }
}

#[async_trait]
impl MessagingBackend for TelegramBackend {
    fn platform_name(&self) -> &str {
        "telegram"
    }

    fn max_message_length(&self) -> usize {
        4096
    }

    async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        let chat_id: i64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid telegram chat id: {channel}"),
            ))?;
        let cid = ChatId(chat_id);

        for chunk in split_message(text, self.max_message_length()) {
            if let Err(e) = self.bot.send_message(cid, chunk).await {
                error!(chat_id, err = %e, "failed to send telegram message");
                return Err(crate::error::SafeAgentError::Messaging(format!(
                    "telegram send failed: {e}"
                )));
            }
        }
        Ok(())
    }

    async fn send_typing(&self, channel: &str) -> Result<()> {
        let chat_id: i64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid telegram chat id: {channel}"),
            ))?;
        let _ = self.bot.send_chat_action(ChatId(chat_id), ChatAction::Typing).await;
        Ok(())
    }

    fn supports_rich_messages(&self) -> bool {
        true
    }

    async fn send_rich(&self, channel: &str, content: &super::RichContent) -> Result<()> {
        use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ReplyMarkup};

        let chat_id: i64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid telegram chat id: {channel}"),
            ))?;
        let cid = ChatId(chat_id);

        match content {
            super::RichContent::Image { url, caption } => {
                let file = InputFile::url(url.parse().map_err(|_| {
                    crate::error::SafeAgentError::Messaging(format!("invalid image url: {url}"))
                })?);
                let mut req = self.bot.send_photo(cid, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                req.await.map_err(|e| {
                    crate::error::SafeAgentError::Messaging(format!("telegram send_photo: {e}"))
                })?;
            }
            super::RichContent::File {
                url,
                filename,
                caption,
            } => {
                let file = InputFile::url(url.parse().map_err(|_| {
                    crate::error::SafeAgentError::Messaging(format!("invalid file url: {url}"))
                })?).file_name(filename.clone());
                let mut req = self.bot.send_document(cid, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                req.await.map_err(|e| {
                    crate::error::SafeAgentError::Messaging(format!("telegram send_document: {e}"))
                })?;
            }
            super::RichContent::Buttons { text, buttons } => {
                let keyboard_buttons: Vec<Vec<InlineKeyboardButton>> = buttons
                    .iter()
                    .map(|b| {
                        vec![match b.style {
                            super::rich::ButtonStyle::Link => {
                                InlineKeyboardButton::url(
                                    b.label.clone(),
                                    b.data.parse().unwrap_or_else(|_| "https://example.com".parse().unwrap()),
                                )
                            }
                            _ => InlineKeyboardButton::callback(b.label.clone(), b.data.clone()),
                        }]
                    })
                    .collect();
                let markup = InlineKeyboardMarkup::new(keyboard_buttons);
                self.bot
                    .send_message(cid, text)
                    .reply_markup(ReplyMarkup::InlineKeyboard(markup))
                    .await
                    .map_err(|e| {
                        crate::error::SafeAgentError::Messaging(format!("telegram buttons: {e}"))
                    })?;
            }
            super::RichContent::Card {
                title,
                description,
                image_url,
                url,
            } => {
                let mut text = format!("<b>{title}</b>");
                if let Some(d) = description {
                    text.push_str(&format!("\n{d}"));
                }
                if let Some(u) = url {
                    text.push_str(&format!("\n<a href=\"{u}\">Open</a>"));
                }
                if let Some(img) = image_url {
                    let file = InputFile::url(img.parse().map_err(|_| {
                        crate::error::SafeAgentError::Messaging(format!("invalid image url: {img}"))
                    })?);
                    self.bot
                        .send_photo(cid, file)
                        .caption(text)
                        .parse_mode(ParseMode::Html)
                        .await
                        .map_err(|e| {
                            crate::error::SafeAgentError::Messaging(format!("telegram card: {e}"))
                        })?;
                } else {
                    self.bot
                        .send_message(cid, &text)
                        .parse_mode(ParseMode::Html)
                        .await
                        .map_err(|e| {
                            crate::error::SafeAgentError::Messaging(format!("telegram card: {e}"))
                        })?;
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dispatcher (long-polling loop)
// ---------------------------------------------------------------------------

/// Shared state accessible by Telegram handlers.
#[derive(Clone)]
struct TelegramState {
    db: Arc<Mutex<Connection>>,
    config: TelegramConfig,
    agent: Arc<Agent>,
}

/// Start the Telegram long-polling dispatcher. Returns the bot handle and a
/// shutdown oneshot.
pub async fn start(
    db: Arc<Mutex<Connection>>,
    config: TelegramConfig,
    agent: Arc<Agent>,
    backend: Arc<TelegramBackend>,
) -> Result<tokio::sync::oneshot::Sender<()>> {
    let bot = backend.bot().clone();

    let state = TelegramState {
        db,
        config: config.clone(),
        agent,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        info!("telegram bot starting");

        let mut shutdown_rx = shutdown_rx;
        loop {
            let state_clone = state.clone();
            let bot_inner = bot.clone();

            let handler = dptree::entry().branch(
                Update::filter_message().endpoint(handle_message),
            );

            let mut dispatcher = Dispatcher::builder(bot_inner, handler)
                .dependencies(dptree::deps![state_clone])
                .default_handler(|upd| async move {
                    warn!("unhandled telegram update: {:?}", upd.kind);
                })
                .error_handler(LoggingErrorHandler::with_custom_text(
                    "telegram handler error",
                ))
                .build();

            tokio::select! {
                _ = dispatcher.dispatch() => {
                    error!("telegram dispatcher exited, restarting in 5 seconds...");
                },
                _ = &mut shutdown_rx => {
                    info!("telegram bot shutting down");
                    return;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            info!("restarting telegram dispatcher");
        }
    });

    Ok(shutdown_tx)
}

// ---------------------------------------------------------------------------
// Message handler
// ---------------------------------------------------------------------------

async fn handle_message(
    bot: Bot,
    msg: Message,
    state: TelegramState,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    info!(chat_id, "telegram message received");

    // Authorization check
    if !state.config.allowed_chat_ids.is_empty()
        && !state.config.allowed_chat_ids.contains(&chat_id)
    {
        bot.send_message(
            msg.chat.id,
            "⛔ Unauthorized. Your chat ID is not in the allowed list.",
        )
        .await?;
        return Ok(());
    }

    // Look up user by Telegram user ID for multi-user routing
    let telegram_user_id = msg.from.as_ref().map(|u| u.id.0 as i64);
    let user_ctx = if let Some(tg_uid) = telegram_user_id {
        state.agent.user_manager.get_by_telegram_id(tg_uid).await
            .map(|u| crate::users::UserContext::from_user(&u, "telegram"))
    } else {
        None
    };

    // --- Photo messages ---
    if let Some(photos) = msg.photo() {
        if let Some(photo) = photos.last() {
            let caption = msg.caption().unwrap_or("Describe this image.").to_string();
            info!(chat_id, file_id = %photo.file.id, "telegram photo received");
            let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

            let agent = state.agent.clone();
            let chat = msg.chat.id;
            let file_id = photo.file.id.clone();

            tokio::spawn(async move {
                let user_text = match download_telegram_file(&bot, &file_id).await {
                    Ok((bytes, _ext)) => {
                        let b64 = base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            &bytes,
                        );
                        format!(
                            "[User sent a photo. Analyze it using the 'image' tool with \
                             the base64 data, or respond based on the caption.]\n\
                             Caption: {caption}\n\
                             <attached_image mime=\"image/jpeg\" base64_length=\"{}\" />",
                            b64.len()
                        )
                    }
                    Err(e) => {
                        warn!(err = %e, "failed to download telegram photo");
                        format!("[User tried to send a photo but download failed: {e}]\nCaption: {caption}")
                    }
                };

                send_agent_reply(&bot, chat, &agent, &user_text, user_ctx.as_ref()).await;
            });
            return Ok(());
        }
    }

    // --- Voice messages ---
    if let Some(voice) = msg.voice() {
        info!(chat_id, file_id = %voice.file.id, "telegram voice received");
        let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

        let agent = state.agent.clone();
        let chat = msg.chat.id;
        let file_id = voice.file.id.clone();

        tokio::spawn(async move {
            let user_text = match download_telegram_file(&bot, &file_id).await {
                Ok((_bytes, _ext)) => {
                    "[User sent a voice message. Use the 'transcribe' tool to convert it \
                     to text, then respond to the transcribed content.]".to_string()
                }
                Err(e) => {
                    warn!(err = %e, "failed to download telegram voice");
                    format!("[User tried to send a voice message but download failed: {e}]")
                }
            };

            send_agent_reply(&bot, chat, &agent, &user_text, user_ctx.as_ref()).await;
        });
        return Ok(());
    }

    // --- Document messages ---
    if let Some(document) = msg.document() {
        let caption = msg.caption().unwrap_or("Extract and summarize this document.").to_string();
        let file_name = document.file_name.clone().unwrap_or_else(|| "document".to_string());
        info!(chat_id, file_name = %file_name, "telegram document received");
        let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

        let agent = state.agent.clone();
        let chat = msg.chat.id;
        let file_id = document.file.id.clone();

        tokio::spawn(async move {
            let user_text = match download_telegram_file(&bot, &file_id).await {
                Ok((bytes, _ext)) => {
                    format!(
                        "[User sent a document: {file_name} ({} bytes). \
                         Use the 'document' tool to extract its contents and respond.]\n\
                         User request: {caption}",
                        bytes.len()
                    )
                }
                Err(e) => {
                    warn!(err = %e, "failed to download telegram document");
                    format!("[User tried to send document '{file_name}' but download failed: {e}]")
                }
            };

            send_agent_reply(&bot, chat, &agent, &user_text, user_ctx.as_ref()).await;
        });
        return Ok(());
    }

    // --- Text messages ---
    let text = msg.text().unwrap_or("");
    info!(chat_id, text, "telegram message authorized");

    match handle_bot_command(text, CommandPrefix::Slash, &state.db, &state.agent).await {
        CommandResult::Reply(reply) => {
            for chunk in split_message(&reply, 4096) {
                bot.send_message(msg.chat.id, chunk).await?;
            }
        }
        CommandResult::NotACommand => {
            let is_group = !msg.chat.is_private();
            if is_group {
                let bot_username = bot.get_me().await.ok()
                    .and_then(|me| me.username.clone());

                let is_mentioned = bot_username.as_ref().map_or(false, |name| {
                    text.contains(&format!("@{name}"))
                });
                let is_reply_to_bot = msg.reply_to_message().map_or(false, |reply| {
                    reply.from.as_ref().map_or(false, |u| u.is_bot)
                });

                if !is_mentioned && !is_reply_to_bot {
                    return Ok(());
                }
            }

            let _ = bot
                .send_chat_action(msg.chat.id, ChatAction::Typing)
                .await;

            let agent = state.agent.clone();
            let chat = msg.chat.id;
            let user_text = if is_group {
                strip_mention_text(text)
            } else {
                text.to_string()
            };

            tokio::spawn(async move {
                send_agent_reply(&bot, chat, &agent, &user_text, user_ctx.as_ref()).await;
            });
        }
    }

    Ok(())
}

async fn send_agent_reply(
    bot: &Bot,
    chat: ChatId,
    agent: &Agent,
    user_text: &str,
    user_ctx: Option<&crate::users::UserContext>,
) {
    let typing_bot = bot.clone();
    let typing_chat = chat;
    let typing_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
            if typing_bot
                .send_chat_action(typing_chat, ChatAction::Typing)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let result = agent.handle_message_as(user_text, user_ctx).await;
    typing_handle.abort();

    match result {
        Ok(reply) => {
            for chunk in split_message(&reply, 4096) {
                if let Err(e) = bot.send_message(chat, chunk).await {
                    error!("failed to send telegram reply: {e}");
                }
            }
        }
        Err(e) => {
            error!("agent generation failed: {e}");
            let _ = bot
                .send_message(chat, format!("⚠️ Error: {e}"))
                .await;
        }
    }
}

async fn download_telegram_file(
    bot: &Bot,
    file_id: &str,
) -> std::result::Result<(Vec<u8>, String), String> {
    let file = bot
        .get_file(file_id)
        .await
        .map_err(|e| format!("get_file failed: {e}"))?;

    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );

    let ext = file
        .path
        .rsplit('.')
        .next()
        .unwrap_or("bin")
        .to_string();

    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| format!("download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("read failed: {e}"))?
        .to_vec();

    Ok((bytes, ext))
}

// ---------------------------------------------------------------------------
// Mention stripping
// ---------------------------------------------------------------------------

/// Strip a leading @username mention from message text so the agent sees
/// clean input (e.g. "@mybot what time is it" → "what time is it").
fn strip_mention_text(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('@') {
        if let Some(idx) = text.find(char::is_whitespace) {
            return text[idx..].trim_start().to_string();
        }
    }
    text.to_string()
}

