use crate::{discord::Attachment, http};
use anyhow::{Context, Result, ensure};
use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Mastodon {
    pub client: Client,
    pub base: String,
    pub token: String,
    pub visibility: String,
}

impl Mastodon {
    pub async fn verify(&self) -> Result<()> {
        http::success(
            http::send(|| {
                self.client
                    .get(format!("{}/api/v1/accounts/verify_credentials", self.base))
                    .bearer_auth(&self.token)
            })
            .await?,
            "Mastodon credentials (read:accounts required)",
        )?;
        Ok(())
    }

    pub async fn upload(&self, attachment: &Attachment, description: &str) -> Result<String> {
        match attachment.source {
            crate::discord::ImageSource::DiscordAttachment => {
                validate_attachment_url(&attachment.url)?
            }
            crate::discord::ImageSource::ActivityAsset => {
                validate_activity_asset_url(&attachment.url)?
            }
        }
        // A separate unauthenticated request never sends the Mastodon/Discord token to a CDN.
        let mut response = http::success(
            http::send(|| self.client.get(&attachment.url)).await?,
            "Discord image download",
        )?;
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= 8 * 1024 * 1024),
            "Image exceeds 8 MiB"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Image download failed"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "Image exceeds 8 MiB"
            );
            bytes.extend(chunk);
        }
        self.upload_bytes(bytes, description).await
    }

    pub(crate) async fn upload_bytes(&self, bytes: Vec<u8>, description: &str) -> Result<String> {
        let (mime, name) =
            image_type(&bytes).context("Unsupported image: expected PNG, JPEG or WebP")?;
        let response = http::send(|| {
            self.client
                .post(format!("{}/api/v2/media", self.base))
                .bearer_auth(&self.token)
                .multipart(
                    Form::new()
                        .text("description", description.to_owned())
                        .part(
                            "file",
                            Part::bytes(bytes.clone())
                                .file_name(name)
                                .mime_str(mime)
                                .expect("known MIME"),
                        ),
                )
        })
        .await?;
        let media: Value = http::success(response, "Mastodon upload")?.json().await?;
        let id = media["id"].as_str().context("Missing media ID")?.to_owned();
        if media["url"].is_string() {
            return Ok(id);
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let response = http::send(|| {
                self.client
                    .get(format!("{}/api/v1/media/{id}", self.base))
                    .bearer_auth(&self.token)
            })
            .await?;
            if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                continue;
            }
            let media: Value = http::success(response, "Mastodon media processing")?
                .json()
                .await?;
            if media["url"].is_string() {
                return Ok(id);
            }
        }
        anyhow::bail!("Mastodon media processing timed out");
    }

    pub async fn post(&self, key: &str, text: &str, media_id: Option<&str>) -> Result<String> {
        let body = json!({"status":text,"visibility":self.visibility,"language":"ja","media_ids":media_id.into_iter().collect::<Vec<_>>()});
        let response = http::send(|| {
            self.client
                .post(format!("{}/api/v1/statuses", self.base))
                .bearer_auth(&self.token)
                .header("Idempotency-Key", key)
                .json(&body)
        })
        .await?;
        let status: Value = http::success(response, "Mastodon status")?.json().await?;
        Ok(status["id"]
            .as_str()
            .context("Missing status ID")?
            .to_owned())
    }
}

fn validate_attachment_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid attachment URL"))?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && matches!(
                url.host_str(),
                Some("cdn.discordapp.com" | "media.discordapp.net")
            )
            && url.path().starts_with("/attachments/"),
        "Only official Discord attachment CDN URLs are accepted"
    );
    Ok(())
}

fn validate_activity_asset_url(value: &str) -> Result<()> {
    let url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid activity asset URL"))?;
    let parts: Vec<_> = url.path().split('/').collect();
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("cdn.discordapp.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && parts.len() == 4
            && parts[1] == "app-assets"
            && crate::activity::snowflake(parts[2])
            && parts[3]
                .strip_suffix(".png")
                .is_some_and(crate::activity::snowflake),
        "Only official Discord application assets are accepted"
    );
    Ok(())
}

fn image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "screenshot.png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", "screenshot.jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "screenshot.webp"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    #[tokio::test]
    async fn asynchronous_media_processing_finishes_before_status_creation() {
        let server = MockServer::start().await;
        let mastodon = Mastodon {
            client: http::client().unwrap(),
            base: server.uri(),
            token: "test-token".into(),
            visibility: "private".into(),
        };
        Mock::given(method("POST"))
            .and(path("/api/v2/media"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({"id":"123","url":null})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/media/123"))
            .respond_with(ResponseTemplate::new(206))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/media/123"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id":"123","url":"https://example.org/image.png"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/statuses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"456"})))
            .expect(1)
            .mount(&server)
            .await;
        let id = mastodon
            .upload_bytes(b"\x89PNG\r\n\x1a\nmock-image".to_vec(), "alt text")
            .await
            .unwrap();
        mastodon
            .post("stable-key", "caption", Some(&id))
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        assert_eq!(body["media_ids"], json!(["123"]));
        assert_eq!(body["visibility"], "private");
        let multipart = String::from_utf8_lossy(&requests[0].body);
        assert!(multipart.contains("image/png") && multipart.contains("alt text"));
    }
    #[test]
    fn rejects_untrusted_media_and_active_content() {
        assert!(
            validate_activity_asset_url(
                "https://cdn.discordapp.com/app-assets/123/456.png?size=512"
            )
            .is_ok()
        );
        for url in [
            "https://example.com/app-assets/123/456.png",
            "https://cdn.discordapp.com/app-assets/a/456.png",
            "https://cdn.discordapp.com/app-assets/123/../456.png",
            "https://cdn.discordapp.com/attachments/123/456.png",
        ] {
            assert!(validate_activity_asset_url(url).is_err());
        }
        for url in [
            "http://127.0.0.1/attachments/a",
            "https://cdn.discordapp.com.evil.test/attachments/a",
            "https://cdn.discordapp.com@evil.test/attachments/a",
            "https://cdn.discordapp.com:444/attachments/a",
            "https://cdn.discordapp.com/other",
        ] {
            assert!(validate_attachment_url(url).is_err());
        }
        assert!(
            validate_attachment_url("https://cdn.discordapp.com/attachments/1/2/image.png?ex=abc")
                .is_ok()
        );
        assert!(image_type(b"<svg></svg>").is_none());
        assert_eq!(
            image_type(b"\x89PNG\r\n\x1a\n"),
            Some(("image/png", "screenshot.png"))
        );
    }
}
