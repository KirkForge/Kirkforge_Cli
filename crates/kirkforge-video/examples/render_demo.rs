use kirkforge_video::demos;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "showcase".into());
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/demo.mp4".into());
    demos::render(&name, out.into()).await?;
    Ok(())
}
