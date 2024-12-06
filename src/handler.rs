use anyhow::Result;
use futures::future::{select_all, BoxFuture};
use hickory_client::{op::DnsResponse, rr::Name};
use hickory_proto::op::Message;
use hickory_server::{
    authority::MessageResponseBuilder,
    proto::op::{Header, MessageType, OpCode, ResponseCode},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
};
use rustls::{ClientConfig, OwnedTrustAnchor, RootCertStore};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{client::RetryableClient, config::Config};

pub struct RaceHandler {
    dns_clients: Vec<(RetryableClient, String)>,
}

impl RaceHandler {
    pub async fn new(config: &Config) -> Result<Self> {
        let mut dns_clients = Vec::new();
        let client_config = Arc::new(create_client_config());

        // 从配置文件初始化 DoH DNS客户端
        let providers = config.get_providers()?;
        for (addr, hostname, name) in providers {
            let client = RetryableClient::new(addr, &hostname, client_config.clone()).await?;
            dns_clients.push((client, name));
        }

        Ok(Self { dns_clients })
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
        let mut futures: Vec<BoxFuture<'_, Result<(DnsResponse, Duration, &String), _>>> =
            Vec::new();
        let mut names: Vec<&str> = Vec::new();

        for (client, name) in &self.dns_clients {
            let start = Instant::now();
            let client = client.clone();
            let name_clone = Name::from(query.name());
            let query_type = query.query_type();
            let query_class = query.query_class();

            let future = Box::pin(async move {
                match client.query(name_clone, query_class, query_type).await {
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
                                has_sent_response = false;
                            } else {
                                tracing::info!(
                                    "✔ {}: {:?} | {}",
                                    name,
                                    elapsed,
                                    format_answers(message.query(), message.answers())
                                );
                                final_response_code = response_code;
                                has_sent_response = true;
                            }
                        } else {
                            tracing::info!(
                                "◼︎ {}: {}{:?} | {}",
                                name,
                                format_response_code(response_code),
                                elapsed,
                                format_answers(message.query(), message.answers())
                            );
                        }
                    } else {
                        tracing::info!(
                            "◼︎ {}: {}{:?} | {}",
                            name,
                            format_response_code(response_code),
                            elapsed,
                            format_answers(message.query(), message.answers())
                        );
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

            let (response_code, message, name, _) = selected_response;
            tracing::info!(
                "● Fallback response {}from {}",
                format_response_code(*response_code),
                name
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
            tracing::error!("✘ All DNS queries failed");
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

fn format_answers(
    query: Option<&hickory_proto::op::Query>,
    answers: &[hickory_proto::rr::Record],
) -> String {
    let query_info = query.map_or(String::from("?"), |q| {
        format!("{}({})", q.name(), q.query_type())
    });

    if answers.is_empty() {
        return format!("{} → (no answers)", query_info);
    }

    let answers_str = answers
        .iter()
        .filter_map(|record| record.data().map(|data| data.to_string()))
        .collect::<Vec<_>>()
        .join(" → ");

    format!("{} → {}", query_info, answers_str)
}

fn format_response_code(code: ResponseCode) -> String {
    if code == ResponseCode::NoError {
        String::new()
    } else {
        format!("({}) ", code.to_str())
    }
}
