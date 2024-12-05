use anyhow::Result;
use hickory_client::{
    client::{AsyncClient, ClientConnection, ClientHandle},
    h2::HttpsClientConnection,
    op::DnsResponse,
    rr::Name,
};
use hickory_proto::{
    iocompat::AsyncIoTokioAsStd,
    rr::{DNSClass, RecordType},
};
use rustls::ClientConfig;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::watch;

#[derive(Clone)]
pub struct RetryableClient {
    dns_name: String,
    addr: SocketAddr,
    client: watch::Receiver<Option<AsyncClient>>,
    client_sender: watch::Sender<Option<AsyncClient>>,
    client_config: Arc<ClientConfig>,
}

impl RetryableClient {
    pub async fn new(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<Self> {
        let client = Self::create_client(addr, dns_name, client_config.clone()).await?;
        let (tx, rx) = watch::channel(Some(client));

        Ok(Self {
            dns_name: dns_name.to_string(),
            addr,
            client: rx,
            client_sender: tx,
            client_config,
        })
    }

    async fn create_client(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<AsyncClient> {
        tracing::debug!(target: concat!(module_path!(), "::stdout"), "Creating HTTPS connection to {}", dns_name);
        let conn: HttpsClientConnection<AsyncIoTokioAsStd<TokioTcpStream>> =
            HttpsClientConnection::new(addr, dns_name.to_string(), client_config);
        tracing::debug!(target: concat!(module_path!(), "::stdout"), "Creating DNS stream: {}", dns_name);
        let stream = conn.new_stream(None);
        tracing::debug!(target: concat!(module_path!(), "::stdout"), "Connecting AsyncClient: {}", dns_name);
        let (client, bg) = AsyncClient::connect(stream).await?;
        tokio::spawn(bg);
        Ok(client)
    }

    pub async fn query(
        &self,
        name: Name,
        query_class: DNSClass,
        query_type: RecordType,
    ) -> Result<DnsResponse> {
        const MAX_RETRIES: u32 = 3;
        const INITIAL_RETRY_DELAY: u64 = 100; // 初始延迟 100ms
        const MAX_RETRY_DELAY: u64 = 500; // 最大延迟 500ms
        let mut retries = 0;
        let mut receiver = self.client.clone();

        tracing::debug!(
            "Client has changed: {}, <{}>",
            receiver.has_changed().unwrap_or(false),
            self.dns_name
        );

        loop {
            let client = {
                let borrowed = receiver.borrow_and_update();
                borrowed.clone()
            };

            if let Some(mut client) = client {
                match client.query(name.clone(), query_class, query_type).await {
                    Ok(response) => {
                        if retries > 0 {
                            tracing::debug!(
                                "Query success after {} retries, <{}>",
                                retries,
                                self.dns_name
                            );
                        }
                        return Ok(response);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Query failed: {:?}, attempting reconnect, <{}>",
                            e,
                            self.dns_name
                        );
                        self.client_sender.send(None)?;
                    }
                }
            }

            if retries >= MAX_RETRIES {
                return Err(anyhow::anyhow!("Max retries exceeded"));
            }

            match Self::create_client(self.addr, &self.dns_name, self.client_config.clone()).await {
                Ok(new_client) => {
                    self.client_sender.send(Some(new_client))?;
                    retries += 1;
                }
                Err(e) => {
                    tracing::error!("Failed to reconnect: {:?}, <{}>", e, self.dns_name);
                    let delay = INITIAL_RETRY_DELAY
                        .saturating_mul(2_u64.saturating_pow(retries))
                        .min(MAX_RETRY_DELAY);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    retries += 1;
                }
            }
        }
    }
}
