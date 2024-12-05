use anyhow::Result;
use clap::Parser;
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;

mod client;
mod handler;
mod logger;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// DNS服务器监听端口
    #[arg(short, long, default_value_t = 5653)]
    port: u16,

    #[arg(long, help = "Log filepath")]
    log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    let _guard = logger::init_logger("race_dns_proxy=info,info", args.log);

    let handler = handler::RaceHandler::new().await?;
    let mut server = ServerFuture::new(handler);

    // 监听UDP端口
    let addr = format!("0.0.0.0:{}", args.port);
    let socket = UdpSocket::bind(&addr).await?;
    tracing::info!("DNS proxy server listening on {}", addr);

    server.register_socket(socket);
    server.block_until_done().await?;

    Ok(())
}
