//! Automatically available metadata. Presence is not proof of the shared screen.
use crate::discord::{Attachment, ImageSource, clean};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct StreamContext {
    /// Nonempty only for an occupied channel without a human stream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voice_members: Vec<String>,
    pub channel_name: String,
    pub participants: usize,
    pub activities: Vec<Activity>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Activity {
    pub name: String,
    pub kind: u64,
    pub details: Option<String>,
    pub state: Option<String>,
    pub image_text: Option<String>,
    pub party: Option<(u64, u64)>,
    pub artwork: Option<Attachment>,
}

impl StreamContext {
    pub fn from_guild(data: &Value, user: &str, channel: &str) -> Self {
        let channel_name = data["channels"]
            .as_array()
            .and_then(|items| items.iter().find(|c| c["id"] == channel))
            .and_then(|c| c["name"].as_str())
            .map(|s| clean(s, 40))
            .unwrap_or_else(|| "ボイスチャンネル".into());
        let members = data["members"].as_array();
        let participants = data["voice_states"]
            .as_array()
            .map(|states| {
                states
                    .iter()
                    .filter(|v| {
                        v["channel_id"] == channel
                            && !members.is_some_and(|items| {
                                items.iter().any(|m| {
                                    m["user"]["id"] == v["user_id"] && m["user"]["bot"] == true
                                })
                            })
                    })
                    .count()
            })
            .unwrap_or(0);
        let mut activities: Vec<_> = data["presences"]
            .as_array()
            .and_then(|items| items.iter().find(|p| p["user"]["id"] == user))
            .and_then(|p| p["activities"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Activity::parse)
            .collect();
        // Canonical ordering prevents updates caused only by array ordering.
        activities.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
        activities.truncate(2);
        Self {
            voice_members: Vec::new(),
            channel_name,
            participants,
            activities,
        }
    }

    pub fn fingerprint(&self, channel_id: &str) -> String {
        // Timestamps, viewers (unavailable), arbitrary URLs and RP secrets are
        // deliberately absent; elapsed time must not produce periodic posts.
        let data = serde_json::to_vec(&(channel_id, self)).expect("serializable context");
        format!("{:x}", Sha256::digest(data))
    }

    pub fn artwork(&self) -> Option<Attachment> {
        self.activities.iter().find_map(|a| a.artwork.clone())
    }

    pub fn render(&self, name: &str, start: bool) -> String {
        let name = clean(name, 60);
        let heading = if !self.voice_members.is_empty() {
            if start {
                "💬 Discordの通話参加を検知しました".into()
            } else {
                "💬 Discordの通話参加状況を更新しました".into()
            }
        } else if start {
            format!("🔴 {name} さんのDiscord配信を検知しました")
        } else {
            format!("🎮 {name} さんのDiscord配信情報を更新しました")
        };
        let mut lines = vec![format!(
            "場所：{}／通話参加 {}人",
            self.channel_name, self.participants
        )];
        if !self.voice_members.is_empty() {
            lines.push("現在、配信はありません".into());
            lines.push(format!("参加者：{}", self.voice_members.join("、")));
        } else if self.activities.is_empty() {
            lines.push("共有アプリの情報はDiscordから取得できませんでした".into());
        } else {
            lines.push("Discordのアクティビティ（共有画面とは一致しない場合があります）".into());
            for a in &self.activities {
                let label = match a.kind {
                    0 => "プレイ中",
                    1 => "外部配信表示",
                    5 => "競技中",
                    _ => "アプリ",
                };
                lines.push(format!("{label}：{}", a.name));
                if let Some(details) = &a.details {
                    lines.push(format!("内容：{details}"));
                }
                if let Some(state) = &a.state {
                    lines.push(format!("状態：{state}"));
                }
                if let Some(text) = &a.image_text {
                    lines.push(format!("ゲーム情報：{text}"));
                }
                if let Some((current, max)) = a.party {
                    lines.push(format!("ゲーム内パーティー：{current}/{max}"));
                }
            }
        }
        let budget = 480_usize.saturating_sub(heading.chars().count() + 1);
        let content = lines.join("\n");
        let text = if content.chars().count() > budget {
            format!(
                "{}…",
                content
                    .chars()
                    .take(budget.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            content
        };
        format!("{heading}\n{text}")
    }
}

impl Activity {
    fn parse(raw: &Value) -> Option<Self> {
        let kind = raw["type"].as_u64()?;
        if ![0, 1, 5].contains(&kind) {
            return None;
        }
        let name = text(&raw["name"], 70)?;
        let details = text(&raw["details"], 90);
        let state = text(&raw["state"], 80).filter(|s| Some(s) != details.as_ref());
        let image_text = text(&raw["assets"]["large_text"], 70)
            .filter(|s| s != &name && Some(s) != details.as_ref() && Some(s) != state.as_ref());
        let party = raw["party"]["size"]
            .as_array()
            .and_then(|s| Some((s.first()?.as_u64()?, s.get(1)?.as_u64()?)))
            .filter(|(current, max)| *max > 0 && *max <= 100_000 && current <= max);
        let artwork = raw["application_id"]
            .as_str()
            .zip(raw["assets"]["large_image"].as_str())
            .filter(|(app, asset)| snowflake(app) && snowflake(asset))
            .map(|(app, asset)| Attachment {
                id: format!("asset:{app}:{asset}"),
                url: format!("https://cdn.discordapp.com/app-assets/{app}/{asset}.png?size=512"),
                content_type: Some("image/png".into()),
                size: 0,
                source: ImageSource::ActivityAsset,
            });
        Some(Self {
            name,
            kind,
            details,
            state,
            image_text,
            party,
            artwork,
        })
    }
}

fn text(value: &Value, max: usize) -> Option<String> {
    value
        .as_str()
        .map(|s| clean(s, max))
        .filter(|s| !s.trim().is_empty())
}
pub fn snowflake(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn guild() -> Value {
        json!({"channels":[{"id":"10","name":"Games"}],"members":[{"user":{"id":"20"}},{"user":{"id":"21","bot":true}}],"voice_states":[{"user_id":"20","channel_id":"10"},{"user_id":"21","channel_id":"10"}],"presences":[{"user":{"id":"20"},"activities":[{"type":0,"name":"Game","details":"Ranked match","state":"Map A","timestamps":{"start":1000},"party":{"id":"secret-party","size":[2,4]},"secrets":{"join":"secret-join"},"application_id":"30","assets":{"large_image":"40","large_text":"Round 2"}},{"type":2,"name":"private listening"}]}]})
    }
    #[test]
    fn extracts_automatic_details_without_unrelated_activity_or_secrets() {
        let context = StreamContext::from_guild(&guild(), "20", "10");
        assert_eq!(context.participants, 1);
        assert_eq!(context.activities.len(), 1);
        let text = context.render("User", true);
        assert!(!text.contains("https://discord.com/channels/"));
        for field in ["Ranked match", "Map A", "Round 2", "2/4", "通話参加 1人"] {
            assert!(text.contains(field));
        }
        assert!(
            !text.contains("private")
                && !serde_json::to_string(&context).unwrap().contains("secret")
        );
        assert!(
            context
                .artwork()
                .unwrap()
                .url
                .ends_with("/30/40.png?size=512")
        );
    }
    #[test]
    fn fingerprints_ignore_clock_and_detect_real_content_changes() {
        let original = guild();
        let mut changed = original.clone();
        changed["presences"][0]["activities"][0]["timestamps"]["start"] = json!(9999);
        let first = StreamContext::from_guild(&original, "20", "10").fingerprint("10");
        assert_eq!(
            first,
            StreamContext::from_guild(&changed, "20", "10").fingerprint("10")
        );
        changed["presences"][0]["activities"][0]["state"] = json!("Map B");
        assert_ne!(
            first,
            StreamContext::from_guild(&changed, "20", "10").fingerprint("10")
        );
    }
    #[test]
    fn handles_missing_presence_long_unicode_and_unsafe_assets() {
        let mut data = guild();
        data["presences"][0]["activities"][0]["assets"]["large_image"] = json!("../../private");
        data["presences"][0]["activities"][0]["details"] = json!("あ".repeat(1000));
        let context = StreamContext::from_guild(&data, "20", "10");
        assert!(context.artwork().is_none());
        assert!(context.render(&"名".repeat(1000), false).chars().count() <= 480);
        assert!(
            StreamContext::from_guild(&data, "missing", "10")
                .render("User", true)
                .contains("取得できません")
        );
    }
}
