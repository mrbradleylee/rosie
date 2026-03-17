use crate::config::{ProviderConfig, StoredConfig};
use crate::credentials::CredentialManager;
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::ollama::OllamaProvider;
use crate::providers::openai::OpenAiProvider;
use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use anyhow::{Result, anyhow};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEvent {
    Token(String),
    Done,
}

pub trait Provider: Send + Sync {
    fn provider_type(&self) -> &'static str;
    fn default_model(&self) -> Option<&str>;
    fn supports_model_discovery(&self) -> bool {
        false
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>>;

    fn stream_chat(
        &self,
        request: ChatRequest,
        tx: mpsc::UnboundedSender<ProviderEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let response = self.chat(request).await?;
            if !response.message.content.is_empty() {
                let _ = tx.send(ProviderEvent::Token(response.message.content));
            }
            let _ = tx.send(ProviderEvent::Done);
            Ok(())
        })
    }

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
        let (provider_name, provider) = config.active_provider_entry()?;
        let credentials = CredentialManager::new();
        let provider: Arc<dyn Provider> = match provider {
            ProviderConfig::Ollama { endpoint, model } => {
                Arc::new(OllamaProvider::new(endpoint.clone(), model.clone()))
            }
            ProviderConfig::OpenAi { model, .. } => {
                Arc::new(OpenAiProvider::new(model.clone(), credentials.clone()))
            }
            ProviderConfig::Anthropic { endpoint, model } => Arc::new(AnthropicProvider::new(
                endpoint.clone(),
                model.clone(),
                credentials.clone(),
            )),
            ProviderConfig::OpenAiCompatible {
                endpoint,
                model,
                allow_insecure_http,
            } => Arc::new(OpenAiCompatibleProvider::new(
                provider_name.to_string(),
                endpoint.clone(),
                model.clone(),
                *allow_insecure_http,
                credentials.clone(),
            )),
        };

        Ok(Self { provider })
    }

    pub fn provider_type(&self) -> &'static str {
        self.provider.provider_type()
    }

    pub fn supports_model_discovery(&self) -> bool {
        self.provider.supports_model_discovery()
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

    pub async fn list_models(&self) -> Result<Vec<String>> {
        self.provider.list_models().await
    }

    pub async fn stream_chat(
        &self,
        request: ChatRequest,
        tx: mpsc::UnboundedSender<ProviderEvent>,
    ) -> Result<()> {
        self.provider.stream_chat(request, tx).await
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
