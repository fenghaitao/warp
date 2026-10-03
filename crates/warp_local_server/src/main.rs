use std::net::SocketAddr;

use clap::Parser;
use warp_local_server::{Config, router};

#[derive(Parser)]
#[command(about = "Local Warp agent protocol server for DeepSeek / OpenAI-compatible providers")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(
        long,
        env = "WARP_LOCAL_PROVIDER_URL",
        default_value = "https://api.deepseek.com/v1"
    )]
    base_url: String,
    #[arg(long, env = "WARP_LOCAL_MODEL")]
    model: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !args.bind.ip().is_loopback() {
        return Err("Bind address must be loopback".into());
    }
    let config = Config {
        base_url: args.base_url,
        model: args.model,
        api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
    };
    let app = router(config)?;
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    println!("Warp local agent server listening on http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
