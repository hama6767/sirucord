//! Share only the latest changed message while the associated voice channels are occupied.
use crate::{
    engine::{App, Entry, Pending, State},
    http, link_titles,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

fn revision(message: Option<&Value>) -> String {
    let identity = message.map(|m| (&m["id"], &m["edited_timestamp"]));
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&identity).expect("JSON identity"))
    )
}

async fn render(message: &Value) -> Option<String> {
    if ![0, 19].contains(&message["type"].as_u64().unwrap_or(0)) {
        return None;
    }
    let author = message["author"]["global_name"]
        .as_str()
        .or(message["author"]["username"].as_str())
        .unwrap_or("参加者");
    let content = message["content"].as_str().unwrap_or("");
    let mut finder = linkify::LinkFinder::new();
    finder.kinds(&[linkify::LinkKind::Url]);
    let mut plain = String::new();
    let mut urls = Vec::new();
    let mut end = 0;
    for link in finder.links(content) {
        plain.push_str(&content[end..link.start()]);
        end = link.end();
        if urls.len() < 2
            && link.as_str().len() <= 2048
            && !urls.iter().any(|(u, _)| u == link.as_str())
        {
            urls.push((link.as_str().to_owned(), None));
        }
    }
    plain.push_str(&content[end..]);
    if let Some(mentions) = message["mentions"].as_array() {
        for user in mentions {
            if let Some(id) = user["id"].as_str() {
                let name = user["global_name"]
                    .as_str()
                    .or(user["username"].as_str())
                    .unwrap_or("参加者");
                plain = plain
                    .replace(&format!("<@{id}>"), name)
                    .replace(&format!("<@!{id}>"), name);
            }
        }
    }
    // Attachments remain links; no file execution or authenticated third-party downloads.
    if let Some(attachments) = message["attachments"].as_array() {
        for attachment in attachments {
            if urls.len() >= 2 {
                break;
            }
            if let Some(url) = attachment["url"].as_str().filter(|u| u.len() <= 2048) {
                urls.push((
                    url.to_owned(),
                    attachment["filename"]
                        .as_str()
                        .and_then(link_titles::clean_title),
                ));
            }
        }
    }
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.is_empty() && urls.is_empty() {
        return None;
    }
    let mut text = format!("💬 通話中の話題\n{}：", crate::discord::clean(author, 45));
    if !plain.is_empty() {
        text.push_str(&crate::discord::clean(&plain, 180));
        if plain.chars().count() > 180 {
            text.push('…');
        }
    }
    for (url, supplied_title) in urls {
        let embedded_title = message["embeds"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|embed| embed["url"].as_str() == Some(&url))
            .and_then(|embed| embed["title"].as_str())
            .and_then(link_titles::clean_title);
        let title = match supplied_title.or(embedded_title) {
            Some(title) => Some(title),
            None => link_titles::title(&url).await,
        };
        if let Some(title) = title {
            text.push_str(&format!("\n{}", crate::discord::clean(&title, 65)));
        }
        text.push_str(&format!("\n{url}"));
    }
    Some(text)
}

impl App {
    pub async fn share_updates(
        &mut self,
        state: &mut State,
        occupied_guilds: &HashSet<String>,
        dry_run: bool,
    ) -> Result<()> {
        let targets: Vec<_> = self
            .config
            .servers
            .iter()
            .filter(|server| occupied_guilds.contains(&server.guild_id))
            .filter_map(|server| {
                server
                    .share_channel_id
                    .as_ref()
                    .map(|channel| (server.guild_id.clone(), channel.clone()))
            })
            .collect();
        for (guild, channel) in targets {
            let key = format!("{guild}:share-{channel}");
            if state.entries.get(&key).is_some_and(|e| e.pending.is_some()) {
                self.deliver(state, &key, dry_run).await?;
                continue;
            }
            let response = http::send(|| {
                self.discord
                    .client
                    .get(format!(
                        "{}/channels/{channel}/messages?limit=1",
                        self.discord.base
                    ))
                    .header("Authorization", format!("Bot {}", self.discord.token))
            })
            .await?;
            let messages: Vec<Value> = http::success(response, "Discord shared channel")?
                .json()
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Invalid shared channel response (details redacted)")
                })?;
            let message = messages.first();
            let message_id = message
                .map(|message| message["id"].as_str().context("Missing shared message ID"))
                .transpose()?
                .unwrap_or("0");
            ensure!(
                message_id == "0" || crate::activity::snowflake(message_id),
                "Invalid shared message ID"
            );
            let fingerprint = revision(message);
            let now = Utc::now();
            let Some(entry) = state.entries.get(&key) else {
                // Establish a baseline instead of publishing pre-installation history.
                state.entries.insert(
                    key.clone(),
                    Entry {
                        metadata_fingerprint: Some(fingerprint),
                        session_id: message_id.into(),
                        first_seen: now,
                        announced: true,
                        last_post: None,
                        last_attachment: None,
                        pending: None,
                    },
                );
                if !dry_run {
                    self.store.save(state).await?;
                }
                println!("Shared channel baseline recorded; existing message not republished.");
                continue;
            };
            if entry.metadata_fingerprint.as_ref() == Some(&fingerprint) {
                continue;
            }
            // Deletion/older history never becomes a new shared message.
            if message_id.parse::<u64>()? < entry.session_id.parse::<u64>()? {
                continue;
            }
            let text = match message {
                Some(message) => render(message).await,
                None => None,
            };
            let entry = state.entries.get_mut(&key).unwrap();
            entry.session_id = message_id.into();
            let Some(text) = text else {
                entry.metadata_fingerprint = Some(fingerprint);
                if !dry_run {
                    self.store.save(state).await?;
                }
                continue;
            };
            entry.pending = Some(Pending {
                shared_message: true,
                avatars: Vec::new(),
                metadata_fingerprint: Some(fingerprint),
                key: uuid::Uuid::new_v4().to_string(),
                text,
                attachment: None,
                media_id: None,
                attempted_at: None,
                is_start: false,
            });
            if !dry_run {
                self.store.save(state).await?;
            }
            self.deliver(state, &key, dry_run).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn shares_link_title_from_discord_without_fetching_the_link() {
        let message = json!({"type":0,"author":{"username":"Alice"},
            "content":"これ面白い https://private.invalid/video",
            "embeds":[{"url":"https://private.invalid/video","title":"動画のタイトル & ゲーム"}]});
        let text = render(&message).await.unwrap();
        assert!(text.contains("動画のタイトル & ゲーム\nhttps://private.invalid/video"));
        assert!(text.contains("Alice：これ面白い"));
        assert!(!text.contains("discord.com/channels/"));
    }
    #[test]
    fn message_edits_change_identity_but_delayed_embed_population_does_not() {
        let mut message = json!({"id":"10", "edited_timestamp":null});
        let initial = revision(Some(&message));
        message["embeds"] = json!([{"title":"new metadata"}]);
        assert_eq!(initial, revision(Some(&message)));
        message["edited_timestamp"] = json!("2026-09-14T20:00:00Z");
        assert_ne!(initial, revision(Some(&message)));
    }
}
