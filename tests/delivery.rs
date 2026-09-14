use chrono::{Duration, Utc};
use serde_json::json;
use sirucord::{
    config::Config,
    discord::{self, Announcement, Discord},
    engine::{App, Entry, Pending, State},
    http,
    mastodon::Mastodon,
    store::{Backend, Store},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

async fn app(server: &MockServer, dir: &tempfile::TempDir) -> App {
    let config = Config::parse(
        r#"
        [mastodon]
        base_url="https://mastodon.social"
        [[streamers]]
        guild_id="1"
        user_id="2"
        display_name="Streamer"
        voice_channel_ids=["3","4"]
        default_title="Test stream"
    "#,
    )
    .unwrap();
    let client = http::client().unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/accounts/verify_credentials"))
        .and(header("Authorization", "Bearer mastodon-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"me"})))
        .mount(server)
        .await;
    App {
        config,
        discord: Discord {
            client: client.clone(),
            base: server.uri(),
            token: "discord-secret".into(),
        },
        mastodon: Mastodon {
            client,
            base: server.uri(),
            token: "mastodon-secret".into(),
            visibility: "unlisted".into(),
        },
        store: Store::new(Backend::Local(dir.path().join("state.enc")), "a test key"),
    }
}

async fn live(server: &MockServer, channel: &str) {
    Mock::given(method("GET"))
        .and(path("/guilds/1/voice-states/2"))
        .and(header("Authorization", "Bot discord-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"channel_id":channel,"session_id":"voice-session","self_stream":true}),
        ))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn start_restart_and_channel_move_do_not_duplicate() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    live(&server, "3").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"posted"})))
        .mount(&server)
        .await;
    app.run(false).await.unwrap();
    live(&server, "3").await;
    app.run(false).await.unwrap();
    let mut restarted = App {
        store: Store::new(Backend::Local(dir.path().join("state.enc")), "a test key"),
        ..app
    };
    live(&server, "4").await;
    restarted.run(false).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let posts: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 1);
    assert!(posts[0].headers.contains_key("idempotency-key"));
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["visibility"], "unlisted");
    assert!(body["status"].as_str().unwrap().contains("Test stream"));
    assert!(
        !body["status"]
            .as_str()
            .unwrap()
            .contains("https://discord.com/channels/")
    );
}

#[tokio::test]
async fn observed_end_then_restart_creates_a_new_announcement() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    live(&server, "3").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"post"})))
        .mount(&server)
        .await;
    app.run(false).await.unwrap();
    Mock::given(method("GET"))
        .and(path("/guilds/1/voice-states/2"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code":10065})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    app.run(false).await.unwrap();
    assert!(app.store.load().await.unwrap().entries.is_empty());
    live(&server, "3").await;
    app.run(false).await.unwrap();
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        2
    );
}

#[tokio::test]
async fn permission_errors_do_not_clear_live_state() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    live(&server, "3").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"post"})))
        .mount(&server)
        .await;
    app.run(false).await.unwrap();
    for (status, code) in [(403, 50001), (404, 10004), (401, 0)] {
        Mock::given(method("GET"))
            .and(path("/guilds/1/voice-states/2"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({"code":code})))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert!(app.run(false).await.is_err());
        assert!(app.store.load().await.unwrap().entries["1:2"].announced);
    }
}

#[tokio::test]
async fn dry_run_neither_posts_nor_saves_state() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    live(&server, "3").await;
    app.run(true).await.unwrap();
    assert!(!dir.path().join("state.enc").exists());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn failed_delivery_reuses_durable_idempotency_key() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    live(&server, "3").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(422))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    assert!(app.run(false).await.is_err());
    let state = app.store.load().await.unwrap();
    let key = state.entries["1:2"].pending.as_ref().unwrap().key.clone();
    assert!(!state.entries["1:2"].announced);
    live(&server, "3").await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"post"})))
        .mount(&server)
        .await;
    app.run(false).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let posts: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 2);
    for post in posts {
        assert_eq!(post.headers["idempotency-key"].to_str().unwrap(), key);
    }
    assert!(
        app.store.load().await.unwrap().entries["1:2"]
            .pending
            .is_none()
    );
}

#[tokio::test]
async fn uncertain_old_delivery_stops_instead_of_reposting() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    let mut state = State::default();
    state.entries.insert(
        "1:2".into(),
        Entry {
            metadata_fingerprint: None,
            session_id: "session".into(),
            first_seen: Utc::now(),
            announced: false,
            last_post: None,
            last_attachment: None,
            pending: Some(Pending {
                metadata_fingerprint: None,
                key: "key".into(),
                text: "announcement".into(),
                attachment: None,
                media_id: None,
                attempted_at: Some(Utc::now() - Duration::hours(2)),
                is_start: true,
            }),
        },
    );
    assert!(
        app.deliver(&mut state, "1:2", false)
            .await
            .unwrap_err()
            .to_string()
            .contains("older than 55 minutes")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(state.entries["1:2"].pending.is_some());
}

#[test]
fn captions_and_images_require_streamer_authorship_explicit_prefix_and_freshness() {
    let now = Utc::now();
    let make = |id: &str, user: &str, content: &str, age: i64| -> Announcement {
        serde_json::from_value(json!({"id":id,"author":{"id":user},"content":content,"timestamp":now-Duration::minutes(age),"attachments":[{"id":id,"url":"https://cdn.discordapp.com/attachments/1/2/p.png","content_type":"image/png","size":100}]})).unwrap()
    };
    let messages = vec![
        make("12", "2", "!sirucord New @person", 1),
        make("11", "2", "!sirucord Old", 2),
        make("13", "9", "!sirucord Someone else", 0),
        make("14", "2", "private conversation", 0),
        make("15", "2", "!sirucord Stale", 800),
    ];
    assert_eq!(discord::title(&messages, "2", now).unwrap(), "New ＠person");
    assert_eq!(
        discord::screenshot(&messages, "2", now - Duration::minutes(30), now, None)
            .unwrap()
            .id,
        "12"
    );
    assert!(
        discord::screenshot(&messages, "2", now - Duration::minutes(30), now, Some("12")).is_none()
    );
    assert!(discord::screenshot(&messages, "2", now, now, None).is_none());
}

#[tokio::test]
async fn github_state_read_failure_is_not_treated_as_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let mut store = Store::new(
        Backend::Github {
            client: http::client().unwrap(),
            base: server.uri(),
            repository: "owner/repo".into(),
            token: "token".into(),
            sha: None,
        },
        "key",
    );
    assert!(store.load().await.is_err());
}

#[tokio::test]
async fn failed_state_write_prevents_publication() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    app.store = Store::new(Backend::Local(dir.path().join("missing/state.enc")), "key");
    live(&server, "3").await;
    assert!(app.run(false).await.is_err());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn github_state_roundtrip_preserves_encryption_and_uses_revision_sha() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start().await;
    let saved: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let read = saved.clone();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/contents/state.enc"))
        .respond_with(move |_: &wiremock::Request| match &*read.lock().unwrap() {
            Some((content, sha)) => {
                ResponseTemplate::new(200).set_body_json(json!({"content":content,"sha":sha}))
            }
            None => ResponseTemplate::new(404),
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/git/ref/heads/sirucord-state"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"default_branch":"main"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/git/ref/heads/main"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"object":{"sha":"main-sha"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/git/refs"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let write = saved.clone();
    Mock::given(method("PUT"))
        .and(path("/repos/owner/repo/contents/state.enc"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let mut saved = write.lock().unwrap();
            if let Some((_, previous)) = &*saved {
                assert_eq!(body["sha"], *previous);
            } else {
                assert!(body.get("sha").is_none());
            }
            let next = if saved.is_none() { "sha-1" } else { "sha-2" };
            *saved = Some((body["content"].as_str().unwrap().into(), next.into()));
            ResponseTemplate::new(200).set_body_json(json!({"content":{"sha":next}}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let make = || {
        Store::new(
            Backend::Github {
                client: http::client().unwrap(),
                base: server.uri(),
                repository: "owner/repo".into(),
                token: "token".into(),
                sha: None,
            },
            "state-key",
        )
    };
    let mut store = make();
    let state = store.load().await.unwrap();
    store.save(&state).await.unwrap();
    let mut restarted = make();
    let state = restarted.load().await.unwrap();
    assert_eq!(state.version, 1);
    restarted.save(&state).await.unwrap();
    use base64::Engine;
    let saved = saved.lock().unwrap();
    let envelope = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&saved.as_ref().unwrap().0)
            .unwrap(),
    )
    .unwrap();
    assert!(envelope.starts_with("sirucord-state-v1:"));
    assert!(!envelope.contains("entries"));
}

#[test]
fn metadata_changes_are_throttled_unchanged_information_never_reposts() {
    use sirucord::engine::metadata_due;
    let now = Utc::now();
    let mut entry = Entry {
        metadata_fingerprint: Some("old".into()),
        session_id: "session".into(),
        first_seen: now - Duration::hours(2),
        announced: true,
        last_post: Some(now - Duration::minutes(29)),
        last_attachment: None,
        pending: None,
    };
    assert!(!metadata_due(&entry, "new", now, 30));
    assert!(metadata_due(&entry, "new", now + Duration::minutes(1), 30));
    assert!(!metadata_due(&entry, "old", now + Duration::days(10), 30));
    entry.metadata_fingerprint = None; // migrate one time from v0.1.0
    assert!(metadata_due(&entry, "new", now, 30));
}

#[tokio::test]
async fn metadata_delivery_persists_fingerprint_and_does_not_read_chat() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(&server, &dir).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/statuses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"metadata-post"})))
        .expect(1)
        .mount(&server)
        .await;
    let mut state:State=serde_json::from_value(json!({"version":1,"entries":{"1:2":{"session_id":"old-session","first_seen":Utc::now(),"announced":true,"last_post":null,"last_attachment":null,"pending":null}}})).unwrap();
    // Old encrypted state has neither of the new fields.
    assert!(state.entries["1:2"].metadata_fingerprint.is_none());
    state.entries.get_mut("1:2").unwrap().pending = Some(Pending {
        key: "metadata-key".into(),
        text: "automatic game details\nhttps://discord.com/channels/1/3".into(),
        attachment: None,
        media_id: None,
        attempted_at: None,
        is_start: false,
        metadata_fingerprint: Some("new-details".into()),
    });
    app.store.save(&state).await.unwrap();
    app.deliver(&mut state, "1:2", false).await.unwrap();
    let reloaded = app.store.load().await.unwrap();
    assert_eq!(
        reloaded.entries["1:2"].metadata_fingerprint.as_deref(),
        Some("new-details")
    );
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["status"], "automatic game details");
    assert_eq!(requests[0].headers["idempotency-key"], "metadata-key");
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "POST")
    );
}
