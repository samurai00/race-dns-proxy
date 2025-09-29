#[cfg(feature = "mimalloc")]
use mimalloc::MiMalloc;
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use clap::Parser;
use hickory_server::ServerFuture;
use std::time::Duration;
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
    /// DNS server listening host
    #[arg(short, long, default_value = "[::]")]
    host: String,

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
    let config = match config::Config::load(&args.config) {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("Failed to load configuration file: {}", err);
            return Err(err);
        }
    };

    let handler = match handler::RaceHandler::new(&config).await {
        Ok(handler) => handler,
        Err(err) => {
            tracing::error!("Failed to initialize race handler: {}", err);
            return Err(err);
        }
    };

    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    tracing::info!("Starting {} v{}", pkg_name, pkg_version);

    let mut server = ServerFuture::new(handler);

    // Listen on UDP port
    let addr = format!("{}:{}", args.host, args.port);
    let socket = match UdpSocket::bind(&addr).await {
        Ok(socket) => socket,
        Err(err) => {
            tracing::error!("Failed to bind UDP socket on {}: {}", addr, err);
            return Err(err.into());
        }
    };
    tracing::info!("DNS proxy server listening on {}/UDP", addr);
    server.register_socket(socket);

    // Listen on TCP port
    let addr = format!("{}:{}", args.host, args.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("Failed to bind TCP listener on {}: {}", addr, err);
            return Err(err.into());
        }
    };
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

    match server.shutdown_gracefully().await {
        Ok(_) => tracing::info!("Server shutdown completed"),
        Err(err) => {
            tracing::error!("Error during server shutdown: {}", err);
            return Err(err.into());
        }
    };

    Ok(())
}
