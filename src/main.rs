#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use clap::Parser;
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;

mod client;
mod config;
mod handler;
mod logger;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// DNS server listening port
    #[arg(short, long, default_value_t = 5653)]
    port: u16,

    #[arg(long, help = "Log filepath")]
    log: Option<String>,

    /// Configuration file path
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let _guard = logger::init_logger("race_dns_proxy=info,info", args.log);

    // Load configuration file
    let config = config::Config::load(&args.config)?;

    let handler = handler::RaceHandler::new(&config).await?;
    let mut server = ServerFuture::new(handler);

    // Listen on UDP port
    let addr = format!("0.0.0.0:{}", args.port);
    let socket = UdpSocket::bind(&addr).await?;
    tracing::info!("DNS proxy server listening on {}", addr);

    server.register_socket(socket);
    server.block_until_done().await?;

    Ok(())
}
