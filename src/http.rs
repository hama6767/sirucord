use anyhow::{Result, bail};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::time::Duration;

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("sirucord/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Rebuild requests so multipart bodies are retryable too. Callers provide stable
/// idempotency keys for status writes. Never include response bodies or URLs in errors.
pub async fn send(build: impl Fn() -> RequestBuilder) -> Result<Response> {
    for attempt in 0..3 {
        let response = build()
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP transport failed (details redacted)"))?;
        let status = response.status();
        if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) && attempt < 2 {
            let header_delay = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.parse::<f64>().ok());
            let body_delay = if status == StatusCode::TOO_MANY_REQUESTS {
                response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v["retry_after"].as_f64())
            } else {
                None
            };
            let delay = header_delay
                .or(body_delay)
                .unwrap_or(2_f64.powi(attempt + 1));
            if !delay.is_finite() || !(0.0..=20.0).contains(&delay) {
                bail!("HTTP rate limit requires a later run");
            }
            tokio::time::sleep(Duration::from_secs_f64(delay.max(0.1))).await;
            continue;
        }
        return Ok(response);
    }
    unreachable!()
}

pub fn success(response: Response, service: &str) -> Result<Response> {
    if !response.status().is_success() {
        bail!(
            "{service}: HTTP {} (body redacted)",
            response.status().as_u16()
        );
    }
    Ok(response)
}
