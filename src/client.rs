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
    client: watch::Receiver<ClientHolder>,
    client_sender: watch::Sender<ClientHolder>,
    client_config: Arc<ClientConfig>,
    reconnect_tx: tokio::sync::mpsc::Sender<()>,
}

#[derive(Clone)]
pub struct ClientHolder {
    client: Option<AsyncClient>,
    version: u64,
}

impl RetryableClient {
    pub async fn new(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<Self> {
        let client = Self::create_client(addr, dns_name, client_config.clone()).await?;
        let client_holder = ClientHolder {
            client: Some(client),
            version: 0,
        };
        let (tx, rx) = watch::channel(client_holder);
        let (reconnect_tx, mut reconnect_rx) = tokio::sync::mpsc::channel(1);

        let reconnect_client = Self {
            dns_name: dns_name.to_string(),
            addr,
            client: rx.clone(),
            client_sender: tx.clone(),
            client_config: client_config.clone(),
            reconnect_tx: reconnect_tx.clone(),
        };

        tokio::spawn(async move {
            while reconnect_rx.recv().await.is_some() {
                reconnect_client.handle_reconnect().await;
            }
        });

        Ok(Self {
            dns_name: dns_name.to_string(),
            addr,
            client: rx,
            client_sender: tx,
            client_config,
            reconnect_tx,
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
        const INITIAL_RETRY_DELAY: u64 = 100;
        const MAX_RETRY_DELAY: u64 = 600;
        let mut retries = 0;
        let mut receiver = self.client.clone();

        loop {
            let client_holder = {
                let borrowed = receiver.borrow_and_update();
                borrowed.clone()
            };

            if let Some(mut client) = client_holder.client {
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
                            "Query failed for <{}>: {:?}, attempting reconnect, <{}>",
                            name,
                            e,
                            self.dns_name
                        );
                        self.client_sender.send_if_modified(|inner| {
                            if inner.version == client_holder.version {
                                inner.client = None;
                                inner.version += 1;
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
            }

            if retries >= MAX_RETRIES {
                return Err(anyhow::anyhow!("Max retries exceeded"));
            }

            match self.reconnect_tx.send(()).await {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(
                        "Failed to send reconnect signal: {:?}, <{}>",
                        e,
                        self.dns_name
                    );
                }
            }

            let delay = INITIAL_RETRY_DELAY
                .saturating_mul(2_u64.saturating_pow(retries))
                .min(MAX_RETRY_DELAY);
            tokio::time::sleep(Duration::from_millis(delay)).await;
            retries += 1;
        }
    }

    async fn handle_reconnect(&self) {
        let mut receiver = self.client.clone();
        let client_holder = {
            let borrowed = receiver.borrow_and_update();
            borrowed.clone()
        };
        if client_holder.client.is_some() {
            return;
        }

        const INITIAL_RETRY_DELAY: u64 = 300;
        const MAX_RETRY_DELAY: u64 = 5000;
        const MAX_RETRIES: u32 = 5;
        let mut retry_count = 0;
        let mut retry_delay = INITIAL_RETRY_DELAY;

        loop {
            match Self::create_client(self.addr, &self.dns_name, self.client_config.clone()).await {
                Ok(new_client) => {
                    self.client_sender.send_if_modified(|inner| {
                        inner.client = Some(new_client);
                        inner.version += 1;
                        true
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!("Failed to reconnect: {:?}, <{}>", e, self.dns_name);
                    if is_network_unreachable_error(&e) {
                        retry_delay = MAX_RETRY_DELAY;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(retry_delay)).await;
            retry_delay = retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
            retry_count += 1;
            if retry_count >= MAX_RETRIES {
                tracing::error!("Max retries exceeded, <{}>", self.dns_name);
                return;
            }
        }
    }
}

fn is_network_unreachable_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().map_or(false, |e| {
        if e.raw_os_error() == Some(51) {
            // 51 = ENETUNREACH on Unix
            return true;
        }
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return true;
        }
        false
    })
}
