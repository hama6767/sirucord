use crate::{
    config::{Server, Streamer},
    http,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio_tungstenite::tungstenite::Message;

pub struct Discord {
    pub client: Client,
    pub base: String,
    pub token: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Voice {
    #[serde(skip)]
    pub context: Option<crate::activity::StreamContext>,
    pub channel_id: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub self_stream: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub bot: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Attachment {
    #[serde(default)]
    pub source: ImageSource,
    pub id: String,
    pub url: String,
    pub content_type: Option<String>,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    #[default]
    DiscordAttachment,
    ActivityAsset,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Announcement {
    pub id: String,
    pub author: User,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub webhook_id: Option<String>,
}

impl Discord {
    pub async fn voice(&self, s: &Streamer) -> Result<Option<Voice>> {
        let response = http::send(|| {
            self.client
                .get(format!(
                    "{}/guilds/{}/voice-states/{}",
                    self.base, s.guild_id, s.user_id
                ))
                .header("Authorization", format!("Bot {}", self.token))
        })
        .await?;
        if response.status() == StatusCode::NOT_FOUND {
            let body: Value = response.json().await?;
            // A missing guild/member/permission must never be treated as stream end.
            if body["code"].as_u64() == Some(10065) {
                return Ok(None);
            }
            bail!("Discord: unexpected 404; check server membership and permissions");
        }
        let voice: Voice = http::success(response, "Discord voice")?.json().await?;
        Ok(voice.self_stream.then_some(voice).filter(|v| {
            v.channel_id
                .as_ref()
                .is_some_and(|id| s.voice_channel_ids.contains(id))
        }))
    }

    pub async fn announcements(&self, channel: &str) -> Result<Vec<Announcement>> {
        let response = http::send(|| {
            self.client
                .get(format!(
                    "{}/channels/{channel}/messages?limit=100",
                    self.base
                ))
                .header("Authorization", format!("Bot {}", self.token))
        })
        .await?;
        Ok(http::success(response, "Discord announcements")?
            .json()
            .await?)
    }

    /// A short Gateway snapshot discovers streams and occupied voice channels. It does
    /// not keep a runner alive. An incomplete/unavailable guild fails the whole
    /// snapshot, so an outage cannot accidentally mark streams as ended.
    pub async fn discover(&self, servers: &[Server]) -> Result<Vec<(Streamer, Voice)>> {
        if servers.is_empty() {
            return Ok(Vec::new());
        }
        let response = http::send(|| {
            self.client
                .get(format!("{}/gateway/bot", self.base))
                .header("Authorization", format!("Bot {}", self.token))
        })
        .await?;
        let gateway: Value = http::success(response, "Discord gateway")?.json().await?;
        ensure!(
            gateway["session_start_limit"]["remaining"]
                .as_u64()
                .unwrap_or(0)
                > 0,
            "Discord identify budget exhausted; wait for reset"
        );
        let url = gateway["url"].as_str().context("Missing gateway URL")?;
        let parsed = reqwest::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "wss"
                && parsed.host_str() == Some("gateway.discord.gg")
                && parsed.username().is_empty()
                && parsed.password().is_none(),
            "Unexpected Discord gateway host"
        );
        tokio::time::timeout(
            Duration::from_secs(40),
            self.snapshot(&format!("{url}/?v=10&encoding=json"), servers),
        )
        .await
        .context("Discord snapshot timed out; state preserved")?
    }

    async fn snapshot(&self, url: &str, servers: &[Server]) -> Result<Vec<(Streamer, Voice)>> {
        let (mut socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|_| anyhow::anyhow!("Discord Gateway connection failed"))?;
        let mut awaiting: HashSet<String> = servers.iter().map(|s| s.guild_id.clone()).collect();
        let mut result = Vec::new();
        let mut sequence = Value::Null;
        let mut heartbeat = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let mut acknowledged = true;
        let intents = 129
            | if servers.iter().any(|s| s.use_activity) {
                256
            } else {
                0
            };
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    ensure!(acknowledged, "Discord heartbeat unacknowledged; snapshot discarded");
                    socket.send(Message::Text(json!({"op":1,"d":sequence}).to_string().into())).await?;
                    acknowledged = false;
                }
                message = socket.next() => {
                    let message = message.context("Discord disconnected during snapshot")??;
                    let Message::Text(text) = message else {
                        if message.is_close() { bail!("Discord closed snapshot connection"); }
                        continue;
                    };
                    let event: Value = serde_json::from_str(&text)?;
                    if !event["s"].is_null() { sequence = event["s"].clone(); }
                    match event["op"].as_u64() {
                        Some(10) => {
                            let ms = event["d"]["heartbeat_interval"].as_u64().context("Missing heartbeat interval")?;
                            ensure!(ms > 0, "Invalid heartbeat interval");
                            heartbeat = tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(ms), Duration::from_millis(ms));
                            socket.send(Message::Text(json!({"op":2,"d":{"token":self.token,"intents":intents,"properties":{"os":std::env::consts::OS,"browser":"sirucord","device":"sirucord"}}}).to_string().into())).await?;
                        }
                        Some(11) => acknowledged = true,
                        Some(1) => { socket.send(Message::Text(json!({"op":1,"d":sequence}).to_string().into())).await?; }
                        Some(7 | 9) => bail!("Discord requested reconnect; retry on next run"),
                        Some(0) if event["t"] == "READY" => {
                            let guilds = event["d"]["guilds"].as_array().context("Missing READY guilds")?;
                            ensure!(awaiting.iter().all(|id| guilds.iter().any(|g| g["id"].as_str() == Some(id))), "Bot is not a member of every configured server");
                        }
                        Some(0) if event["t"] == "GUILD_CREATE" => {
                            let data = &event["d"];
                            let id = data["id"].as_str().context("Missing guild ID")?;
                            if let Some(server) = servers.iter().find(|s| s.guild_id == id) {
                                ensure!(data["unavailable"] != true, "Configured Discord server is unavailable");
                                result.extend(parse_guild(server, data)?);
                                awaiting.remove(id);
                                if awaiting.is_empty() { let _ = socket.close(None).await; return Ok(result); }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

pub fn parse_guild(server: &Server, data: &Value) -> Result<Vec<(Streamer, Voice)>> {
    let members: HashMap<&str, &Value> = data["members"]
        .as_array()
        .context("Missing guild members")?
        .iter()
        .filter_map(|m| m["user"]["id"].as_str().map(|id| (id, m)))
        .collect();
    let channels = data["channels"]
        .as_array()
        .context("Missing guild channels")?;
    ensure!(
        server
            .voice_channel_ids
            .iter()
            .all(|id| channels.iter().any(|c| c["id"].as_str() == Some(id))),
        "Configured voice channel is missing or inaccessible"
    );
    let mut result = Vec::new();
    for raw in data["voice_states"]
        .as_array()
        .context("Missing guild voice states")?
    {
        let mut voice: Voice = serde_json::from_value(raw.clone())?;
        if !voice.self_stream
            || !voice
                .channel_id
                .as_ref()
                .is_some_and(|id| server.voice_channel_ids.contains(id))
        {
            continue;
        }
        let id = raw["user_id"].as_str().context("Missing voice user ID")?;
        let member = members
            .get(id)
            .context("Streaming member absent from snapshot; retry later")?;
        if member["user"]["bot"] == true {
            continue;
        }
        let context = crate::activity::StreamContext::from_guild(
            data,
            id,
            voice.channel_id.as_deref().unwrap(),
        )?;
        // Keep a single stable channel target while alone, even if Go Live is
        // toggled. This prevents repeating the same gathering announcement.
        if context.participants == 1 {
            continue;
        }
        if server.use_activity {
            voice.context = Some(context);
        }
        let name = member["nick"]
            .as_str()
            .or(member["user"]["global_name"].as_str())
            .or(member["user"]["username"].as_str())
            .unwrap_or("Discord配信者");
        result.push((
            Streamer {
                guild_id: server.guild_id.clone(),
                user_id: id.into(),
                display_name: name.into(),
                voice_channel_ids: server.voice_channel_ids.clone(),
                default_title: if server.use_activity {
                    activity_title(data, id).unwrap_or_else(|| server.default_title.clone())
                } else {
                    server.default_title.clone()
                },
                announcement_channel_id: server.announcement_channel_id.clone(),
                screenshots: server.screenshots,
            },
            voice,
        ));
    }
    // A single channel-level notification covers human presence when nobody is
    // streaming. Synthetic target IDs cannot collide with Discord user snowflakes.
    for channel in &server.voice_channel_ids {
        if result
            .iter()
            .any(|(_, v)| v.channel_id.as_ref() == Some(channel))
        {
            continue;
        }
        let mut context = crate::activity::StreamContext::from_guild(data, "", channel)?;
        if context.participants == 0 {
            continue;
        }
        context.voice_members = context.avatars.iter().map(|a| a.name.clone()).collect();
        let streaming = data["voice_states"].as_array().unwrap().iter().any(|v| {
            v["channel_id"].as_str() == Some(channel)
                && v["self_stream"] == true
                && v["user_id"]
                    .as_str()
                    .and_then(|id| members.get(id))
                    .is_some_and(|m| m["user"]["bot"] != true)
        });
        result.push((
            Streamer {
                guild_id: server.guild_id.clone(),
                user_id: format!("voice-{channel}"),
                display_name: "通話参加者".into(),
                voice_channel_ids: vec![channel.clone()],
                default_title: server.default_title.clone(),
                announcement_channel_id: None,
                screenshots: false,
            },
            Voice {
                channel_id: Some(channel.clone()),
                session_id: format!("voice-{channel}"),
                self_stream: streaming,
                context: Some(context),
            },
        ));
    }
    Ok(result)
}

fn activity_title(data: &Value, user: &str) -> Option<String> {
    let presence = data["presences"]
        .as_array()?
        .iter()
        .find(|p| p["user"]["id"].as_str() == Some(user))?;
    let activity = presence["activities"]
        .as_array()?
        .iter()
        .find(|a| a["type"].as_u64() == Some(0))?;
    let name = clean(activity["name"].as_str()?, 150);
    if name.trim().is_empty() {
        None
    } else {
        Some(format!("{name}（Discordのプレイ中表示）"))
    }
}

pub fn title(messages: &[Announcement], user: &str, now: DateTime<Utc>) -> Option<String> {
    messages
        .iter()
        .filter(|m| {
            eligible(m, user)
                && m.timestamp <= now
                && m.timestamp >= now - chrono::Duration::hours(12)
        })
        .filter_map(|m| {
            m.content
                .strip_prefix("!sirucord ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| (m.timestamp, s))
        })
        .max_by_key(|(timestamp, _)| *timestamp)
        .map(|(_, text)| clean(text, 200))
}

pub fn screenshot(
    messages: &[Announcement],
    user: &str,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    used: Option<&str>,
) -> Option<Attachment> {
    messages
        .iter()
        .filter(|m| {
            eligible(m, user)
                && m.content.starts_with("!sirucord")
                && (m.content == "!sirucord" || m.content.starts_with("!sirucord "))
                && m.timestamp >= since
                && m.timestamp <= now
        })
        .flat_map(|m| m.attachments.iter().map(move |a| (m.timestamp, a)))
        .filter(|(_, a)| {
            used.is_none_or(|previous| {
                a.id.parse::<u64>()
                    .ok()
                    .zip(previous.parse::<u64>().ok())
                    .is_some_and(|(current, last)| current > last)
            }) && a.size <= 8 * 1024 * 1024
                && matches!(
                    a.content_type.as_deref(),
                    Some("image/png" | "image/jpeg" | "image/webp")
                )
        })
        .max_by_key(|(timestamp, _)| *timestamp)
        .map(|(_, a)| a.clone())
}

fn eligible(m: &Announcement, user: &str) -> bool {
    m.author.id == user && !m.author.bot && m.webhook_id.is_none()
}

pub fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .replace('@', "＠")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gateway_waits_for_hello_identifies_and_handles_heartbeat() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            // A heartbeat before Hello would violate the expected handshake.
            assert!(
                tokio::time::timeout(Duration::from_millis(30), ws.next())
                    .await
                    .is_err()
            );
            ws.send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":1000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let identify: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(identify["op"], 2);
            assert_eq!(identify["d"]["intents"], 129);
            assert_eq!(identify["d"]["token"], "test-token");
            ws.send(Message::Text(
                json!({"op":0,"s":1,"t":"READY","d":{"guilds":[{"id":"1"}]}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(json!({"op":1,"d":null}).to_string().into()))
                .await
                .unwrap();
            let beat: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(beat, json!({"op":1,"d":1}));
            ws.send(Message::Text(json!({"op":11,"d":null}).to_string().into()))
                .await
                .unwrap();
            ws.send(Message::Text(json!({"op":0,"s":2,"t":"GUILD_CREATE","d":{"id":"1","members":[{"user":{"id":"3","username":"Streamer"}}],"channels":[{"id":"2"}],"voice_states":[{"user_id":"3","channel_id":"2","session_id":"session","self_stream":true}]}}).to_string().into())).await.unwrap();
            let _ = ws.next().await;
        });
        let discord = Discord {
            client: http::client().unwrap(),
            base: "unused".into(),
            token: "test-token".into(),
        };
        let servers = [Server {
            guild_id: "1".into(),
            use_activity: false,
            voice_channel_ids: vec!["2".into()],
            default_title: "title".into(),
            announcement_channel_id: None,
            screenshots: false,
        }];
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            discord.snapshot(&format!("ws://{address}"), &servers),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.key(), "1:voice-2");
        assert_eq!(
            result[0].1.context.as_ref().unwrap().render("", true),
            "汁が開始されました！集合汁！"
        );
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_missing_configured_guild_fails_closed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::Text(
                json!({"op":10,"d":{"heartbeat_interval":45000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let _ = ws.next().await;
            ws.send(Message::Text(
                json!({"op":0,"s":1,"t":"READY","d":{"guilds":[]}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        });
        let discord = Discord {
            client: http::client().unwrap(),
            base: "unused".into(),
            token: "test-token".into(),
        };
        let servers = [Server {
            guild_id: "1".into(),
            use_activity: false,
            voice_channel_ids: vec!["2".into()],
            default_title: "title".into(),
            announcement_channel_id: None,
            screenshots: false,
        }];
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            discord.snapshot(&format!("ws://{address}"), &servers),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert!(err.to_string().contains("not a member"));
        peer.await.unwrap();
    }
    #[test]
    fn monitors_both_channels_and_keeps_the_remaining_occupied_channel() {
        let server = Server {
            guild_id: "1".into(),
            use_activity: true,
            voice_channel_ids: vec!["2".into(), "6".into()],
            default_title: "test".into(),
            announcement_channel_id: None,
            screenshots: false,
        };
        let mut data = json!({"id":"1","channels":[{"id":"2","name":"A"},{"id":"6","name":"B"}],
            "members":[{"user":{"id":"3","username":"Alice"}},{"user":{"id":"4","username":"Bob"}},{"user":{"id":"5","bot":true}}],
            "voice_states":[{"user_id":"3","channel_id":"2","session_id":"a","self_stream":true},{"user_id":"4","channel_id":"6","session_id":"b"},{"user_id":"5","channel_id":"6","session_id":"bot"}]});
        let found = parse_guild(&server, &data).unwrap();
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .all(|(_, v)| v.context.as_ref().unwrap().participants == 1)
        );
        data["voice_states"].as_array_mut().unwrap().remove(0);
        let found = parse_guild(&server, &data).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.channel_id.as_deref(), Some("6"));
        data["voice_states"].as_array_mut().unwrap().remove(0);
        assert!(parse_guild(&server, &data).unwrap().is_empty());
        data["channels"].as_array_mut().unwrap().pop();
        assert!(parse_guild(&server, &data).is_err());
    }

    #[test]
    fn occupied_channels_notify_without_streams_and_ignore_bots() {
        let server = Server {
            guild_id: "1".into(),
            use_activity: true,
            voice_channel_ids: vec!["2".into()],
            default_title: "test".into(),
            announcement_channel_id: None,
            screenshots: false,
        };
        let mut data = json!({"channels":[{"id":"2","name":"雑談"}],
            "members":[{"user":{"id":"3","username":"Alice"}},{"user":{"id":"4","username":"Bob"}},{"user":{"id":"5","bot":true}}],
            "voice_states":[{"user_id":"3","channel_id":"2","session_id":"a"},{"user_id":"4","channel_id":"2","session_id":"b"},{"user_id":"5","channel_id":"2","session_id":"c","self_stream":true}]});
        let found = parse_guild(&server, &data).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.key(), "1:voice-2");
        assert!(!found[0].1.self_stream);
        let context = found[0].1.context.as_ref().unwrap();
        let rendered = context.render("", true);
        for expected in ["通話参加 2人", "Alice", "Bob", "現在、配信はありません"] {
            assert!(rendered.contains(expected));
        }
        assert!(!rendered.contains("https://"));
        let fingerprint = context.fingerprint("2");
        data["voice_states"].as_array_mut().unwrap().reverse();
        let same = parse_guild(&server, &data).unwrap();
        assert_eq!(
            fingerprint,
            same[0].1.context.as_ref().unwrap().fingerprint("2")
        );
        data["members"][1]["user"]["username"] = json!("Carol");
        assert_ne!(
            fingerprint,
            parse_guild(&server, &data).unwrap()[0]
                .1
                .context
                .as_ref()
                .unwrap()
                .fingerprint("2")
        );
        data["voice_states"][2]["self_stream"] = json!(true);
        let streaming = parse_guild(&server, &data).unwrap();
        assert_eq!(streaming.len(), 1); // No extra channel notification alongside the stream.
        assert_eq!(streaming[0].0.user_id, "3");
        assert!(streaming[0].1.self_stream);
        data["voice_states"] = json!([{"user_id":"5","channel_id":"2","session_id":"bot"}]);
        assert!(parse_guild(&server, &data).unwrap().is_empty());
        data["voice_states"] = json!([]);
        assert!(parse_guild(&server, &data).unwrap().is_empty());
        data["voice_states"] =
            json!([{"user_id":"missing","channel_id":"2","session_id":"missing"}]);
        assert!(parse_guild(&server, &data).is_err());
    }

    #[test]
    fn discovers_only_human_streams_in_allowed_channels() {
        let server = Server {
            guild_id: "1".into(),
            use_activity: false,
            voice_channel_ids: vec!["2".into()],
            default_title: "test".into(),
            announcement_channel_id: None,
            screenshots: false,
        };
        let data = json!({"channels":[{"id":"2"}],"members":[{"user":{"id":"3","username":"name"},"nick":"nickname"},{"user":{"id":"4","bot":true}}],"voice_states":[{"user_id":"3","channel_id":"2","session_id":"s","self_stream":true},{"user_id":"4","channel_id":"2","session_id":"b","self_stream":true},{"user_id":"5","channel_id":"9","session_id":"x","self_stream":true}]});
        let found = parse_guild(&server, &data).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].1.context.as_ref().unwrap().voice_members,
            vec!["nickname"]
        );
        let mut stopped = data.clone();
        stopped["voice_states"][0]["self_stream"] = json!(false);
        let stopped = parse_guild(&server, &stopped).unwrap();
        assert_eq!(found[0].0.key(), stopped[0].0.key());
        assert_eq!(found[0].1.session_id, stopped[0].1.session_id);
        assert_eq!(
            found[0].1.context.as_ref().unwrap().fingerprint("2"),
            stopped[0].1.context.as_ref().unwrap().fingerprint("2")
        );
        let activity = json!({"presences":[{"user":{"id":"3"},"activities":[{"type":4,"name":"custom status"},{"type":0,"name":"Minecraft"}]}]});
        assert_eq!(
            activity_title(&activity, "3").unwrap(),
            "Minecraft（Discordのプレイ中表示）"
        );
        assert!(activity_title(&activity, "9").is_none());
        assert!(
            parse_guild(
                &server,
                &json!({"members":[],"channels":[],"voice_states":[]})
            )
            .is_err()
        );
    }
}
