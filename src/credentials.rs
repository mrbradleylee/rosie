use crate::config::{ProviderConfig, StoredConfig};
use anyhow::{Result, anyhow};
use std::borrow::Cow;
use std::env;
use std::fmt;
use std::sync::Arc;

const KEYRING_SERVICE: &str = "rosie";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialTarget {
    OpenAi,
    Anthropic,
    NamedProvider(String),
}

impl fmt::Display for CredentialTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAi => write!(f, "openai"),
            Self::Anthropic => write!(f, "anthropic"),
            Self::NamedProvider(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Env(String),
    Keychain,
}

#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    pub secret: String,
    pub source: CredentialSource,
}

#[derive(Clone, Debug)]
pub struct CredentialStatus {
    pub target: CredentialTarget,
    pub env_var: Option<String>,
    pub has_env: bool,
    pub has_keychain: bool,
}

pub trait SecretStore: Send + Sync {
    fn get_secret(&self, target: &CredentialTarget) -> Result<Option<String>>;
    fn set_secret(&self, target: &CredentialTarget, secret: &str) -> Result<()>;
    fn delete_secret(&self, target: &CredentialTarget) -> Result<()>;
}

pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn get_secret(&self, target: &CredentialTarget) -> Result<Option<String>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &account_name(target))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(anyhow!("Keychain read failed: {err}")),
        }
    }

    fn set_secret(&self, target: &CredentialTarget, secret: &str) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &account_name(target))?;
        entry
            .set_password(secret)
            .map_err(|err| anyhow!("Keychain write failed: {err}"))
    }

    fn delete_secret(&self, target: &CredentialTarget) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &account_name(target))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(anyhow!("Keychain delete failed: {err}")),
        }
    }
}

#[derive(Clone)]
pub struct CredentialManager {
    store: Arc<dyn SecretStore>,
}

impl CredentialManager {
    pub fn new() -> Self {
        Self {
            store: Arc::new(KeyringStore),
        }
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    pub fn resolve(
        &self,
        target: &CredentialTarget,
        cli_override: Option<&str>,
    ) -> Result<Option<ResolvedCredential>> {
        if let Some(secret) = cli_override.filter(|value| !value.trim().is_empty()) {
            return Ok(Some(ResolvedCredential {
                secret: secret.to_string(),
                source: CredentialSource::Env("cli-override".to_string()),
            }));
        }

        if let Some(env_var) = env_var_name(target) {
            if let Some(secret) = env::var_os(&env_var) {
                let secret = secret.to_string_lossy().trim().to_string();
                if !secret.is_empty() {
                    return Ok(Some(ResolvedCredential {
                        secret,
                        source: CredentialSource::Env(env_var),
                    }));
                }
            }
        }

        if let Some(secret) = self.store.get_secret(target)? {
            return Ok(Some(ResolvedCredential {
                secret,
                source: CredentialSource::Keychain,
            }));
        }

        Ok(None)
    }

    pub fn set(&self, target: &CredentialTarget, secret: &str) -> Result<()> {
        if secret.trim().is_empty() {
            return Err(anyhow!("Credential cannot be empty"));
        }
        self.store.set_secret(target, secret)
    }

    pub fn remove(&self, target: &CredentialTarget) -> Result<()> {
        self.store.delete_secret(target)
    }

    pub fn list_statuses(&self, config: Option<&StoredConfig>) -> Result<Vec<CredentialStatus>> {
        let mut targets = vec![CredentialTarget::OpenAi, CredentialTarget::Anthropic];
        if let Some(config) = config {
            for (name, provider) in &config.providers {
                if matches!(provider, ProviderConfig::OpenAiCompatible { .. }) {
                    let target = CredentialTarget::NamedProvider(name.clone());
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
        }

        let mut statuses = Vec::with_capacity(targets.len());
        for target in targets {
            let env_var = env_var_name(&target);
            let has_env = env_var
                .as_ref()
                .and_then(|name| env::var_os(name))
                .map(|value| !value.to_string_lossy().trim().is_empty())
                .unwrap_or(false);
            let has_keychain = self.store.get_secret(&target)?.is_some();
            statuses.push(CredentialStatus {
                target,
                env_var,
                has_env,
                has_keychain,
            });
        }

        Ok(statuses)
    }
}

pub fn credential_target_for_provider(
    provider_name: &str,
    provider: &ProviderConfig,
) -> Result<Option<CredentialTarget>> {
    match provider {
        ProviderConfig::Ollama { .. } => Ok(None),
        ProviderConfig::OpenAi { .. } => Ok(Some(CredentialTarget::OpenAi)),
        ProviderConfig::Anthropic { .. } => Ok(Some(CredentialTarget::Anthropic)),
        ProviderConfig::OpenAiCompatible { .. } => Ok(Some(CredentialTarget::NamedProvider(
            provider_name.to_string(),
        ))),
    }
}

pub fn credential_target_from_name(
    config: Option<&StoredConfig>,
    provider_name: &str,
) -> Result<CredentialTarget> {
    match provider_name {
        "openai" => return Ok(CredentialTarget::OpenAi),
        "anthropic" => return Ok(CredentialTarget::Anthropic),
        _ => {}
    }

    let config = config.ok_or_else(|| {
        anyhow!("Provider '{provider_name}' is unknown and no config was available to resolve it")
    })?;
    let provider = config
        .providers
        .get(provider_name)
        .ok_or_else(|| anyhow!("Provider '{provider_name}' is not defined in config"))?;
    credential_target_for_provider(provider_name, provider)?
        .ok_or_else(|| anyhow!("Provider '{provider_name}' does not use API-key credentials"))
}

pub fn env_var_name(target: &CredentialTarget) -> Option<String> {
    match target {
        CredentialTarget::OpenAi => Some("OPENAI_API_KEY".to_string()),
        CredentialTarget::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        CredentialTarget::NamedProvider(name) => {
            Some(format!("ROSIE_{}_API_KEY", normalize_env_name(name)))
        }
    }
}

fn account_name(target: &CredentialTarget) -> Cow<'_, str> {
    match target {
        CredentialTarget::OpenAi => Cow::Borrowed("openai"),
        CredentialTarget::Anthropic => Cow::Borrowed("anthropic"),
        CredentialTarget::NamedProvider(name) => Cow::Owned(format!("provider:{name}")),
    }
}

fn normalize_env_name(name: &str) -> String {
    let mut value = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            value.push(ch.to_ascii_uppercase());
        } else {
            value.push('_');
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialManager, CredentialSource, CredentialTarget, SecretStore,
        credential_target_from_name, env_var_name,
    };
    use crate::config::{ProviderConfig, StoredConfig};
    use anyhow::Result;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    struct MemoryStore {
        secrets: Mutex<BTreeMap<String, String>>,
    }

    impl MemoryStore {
        fn new() -> Self {
            Self {
                secrets: Mutex::new(BTreeMap::new()),
            }
        }
    }

    impl SecretStore for MemoryStore {
        fn get_secret(&self, target: &CredentialTarget) -> Result<Option<String>> {
            Ok(self
                .secrets
                .lock()
                .expect("lock")
                .get(&target.to_string())
                .cloned())
        }

        fn set_secret(&self, target: &CredentialTarget, secret: &str) -> Result<()> {
            self.secrets
                .lock()
                .expect("lock")
                .insert(target.to_string(), secret.to_string());
            Ok(())
        }

        fn delete_secret(&self, target: &CredentialTarget) -> Result<()> {
            self.secrets
                .lock()
                .expect("lock")
                .remove(&target.to_string());
            Ok(())
        }
    }

    #[test]
    fn named_provider_env_var_is_stable() {
        assert_eq!(
            env_var_name(&CredentialTarget::NamedProvider("local-llm".to_string())),
            Some("ROSIE_LOCAL_LLM_API_KEY".to_string())
        );
    }

    #[test]
    fn resolve_prefers_keychain_after_env_miss() {
        let manager = CredentialManager::with_store(Arc::new(MemoryStore::new()));
        manager
            .set(&CredentialTarget::OpenAi, "test-secret")
            .expect("set secret");

        let resolved = manager
            .resolve(&CredentialTarget::OpenAi, None)
            .expect("resolve")
            .expect("credential");
        assert_eq!(resolved.secret, "test-secret");
        assert_eq!(resolved.source, CredentialSource::Keychain);
    }

    #[test]
    fn resolve_provider_name_from_config() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "local".to_string(),
            ProviderConfig::OpenAiCompatible {
                endpoint: "http://192.168.1.2:8080/v1".to_string(),
                model: Some("omnicoder".to_string()),
                allow_insecure_http: false,
            },
        );
        let config = StoredConfig {
            active_provider: Some("local".to_string()),
            providers,
            theme: None,
            execution_enabled: Some(true),
        };

        let target = credential_target_from_name(Some(&config), "local").expect("target");
        assert_eq!(target, CredentialTarget::NamedProvider("local".to_string()));
    }
}
