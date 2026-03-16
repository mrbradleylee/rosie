use crate::provider::{ChatRequest, ChatResponse, Message, Provider, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

pub struct OllamaProvider {
    endpoint: String,
    model: Option<String>,
}

impl OllamaProvider {
    pub fn new(endpoint: String, model: Option<String>) -> Self {
        Self { endpoint, model }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[derive(Serialize)]
struct OllamaChatCompletionRequest {
    model: String,
    messages: Vec<OllamaMessage>,
}

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: OllamaMessage,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
}

#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModel>,
}

impl Provider for OllamaProvider {
    fn provider_type(&self) -> &'static str {
        "ollama"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let url = format!(
                "{}/v1/chat/completions",
                self.endpoint.trim_end_matches('/')
            );
            let request_body = OllamaChatCompletionRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OllamaMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
            };

            let resp = reqwest::Client::new()
                .post(url)
                .json(&request_body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error: {}", e))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "API returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let completion: CompletionResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("JSON parse error: {}", e))?;

            completion
                .choices
                .into_iter()
                .next()
                .map(|choice| ChatResponse {
                    message: Message {
                        role: match choice.message.role.as_str() {
                            "system" => Role::System,
                            "assistant" => Role::Assistant,
                            "tool" => Role::Tool,
                            _ => Role::User,
                        },
                        content: choice.message.content.trim().to_string(),
                    },
                })
                .ok_or_else(|| anyhow!("No choices returned"))
        })
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        Box::pin(async move { discover_ollama_models(self.endpoint()).await })
    }
}

pub async fn discover_ollama_models(endpoint: &str) -> Result<Vec<String>> {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error for model discovery: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Model discovery returned {}: {}",
            resp.status(),
            resp.text().await?
        ));
    }

    let response: OllamaTagsResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse model discovery JSON: {}", e))?;

    Ok(response
        .models
        .into_iter()
        .map(|model| model.name)
        .collect())
}
