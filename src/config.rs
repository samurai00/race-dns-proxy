use anyhow::Result;
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, str::FromStr};

pub type DomainRules = (Vec<String>, Vec<String>);
pub type ProviderInfo = (SocketAddr, String, String, DomainRules);

#[derive(Debug, Deserialize)]
pub struct Config {
    pub providers: HashMap<String, Provider>,
    #[serde(default)]
    pub domain_groups: HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub addr: String,
    pub hostname: String,
    #[serde(default)]
    pub domain_groups: Vec<String>,
}

impl Config {
    pub fn load(filepath: &str) -> Result<Self> {
        let config_str = std::fs::read_to_string(filepath)?;
        let config: Config = toml::from_str(&config_str)?;
        Ok(config)
    }

    pub fn get_providers(&self) -> Result<Vec<ProviderInfo>> {
        let mut providers = Vec::new();
        for (key, provider) in &self.providers {
            let addr = SocketAddr::from_str(&provider.addr)?;

            let mut includes = Vec::new();
            let mut excludes = Vec::new();

            for group_name in &provider.domain_groups {
                if let Some(group_domains) = self.domain_groups.get(group_name) {
                    for domain in group_domains {
                        if let Some(stripped_domain) = domain.strip_prefix('!') {
                            excludes.push(stripped_domain.to_string());
                        } else {
                            includes.push(domain.clone());
                        }
                    }
                }
            }

            if provider.domain_groups.iter().any(|g| {
                self.domain_groups
                    .get(g)
                    .map_or(false, |domains| domains.is_empty())
            }) {
                includes.clear();
                excludes.clear();
            }

            providers.push((
                addr,
                provider.hostname.clone(),
                key.clone(),
                (includes, excludes),
            ));
        }
        Ok(providers)
    }
}
