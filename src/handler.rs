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

use crate::{
    client::{DnsClientEntry, RetryableClient},
    config::Config,
};

pub struct RaceHandler {
    dns_clients: Vec<DnsClientEntry>,
}

impl RaceHandler {
    pub async fn new(config: &Config) -> Result<Self> {
        let mut dns_clients = Vec::new();
        let client_config = Arc::new(create_client_config());

        let providers = config.get_providers()?;
        for (addr, hostname, name, domain_rules) in providers {
            let client = RetryableClient::new(addr, &hostname, client_config.clone()).await?;
            dns_clients.push(DnsClientEntry {
                client,
                name,
                domain_rules,
            });
        }

        Ok(Self { dns_clients })
    }

    fn matches_domain(query_name: &str, domain_rules: &(Vec<String>, Vec<String>)) -> bool {
        let (includes, excludes) = domain_rules;

        // If the include list is empty, it means process all domains
        if includes.is_empty() {
            return true;
        }

        let query_name = query_name.trim_end_matches('.');

        // First check if it's in the exclude list
        for exclude in excludes {
            if query_name.ends_with(exclude) {
                return false;
            }
        }

        // Then check if it's in the include list
        includes
            .iter()
            .any(|domain| is_domain_match(query_name, domain))
    }
}

#[async_trait::async_trait]
impl RequestHandler for RaceHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let query = request.query();
        let request_id = request.id();
        let query_name = query.name().to_string();

        let matching_clients: Vec<_> = self
            .dns_clients
            .iter()
            .filter(|dns_client_entry| {
                let matches = !dns_client_entry.domain_rules.0.is_empty()
                    && Self::matches_domain(&query_name, &dns_client_entry.domain_rules);
                tracing::debug!(
                    "Provider {} matches domain {}: {}",
                    dns_client_entry.name,
                    query_name,
                    matches
                );
                matches
            })
            .collect();

        tracing::debug!(
            "Found {} matching providers for domain {}",
            matching_clients.len(),
            query_name
        );

        let clients_to_use = if matching_clients.is_empty() {
            self.dns_clients
                .iter()
                .filter(|dns_client_entry| dns_client_entry.domain_rules.0.is_empty())
                .collect::<Vec<_>>()
        } else {
            tracing::info!("Using specific DNS provider for domain: {}", query_name);
            matching_clients
        };

        if clients_to_use.is_empty() {
            tracing::error!("No DNS provider available for domain: {}", query_name);
            return create_servfail_response(request_id);
        }

        let mut futures: Vec<BoxFuture<'_, Result<(DnsResponse, Duration, &String), _>>> =
            Vec::new();
        let mut names: Vec<&str> = Vec::new();

        for dns_client_entry in clients_to_use {
            let start = Instant::now();
            let client = dns_client_entry.client.clone();
            let name_clone = Name::from(query.name());
            let query_type = query.query_type();
            let query_class = query.query_class();

            let future = Box::pin(async move {
                match client.query(name_clone, query_class, query_type).await {
                    Ok(response) => Ok((response, start.elapsed(), &dns_client_entry.name)),
                    Err(e) => Err(e),
                }
            });

            futures.push(future);
            names.push(&dns_client_entry.name);
        }

        let mut final_response_code = ResponseCode::ServFail;
        let mut responses: Vec<(ResponseCode, Message, &str, Duration)> = Vec::new();
        let mut has_sent_response = false;
        let mut remaining = futures;

        while !remaining.is_empty() {
            match select_all(remaining).await {
                (Ok((response, elapsed, name)), _index, rest) => {
                    let response_code = response.header().response_code();
                    let mut message = response.into_message();
                    message.set_id(request_id);

                    responses.push((response_code, message.clone(), name, elapsed));

                    if !has_sent_response {
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

        if !has_sent_response && !responses.is_empty() {
            let selected_response = responses
                .iter()
                .find(|(code, ..)| *code == ResponseCode::NXDomain)
                .or_else(|| {
                    responses
                        .iter()
                        .find(|(code, ..)| *code == ResponseCode::ServFail)
                })
                .or_else(|| responses.first())
                .unwrap();

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
                has_sent_response = false;
            } else {
                final_response_code = *response_code;
                has_sent_response = true;
            }
        }

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

    // Group answers by record type
    let mut non_a_records = Vec::new();
    let mut a_records = Vec::new();

    for record in answers {
        if let Some(data) = record.data() {
            let data_str = data.to_string();
            // Check if it's an A record (contains only numbers and dots)
            if data_str.chars().all(|c| c.is_ascii_digit() || c == '.') {
                a_records.push(data_str);
            } else {
                non_a_records.push(data_str);
            }
        }
    }

    // Format non-A records with arrows, A records with commas
    let mut result = String::new();
    if !non_a_records.is_empty() {
        result.push_str(&non_a_records.join(" → "));
    }
    if !a_records.is_empty() {
        if !result.is_empty() {
            result.push_str(" → ");
        }
        result.push_str(&a_records.join(", "));
    }

    format!("{} → {}", query_info, result)
}

fn format_response_code(code: ResponseCode) -> String {
    if code == ResponseCode::NoError {
        String::new()
    } else {
        format!("({}) ", code.to_str())
    }
}

fn create_servfail_response(request_id: u16) -> ResponseInfo {
    let mut header = Header::new();
    header.set_id(request_id);
    header.set_message_type(MessageType::Response);
    header.set_op_code(OpCode::Query);
    header.set_response_code(ResponseCode::ServFail);
    ResponseInfo::from(header)
}

#[inline]
fn is_domain_match(query: &str, pattern: &str) -> bool {
    if query == pattern {
        return true;
    }

    if query.ends_with(pattern) {
        let prefix_len = query.len() - pattern.len();
        if prefix_len > 0 {
            return query.as_bytes()[prefix_len - 1] == b'.';
        }
    }

    false
}
