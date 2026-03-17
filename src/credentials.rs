use crate::config::{ProviderConfig, StoredConfig};
use anyhow::{Result, anyhow};
use std::borrow::Cow;
use std::env;
use std::fmt;
use std::sync::Arc;

const KEYRING_SERVICE: &str = "rosie";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialTarget {
    Anthropic,
    NamedProvider(String),
}

impl fmt::Display for CredentialTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic => write!(f, "anthropic"),
            Self::NamedProvider(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    pub secret: String,
}

#[derive(Clone, Debug)]
pub struct CredentialStatus {
    pub target: CredentialTarget,
    pub env_var: Option<String>,
    pub has_env: bool,
    pub has_keychain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthKind {
    Native,
    ApiKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAuthStatus {
    pub cli_available: bool,
    pub logged_in: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAuthStatus {
    pub provider_name: String,
    pub auth_kind: AuthKind,
    pub env_var: Option<String>,
    pub has_env: bool,
    pub has_keychain: bool,
    pub cli_available: bool,
    pub logged_in: bool,
    pub detail: Option<String>,
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
            }));
        }

        if let Some(env_var) = env_var_name(target)
            && let Some(secret) = env::var_os(&env_var)
        {
            let secret = secret.to_string_lossy().trim().to_string();
            if !secret.is_empty() {
                return Ok(Some(ResolvedCredential { secret }));
            }
        }

        if let Some(secret) = self.store.get_secret(target)? {
            return Ok(Some(ResolvedCredential { secret }));
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
        let mut targets = vec![CredentialTarget::Anthropic];
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
                .and_then(env::var_os)
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

    pub fn list_provider_auth_statuses<F>(
        &self,
        config: Option<&StoredConfig>,
        native_status_for_provider: F,
    ) -> Result<Vec<ProviderAuthStatus>>
    where
        F: Fn(&str) -> Option<NativeAuthStatus>,
    {
        let mut statuses = Vec::new();

        if let Some(status) = native_status_for_provider("openai") {
            statuses.push(ProviderAuthStatus {
                provider_name: "openai".to_string(),
                auth_kind: AuthKind::Native,
                env_var: None,
                has_env: false,
                has_keychain: false,
                cli_available: status.cli_available,
                logged_in: status.logged_in,
                detail: Some(status.detail),
            });
        }

        for status in self.list_statuses(config)? {
            statuses.push(ProviderAuthStatus {
                provider_name: status.target.to_string(),
                auth_kind: AuthKind::ApiKey,
                env_var: status.env_var,
                has_env: status.has_env,
                has_keychain: status.has_keychain,
                cli_available: false,
                logged_in: false,
                detail: None,
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
        ProviderConfig::OpenAi { .. } => Ok(None),
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
    if provider_name == "openai" {
        return Err(anyhow!(
            "Provider 'openai' uses native login. Run `rosie auth login openai`."
        ));
    }
    if provider_name == "anthropic" {
        return Ok(CredentialTarget::Anthropic);
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
        CredentialTarget::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
        CredentialTarget::NamedProvider(name) => {
            Some(format!("ROSIE_{}_API_KEY", normalize_env_name(name)))
        }
    }
}

fn account_name(target: &CredentialTarget) -> Cow<'_, str> {
    match target {
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
        AuthKind, CredentialManager, CredentialTarget, NativeAuthStatus, ProviderAuthStatus,
        SecretStore, credential_target_from_name, env_var_name,
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
            .set(&CredentialTarget::Anthropic, "test-secret")
            .expect("set secret");

        let resolved = manager
            .resolve(&CredentialTarget::Anthropic, None)
            .expect("resolve")
            .expect("credential");
        assert_eq!(resolved.secret, "test-secret");
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

    #[test]
    fn openai_requires_native_login() {
        let err = credential_target_from_name(None, "openai").expect_err("openai should fail");
        assert!(err.to_string().contains("Run `rosie auth login openai`"));
    }

    #[test]
    fn list_provider_auth_statuses_includes_native_and_api_key_entries() {
        let manager = CredentialManager::with_store(Arc::new(MemoryStore::new()));
        let mut providers = BTreeMap::new();
        providers.insert(
            "local".to_string(),
            ProviderConfig::OpenAiCompatible {
                endpoint: "https://api.openai.com/v1".to_string(),
                model: Some("gpt-5".to_string()),
                allow_insecure_http: false,
            },
        );
        let config = StoredConfig {
            active_provider: Some("local".to_string()),
            providers,
            theme: None,
            execution_enabled: Some(true),
        };

        let statuses = manager
            .list_provider_auth_statuses(Some(&config), |provider_name| {
                (provider_name == "openai").then(|| NativeAuthStatus {
                    cli_available: true,
                    logged_in: true,
                    detail: "Logged in using ChatGPT".to_string(),
                })
            })
            .expect("list statuses");

        assert_eq!(
            statuses[0],
            ProviderAuthStatus {
                provider_name: "openai".to_string(),
                auth_kind: AuthKind::Native,
                env_var: None,
                has_env: false,
                has_keychain: false,
                cli_available: true,
                logged_in: true,
                detail: Some("Logged in using ChatGPT".to_string()),
            }
        );
        assert_eq!(statuses[1].provider_name, "anthropic");
        assert_eq!(statuses[1].auth_kind, AuthKind::ApiKey);
        assert_eq!(statuses[2].provider_name, "local");
        assert_eq!(statuses[2].env_var, Some("ROSIE_LOCAL_API_KEY".to_string()));
    }
}
