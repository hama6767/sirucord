//! Best-effort title check for a public URL, without account credentials.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Pass a public HTTPS URL"))?;
    match sirucord::link_titles::title(&url).await {
        Some(title) => println!("{title}"),
        None => anyhow::bail!("Title unavailable; normal sharing would retain the URL"),
    }
    Ok(())
}
