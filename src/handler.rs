use anyhow::Result;
use futures::future::{select_all, BoxFuture};
use hickory_client::{
    client::{AsyncClient, ClientConnection},
    h2::HttpsClientConnection,
    proto::error::ProtoError,
    rr::Name,
};
use hickory_proto::{iocompat::AsyncIoTokioAsStd, op::Message};
use hickory_server::{
    authority::MessageResponseBuilder,
    proto::op::{Header, MessageType, OpCode, ResponseCode},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
};
use rustls::{ClientConfig, OwnedTrustAnchor, RootCertStore};
use std::{
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpStream as TokioTcpStream;

use crate::client::RetryableClient;

// DNS服务器配置
const ALIDNS_DOH: (&str, &str, &str) = ("223.5.5.5:443", "dns.alidns.com", "AliDNS-DoH");
const DNSPOD_DOH: (&str, &str, &str) = ("1.12.12.12:443", "doh.pub", "DNSPod-DoH");

// const CLOUDFLARE_DOH: (&str, &str, &str) = (
//     "1.1.1.1:443", // 使用 Cloudflare 的主要 DNS IP
//     "1.1.1.1",     // 使用正确的域名
//     "Cloudflare-DoH",
// );
// const GOOGLE_DOH: (&str, &str, &str) = ("8.8.8.8:443", "dns.google", "Google-DoH");

pub struct RaceHandler {
    // dns_clients: Vec<(Arc<Mutex<RetryableClient>>, &'static str)>,
    dns_clients: Vec<(RetryableClient, &'static str)>,
}

impl RaceHandler {
    pub async fn new() -> Result<Self> {
        let mut dns_clients = Vec::new();
        let client_config = Arc::new(create_client_config());

        // 初始化 DoH DNS客户端
        for (addr, dns_name, name) in [ALIDNS_DOH, DNSPOD_DOH] {
            let addr = SocketAddr::from_str(addr)?;
            let client = RetryableClient::new(addr, dns_name, client_config.clone()).await?;
            // dns_clients.push((Arc::new(Mutex::new(client)), name));
            dns_clients.push((client, name));
        }

        Ok(Self { dns_clients })
    }

    #[allow(dead_code)]
    async fn create_client_doh(
        addr: SocketAddr,
        dns_name: &str,
    ) -> Result<
        (
            AsyncClient,
            impl std::future::Future<Output = Result<(), ProtoError>>,
        ),
        ProtoError,
    > {
        tracing::debug!("Creating DoH client for {} ({})", dns_name, addr);

        // 创建根证书存储
        let root_store = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS
                .iter()
                .map(|ta| {
                    OwnedTrustAnchor::from_subject_spki_name_constraints(
                        ta.subject.as_ref().to_vec(),
                        ta.subject_public_key_info.as_ref().to_vec(),
                        ta.name_constraints.as_ref().map(|nc| nc.as_ref().to_vec()),
                    )
                })
                .collect(),
        };

        tracing::debug!(
            "Created root store with {} certificates",
            root_store.roots.len()
        );

        // 构建 TLS 客户端配置
        let client_config = ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let client_config = Arc::new(client_config);
        tracing::debug!("Creating HTTPS connection to {}", dns_name);

        let conn: HttpsClientConnection<AsyncIoTokioAsStd<TokioTcpStream>> =
            HttpsClientConnection::new(addr, dns_name.to_string(), client_config);

        tracing::debug!("Creating DNS stream");
        let stream = conn.new_stream(None);

        tracing::debug!("Connecting AsyncClient");
        AsyncClient::connect(stream).await
    }
}

#[async_trait::async_trait]
impl RequestHandler for RaceHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        // 构建查询任务
        let query = request.query();
        let request_id = request.id();
        let mut futures: Vec<BoxFuture<'_, Result<(_, _, _), _>>> = Vec::new();
        let mut names: Vec<&str> = Vec::new();

        for (client, name) in &self.dns_clients {
            let start = Instant::now();
            let client = client.clone();
            let name = *name;
            let name_clone = Name::from(query.name());
            let query_type = query.query_type();
            let query_class = query.query_class();

            let future = Box::pin(async move {
                let mut clien_clone = client;
                match clien_clone.query(name_clone, query_class, query_type).await {
                    Ok(response) => Ok((response, start.elapsed(), name)),
                    Err(e) => Err(e),
                }
            });

            futures.push(future);
            names.push(name);
        }

        // 使用select_all获取所有响应
        let mut final_response_code = ResponseCode::ServFail; // 默认值
        let mut responses: Vec<(ResponseCode, Message, &str, Duration)> = Vec::new();
        let mut has_sent_response = false;
        let mut remaining = futures;

        while !remaining.is_empty() {
            match select_all(remaining).await {
                (Ok((response, elapsed, name)), _index, rest) => {
                    let response_code = response.header().response_code();
                    let mut message = response.into_message();
                    message.set_id(request_id);

                    // 记录所有响应
                    responses.push((response_code, message.clone(), name, elapsed));

                    if !has_sent_response {
                        // 如果是成功响应（非ServFail且非NXDomain），直接发送给客户端
                        if response_code != ResponseCode::ServFail
                            && response_code != ResponseCode::NXDomain
                        {
                            tracing::info!("✔ {} => {:?}", name, elapsed);

                            let builder = MessageResponseBuilder::from_message_request(request);
                            let response = builder.build(
                                *message.header(),
                                message.answers(),
                                message.name_servers(),
                                None,
                                message.additionals(),
                            );

                            if let Err(e) = response_handle.send_response(response).await {
                                tracing::error!("Failed to send successful DNS response: {}", e);
                                has_sent_response = false; // 发送失败则标记为未发送
                            } else {
                                final_response_code = response_code;
                                has_sent_response = true;
                            }
                        } else {
                            tracing::info!("{}: ({:?}) -> {:?}", name, response_code, elapsed);
                        }
                    } else {
                        tracing::info!("{}: ({:?}) -> {:?}", name, response_code, elapsed);
                    }
                    remaining = rest;
                }
                (Err(e), index, rest) => {
                    let name = names[index];
                    tracing::error!("Query failed: {:?}, <{}>", e, name);
                    remaining = rest;
                }
            }
        }

        // 如果还没有发送响应，从已收集的响应中选择一个发送
        if !has_sent_response && !responses.is_empty() {
            // 优先选择 NXDomain，其次选择 ServFail
            let selected_response = responses
                .iter()
                .find(|(code, ..)| *code == ResponseCode::NXDomain)
                .or_else(|| {
                    responses
                        .iter()
                        .find(|(code, ..)| *code == ResponseCode::ServFail)
                })
                .or_else(|| responses.first())
                .unwrap(); // 前面已经加了非空判断，所以这里必定至少有一个

            let (response_code, message, name, elapsed) = selected_response;
            tracing::info!(
                "Fallback response ({:?}) from {} in {:?}",
                response_code,
                name,
                elapsed
            );

            let builder = MessageResponseBuilder::from_message_request(request);
            let response = builder.build(
                *message.header(),
                message.answers(),
                message.name_servers(),
                None,
                message.additionals(),
            );

            if let Err(e) = response_handle.send_response(response).await {
                tracing::error!("Failed to send successful DNS response: {}", e);
                has_sent_response = false; // 发送失败则标记为未发送
            } else {
                final_response_code = *response_code;
                has_sent_response = true;
            }
        }

        // 返回适当的 ResponseInfo
        if has_sent_response {
            let mut header = Header::new();
            header.set_id(request_id);
            header.set_message_type(MessageType::Response);
            header.set_op_code(OpCode::Query);
            header.set_response_code(final_response_code);
            ResponseInfo::from(header)
        } else {
            tracing::error!("All DNS queries failed");
            let mut header = Header::new();
            header.set_id(request_id);
            header.set_message_type(MessageType::Response);
            header.set_op_code(OpCode::Query);
            header.set_response_code(ResponseCode::ServFail);

            let builder = MessageResponseBuilder::from_message_request(request);
            let response = builder.build(
                header,
                vec![], // empty answers
                vec![], // empty name servers
                None,   // empty zone
                vec![], // empty additionals
            );
            if let Err(e) = response_handle.send_response(response).await {
                tracing::error!("Failed to send ServFail DNS response: {}", e);
            }

            ResponseInfo::from(header)
        }
    }
}

fn create_client_config() -> ClientConfig {
    // 创建根证书存储
    let root_store = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS
            .iter()
            .map(|ta| {
                OwnedTrustAnchor::from_subject_spki_name_constraints(
                    ta.subject.as_ref().to_vec(),
                    ta.subject_public_key_info.as_ref().to_vec(),
                    ta.name_constraints.as_ref().map(|nc| nc.as_ref().to_vec()),
                )
            })
            .collect(),
    };

    tracing::debug!(
        "Created root store with {} certificates",
        root_store.roots.len()
    );

    // 构建 TLS 客户端配置
    ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth()
}
