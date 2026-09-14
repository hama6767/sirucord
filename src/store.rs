use crate::{engine::State, http};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, OsRng, rand_core::RngCore},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use hkdf::Hkdf;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sha2::Sha256;
use std::path::PathBuf;

pub enum Backend {
    Local(PathBuf),
    Github {
        client: Client,
        base: String,
        repository: String,
        token: String,
        sha: Option<String>,
    },
}

pub struct Store {
    pub backend: Backend,
    key: [u8; 32],
}

impl Store {
    pub fn new(backend: Backend, secret: &str) -> Self {
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(b"sirucord-state-v1"), secret.as_bytes())
            .expand(b"AES-256-GCM encryption", &mut key)
            .expect("fixed key length");
        Self { backend, key }
    }

    pub async fn load(&mut self) -> Result<State> {
        let encoded = match &mut self.backend {
            Backend::Local(path) => match std::fs::read_to_string(path) {
                Ok(v) => Some(v),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            Backend::Github {
                client,
                base,
                repository,
                token,
                sha,
            } => {
                let response = http::send(|| {
                    client
                        .get(format!(
                            "{base}/repos/{repository}/contents/state.enc?ref=sirucord-state"
                        ))
                        .bearer_auth(&*token)
                        .header("Accept", "application/vnd.github+json")
                })
                .await?;
                if response.status() == StatusCode::NOT_FOUND {
                    // Distinguish first-run from lost state: an existing state branch
                    // without its file is corruption, not permission to repost.
                    let branch = http::send(|| {
                        client
                            .get(format!(
                                "{base}/repos/{repository}/git/ref/heads/sirucord-state"
                            ))
                            .bearer_auth(&*token)
                    })
                    .await?;
                    ensure!(
                        branch.status() == StatusCode::NOT_FOUND,
                        "State branch exists but state.enc is missing; restore it before running"
                    );
                    None
                } else {
                    let data: Value = http::success(response, "GitHub state read")?.json().await?;
                    *sha = Some(data["sha"].as_str().context("Missing state SHA")?.into());
                    let content = data["content"]
                        .as_str()
                        .context("Missing state content")?
                        .replace(['\n', '\r'], "");
                    Some(String::from_utf8(STANDARD.decode(content)?)?)
                }
            }
        };
        match encoded {
            Some(data) => self.decrypt(&data),
            None => Ok(State::default()),
        }
    }

    pub async fn save(&mut self, state: &State) -> Result<()> {
        let encoded = self.encrypt(state)?;
        match &mut self.backend {
            Backend::Local(path) => {
                let temp = path.with_extension("tmp");
                {
                    use std::io::Write;
                    let mut f = std::fs::File::create(&temp)?;
                    f.write_all(encoded.as_bytes())?;
                    f.sync_all()?;
                }
                // std::fs::rename replaces an existing file on Unix and Windows.
                std::fs::rename(temp, path)?;
            }
            Backend::Github {
                client,
                base,
                repository,
                token,
                sha,
            } => {
                if sha.is_none() {
                    let branch = http::send(|| {
                        client
                            .get(format!(
                                "{base}/repos/{repository}/git/ref/heads/sirucord-state"
                            ))
                            .bearer_auth(&*token)
                    })
                    .await?;
                    if branch.status() == StatusCode::NOT_FOUND {
                        let repo: Value = http::success(
                            http::send(|| {
                                client
                                    .get(format!("{base}/repos/{repository}"))
                                    .bearer_auth(&*token)
                            })
                            .await?,
                            "GitHub repository",
                        )?
                        .json()
                        .await?;
                        let default_branch = repo["default_branch"]
                            .as_str()
                            .context("Missing default branch")?;
                        let head: Value = http::success(
                            http::send(|| {
                                client
                                    .get(format!(
                                        "{base}/repos/{repository}/git/ref/heads/{default_branch}"
                                    ))
                                    .bearer_auth(&*token)
                            })
                            .await?,
                            "GitHub main ref",
                        )?
                        .json()
                        .await?;
                        http::success(http::send(|| client.post(format!("{base}/repos/{repository}/git/refs")).bearer_auth(&*token).json(&json!({"ref":"refs/heads/sirucord-state","sha":head["object"]["sha"]}))).await?, "GitHub state branch creation")?;
                    } else {
                        http::success(branch, "GitHub state branch")?;
                    }
                }
                let mut body = json!({"message":"Persist encrypted Sirucord delivery state [skip ci]","content":STANDARD.encode(encoded),"branch":"sirucord-state"});
                if let Some(sha) = sha.as_ref() {
                    body["sha"] = json!(sha);
                }
                // Do not retry a write with a stale SHA after an ambiguous response.
                let response = client
                    .put(format!("{base}/repos/{repository}/contents/state.enc"))
                    .bearer_auth(&*token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("GitHub state write transport failed"))?;
                let data: Value = http::success(
                    response,
                    "GitHub state write (check contents:write permission)",
                )?
                .json()
                .await?;
                *sha = Some(
                    data["content"]["sha"]
                        .as_str()
                        .context("Missing saved state SHA")?
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn encrypt(&self, state: &State) -> Result<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("key length");
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                serde_json::to_vec(state)?.as_ref(),
            )
            .map_err(|_| anyhow::anyhow!("State encryption failed"))?;
        let mut data = nonce.to_vec();
        data.extend(encrypted);
        Ok(format!("sirucord-state-v1:{}", STANDARD.encode(data)))
    }

    fn decrypt(&self, encoded: &str) -> Result<State> {
        let encoded = encoded
            .strip_prefix("sirucord-state-v1:")
            .context("Unknown state envelope version")?;
        let data = STANDARD.decode(encoded)?;
        ensure!(data.len() >= 28, "Truncated encrypted state");
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("key length");
        let decrypted = cipher.decrypt(Nonce::from_slice(&data[..12]), &data[12..]).map_err(|_| anyhow::anyhow!("Cannot decrypt state: wrong SIRUCORD_STATE_KEY or corrupted state. Do not delete state to retry."))?;
        let state: State = serde_json::from_slice(&decrypted)?;
        if state.version != 1 {
            bail!("Unsupported state version");
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encryption_hides_state_and_rejects_wrong_keys_and_tampering() {
        let store = Store::new(Backend::Local("unused".into()), "secret");
        let text = store.encrypt(&State::default()).unwrap();
        assert!(!text.contains("entries"));
        assert_eq!(store.decrypt(&text).unwrap().version, 1);
        assert!(
            Store::new(Backend::Local("unused".into()), "wrong")
                .decrypt(&text)
                .is_err()
        );
        assert!(store.decrypt(&(text + "X")).is_err());
    }
    #[tokio::test]
    async fn local_save_survives_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::new(Backend::Local(dir.path().join("state.enc")), "key");
        store.save(&State::default()).await.unwrap();
        store.save(&State::default()).await.unwrap();
        assert_eq!(store.load().await.unwrap().version, 1);
    }
}
