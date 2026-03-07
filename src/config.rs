use crate::paths::config_path;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StoredConfig {
    pub ollama_host: Option<String>,
    pub default_model: Option<String>,
    pub ask_model: Option<String>,
    pub cmd_model: Option<String>,
    pub theme: Option<String>,
    pub execution_enabled: Option<bool>,
    #[serde(default, alias = "model", skip_serializing)]
    pub legacy_model: Option<String>,
}

impl StoredConfig {
    pub fn effective_default_model(&self) -> Option<String> {
        self.default_model
            .clone()
            .or_else(|| self.legacy_model.clone())
    }
}

pub fn load_config() -> Result<StoredConfig> {
    let path = config_path()?;
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(StoredConfig::default()),
        Err(err) => Err(err.into()),
    }
}
