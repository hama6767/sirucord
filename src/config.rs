use anyhow::{Result, ensure};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mastodon: Mastodon,
    #[serde(default)]
    pub streamers: Vec<Streamer>,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default = "interval")]
    pub screenshot_interval_minutes: u64,
    #[serde(default = "interval")]
    pub activity_update_interval_minutes: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub guild_id: String,
    #[serde(default)]
    pub use_activity: bool,
    pub voice_channel_ids: Vec<String>,
    pub default_title: String,
    pub announcement_channel_id: Option<String>,
    pub share_channel_id: Option<String>,
    #[serde(default)]
    pub screenshots: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mastodon {
    pub base_url: String,
    #[serde(default = "visibility")]
    pub visibility: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Streamer {
    pub guild_id: String,
    pub user_id: String,
    pub display_name: String,
    /// Only these voice channels are announced. An empty list is invalid.
    pub voice_channel_ids: Vec<String>,
    pub default_title: String,
    pub announcement_channel_id: Option<String>,
    #[serde(default)]
    pub screenshots: bool,
}

fn interval() -> u64 {
    30
}
fn visibility() -> String {
    "unlisted".into()
}

impl Streamer {
    pub fn key(&self) -> String {
        format!("{}:{}", self.guild_id, self.user_id)
    }
}

impl Config {
    pub fn parse(input: &str) -> Result<Self> {
        let config: Self = toml::from_str(input).map_err(|_| {
            anyhow::anyhow!(
                "Invalid configuration TOML (details redacted; compare config.example.toml)"
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.mastodon.base_url)?;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some(),
            "Mastodon base_url must use HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Mastodon base_url must be an origin without credentials, path, query or fragment"
        );
        ensure!(
            ["public", "unlisted", "private"].contains(&self.mastodon.visibility.as_str()),
            "visibility must be public, unlisted or private"
        );
        ensure!(
            !self.streamers.is_empty() || !self.servers.is_empty(),
            "Configure at least one server or streamer"
        );
        ensure!(
            self.streamers.len() <= 50 && self.servers.len() <= 20,
            "At most 50 individual streamers and 20 servers are supported"
        );
        ensure!(
            (5..=1440).contains(&self.screenshot_interval_minutes),
            "screenshot interval must be 5–1440 minutes"
        );
        ensure!(
            (30..=1440).contains(&self.activity_update_interval_minutes),
            "Activity update interval must be 30–1440 minutes"
        );
        let mut keys = HashSet::new();
        let mut guilds = HashSet::new();
        for s in &self.servers {
            ensure!(guilds.insert(&s.guild_id), "Duplicate server");
            ensure!(snowflake(&s.guild_id), "Invalid guild ID");
            ensure!(
                s.share_channel_id.as_ref().is_none_or(|id| snowflake(id)),
                "Invalid share channel ID"
            );
            ensure!(
                !s.voice_channel_ids.is_empty()
                    && s.voice_channel_ids.iter().all(|id| snowflake(id)),
                "Explicit voice channel IDs are required"
            );
            ensure!(
                s.announcement_channel_id
                    .as_ref()
                    .is_none_or(|id| snowflake(id)),
                "Invalid announcement channel ID"
            );
            ensure!(
                !s.screenshots || s.announcement_channel_id.is_some(),
                "Screenshots require an announcement channel"
            );
            ensure!(
                !s.default_title.trim().is_empty() && s.default_title.chars().count() <= 200,
                "default_title must be 1–200 characters"
            );
        }
        for s in &self.streamers {
            ensure!(
                !guilds.contains(&s.guild_id),
                "Use either server discovery or individual streamers for each guild"
            );
            ensure!(keys.insert(s.key()), "Duplicate streamer");
            ensure!(
                snowflake(&s.guild_id) && snowflake(&s.user_id),
                "Guild/user IDs must be numeric Discord IDs"
            );
            ensure!(
                !s.voice_channel_ids.is_empty()
                    && s.voice_channel_ids.iter().all(|id| snowflake(id)),
                "Explicit voice channel IDs are required"
            );
            ensure!(
                s.announcement_channel_id
                    .as_ref()
                    .is_none_or(|id| snowflake(id)),
                "Invalid announcement channel ID"
            );
            ensure!(
                !s.screenshots || s.announcement_channel_id.is_some(),
                "Screenshots require an announcement channel"
            );
            ensure!(
                !s.display_name.trim().is_empty() && s.display_name.chars().count() <= 80,
                "display_name must be 1–80 characters"
            );
            ensure!(
                !s.default_title.trim().is_empty() && s.default_title.chars().count() <= 200,
                "default_title must be 1–200 characters"
            );
        }
        Ok(())
    }
}

fn snowflake(s: &str) -> bool {
    (1..=20).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok_and(|v| v > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn example_is_valid() {
        Config::parse(include_str!("../config.example.toml")).unwrap();
    }
    #[test]
    fn rejects_dangerous_or_misspelled_configuration() {
        let example = include_str!("../config.example.toml");
        for bad in [
            example.replace("https://mastodon.social", "http://mastodon.social"),
            example.replace("https://mastodon.social", "https://secret@mastodon.social"),
            example.replace("screenshots = false", "screenshot = false"),
            example.replace(
                "voice_channel_ids = [\"345678901234567890\"]",
                "voice_channel_ids = []",
            ),
        ] {
            assert!(Config::parse(&bad).is_err());
        }
    }
}
