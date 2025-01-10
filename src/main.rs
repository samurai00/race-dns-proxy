use std::time::Duration;

#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use clap::Parser;
use hickory_server::ServerFuture;
use tokio::{
    net::{TcpListener, UdpSocket},
    signal,
};

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
    #[arg(short, long, default_value = "race-dns-proxy.toml")]
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
    tracing::info!("DNS proxy server listening on {}/UDP", addr);
    server.register_socket(socket);

    // Listen on TCP port
    let addr = format!("0.0.0.0:{}", args.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("DNS proxy server listening on {}/TCP", addr);
    server.register_listener(listener, Duration::from_secs(10));

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    server.shutdown_gracefully().await?;
    tracing::info!("Server shutdown completed");

    Ok(())
}
