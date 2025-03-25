use anyhow::Result;
use hickory_client::{
    client::{Client, ClientHandle},
    proto::{
        rr::{DNSClass, Name, RecordType},
        runtime::TokioRuntimeProvider,
    },
};
use hickory_proto::{h2::HttpsClientStreamBuilder, xfer::DnsResponse};
use rustls::ClientConfig;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::watch;

use crate::config::DomainRules;

#[derive(Clone)]
pub struct RetryableClient {
    dns_name: String,
    addr: SocketAddr,
    client: watch::Receiver<ClientHolder>,
    client_sender: watch::Sender<ClientHolder>,
    client_config: Arc<ClientConfig>,
    reconnect_tx: tokio::sync::mpsc::Sender<()>,
}

pub struct DnsClientEntry {
    pub client: RetryableClient,
    pub name: String,
    pub domain_rules: DomainRules,
}

#[derive(Clone)]
pub struct ClientHolder {
    client: Option<Client>,
    version: u64,
}

impl RetryableClient {
    pub async fn new(
        addr: SocketAddr,
        dns_name: &str,
        client_config: Arc<ClientConfig>,
    ) -> Result<Self> {
        let client_holder = ClientHolder {
            client: None,
            version: 0,
        };
        let (tx, rx) = watch::channel(client_holder);
        let (reconnect_tx, mut reconnect_rx) = tokio::sync::mpsc::channel(100);

        let reconnect_client = Self {
            dns_name: dns_name.to_string(),
            addr,
            client: rx.clone(),
            client_sender: tx.clone(),
            client_config: client_config.clone(),
            reconnect_tx: reconnect_tx.clone(),
        };

        tokio::spawn(async move {
            // initialize the connection
            reconnect_client.handle_reconnect().await;
            // wait for the reconnection signal
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
    ) -> Result<Client> {
        tracing::debug!(target: concat!(module_path!(), "::stdout"), "Creating HTTPS connection to {}", dns_name);

        let provider = TokioRuntimeProvider::new();
        let https_builder = HttpsClientStreamBuilder::with_client_config(client_config, provider);
        let connect = https_builder.build(addr, dns_name.to_string(), "/dns-query".to_string());
        tracing::debug!(target: concat!(module_path!(), "::stdout"), "Connecting AsyncClient: {}", dns_name);
        let (client, bg) = Client::connect(connect).await?;
        tokio::spawn(bg);
        Ok(client)
    }

    pub async fn query(
        &self,
        name: Name,
        query_class: DNSClass,
        query_type: RecordType,
    ) -> Result<DnsResponse> {
        const MAX_RETRIES: u32 = 6;
        const INITIAL_RETRY_DELAY: u64 = 200;
        const MAX_RETRY_DELAY: u64 = 600;
        let mut retries = 0;
        let mut receiver = self.client.clone();
        let mut reconnect_sent = false;

        loop {
            let client_holder = {
                let borrowed = receiver.borrow_and_update();
                borrowed.clone()
            };

            if let Some(mut client) = client_holder.client {
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    client.query(name.clone(), query_class, query_type),
                )
                .await
                {
                    Ok(result) => match result {
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
                        }
                    },
                    Err(_) => {
                        tracing::warn!("Query timeout for <{}>, <{}>", name, self.dns_name);
                    }
                }
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

            if retries >= MAX_RETRIES {
                return Err(anyhow::anyhow!("Max retries exceeded"));
            }

            if !reconnect_sent {
                match self.reconnect_tx.send(()).await {
                    Ok(_) => {
                        reconnect_sent = true;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to send reconnect signal: {:?}, <{}>",
                            e,
                            self.dns_name
                        );
                    }
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
        const MAX_RETRY_DELAY: u64 = 3000;
        const MAX_RETRIES: u32 = 5;
        let mut retry_count = 0;
        let mut retry_delay = INITIAL_RETRY_DELAY;

        loop {
            tracing::info!("Attempting to reconnect to <{}>", self.dns_name);
            match Self::create_client(self.addr, &self.dns_name, self.client_config.clone()).await {
                Ok(new_client) => {
                    self.client_sender.send_if_modified(|inner| {
                        tracing::info!("Established connection with <{}>", self.dns_name);
                        inner.client = Some(new_client);
                        inner.version += 1;
                        true
                    });
                    return;
                }
                Err(e) => {
                    tracing::error!(
                        "Unable to establish connection: {:?}, <{}>",
                        e,
                        self.dns_name
                    );
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
    e.downcast_ref::<std::io::Error>().is_some_and(|e| {
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
