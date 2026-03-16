use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, ProviderEvent, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use tokio::sync::mpsc;

pub struct OpenAiCompatibleProvider {
    provider_name: String,
    endpoint: String,
    model: Option<String>,
    allow_insecure_http: bool,
    credentials: CredentialManager,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        provider_name: String,
        endpoint: String,
        model: Option<String>,
        allow_insecure_http: bool,
        credentials: CredentialManager,
    ) -> Self {
        Self {
            provider_name,
            endpoint,
            model,
            allow_insecure_http,
            credentials,
        }
    }

    fn credential_target(&self) -> CredentialTarget {
        CredentialTarget::NamedProvider(self.provider_name.clone())
    }
}

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiCompatibleMessage>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Serialize)]
struct OpenAiCompatibleMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: OpenAiCompatibleResponseMessage,
}

#[derive(Deserialize)]
struct OpenAiCompatibleResponseMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct ModelListResponse {
    data: Vec<ModelItem>,
}

#[derive(Deserialize)]
struct ModelItem {
    id: String,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

#[derive(Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

impl Provider for OpenAiCompatibleProvider {
    fn provider_type(&self) -> &'static str {
        "openai-compatible"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let auth = resolve_auth(&self.credentials, &self.credential_target())?;
            validate_compatible_endpoint(&self.endpoint, self.allow_insecure_http, auth.is_some())?;

            let request_body = ChatCompletionsRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OpenAiCompatibleMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
                stream: false,
            };

            let client = reqwest::Client::new();
            let request = client.post(format!(
                "{}/chat/completions",
                self.endpoint.trim_end_matches('/')
            ));
            let request = if let Some(secret) = auth {
                request.bearer_auth(secret)
            } else {
                request
            };
            let resp = request
                .json(&request_body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "API returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let completion: ChatCompletionsResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("JSON parse error: {e}"))?;

            completion
                .choices
                .into_iter()
                .next()
                .map(|choice| ChatResponse {
                    message: Message {
                        role: role_from_str(&choice.message.role),
                        content: choice.message.content.trim().to_string(),
                    },
                })
                .ok_or_else(|| anyhow!("No choices returned"))
        })
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        Box::pin(async move {
            let auth = resolve_auth(&self.credentials, &self.credential_target())?;
            validate_compatible_endpoint(&self.endpoint, self.allow_insecure_http, auth.is_some())?;

            let client = reqwest::Client::new();
            let request = client.get(format!("{}/models", self.endpoint.trim_end_matches('/')));
            let request = if let Some(secret) = auth {
                request.bearer_auth(secret)
            } else {
                request
            };
            let resp = request
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error for model discovery: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "Model discovery returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let response: ModelListResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse model discovery JSON: {e}"))?;
            Ok(response.data.into_iter().map(|item| item.id).collect())
        })
    }

    fn stream_chat(
        &self,
        request: ChatRequest,
        tx: mpsc::UnboundedSender<ProviderEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let auth = resolve_auth(&self.credentials, &self.credential_target())?;
            validate_compatible_endpoint(&self.endpoint, self.allow_insecure_http, auth.is_some())?;

            let request_body = ChatCompletionsRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OpenAiCompatibleMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
                stream: true,
            };

            let client = reqwest::Client::new();
            let request = client.post(format!(
                "{}/chat/completions",
                self.endpoint.trim_end_matches('/')
            ));
            let request = if let Some(secret) = auth {
                request.bearer_auth(secret)
            } else {
                request
            };
            let mut resp = request
                .json(&request_body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "API returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let mut buffer = String::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| anyhow!("Stream read error: {e}"))?
            {
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();
                    if line.is_empty() {
                        continue;
                    }
                    parse_stream_line(&line, &tx)?;
                }
            }

            if !buffer.trim().is_empty() {
                parse_stream_line(buffer.trim(), &tx)?;
            }

            let _ = tx.send(ProviderEvent::Done);
            Ok(())
        })
    }
}

pub fn validate_compatible_endpoint(
    endpoint: &str,
    allow_insecure_http: bool,
    has_auth: bool,
) -> Result<()> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| anyhow!("Invalid endpoint '{endpoint}'"))?;
    match url.scheme() {
        "https" => return Ok(()),
        "http" => {}
        scheme => return Err(anyhow!("Unsupported endpoint scheme '{scheme}'")),
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Endpoint '{endpoint}' is missing a host"))?;
    let trusted_local = is_trusted_insecure_host(host);
    if trusted_local {
        return Ok(());
    }
    if allow_insecure_http {
        return Ok(());
    }
    if has_auth {
        return Err(anyhow!(
            "Insecure HTTP is disabled for non-local provider '{}'. Use HTTPS or set `allow_insecure_http = true`.",
            host
        ));
    }
    Err(anyhow!(
        "Refusing insecure non-local endpoint '{}'. Use HTTPS or set `allow_insecure_http = true`.",
        host
    ))
}

fn resolve_auth(
    credentials: &CredentialManager,
    target: &CredentialTarget,
) -> Result<Option<String>> {
    Ok(credentials
        .resolve(target, None)?
        .map(|resolved| resolved.secret))
}

fn is_trusted_insecure_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>()
        .map(|ip| match ip {
            IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 192 && v4.octets()[1] == 168),
            IpAddr::V6(v6) => v6.is_loopback(),
        })
        .unwrap_or(false)
}

fn role_from_str(value: &str) -> Role {
    match value {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn parse_stream_line(line: &str, tx: &mpsc::UnboundedSender<ProviderEvent>) -> Result<()> {
    let Some(payload) = sse_payload(line) else {
        return Ok(());
    };
    if payload == "[DONE]" {
        return Ok(());
    }

    let parsed: StreamResponse =
        serde_json::from_str(payload).map_err(|e| anyhow!("Failed to parse stream JSON: {e}"))?;
    for choice in parsed.choices {
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            let _ = tx.send(ProviderEvent::Token(content));
        }
    }
    Ok(())
}

fn sse_payload(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("event:") {
        return None;
    }

    let payload = trimmed.strip_prefix("data:").unwrap_or(trimmed).trim();
    (!payload.is_empty()).then_some(payload)
}

#[cfg(test)]
mod tests {
    use super::{OpenAiCompatibleProvider, parse_stream_line, validate_compatible_endpoint};
    use crate::credentials::{CredentialManager, CredentialTarget, SecretStore};
    use crate::provider::{ChatRequest, Message, Provider, ProviderEvent, Role};
    use anyhow::Result;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

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
    fn allows_local_network_http_by_default() {
        validate_compatible_endpoint("http://192.168.1.15:8080/v1", false, false)
            .expect("192.168 should be allowed");
    }

    #[test]
    fn rejects_remote_http_without_opt_in() {
        let err = validate_compatible_endpoint("http://10.0.0.9:8080/v1", false, false)
            .expect_err("remote insecure http should fail");
        assert!(
            err.to_string()
                .contains("Refusing insecure non-local endpoint")
        );
    }

    #[test]
    fn parser_ignores_non_data_sse_lines() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        parse_stream_line(": keep-alive", &tx).expect("comment ignored");
        parse_stream_line("event: message", &tx).expect("event ignored");
        parse_stream_line("data:", &tx).expect("empty ignored");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn provider_streams_compatible_sse_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).await.expect("read");
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"omni\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"coder\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        });

        let store = Arc::new(MemoryStore::new());
        store
            .set_secret(
                &CredentialTarget::NamedProvider("local".to_string()),
                "test-key",
            )
            .expect("set secret");
        let provider = OpenAiCompatibleProvider::new(
            "local".to_string(),
            format!("http://127.0.0.1:{}/v1", addr.port()),
            Some("test-model".to_string()),
            false,
            CredentialManager::with_store(store),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        provider
            .stream_chat(
                ChatRequest {
                    model: "test-model".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: "hello".to_string(),
                    }],
                    temperature: None,
                },
                tx,
            )
            .await
            .expect("stream chat");

        let mut chunks = Vec::new();
        while let Ok(event) = rx.try_recv() {
            chunks.push(event);
        }
        assert_eq!(
            chunks,
            vec![
                ProviderEvent::Token("omni".to_string()),
                ProviderEvent::Token("coder".to_string()),
                ProviderEvent::Done,
            ]
        );

        server.await.expect("server task");
    }
}
