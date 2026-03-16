use crate::config::{ProviderConfig, StoredConfig};
use crate::providers::ollama::OllamaProvider;
use anyhow::{Result, anyhow};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[allow(dead_code)]
    pub temperature: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct ChatResponse {
    pub message: Message,
}

pub trait Provider: Send + Sync {
    fn provider_type(&self) -> &'static str;
    fn default_model(&self) -> Option<&str>;

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>>;

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        Box::pin(async move {
            Err(anyhow!(
                "Model discovery is not supported for {}",
                self.provider_type()
            ))
        })
    }
}

pub struct ProviderRouter {
    provider: Arc<dyn Provider>,
}

impl ProviderRouter {
    pub fn from_config(config: &StoredConfig) -> Result<Self> {
        let (_name, provider) = config.active_provider_entry()?;
        let provider: Arc<dyn Provider> = match provider {
            ProviderConfig::Ollama { endpoint, model } => {
                Arc::new(OllamaProvider::new(endpoint.clone(), model.clone()))
            }
            ProviderConfig::OpenAi { .. } => Arc::new(UnsupportedProvider::new("openai")),
            ProviderConfig::Anthropic { .. } => Arc::new(UnsupportedProvider::new("anthropic")),
            ProviderConfig::OpenAiCompatible { .. } => {
                Arc::new(UnsupportedProvider::new("openai-compatible"))
            }
        };

        Ok(Self { provider })
    }

    pub fn provider_type(&self) -> &'static str {
        self.provider.provider_type()
    }

    pub async fn resolve_model(&self, runtime_model: Option<&str>) -> Result<String> {
        if let Some(model) = runtime_model.filter(|value| !value.trim().is_empty()) {
            return Ok(model.to_string());
        }

        if let Some(model) = self
            .provider
            .default_model()
            .filter(|value| !value.is_empty())
        {
            return Ok(model.to_string());
        }

        let models = self.provider.list_models().await?;
        models
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No models available for {}", self.provider_type()))
    }

    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.provider.chat(request).await
    }
}

struct UnsupportedProvider {
    provider_type: &'static str,
}

impl UnsupportedProvider {
    fn new(provider_type: &'static str) -> Self {
        Self { provider_type }
    }
}

impl Provider for UnsupportedProvider {
    fn provider_type(&self) -> &'static str {
        self.provider_type
    }

    fn default_model(&self) -> Option<&str> {
        None
    }

    fn chat(
        &self,
        _request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            Err(anyhow!(
                "{} is not wired into Rosie yet in this first provider refactor pass",
                self.provider_type
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderRouter;
    use crate::config::{ProviderConfig, StoredConfig};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn resolve_model_prefers_runtime_override() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "ollama".to_string(),
            ProviderConfig::Ollama {
                endpoint: "http://localhost:11434".to_string(),
                model: Some("llama3.2".to_string()),
            },
        );

        let config = StoredConfig {
            active_provider: Some("ollama".to_string()),
            providers,
            theme: None,
            execution_enabled: None,
        };

        let router = ProviderRouter::from_config(&config).expect("build router");
        let model = router
            .resolve_model(Some("qwen2.5-coder"))
            .await
            .expect("resolve model");

        assert_eq!(model, "qwen2.5-coder");
    }
}
