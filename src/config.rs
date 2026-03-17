use crate::paths::config_path;
use crate::providers::anthropic::DEFAULT_ANTHROPIC_ENDPOINT;
use crate::providers::ollama::DEFAULT_OLLAMA_ENDPOINT;
use crate::providers::openai_compatible::validate_compatible_endpoint;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredConfig {
    pub active_provider: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    pub theme: Option<String>,
    pub execution_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ProviderConfig {
    Ollama {
        endpoint: String,
        #[serde(default)]
        model: Option<String>,
    },
    #[serde(rename = "openai")]
    OpenAi {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
    },
    Anthropic {
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible {
        endpoint: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        allow_insecure_http: bool,
    },
}

impl Default for StoredConfig {
    fn default() -> Self {
        let mut providers = BTreeMap::new();
        providers.insert(
            "ollama".to_string(),
            ProviderConfig::Ollama {
                endpoint: DEFAULT_OLLAMA_ENDPOINT.to_string(),
                model: None,
            },
        );
        Self {
            active_provider: Some("ollama".to_string()),
            providers,
            theme: None,
            execution_enabled: Some(true),
        }
    }
}

impl StoredConfig {
    pub fn active_provider_entry(&self) -> Result<(&str, &ProviderConfig)> {
        let active_provider = self
            .active_provider
            .as_deref()
            .ok_or_else(|| anyhow!("Config missing required `active_provider`"))?;
        let provider = self.providers.get(active_provider).ok_or_else(|| {
            anyhow!(
                "Active provider '{}' is missing from `[providers.*]`",
                active_provider
            )
        })?;
        Ok((active_provider, provider))
    }
}

pub fn load_config() -> Result<StoredConfig> {
    let path = config_path()?;
    match fs::read_to_string(path) {
        Ok(contents) => {
            let config: StoredConfig = toml::from_str(&contents)?;
            validate_config(&config)?;
            Ok(config)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(StoredConfig::default()),
        Err(err) => Err(err.into()),
    }
}

pub fn validate_config(config: &StoredConfig) -> Result<()> {
    if config.active_provider.is_none() || config.providers.is_empty() {
        return Err(anyhow!(
            "Config must define `active_provider` and at least one `[providers.<name>]` block"
        ));
    }

    let (_name, provider) = config.active_provider_entry()?;
    match provider {
        ProviderConfig::Ollama { endpoint, .. } => {
            validate_endpoint(endpoint)?;
        }
        ProviderConfig::OpenAi { endpoint, .. } => {
            if endpoint
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(anyhow!(
                    "OpenAI no longer accepts `endpoint`. Use `type = \"openai-compatible\"` with `endpoint = \"https://api.openai.com/v1\"` for API-key access."
                ));
            }
        }
        ProviderConfig::Anthropic { endpoint, .. } => {
            validate_https_endpoint(
                endpoint.as_deref().unwrap_or(DEFAULT_ANTHROPIC_ENDPOINT),
                "Anthropic",
            )?;
        }
        ProviderConfig::OpenAiCompatible {
            endpoint,
            allow_insecure_http,
            ..
        } => {
            validate_endpoint(endpoint)?;
            validate_compatible_endpoint(endpoint, *allow_insecure_http, false)?;
        }
    }

    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    reqwest::Url::parse(endpoint).map_err(|_| anyhow!("Invalid endpoint '{}'", endpoint))?;
    Ok(())
}

fn validate_https_endpoint(endpoint: &str, provider_name: &str) -> Result<()> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| anyhow!("Invalid endpoint '{}'", endpoint))?;
    if url.scheme() != "https" {
        return Err(anyhow!("{provider_name} endpoints must use HTTPS"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProviderConfig, StoredConfig, validate_config};
    use std::collections::BTreeMap;

    #[test]
    fn rejects_legacy_only_shape() {
        let config: StoredConfig = toml::from_str(
            r#"
            theme = "rose-pine"
            execution_enabled = true
            "#,
        )
        .expect("parse config");

        let err = validate_config(&config).expect_err("legacy config should fail");
        assert!(
            err.to_string()
                .contains("Config must define `active_provider`")
        );
    }

    #[test]
    fn validates_active_provider_membership() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "ollama".to_string(),
            ProviderConfig::Ollama {
                endpoint: "http://localhost:11434".to_string(),
                model: Some("llama3.2".to_string()),
            },
        );

        let config = StoredConfig {
            active_provider: Some("missing".to_string()),
            providers,
            theme: None,
            execution_enabled: Some(true),
        };

        let err = validate_config(&config).expect_err("missing provider should fail");
        assert!(
            err.to_string()
                .contains("Active provider 'missing' is missing")
        );
    }

    #[test]
    fn rejects_openai_endpoint_migration_path() {
        let config: StoredConfig = toml::from_str(
            r#"
            active_provider = "openai"

            [providers.openai]
            type = "openai"
            endpoint = "http://example.com/v1"
            model = "gpt-4.1"
            "#,
        )
        .expect("parse config");

        let err = validate_config(&config).expect_err("openai endpoint should fail");
        assert!(
            err.to_string()
                .contains("OpenAI no longer accepts `endpoint`")
        );
    }
}
