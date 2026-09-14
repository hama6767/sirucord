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
    #[serde(default)]
    pub metadata_fingerprint: Option<String>,
    pub session_id: String,
    pub first_seen: DateTime<Utc>,
    pub announced: bool,
    pub last_post: Option<DateTime<Utc>>,
    pub last_attachment: Option<String>,
    pub pending: Option<Pending>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    #[serde(default)]
    pub avatars: Vec<crate::avatars::Avatar>,
    #[serde(default)]
    pub metadata_fingerprint: Option<String>,
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
    /// Returns whether any configured target is occupied after a complete check.
    pub async fn run(&mut self, dry_run: bool) -> Result<bool> {
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
        let active = observed
            .values()
            .filter(|(_, v)| v.as_ref().is_some_and(|v| v.self_stream))
            .count();
        let occupied = observed
            .values()
            .filter(|(_, v)| v.as_ref().is_some_and(|v| !v.self_stream))
            .count();
        println!(
            "Checked {} target(s); {} live stream(s); {} occupied channel(s) without streams.",
            observed.len(),
            active,
            occupied
        );
        let contexts: Vec<_> = observed
            .values()
            .filter_map(|(_, voice)| voice.as_ref().filter(|v| v.self_stream)?.context.as_ref())
            .collect();
        if !contexts.is_empty() {
            println!(
                "Automatic metadata: {} stream(s) with activity details; {} with artwork.",
                contexts.iter().filter(|c| !c.activities.is_empty()).count(),
                contexts.iter().filter(|c| c.artwork().is_some()).count()
            );
        }
        for (key, (streamer, voice)) in observed {
            // Finish an in-flight write even when the stream has ended. Text says
            // 'detected a start' and carries no claim of being live right now.
            if state.entries.get(&key).is_some_and(|e| e.pending.is_some()) {
                let entry = &state.entries[&key];
                let pending = entry.pending.as_ref().unwrap();
                if pending
                    .attachment
                    .as_ref()
                    .is_some_and(|a| a.source == discord::ImageSource::DiscordAttachment)
                    && pending.media_id.is_none()
                    && !dry_run
                {
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
                        metadata_fingerprint: None,
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
            if let Some(context) = &voice.context {
                let channel = voice
                    .channel_id
                    .as_deref()
                    .expect("active voice has channel");
                let fingerprint = context.fingerprint(channel);
                if !metadata_due(
                    entry,
                    &fingerprint,
                    now,
                    self.config.activity_update_interval_minutes,
                ) {
                    continue;
                }
                let pending = Pending {
                    avatars: if context.participants >= 2 {
                        context.avatars.clone()
                    } else {
                        Vec::new()
                    },
                    key: uuid::Uuid::new_v4().to_string(),
                    text: context.render(&streamer.display_name, start),
                    attachment: context.artwork(),
                    media_id: None,
                    attempted_at: None,
                    is_start: start,
                    metadata_fingerprint: Some(fingerprint),
                };
                // Automatic mode does not read announcement messages or require
                // captions/uploads, even if a legacy channel is configured.
                state.entries.get_mut(&key).unwrap().pending = Some(pending);
                if !dry_run {
                    self.store.save(&state).await?;
                }
                self.deliver(&mut state, &key, dry_run).await?;
                continue;
            }
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
            let name = discord::clean(&streamer.display_name, 80);
            let text = if start {
                format!("🔴 {name} さんのDiscord配信開始を検知しました\n内容：{title}")
            } else {
                format!("📷 {name} さんから配信スクリーンショット\n内容：{title}")
            };
            let pending = Pending {
                avatars: Vec::new(),
                metadata_fingerprint: None,
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
        Ok(active > 0 || occupied > 0)
    }

    pub async fn deliver(&mut self, state: &mut State, key: &str, dry_run: bool) -> Result<()> {
        let Some(mut pending) = state.entries.get(key).and_then(|e| e.pending.clone()) else {
            return Ok(());
        };
        // Old versions may have persisted an unsent announcement with a join
        // link. Remove that generated footer too, preserving the idempotency key.
        let text = pending
            .text
            .lines()
            .filter(|line| !line.starts_with("https://discord.com/channels/"))
            .collect::<Vec<_>>()
            .join("\n");
        if text != pending.text {
            pending.text = text;
            state.entries.get_mut(key).unwrap().pending = Some(pending.clone());
            if !dry_run {
                self.store.save(state).await?;
            }
        }
        if dry_run {
            // Do not put names, IDs, captions or screenshot URLs into public Actions logs.
            println!(
                "Dry run: would publish {} (content redacted).",
                if pending.is_start {
                    "start announcement"
                } else if pending.metadata_fingerprint.is_some() {
                    "stream information update"
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
        if !pending.avatars.is_empty() && pending.media_id.is_none() {
            let (bytes, description) =
                crate::avatars::collage(&self.mastodon.client, &pending.avatars).await?;
            pending.media_id = Some(self.mastodon.upload_bytes(bytes, &description).await?);
            state.entries.get_mut(key).unwrap().pending = Some(pending.clone());
            self.store.save(state).await?;
        }
        if let Some(attachment) = &pending.attachment
            && pending.media_id.is_none()
        {
            let artwork = attachment.source == discord::ImageSource::ActivityAsset;
            let description = if artwork {
                "Discord Rich Presenceで公開されているゲームの画像（配信のスクリーンショットではありません）"
            } else {
                "配信者本人が共有したDiscord配信のスクリーンショット"
            };
            match self.mastodon.upload(attachment, description).await {
                Ok(id) => pending.media_id = Some(id),
                Err(_) if artwork => {
                    // Decoration must never prevent automatic content delivery.
                    println!("Activity artwork unavailable; publishing metadata without image.");
                    pending.attachment = None;
                }
                Err(error) => return Err(error),
            }
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
            } else if pending.metadata_fingerprint.is_some() {
                "stream information update"
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
        if let Some(fingerprint) = pending.metadata_fingerprint {
            entry.metadata_fingerprint = Some(fingerprint);
        }
        if pending.is_start {
            entry.announced = true;
        }
        if let Some(attachment) = pending.attachment
            && attachment.source == discord::ImageSource::DiscordAttachment
        {
            entry.last_attachment = Some(attachment.id);
        }
        entry.last_post = Some(now);
    }
}

pub fn metadata_due(
    entry: &Entry,
    fingerprint: &str,
    now: DateTime<Utc>,
    interval_minutes: u64,
) -> bool {
    !entry.announced
        || entry.metadata_fingerprint.is_none()
        || (entry.metadata_fingerprint.as_deref() != Some(fingerprint)
            && entry.last_post.is_none_or(|last| {
                now.signed_duration_since(last).num_seconds() >= interval_minutes as i64 * 60
            }))
}
