//! Best-effort public link titles. No Discord/Mastodon credentials leave their clients.
use anyhow::{Result, ensure};
use reqwest::Url;
use scraper::{Html, Selector};
use std::{net::IpAddr, time::Duration};

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            // Only global-unicast space; exclude documentation, tunneling and special use.
            (0x2000..=0x3fff).contains(&a)
                && a != 0x2002
                && !(a == 0x2001 && (b < 0x0200 || b == 0x0db8))
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

fn valid_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.as_str().len() <= 4096
        && url.host_str().is_some_and(|host| {
            host.contains('.')
                && host.parse::<IpAddr>().is_err()
                && !host.ends_with(".local")
                && !host.ends_with(".localhost")
                && !host.ends_with(".internal")
        })
}

async fn document(mut url: Url) -> Result<String> {
    for _ in 0..3 {
        ensure!(valid_url(&url), "Unsupported public URL");
        let host = url.host_str().unwrap().to_owned();
        let addresses: Vec<_> = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host.as_str(), 443)),
        )
        .await??
        .collect();
        ensure!(
            !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
            "Non-public address"
        );
        // Pin exactly the validated DNS result to prevent DNS rebinding. Recheck every redirect.
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve_to_addrs(&host, &addresses)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(7))
            .user_agent("SirucordLinkPreview/0.6")
            .build()?;
        let mut response = client.get(url.clone()).send().await?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("Missing redirect"))?;
            url = url.join(location)?;
            continue;
        }
        ensure!(response.status().is_success(), "Link unavailable");
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        ensure!(
            content_type.contains("html") || content_type.contains("json"),
            "Not a document"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            let remaining = (256 * 1024_usize).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if bytes.len() == 256 * 1024 {
                break;
            }
        }
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    anyhow::bail!("Too many redirects")
}

pub fn clean_title(value: &str) -> Option<String> {
    let title = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = crate::discord::clean(&title, 120);
    (!title.is_empty()).then_some(title)
}

pub fn html_title(source: &str) -> Option<String> {
    let document = Html::parse_document(source);
    for query in [
        "meta[property='og:title']",
        "meta[name='twitter:title']",
        "title",
    ] {
        let selector = Selector::parse(query).ok()?;
        for element in document.select(&selector) {
            let value = if query == "title" {
                element.text().collect::<String>()
            } else {
                element.value().attr("content").unwrap_or("").to_owned()
            };
            if let Some(title) = clean_title(&value) {
                return Some(title);
            }
        }
    }
    None
}

pub async fn title(value: &str) -> Option<String> {
    // Errors are intentionally swallowed; failed metadata must not suppress the shared message.
    tokio::time::timeout(Duration::from_secs(10), async {
        let url = Url::parse(value).ok()?;
        if !valid_url(&url) {
            return None;
        }
        let oembed = match url.host_str()? {
            "youtube.com" | "www.youtube.com" | "m.youtube.com" | "youtu.be" => {
                Some("https://www.youtube.com/oembed")
            }
            "vimeo.com" | "www.vimeo.com" | "player.vimeo.com" => {
                Some("https://vimeo.com/api/oembed.json")
            }
            _ => None,
        };
        if let Some(endpoint) = oembed {
            let mut endpoint = Url::parse(endpoint).ok()?;
            endpoint
                .query_pairs_mut()
                .append_pair("url", url.as_str())
                .append_pair("format", "json");
            if let Ok(body) = document(endpoint).await
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(title) = data["title"].as_str().and_then(clean_title)
            {
                return Some(title);
            }
        }
        html_title(&document(url).await.ok()?)
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_private_and_special_addresses_including_ipv6_tunnels() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "198.18.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(!public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        for address in [
            "http://example.com",
            "https://secret@example.com",
            "https://example.com:444",
            "https://127.0.0.1",
            "https://metadata.internal",
        ] {
            assert!(!valid_url(&Url::parse(address).unwrap()));
        }
    }
    #[test]
    fn prefers_open_graph_title_and_decodes_entities() {
        assert_eq!(
            html_title(
                "<title>fallback</title><meta content='動画 &amp; ゲーム' property='og:title'>"
            )
            .as_deref(),
            Some("動画 & ゲーム")
        );
        assert_eq!(
            html_title("<title>  Example\n Page </title>").as_deref(),
            Some("Example Page")
        );
        assert_eq!(html_title("<body>no title</body>"), None);
    }
}
