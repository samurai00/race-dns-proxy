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

#[derive(Clone)]
pub struct RetryableClient {
    dns_name: String,
    addr: SocketAddr,
    client: Option<AsyncClient>,
    client_config: Arc<ClientConfig>,
}

impl RetryableClient {
    pub async fn new(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<Self> {
        let client = Self::create_client(addr, dns_name, client_config.clone()).await?;
        Ok(Self {
            dns_name: dns_name.to_string(),
            addr,
            client: Some(client),
            client_config,
        })
    }

    async fn create_client(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<AsyncClient> {
        tracing::debug!("Creating HTTPS connection to {}", dns_name);
        let conn: HttpsClientConnection<AsyncIoTokioAsStd<TokioTcpStream>> =
            HttpsClientConnection::new(addr, dns_name.to_string(), client_config);
        tracing::debug!("Creating DNS stream: {}", dns_name);
        let stream = conn.new_stream(None);
        tracing::debug!("Connecting AsyncClient: {}", dns_name);
        let (client, bg) = AsyncClient::connect(stream).await?;
        tokio::spawn(bg);
        Ok(client)
    }

    pub async fn query(
        &mut self,
        name: Name,
        query_class: DNSClass,
        query_type: RecordType,
    ) -> Result<DnsResponse> {
        const MAX_RETRIES: u32 = 3;
        let mut retries = 0;

        loop {
            if let Some(client) = &mut self.client {
                match client.query(name.clone(), query_class, query_type).await {
                    Ok(response) => {
                        if retries > 0 {
                            tracing::debug!("Query success after {} retries", retries);
                        }
                        return Ok(response);
                    }
                    Err(e) => {
                        tracing::warn!("Query failed: {:?}, attempting reconnect", e);
                        self.client = None;
                    }
                }
            }

            if retries >= MAX_RETRIES {
                return Err(anyhow::anyhow!("Max retries exceeded"));
            }

            match Self::create_client(self.addr, &self.dns_name, self.client_config.clone()).await {
                Ok(new_client) => {
                    self.client = Some(new_client);
                    retries += 1;
                }
                Err(e) => {
                    tracing::error!("Failed to reconnect: {:?}", e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    retries += 1;
                }
            }
        }
    }
}
