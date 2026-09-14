//! Participant portraits come only from Discord's CDN; no authenticated CDN requests.
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use image::{ImageReader, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Cursor, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Avatar {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
}

fn hash(s: &str) -> bool {
    let s = s.strip_prefix("a_").unwrap_or(s);
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

pub fn member_avatar(member: &Value, guild: &str) -> Result<Avatar> {
    let user = &member["user"];
    let id = user["id"].as_str().context("Missing member ID")?;
    ensure!(crate::activity::snowflake(id), "Invalid member ID");
    let path = if let Some(avatar) = member["avatar"].as_str().filter(|s| hash(s))
        && crate::activity::snowflake(guild)
    {
        format!("guilds/{guild}/users/{id}/avatars/{avatar}.png")
    } else if let Some(avatar) = user["avatar"].as_str().filter(|s| hash(s)) {
        format!("avatars/{id}/{avatar}.png")
    } else {
        let discriminator = user["discriminator"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let index = if discriminator == 0 {
            (id.parse::<u64>()? >> 22) % 6
        } else {
            discriminator % 5
        };
        format!("embed/avatars/{index}.png")
    };
    Ok(Avatar {
        name: crate::discord::clean(
            member["nick"]
                .as_str()
                .or(user["global_name"].as_str())
                .or(user["username"].as_str())
                .unwrap_or("Discord参加者"),
            60,
        ),
        url: format!("https://cdn.discordapp.com/{path}?size=128"),
        streaming: false,
        game: None,
    })
}

pub fn validate_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("cdn.discordapp.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443),
        "Invalid avatar host"
    );
    let parts: Vec<_> = url.path().trim_start_matches('/').split('/').collect();
    let valid_hash = |s: &str| s.strip_suffix(".png").is_some_and(hash);
    let valid = match parts.as_slice() {
        ["avatars", user, image] => crate::activity::snowflake(user) && valid_hash(image),
        ["guilds", guild, "users", user, "avatars", image] => {
            crate::activity::snowflake(guild)
                && crate::activity::snowflake(user)
                && valid_hash(image)
        }
        ["embed", "avatars", image] => image
            .strip_suffix(".png")
            .is_some_and(|i| ["0", "1", "2", "3", "4", "5"].contains(&i)),
        _ => false,
    };
    ensure!(valid, "Invalid avatar path");
    Ok(())
}

async fn download(client: &reqwest::Client, avatar: &Avatar) -> Result<RgbaImage> {
    validate_url(&avatar.url)?;
    let mut response = crate::http::success(
        client
            .get(&avatar.url)
            .timeout(Duration::from_secs(8))
            .send()
            .await?,
        "Avatar download",
    )?;
    ensure!(
        response.content_length().is_none_or(|n| n <= 1024 * 1024),
        "Avatar too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= 1024 * 1024, "Avatar too large");
        bytes.extend(chunk);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader
        .decode()?
        .resize_to_fill(128, 128, image::imageops::FilterType::Triangle)
        .into_rgba8())
}

fn placeholder() -> RgbaImage {
    RgbaImage::from_fn(128, 128, |x, y| {
        let x = x as i32;
        let y = y as i32;
        if (x - 64).pow(2) + (y - 43).pow(2) < 20 * 20
            || ((x - 64).pow(2) + (y - 110).pow(2) < 40 * 40 && y > 73)
        {
            Rgba([210, 215, 225, 255])
        } else {
            Rgba([74, 82, 101, 255])
        }
    })
}

pub async fn collage(client: &reqwest::Client, avatars: &[Avatar]) -> Result<(Vec<u8>, String)> {
    ensure!(
        (2..=256).contains(&avatars.len()),
        "Avatar grid requires 2–256 participants"
    );
    let images: Vec<_> = stream::iter(avatars.iter().map(|avatar| async move {
        match download(client, avatar).await {
            Ok(image) => (image, false),
            Err(_) => (placeholder(), true),
        }
    }))
    .buffered(8)
    .collect()
    .await;
    let missing = images.iter().filter(|(_, failed)| *failed).count();
    println!(
        "Participant icons: {} downloaded; {} placeholders.",
        images.len() - missing,
        missing
    );
    let description = format!(
        "通話参加者（左から右、上から下）：{}。{}{}",
        avatars
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join("、"),
        avatars
            .iter()
            .map(|a| format!(
                "{}：{}{}",
                a.name,
                if a.streaming {
                    "配信中"
                } else {
                    "通話中"
                },
                a.game
                    .as_ref()
                    .map(|game| format!("、プレイ中：{game}"))
                    .unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join("。"),
        if missing > 0 {
            "。取得できないアイコンは灰色の人型で表示"
        } else {
            ""
        }
    );
    let description: String = description.chars().take(1500).collect();
    Ok((
        crate::card::render(
            avatars,
            &images
                .into_iter()
                .map(|(image, _)| image)
                .collect::<Vec<_>>(),
        )?,
        description,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn avatars_use_server_then_user_then_correct_default() {
        let mut m = json!({"user":{"id":"4194304","username":"name","avatar":"0123456789abcdef0123456789abcdef"},"avatar":"abcdef0123456789abcdef0123456789"});
        assert!(
            member_avatar(&m, "10")
                .unwrap()
                .url
                .contains("guilds/10/users/4194304/avatars/")
        );
        m["avatar"] = Value::Null;
        assert!(
            member_avatar(&m, "10")
                .unwrap()
                .url
                .contains("/avatars/4194304/")
        );
        m["user"]["avatar"] = json!("../../private");
        assert!(
            member_avatar(&m, "10")
                .unwrap()
                .url
                .contains("embed/avatars/1.png")
        );
        m["user"]["discriminator"] = json!("1234");
        assert!(
            member_avatar(&m, "10")
                .unwrap()
                .url
                .contains("embed/avatars/4.png")
        );
        for url in [
            "http://localhost/a.png",
            "https://cdn.discordapp.com.evil.test/embed/avatars/1.png",
            "https://cdn.discordapp.com/attachments/1/2/a.png",
            "https://cdn.discordapp.com/embed/avatars/6.png",
        ] {
            assert!(validate_url(url).is_err());
        }
        assert!(validate_url(&member_avatar(&m, "10").unwrap().url).is_ok());
    }
}
