//! Render a sample with fictional names and public Discord default avatars.
use sirucord::{
    avatars::{Avatar, collage},
    http,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Pass an output PNG path"))?;
    let people = [
        Avatar {
            name: "しるねこ".into(),
            url: "https://cdn.discordapp.com/embed/avatars/0.png".into(),
            streaming: true,
            game: Some("Minecraft".into()),
        },
        Avatar {
            name: "あおい".into(),
            url: "https://cdn.discordapp.com/embed/avatars/3.png".into(),
            streaming: true,
            game: Some("FINAL FANTASY XIV".into()),
        },
        Avatar {
            name: "まつり".into(),
            url: "https://cdn.discordapp.com/embed/avatars/4.png".into(),
            streaming: false,
            game: None,
        },
    ];
    let (png, _) = collage(&http::client()?, &people).await?;
    std::fs::write(path, png)?;
    Ok(())
}
