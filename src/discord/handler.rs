//! Discord Message Handler
//!
//! Processes incoming Discord messages: text + image attachments, allowlist enforcement,
//! session routing (owner shares TUI session, others get per-user sessions).

use super::DiscordState;
use crate::config::RespondTo;
use crate::llm::agent::AgentService;
use crate::services::SessionService;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use serenity::model::channel::Message;
use serenity::prelude::*;

/// Header prepended to all outgoing messages so the user knows it's from the agent.

/// Split a message into chunks that fit Discord's 2000 char limit.
pub fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_message(
    ctx: &Context,
    msg: &Message,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    allowed: Arc<HashSet<i64>>,
    extra_sessions: Arc<Mutex<HashMap<u64, Uuid>>>,
    shared_session: Arc<Mutex<Option<Uuid>>>,
    discord_state: Arc<DiscordState>,
    respond_to: &RespondTo,
    allowed_channels: &HashSet<String>,
) {
    let user_id = msg.author.id.get() as i64;

    // Allowlist check — if allowed list is empty, accept all
    if !allowed.is_empty() && !allowed.contains(&user_id) {
        tracing::debug!("Discord: ignoring message from non-allowed user {}", user_id);
        return;
    }

    // respond_to / allowed_channels filtering — DMs always pass
    let is_dm = msg.guild_id.is_none();
    if !is_dm {
        let channel_str = msg.channel_id.get().to_string();

        // Check allowed_channels (empty = all channels allowed)
        if !allowed_channels.is_empty() && !allowed_channels.contains(&channel_str) {
            tracing::debug!("Discord: ignoring message in non-allowed channel {}", channel_str);
            return;
        }

        match respond_to {
            RespondTo::DmOnly => {
                tracing::debug!("Discord: respond_to=dm_only, ignoring channel message");
                return;
            }
            RespondTo::Mention => {
                let bot_id = discord_state.bot_user_id().await;
                let mentioned = bot_id.is_some_and(|bid| {
                    msg.mentions.iter().any(|u| u.id.get() == bid)
                });
                if !mentioned {
                    tracing::debug!("Discord: respond_to=mention, bot not mentioned — ignoring");
                    return;
                }
            }
            RespondTo::All => {} // pass through
        }
    }

    // Extract text content
    let mut content = msg.content.clone();

    // Strip bot @mention from content when responding to a mention
    if !is_dm && *respond_to == RespondTo::Mention {
        if let Some(bot_id) = discord_state.bot_user_id().await {
            let mention_tag = format!("<@{}>", bot_id);
            content = content.replace(&mention_tag, "").trim().to_string();
        }
    }
    if content.is_empty() && msg.attachments.is_empty() {
        return;
    }

    // Handle image attachments — append <<IMG:url>> markers
    for attachment in &msg.attachments {
        if let Some(ref content_type) = attachment.content_type
            && content_type.starts_with("image/")
        {
            if content.is_empty() {
                content = "Describe this image.".to_string();
            }
            content.push_str(&format!(" <<IMG:{}>>", attachment.url));
        }
    }

    if content.is_empty() {
        return;
    }

    let text_preview = &content[..content.len().min(50)];
    tracing::info!("Discord: message from {} ({}): {}", msg.author.name, user_id, text_preview);

    // Track owner's channel for proactive messaging
    let is_owner = allowed.is_empty()
        || allowed
            .iter()
            .next()
            .map(|&a| a == user_id)
            .unwrap_or(false);

    if is_owner {
        discord_state.set_owner_channel(msg.channel_id.get()).await;
    }

    // Resolve session: owner shares TUI session, others get per-user sessions
    let session_id = if is_owner {
        let shared = shared_session.lock().await;
        match *shared {
            Some(id) => id,
            None => {
                tracing::warn!("Discord: no active TUI session, creating one for owner");
                drop(shared);
                match session_svc.create_session(Some("Chat".to_string())).await {
                    Ok(session) => {
                        *shared_session.lock().await = Some(session.id);
                        session.id
                    }
                    Err(e) => {
                        tracing::error!("Discord: failed to create session: {}", e);
                        return;
                    }
                }
            }
        }
    } else {
        let mut map = extra_sessions.lock().await;
        let disc_user_id = msg.author.id.get();
        match map.get(&disc_user_id) {
            Some(id) => *id,
            None => {
                let title = format!("Discord: {}", msg.author.name);
                match session_svc.create_session(Some(title)).await {
                    Ok(session) => {
                        map.insert(disc_user_id, session.id);
                        session.id
                    }
                    Err(e) => {
                        tracing::error!("Discord: failed to create session: {}", e);
                        return;
                    }
                }
            }
        }
    };

    // Send to agent
    match agent.send_message_with_tools(session_id, content, None).await {
        Ok(response) => {
            let tagged = response.content.clone();
            for chunk in split_message(&tagged, 2000) {
                if let Err(e) = msg.channel_id.say(&ctx.http, chunk).await {
                    tracing::error!("Discord: failed to send reply: {}", e);
                }
            }
        }
        Err(e) => {
            tracing::error!("Discord: agent error: {}", e);
            let error_msg = format!("Error: {}", e);
            let _ = msg.channel_id.say(&ctx.http, error_msg).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_short_message() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_long_message() {
        let text = "a\n".repeat(1500);
        let chunks = split_message(&text, 2000);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
        let joined: String = chunks.into_iter().collect();
        assert_eq!(joined, text);
    }
}
