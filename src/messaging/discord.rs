use std::sync::Arc;

use async_trait::async_trait;
use serenity::all::*;
use tracing::{error, info};

use crate::agent::Agent;
use crate::config::DiscordConfig;
use crate::error::Result;

use super::{split_message, MessagingBackend};

// ---------------------------------------------------------------------------
// MessagingBackend implementation
// ---------------------------------------------------------------------------

pub struct DiscordBackend {
    http: Arc<Http>,
}

impl DiscordBackend {
    pub fn new(http: Arc<Http>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl MessagingBackend for DiscordBackend {
    fn platform_name(&self) -> &str {
        "discord"
    }

    fn max_message_length(&self) -> usize {
        2000
    }

    async fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        let channel_id: u64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid discord channel id: {channel}"),
            ))?;
        let cid = ChannelId::new(channel_id);

        for chunk in split_message(text, self.max_message_length()) {
            if let Err(e) = cid.say(&self.http, chunk).await {
                error!(channel_id, err = %e, "failed to send discord message");
                return Err(crate::error::SafeAgentError::Messaging(format!(
                    "discord send failed: {e}"
                )));
            }
        }
        Ok(())
    }

    async fn send_typing(&self, channel: &str) -> Result<()> {
        let channel_id: u64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid discord channel id: {channel}"),
            ))?;
        let _ = ChannelId::new(channel_id).broadcast_typing(&self.http).await;
        Ok(())
    }

    fn supports_rich_messages(&self) -> bool {
        true
    }

    async fn send_rich(
        &self,
        channel: &str,
        content: &super::RichContent,
    ) -> Result<()> {
        let channel_id: u64 = channel
            .parse()
            .map_err(|_| crate::error::SafeAgentError::Messaging(
                format!("invalid discord channel id: {channel}"),
            ))?;
        let cid = ChannelId::new(channel_id);

        match content {
            super::RichContent::Image { url, caption } => {
                let embed = CreateEmbed::new()
                    .image(url)
                    .description(caption.as_deref().unwrap_or(""));
                let msg = CreateMessage::new().embed(embed);
                cid.send_message(&self.http, msg).await.map_err(|e| {
                    crate::error::SafeAgentError::Messaging(format!("discord image: {e}"))
                })?;
            }
            super::RichContent::Card {
                title,
                description,
                image_url,
                url,
            } => {
                let mut embed = CreateEmbed::new().title(title);
                if let Some(d) = description {
                    embed = embed.description(d);
                }
                if let Some(img) = image_url {
                    embed = embed.image(img);
                }
                if let Some(u) = url {
                    embed = embed.url(u);
                }
                let msg = CreateMessage::new().embed(embed);
                cid.send_message(&self.http, msg).await.map_err(|e| {
                    crate::error::SafeAgentError::Messaging(format!("discord card: {e}"))
                })?;
            }
            other => {
                self.send_message(channel, &other.to_text_fallback()).await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Serenity event handler
// ---------------------------------------------------------------------------

struct Handler {
    config: DiscordConfig,
    agent: Arc<Agent>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        // Skip messages from bots (including ourselves)
        if msg.author.bot {
            return;
        }

        // Guild authorization: if allowed_guild_ids is non-empty, only
        // process messages from those guilds.
        if !self.config.allowed_guild_ids.is_empty() {
            match msg.guild_id {
                Some(gid) if self.config.allowed_guild_ids.contains(&gid.get()) => {}
                Some(_) => return,
                // DMs have no guild_id — allow them through
                None => {}
            }
        }

        // Channel authorization: if allowed_channel_ids is non-empty, only
        // process messages from those channels.
        if !self.config.allowed_channel_ids.is_empty()
            && !self.config.allowed_channel_ids.contains(&msg.channel_id.get())
        {
            return;
        }

        // Group message gating: in guild channels, only respond when
        // @mentioned or when the message is a reply to the bot.
        let is_group = msg.guild_id.is_some();
        if is_group {
            let bot_id = ctx.cache.current_user().id;
            let is_mentioned = msg.mentions.iter().any(|u| u.id == bot_id);
            let is_reply_to_bot = msg.referenced_message.as_ref()
                .map_or(false, |r| r.author.bot);

            if !is_mentioned && !is_reply_to_bot {
                return;
            }
        }

        let discord_user_id = msg.author.id.get().to_string();
        info!(
            channel_id = msg.channel_id.get(),
            author = %msg.author.name,
            is_group,
            "discord message received"
        );

        // Look up user by Discord ID for multi-user routing
        let user_ctx = self
            .agent
            .user_manager
            .get_by_discord_id(&discord_user_id)
            .await
            .map(|u| crate::users::UserContext::from_user(&u, "discord"));

        let agent = self.agent.clone();
        let channel_id = msg.channel_id;
        // Strip @mention from text so the agent sees clean input
        let user_text = if is_group {
            strip_discord_mention(&msg.content, &ctx.cache.current_user().id.get().to_string())
        } else {
            msg.content.clone()
        };
        let http = ctx.http.clone();

        tokio::spawn(async move {
            // Send typing indicator while processing
            let typing_http = http.clone();
            let typing_channel = channel_id;
            let typing_handle = tokio::spawn(async move {
                loop {
                    let _ = typing_channel.broadcast_typing(&typing_http).await;
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            });

            let result = agent.handle_message_as(&user_text, user_ctx.as_ref()).await;
            typing_handle.abort();

            match result {
                Ok(reply) => {
                    for chunk in split_message(&reply, 2000) {
                        if let Err(e) = channel_id.say(&http, chunk).await {
                            error!("failed to send discord reply: {e}");
                        }
                    }
                }
                Err(e) => {
                    error!("agent generation failed: {e}");
                    let _ = channel_id
                        .say(&http, format!("Error: {e}"))
                        .await;
                }
            }
        });
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "discord bot connected");
    }
}

// ---------------------------------------------------------------------------
// Start function
// ---------------------------------------------------------------------------

/// Start the Discord gateway client. Returns a oneshot sender that, when
/// dropped or sent to, triggers a graceful shutdown of the gateway.
pub async fn start(
    config: DiscordConfig,
    agent: Arc<Agent>,
) -> Result<tokio::sync::oneshot::Sender<()>> {
    let token = std::env::var("DISCORD_BOT_TOKEN").map_err(|_| {
        crate::error::SafeAgentError::Config("DISCORD_BOT_TOKEN not set".into())
    })?;

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let handler = Handler {
        config,
        agent,
    };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .map_err(|e| {
            crate::error::SafeAgentError::Messaging(format!("failed to build discord client: {e}"))
        })?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let shard_manager = client.shard_manager.clone();

    tokio::spawn(async move {
        if let Err(e) = client.start().await {
            error!("discord client error: {e}");
        }
    });

    tokio::spawn(async move {
        let _ = shutdown_rx.await;
        info!("discord bot shutting down");
        shard_manager.shutdown_all().await;
    });

    Ok(shutdown_tx)
}

// ---------------------------------------------------------------------------
// Mention stripping
// ---------------------------------------------------------------------------

/// Remove Discord-style `<@BOT_ID>` or `<@!BOT_ID>` mentions from the message
/// so the agent sees clean input.
fn strip_discord_mention(text: &str, bot_id: &str) -> String {
    let mention = format!("<@{bot_id}>");
    let mention_nick = format!("<@!{bot_id}>");
    text.replace(&mention, "").replace(&mention_nick, "").trim().to_string()
}
