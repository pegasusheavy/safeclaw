use teloxide::prelude::*;
use teloxide::types::ChatAction;
use tracing::{info, warn};

use super::TelegramState;

/// Handle incoming Telegram messages (text, photos, voice, documents).
pub async fn handle_message(
    bot: Bot,
    msg: Message,
    state: TelegramState,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    info!(chat_id, "telegram message received from chat");

    // Authorization check
    if !state.config.allowed_chat_ids.is_empty()
        && !state.config.allowed_chat_ids.contains(&chat_id)
    {
        bot.send_message(msg.chat.id, "⛔ Unauthorized. Your chat ID is not in the allowed list.")
            .await?;
        return Ok(());
    }

    // Handle photos — download and analyze with vision
    if let Some(photos) = msg.photo() {
        if let Some(photo) = photos.last() {
            let caption = msg.caption().unwrap_or("Describe this image.");
            info!(chat_id, file_id = %photo.file.id, "telegram photo received");
            let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

            let agent = state.agent.clone();
            let chat = msg.chat.id;
            let file_id = photo.file.id.clone();
            let caption = caption.to_string();
            let bot2 = bot.clone();

            tokio::spawn(async move {
                let user_text = match download_telegram_file(&bot2, &file_id).await {
                    Ok((bytes, _ext)) => {
                        let b64 = base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            &bytes,
                        );
                        format!(
                            "[User sent a photo. Base64-encoded image data is attached to context.]\n\
                             User caption: {caption}\n\
                             <image_data mime=\"image/jpeg\" base64=\"{b64}\" />"
                        )
                    }
                    Err(e) => {
                        warn!(err = %e, "failed to download telegram photo");
                        format!("[User tried to send a photo but download failed: {e}]\nCaption: {caption}")
                    }
                };

                let result = agent.handle_message(&user_text).await;
                send_reply(&bot2, chat, result).await;
            });
            return Ok(());
        }
    }

    // Handle voice messages — download and transcribe
    if let Some(voice) = msg.voice() {
        info!(chat_id, file_id = %voice.file.id, "telegram voice received");
        let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

        let agent = state.agent.clone();
        let chat = msg.chat.id;
        let file_id = voice.file.id.clone();
        let bot2 = bot.clone();

        tokio::spawn(async move {
            let user_text = match download_telegram_file(&bot2, &file_id).await {
                Ok((bytes, _ext)) => {
                    let b64 = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &bytes,
                    );
                    format!(
                        "[User sent a voice message. Please transcribe it using the 'transcribe' tool \
                         and then respond to the transcribed text.]\n\
                         <voice_data mime=\"audio/ogg\" base64=\"{b64}\" />"
                    )
                }
                Err(e) => {
                    warn!(err = %e, "failed to download telegram voice");
                    format!("[User tried to send a voice message but download failed: {e}]")
                }
            };

            let result = agent.handle_message(&user_text).await;
            send_reply(&bot2, chat, result).await;
        });
        return Ok(());
    }

    // Handle documents — download and extract
    if let Some(document) = msg.document() {
        let caption = msg.caption().unwrap_or("Extract and summarize this document.");
        info!(chat_id, file_name = ?document.file_name, "telegram document received");
        let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

        let agent = state.agent.clone();
        let chat = msg.chat.id;
        let file_id = document.file.id.clone();
        let file_name = document.file_name.clone().unwrap_or_else(|| "document".to_string());
        let caption = caption.to_string();
        let bot2 = bot.clone();

        tokio::spawn(async move {
            let user_text = match download_telegram_file(&bot2, &file_id).await {
                Ok((bytes, _ext)) => {
                    // Save to sandbox and tell agent to use document tool
                    format!(
                        "[User sent a document: {file_name} ({} bytes). \
                         Use the 'document' tool to extract its contents, then respond to the user.]\n\
                         User request: {caption}\n\
                         <document_data filename=\"{file_name}\" size=\"{}\" />",
                        bytes.len(),
                        bytes.len()
                    )
                }
                Err(e) => {
                    warn!(err = %e, "failed to download telegram document");
                    format!("[User tried to send a document but download failed: {e}]")
                }
            };

            let result = agent.handle_message(&user_text).await;
            send_reply(&bot2, chat, result).await;
        });
        return Ok(());
    }

    let text = msg.text().unwrap_or("");
    info!(chat_id, text, "telegram message authorized");

    if text.starts_with('/') {
        handle_command(&bot, &msg, text, &state).await?;
    } else {
        let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

        let agent = state.agent.clone();
        let chat = msg.chat.id;
        let user_text = text.to_string();

        tokio::spawn(async move {
            let typing_bot = bot.clone();
            let typing_handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
                    if typing_bot
                        .send_chat_action(chat, ChatAction::Typing)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });

            let result = agent.handle_message(&user_text).await;
            typing_handle.abort();
            send_reply(&bot, chat, result).await;
        });
    }

    Ok(())
}

async fn send_reply(bot: &Bot, chat: ChatId, result: std::result::Result<String, crate::error::SafeAgentError>) {
    match result {
        Ok(reply) => {
            for chunk in split_message(&reply, 4096) {
                if let Err(e) = bot.send_message(chat, chunk).await {
                    tracing::error!("failed to send telegram reply: {e}");
                }
            }
        }
        Err(e) => {
            tracing::error!("generation failed: {e}");
            let _ = bot.send_message(chat, format!("⚠️ Error: {e}")).await;
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

async fn handle_command(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &TelegramState,
) -> ResponseResult<()> {
    let parts: Vec<&str> = text.splitn(3, ' ').collect();
    let cmd = parts[0].split('@').next().unwrap_or(parts[0]);
    info!(cmd, "handling telegram command");

    match cmd {
        "/start" | "/help" => {
            let help = "\
🤖 safeclaw Telegram Control

/status - Agent status & stats
/pending - List pending actions
/approve <id|all> - Approve action(s)
/reject <id|all> - Reject action(s)
/pause - Pause agent loop
/resume - Resume agent loop
/tick - Force immediate tick
/memory <query> - Search archival memory
/help - This message

Or just type a message and the agent will read it on the next tick.";
            match bot.send_message(msg.chat.id, help).await {
                Ok(_) => info!("help message sent successfully"),
                Err(e) => info!("failed to send help message: {}", e),
            }
        }
        "/status" => {
            let db = state.db.lock().await;
            let stats = db.query_row(
                "SELECT total_ticks, total_actions, total_approved, total_rejected, last_tick_at FROM agent_stats WHERE id = 1",
                [],
                |row| {
                    Ok(format!(
                        "📊 Ticks: {}\n⚡ Actions: {}\n✅ Approved: {}\n❌ Rejected: {}\n⏰ Last tick: {}",
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "never".into()),
                    ))
                },
            );
            drop(db);
            let text = stats.unwrap_or_else(|_| "Could not fetch stats.".to_string());
            bot.send_message(msg.chat.id, text).await?;
        }
        "/pending" => {
            let actions = {
                let db = state.db.lock().await;
                let mut stmt = db.prepare(
                    "SELECT id, action_json, reasoning FROM pending_actions WHERE status = 'pending' ORDER BY proposed_at DESC LIMIT 10"
                ).unwrap();
                stmt.query_map([], |row| {
                    Ok(format!(
                        "🔔 *{}*\n{}\n_Reason: {}_",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect::<Vec<String>>()
            };

            if actions.is_empty() {
                bot.send_message(msg.chat.id, "No pending actions.").await?;
            } else {
                for action in &actions {
                    bot.send_message(msg.chat.id, action).await?;
                }
            }
        }
        "/approve" => {
            let target = parts.get(1).unwrap_or(&"");
            let db = state.db.lock().await;
            if *target == "all" {
                let n = db.execute(
                    "UPDATE pending_actions SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending'",
                    [],
                ).unwrap_or(0);
                drop(db);
                bot.send_message(msg.chat.id, format!("✅ Approved {n} action(s).")).await?;
            } else if !target.is_empty() {
                let n = db.execute(
                    "UPDATE pending_actions SET status = 'approved', resolved_at = datetime('now') WHERE id = ?1 AND status = 'pending'",
                    [target],
                ).unwrap_or(0);
                drop(db);
                if n > 0 {
                    bot.send_message(msg.chat.id, format!("✅ Approved {target}")).await?;
                } else {
                    bot.send_message(msg.chat.id, format!("Action {target} not found or already resolved.")).await?;
                }
            } else {
                bot.send_message(msg.chat.id, "Usage: /approve <id|all>").await?;
            }
        }
        "/reject" => {
            let target = parts.get(1).unwrap_or(&"");
            let db = state.db.lock().await;
            if *target == "all" {
                let n = db.execute(
                    "UPDATE pending_actions SET status = 'rejected', resolved_at = datetime('now') WHERE status = 'pending'",
                    [],
                ).unwrap_or(0);
                drop(db);
                bot.send_message(msg.chat.id, format!("❌ Rejected {n} action(s).")).await?;
            } else if !target.is_empty() {
                let n = db.execute(
                    "UPDATE pending_actions SET status = 'rejected', resolved_at = datetime('now') WHERE id = ?1 AND status = 'pending'",
                    [target],
                ).unwrap_or(0);
                drop(db);
                if n > 0 {
                    bot.send_message(msg.chat.id, format!("❌ Rejected {target}")).await?;
                } else {
                    bot.send_message(msg.chat.id, format!("Action {target} not found or already resolved.")).await?;
                }
            } else {
                bot.send_message(msg.chat.id, "Usage: /reject <id|all>").await?;
            }
        }
        "/tick" => {
            bot.send_message(msg.chat.id, "⏩ Forcing immediate tick...")
                .await?;
            let agent = state.agent.clone();
            tokio::spawn(async move {
                if let Err(e) = agent.force_tick().await {
                    tracing::error!("forced tick failed: {e}");
                }
            });
        }
        "/pause" => {
            state.agent.pause();
            bot.send_message(msg.chat.id, "⏸ Agent paused.").await?;
        }
        "/resume" => {
            state.agent.resume();
            bot.send_message(msg.chat.id, "▶️ Agent resumed.").await?;
        }
        "/memory" => {
            let query = parts.get(1).unwrap_or(&"");
            if query.is_empty() {
                bot.send_message(msg.chat.id, "Usage: /memory <search query>").await?;
            } else {
                let results = {
                    let db = state.db.lock().await;
                    let mut stmt = db.prepare(
                        "SELECT am.content, am.category, am.created_at
                         FROM archival_memory_fts fts
                         JOIN archival_memory am ON am.id = fts.rowid
                         WHERE archival_memory_fts MATCH ?1
                         ORDER BY rank LIMIT 5"
                    ).unwrap();
                    stmt.query_map([query], |row| {
                        Ok(format!(
                            "📌 [{}] {}: {}",
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(0)?,
                        ))
                    })
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect::<Vec<String>>()
                };

                if results.is_empty() {
                    bot.send_message(msg.chat.id, "No matching memories.").await?;
                } else {
                    bot.send_message(msg.chat.id, results.join("\n\n")).await?;
                }
            }
        }
        _ => {
            bot.send_message(msg.chat.id, "Unknown command. Use /help for available commands.").await?;
        }
    }

    Ok(())
}

/// Split a long message into chunks that fit within Telegram's character limit.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // Try to break at a newline within the last 200 chars of the chunk
        let break_at = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .filter(|&pos| pos > end - start - 200)
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
