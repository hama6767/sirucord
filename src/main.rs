use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use sirucord::{
    config::Config,
    discord::Discord,
    engine::{App, complete},
    http,
    mastodon::Mastodon,
    store::{Backend, Store},
};
use std::{env, path::PathBuf};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, default_value = "config.toml", global = true)]
    config: PathBuf,
    #[arg(long, default_value = "state.enc", global = true)]
    state: PathBuf,
    #[arg(long, global = true)]
    github_state: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate configuration without network access or credentials.
    Check,
    /// Poll Discord once and publish new announcements.
    Run {
        #[arg(long)]
        dry_run: bool,
    },
    /// List unresolved deliveries locally (contains Discord IDs; do not put in public logs).
    Pending,
    /// Resolve a delivery after checking Mastodon. --retry can duplicate a post.
    Resolve {
        #[arg(long)]
        target: String,
        #[arg(long, conflicts_with = "retry", required_unless_present = "retry")]
        posted: bool,
        #[arg(long)]
        retry: bool,
    },
}

fn secret(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("Set {name} in the environment / GitHub Actions secrets"))?;
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::parse(&match env::var("SIRUCORD_CONFIG") {
        Ok(s) => s,
        Err(_) => std::fs::read_to_string(&cli.config)
            .context("Read config.toml (see config.example.toml)")?,
    })?;
    if matches!(cli.command, Command::Check) {
        println!("Configuration is valid.");
        return Ok(());
    }
    let client = http::client()?;
    let key = secret("SIRUCORD_STATE_KEY")?;
    ensure!(
        key.len() >= 32,
        "SIRUCORD_STATE_KEY must be at least 32 random characters"
    );
    let backend = if cli.github_state {
        let repository = secret("GITHUB_REPOSITORY")?;
        ensure!(
            repository.split('/').count() == 2
                && repository
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._/".contains(c)),
            "Invalid GITHUB_REPOSITORY"
        );
        Backend::Github {
            client: client.clone(),
            base: "https://api.github.com".into(),
            repository,
            token: secret("GITHUB_TOKEN")?,
            sha: None,
        }
    } else {
        Backend::Local(cli.state)
    };
    let mut store = Store::new(backend, &key);
    match cli.command {
        Command::Check => unreachable!(),
        Command::Pending => {
            for (key, entry) in store.load().await?.entries {
                if let Some(p) = entry.pending {
                    println!("{key}: attempted_at={:?}, key={}", p.attempted_at, p.key);
                }
            }
        }
        Command::Resolve {
            target,
            posted,
            retry,
        } => {
            let mut state = store.load().await?;
            ensure!(
                state
                    .entries
                    .get(&target)
                    .is_some_and(|e| e.pending.is_some()),
                "No pending delivery for target"
            );
            if posted {
                complete(&mut state, &target, chrono::Utc::now());
            } else if retry {
                let p = state
                    .entries
                    .get_mut(&target)
                    .unwrap()
                    .pending
                    .as_mut()
                    .unwrap();
                p.attempted_at = None;
                p.key = uuid::Uuid::new_v4().to_string();
            }
            store.save(&state).await?;
            println!("Delivery resolution saved.");
        }
        Command::Run { dry_run } => {
            let discord = Discord {
                client: client.clone(),
                base: "https://discord.com/api/v10".into(),
                token: secret("DISCORD_BOT_TOKEN")?,
            };
            let mastodon = Mastodon {
                client,
                base: config.mastodon.base_url.trim_end_matches('/').into(),
                token: secret("MASTODON_ACCESS_TOKEN")?,
                visibility: config.mastodon.visibility.clone(),
            };
            App {
                config,
                discord,
                mastodon,
                store,
            }
            .run(dry_run)
            .await?;
        }
    }
    Ok(())
}
