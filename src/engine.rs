use crate::{
    config::{Config, Streamer},
    discord::{self, Discord, Voice},
    mastodon::Mastodon,
    store::Store,
};
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub entries: BTreeMap<String, Entry>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub session_id: String,
    pub first_seen: DateTime<Utc>,
    pub announced: bool,
    pub last_post: Option<DateTime<Utc>>,
    pub last_attachment: Option<String>,
    pub pending: Option<Pending>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub key: String,
    pub text: String,
    pub attachment: Option<discord::Attachment>,
    pub media_id: Option<String>,
    pub attempted_at: Option<DateTime<Utc>>,
    pub is_start: bool,
}

pub struct App {
    pub config: Config,
    pub discord: Discord,
    pub mastodon: Mastodon,
    pub store: Store,
}

impl App {
    pub async fn run(&mut self, dry_run: bool) -> Result<()> {
        let mut state = self.store.load().await?;
        self.mastodon.verify().await?;
        let mut observed: BTreeMap<String, (Streamer, Option<Voice>)> = BTreeMap::new();
        // Gather all states before mutating anything: one failed API request must
        // not turn an unavailable server into an empty server.
        for (s, v) in self.discord.discover(&self.config.servers).await? {
            observed.insert(s.key(), (s, Some(v)));
        }
        for s in &self.config.streamers {
            observed.insert(s.key(), (s.clone(), self.discord.voice(s).await?));
        }
        // Recreate offline targets from discovery config to finish durable outbox
        // delivery and clear ended streams, without publishing removed targets.
        for key in state.entries.keys() {
            if observed.contains_key(key) {
                continue;
            }
            let Some((guild, user)) = key.split_once(':') else {
                bail!("Malformed state target");
            };
            if let Some(server) = self.config.servers.iter().find(|s| s.guild_id == guild) {
                observed.insert(
                    key.clone(),
                    (
                        Streamer {
                            guild_id: guild.into(),
                            user_id: user.into(),
                            display_name: "Discord配信者".into(),
                            voice_channel_ids: server.voice_channel_ids.clone(),
                            default_title: server.default_title.clone(),
                            announcement_channel_id: server.announcement_channel_id.clone(),
                            screenshots: server.screenshots,
                        },
                        None,
                    ),
                );
            }
        }
        let active = observed.values().filter(|(_, v)| v.is_some()).count();
        println!(
            "Checked {} target(s); {} live stream(s).",
            observed.len(),
            active
        );
        for (key, (streamer, voice)) in observed {
            // Finish an in-flight write even when the stream has ended. Text says
            // 'detected a start' and carries no claim of being live right now.
            if state.entries.get(&key).is_some_and(|e| e.pending.is_some()) {
                let entry = &state.entries[&key];
                let pending = entry.pending.as_ref().unwrap();
                if pending.attachment.is_some() && pending.media_id.is_none() && !dry_run {
                    let channel = streamer.announcement_channel_id.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("Pending image requires its announcement channel")
                    })?;
                    let messages = self.discord.announcements(channel).await?;
                    let image = discord::screenshot(&messages, &streamer.user_id, entry.first_seen, Utc::now(), entry.last_attachment.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("Pending image no longer available; ask the streamer to resend it with !sirucord"))?;
                    // Discord CDN URLs expire. Refetch the attachment, or a newer
                    // explicitly shared image, before retrying an unsent upload.
                    state
                        .entries
                        .get_mut(&key)
                        .unwrap()
                        .pending
                        .as_mut()
                        .unwrap()
                        .attachment = Some(image);
                    self.store.save(&state).await?;
                }
                self.deliver(&mut state, &key, dry_run).await?;
            }
            let Some(voice) = voice else {
                if !dry_run && state.entries.remove(&key).is_some() {
                    self.store.save(&state).await?;
                }
                continue;
            };
            let now = Utc::now();
            let fresh = state
                .entries
                .get(&key)
                .is_none_or(|e| e.session_id != voice.session_id);
            if fresh {
                state.entries.insert(
                    key.clone(),
                    Entry {
                        session_id: voice.session_id.clone(),
                        first_seen: now,
                        announced: false,
                        last_post: None,
                        last_attachment: None,
                        pending: None,
                    },
                );
            }
            let entry = state.entries.get(&key).expect("entry inserted");
            let start = !entry.announced;
            let due = streamer.screenshots
                && entry.last_post.is_none_or(|t| {
                    now.signed_duration_since(t).num_seconds()
                        >= self.config.screenshot_interval_minutes as i64 * 60
                });
            if !start && !due {
                continue;
            }
            let messages = if let Some(channel) = &streamer.announcement_channel_id {
                self.discord.announcements(channel).await?
            } else {
                Vec::new()
            };
            let title = discord::title(&messages, &streamer.user_id, now)
                .unwrap_or_else(|| discord::clean(&streamer.default_title, 200));
            let attachment = if !start && due {
                discord::screenshot(
                    &messages,
                    &streamer.user_id,
                    entry.first_seen,
                    now,
                    entry.last_attachment.as_deref(),
                )
            } else {
                None
            };
            if !start && attachment.is_none() {
                continue;
            }
            let channel = voice.channel_id.as_ref().expect("active voice has channel");
            let name = discord::clean(&streamer.display_name, 80);
            let text = if start {
                format!(
                    "🔴 {name} さんのDiscord配信開始を検知しました\n内容：{title}\nhttps://discord.com/channels/{}/{channel}",
                    streamer.guild_id
                )
            } else {
                format!(
                    "📷 {name} さんから配信スクリーンショット\n内容：{title}\nhttps://discord.com/channels/{}/{channel}",
                    streamer.guild_id
                )
            };
            let pending = Pending {
                key: uuid::Uuid::new_v4().to_string(),
                text,
                attachment,
                media_id: None,
                attempted_at: None,
                is_start: start,
            };
            state.entries.get_mut(&key).unwrap().pending = Some(pending);
            if !dry_run {
                self.store.save(&state).await?;
            }
            self.deliver(&mut state, &key, dry_run).await?;
        }
        Ok(())
    }

    pub async fn deliver(&mut self, state: &mut State, key: &str, dry_run: bool) -> Result<()> {
        let Some(mut pending) = state.entries.get(key).and_then(|e| e.pending.clone()) else {
            return Ok(());
        };
        if dry_run {
            // Do not put names, IDs, captions or screenshot URLs into public Actions logs.
            println!(
                "Dry run: would publish {} (content redacted).",
                if pending.is_start {
                    "start announcement"
                } else {
                    "screenshot"
                }
            );
            complete(state, key, Utc::now());
            return Ok(());
        }
        if pending
            .attempted_at
            .is_some_and(|t| Utc::now().signed_duration_since(t).num_seconds() >= 55 * 60)
        {
            bail!(
                "Uncertain Mastodon delivery is older than 55 minutes. Inspect the destination account, then use resolve --target GUILD:USER --posted or --retry. State preserved; automatic duplicate posting stopped."
            );
        }
        if let Some(attachment) = &pending.attachment
            && pending.media_id.is_none()
        {
            pending.media_id = Some(
                self.mastodon
                    .upload(
                        attachment,
                        "配信者本人が共有したDiscord配信のスクリーンショット",
                    )
                    .await?,
            );
            state.entries.get_mut(key).unwrap().pending = Some(pending.clone());
            self.store.save(state).await?;
        }
        if pending.attempted_at.is_none() {
            pending.attempted_at = Some(Utc::now());
            state.entries.get_mut(key).unwrap().pending = Some(pending.clone());
            // Durable intent BEFORE external publication.
            self.store.save(state).await?;
        }
        self.mastodon
            .post(&pending.key, &pending.text, pending.media_id.as_deref())
            .await?;
        complete(state, key, Utc::now());
        self.store.save(state).await?;
        println!(
            "Published {} successfully.",
            if pending.is_start {
                "start announcement"
            } else {
                "screenshot"
            }
        );
        Ok(())
    }
}

pub fn complete(state: &mut State, key: &str, now: DateTime<Utc>) {
    let entry = state.entries.get_mut(key).expect("entry exists");
    if let Some(pending) = entry.pending.take() {
        if pending.is_start {
            entry.announced = true;
        }
        if let Some(attachment) = pending.attachment {
            entry.last_attachment = Some(attachment.id);
        }
        entry.last_post = Some(now);
    }
}
