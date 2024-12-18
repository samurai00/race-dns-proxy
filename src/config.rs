use anyhow::Result;
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, str::FromStr};

pub type ProviderInfo = (SocketAddr, String, String, Vec<String>);

#[derive(Debug, Deserialize)]
pub struct Config {
    pub providers: HashMap<String, Provider>,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub addr: String,
    pub hostname: String,
    #[serde(default)]
    pub domains: Vec<String>,
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
            providers.push((
                addr,
                provider.hostname.clone(),
                key.clone(),
                provider.domains.clone(),
            ));
        }
        Ok(providers)
    }
}
