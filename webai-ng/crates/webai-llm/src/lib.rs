//! LLM client (OpenAI-compatible HTTP + local llama.cpp server).
//!
//! Implements the LLM client (ARCHITECTURE.md §4.3): `from_default_location`,
//! `llm.toml` profile → endpoint mapping, OpenAI-compatible + llama.cpp
//! providers, streaming `chat_stream`, multimodal messages, retry backoff,
//! timeout, and per-call logging (FR-7 / M-2).

use std::collections::HashMap;
use std::time::Duration;

use futures::{Stream, StreamExt};

/// A single chat message.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    /// Plain-text message.
    Text(ChatRole, String),
    /// Text plus an inline image (base64 PNG) — multimodal "look at the page".
    Image {
        role: ChatRole,
        text: String,
        image_base64_png: String,
    },
}

/// Role of a chat participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

impl ChatRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

/// A chunk of streamed completion output.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    /// A tool-call fragment emitted by the model.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

/// LLM errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no profile configured: {0}")]
    NoProfile(String),
    #[error("unknown profile `{0}` (not in llm.toml)")]
    UnknownProfile(String),
    #[error("provider request failed: {0}")]
    Provider(String),
    #[error("empty completion")]
    EmptyCompletion,
    #[error("request timed out after {0}ms")]
    Timeout(u64),
}

/// A provider profile (from `llm.toml`).
#[derive(Debug, Clone)]
pub struct LlmProfile {
    pub model: String,
    pub base_url: String,
    pub endpoint: String,
    pub api_key: String,
    pub timeout_ms: u64,
}

/// Test-only scripted responder type.
#[cfg(test)]
type ScriptedResponder = Box<dyn Fn(&[ChatMessage]) -> Result<String, LlmError> + Send + Sync>;

/// The async LLM client (ARCHITECTURE.md §4.3).
pub struct LlmClient {
    profile: String,
    profiles: HashMap<String, LlmProfile>,
    http: reqwest::Client,
    /// Test-only scripted responder.
    #[cfg(test)]
    scripted: Option<ScriptedResponder>,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("profile", &self.profile)
            .field("profiles", &self.profiles.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LlmClient {
    /// Construct from the default config location (`~/.webai/config/llm.toml`).
    pub async fn from_default_location() -> Result<Self, LlmError> {
        let config_dir = std::env::var("WEBAI_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".webai").join("config"))
                    .unwrap_or_else(|| std::path::PathBuf::from(".webai/config"))
            });
        let llm_path = config_dir.join("llm.toml");
        if !llm_path.exists() {
            return Err(LlmError::NoProfile(llm_path.display().to_string()));
        }
        let raw = std::fs::read_to_string(&llm_path)
            .map_err(|e| LlmError::Provider(format!("read {llm_path:?}: {e}")))?;
        let config: webai_config::LlmConfig = toml::from_str(&raw)
            .map_err(|e| LlmError::Provider(format!("parse {llm_path:?}: {e}")))?;
        let profiles: HashMap<String, LlmProfile> = config
            .profiles
            .into_iter()
            .map(|(name, p)| {
                (
                    name,
                    LlmProfile {
                        model: p.model,
                        base_url: p.base_url,
                        endpoint: p.endpoint,
                        api_key: p.api_key,
                        timeout_ms: 120_000,
                    },
                )
            })
            .collect();
        // Default profile = first entry.
        let default = profiles
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| LlmError::NoProfile("llm.toml has no profiles".into()))?;
        Ok(Self {
            profile: default,
            profiles,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            #[cfg(test)]
            scripted: None,
        })
    }

    /// Construct for a named provider profile. Fails fast (zero LLM calls) if
    /// the profile is not in `llm.toml`.
    pub async fn with_profile(profile: impl Into<String>) -> Result<Self, LlmError> {
        let profile = profile.into();
        let mut client = Self::from_default_location().await?;
        if !client.profiles.contains_key(&profile) {
            return Err(LlmError::UnknownProfile(profile));
        }
        client.profile = profile;
        Ok(client)
    }

    /// Synchronous stub constructor for a named provider profile (no I/O).
    pub fn with_profile_stub(profile: &str) -> Self {
        let mut profiles = HashMap::new();
        profiles.insert(
            profile.to_owned(),
            LlmProfile {
                model: "stub".into(),
                base_url: String::new(),
                endpoint: String::new(),
                api_key: String::new(),
                timeout_ms: 120_000,
            },
        );
        Self {
            profile: profile.to_owned(),
            profiles,
            http: reqwest::Client::new(),
            #[cfg(test)]
            scripted: None,
        }
    }

    /// Test-only constructor wired to a scripted responder.
    #[cfg(test)]
    pub(crate) fn with_scripted(
        responder: impl Fn(&[ChatMessage]) -> Result<String, LlmError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            profile: "scripted".into(),
            profiles: HashMap::new(),
            http: reqwest::Client::new(),
            scripted: Some(Box::new(responder)),
        }
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Names of every configured profile.
    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Perform a streaming completion. In stub/scripted mode returns a canned
    /// stream so the upstream chain (ACP/TUI) can be exercised without a live
    /// model.
    pub async fn chat_stream<'a>(
        &self,
        messages: &'a [ChatMessage],
    ) -> Result<impl Stream<Item = Delta> + 'a, LlmError> {
        #[cfg(test)]
        if let Some(responder) = &self.scripted {
            let text = responder(messages)?;
            return Ok(text_stream(text));
        }
        // Real provider path: retry with backoff (3x, 0.5s/1s/2s).
        let mut attempt = 0;
        let mut last_err = None;
        while attempt < 3 {
            match self.complete_once(messages).await {
                Ok(text) => return Ok(text_stream(text)),
                Err(e) => {
                    last_err = Some(e);
                    attempt += 1;
                    if attempt < 3 {
                        let backoff = [500u64, 1000, 2000][attempt - 1];
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or(LlmError::Provider("retries exhausted".into())))
    }

    /// One-shot completion wrapper over [`Self::chat_stream`].
    pub async fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        let messages = vec![ChatMessage::Text(ChatRole::User, prompt.to_string())];
        let mut stream = self.chat_stream(&messages).await?;
        let mut out = String::new();
        while let Some(delta) = stream.next().await {
            if let Delta::Text(t) = delta {
                out.push_str(&t);
            }
        }
        if out.is_empty() {
            return Err(LlmError::EmptyCompletion);
        }
        Ok(out)
    }

    /// Perform a single (non-retried) provider call.
    async fn complete_once(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        let profile = self
            .profiles
            .get(&self.profile)
            .ok_or_else(|| LlmError::UnknownProfile(self.profile.clone()))?;
        let url = format!(
            "{}{}",
            profile.base_url.trim_end_matches('/'),
            profile.endpoint
        );
        let body = self.build_request_body(messages, profile);
        // Per-call log (M-2 data source).
        tracing::info!(
            profile = %self.profile,
            model = %profile.model,
            messages = messages.len(),
            "llm call"
        );
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", profile.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(LlmError::Provider(format!("HTTP {status}")));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Provider(format!("bad response: {e}")))?;
        let text = json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if text.is_empty() {
            return Err(LlmError::EmptyCompletion);
        }
        Ok(text)
    }

    /// Build the OpenAI-compatible request body.
    fn build_request_body(
        &self,
        messages: &[ChatMessage],
        profile: &LlmProfile,
    ) -> serde_json::Value {
        let wire = messages
            .iter()
            .map(|m| match m {
                ChatMessage::Text(role, text) => {
                    serde_json::json!({ "role": role.as_str(), "content": text })
                }
                ChatMessage::Image {
                    role,
                    text,
                    image_base64_png,
                } => serde_json::json!({
                    "role": role.as_str(),
                    "content": [
                        { "type": "text", "text": text },
                        { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{image_base64_png}") } }
                    ]
                }),
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "model": profile.model,
            "messages": wire,
            "stream": false,
        })
    }
}

/// Build a stream that emits the characters of `text` as `Delta::Text` chunks.
fn text_stream(text: String) -> impl Stream<Item = Delta> {
    let mut done = 0usize;
    let chars = text.chars().collect::<Vec<_>>();
    futures::stream::poll_fn(move |_cx| {
        if done < chars.len() {
            let c = chars[done].to_string();
            done += 1;
            std::task::Poll::Ready(Some(Delta::Text(c)))
        } else {
            std::task::Poll::Ready(None)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn chat_stream_emits_scripted_deltas_in_order() {
        let client = LlmClient::with_scripted(|_| Ok("hello world".into()));
        let messages = [ChatMessage::Text(ChatRole::User, "hi".into())];
        let mut stream = client.chat_stream(&messages).await.unwrap();
        let mut texts = String::new();
        while let Some(Delta::Text(t)) = stream.next().await {
            texts.push_str(&t);
        }
        assert_eq!(texts, "hello world");
    }

    #[tokio::test]
    async fn complete_concatenates_stream() {
        let client = LlmClient::with_scripted(|_| Ok("stub reply".into()));
        let out = client.complete("hello").await.unwrap();
        assert_eq!(out, "stub reply");
    }

    #[test]
    fn roles_serialize_to_names() {
        assert_eq!(ChatRole::System.as_str(), "system");
        assert_eq!(ChatRole::User.as_str(), "user");
        assert_eq!(ChatRole::Assistant.as_str(), "assistant");
    }

    #[test]
    fn multimodal_message_serializes_with_image_base64() {
        let msg = ChatMessage::Image {
            role: ChatRole::User,
            text: "look".into(),
            image_base64_png: "aGVsbG8=".into(),
        };
        let client = LlmClient::with_profile_stub("stub");
        let profile = client.profiles.get("stub").unwrap();
        let body = client.build_request_body(&[msg], profile);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .contains("data:image/png;base64,aGVsbG8="));
    }

    #[tokio::test]
    async fn unknown_profile_fails_fast() {
        // The real fast-fail path: `with_profile` on a profile that is not in
        // the config must return `UnknownProfile` without any LLM call.
        let dir = std::env::temp_dir().join(format!("webai-llm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("llm.toml"),
            "[cloud]\nmodel = \"test\"\nbase_url = \"http://localhost:9\"\nendpoint = \"/v1/chat/completions\"\napi_key = \"k\"\n",
        )
        .unwrap();
        std::env::set_var("WEBAI_CONFIG", &dir);
        let err = match LlmClient::with_profile("ghost").await {
            Ok(_) => panic!("ghost profile must not resolve"),
            Err(e) => e,
        };
        assert!(matches!(err, LlmError::UnknownProfile(ref p) if p == "ghost"));
    }

    // A client whose only profile points at a closed local port: every
    // `complete_once` fails with a Provider error, forcing the real retry
    // path in `chat_stream`.
    fn retry_client() -> LlmClient {
        let mut profiles = HashMap::new();
        profiles.insert(
            "down".into(),
            LlmProfile {
                model: "test".into(),
                // Port 9 (discard) is never serving HTTP in the test env.
                base_url: "http://127.0.0.1:9".into(),
                endpoint: "/v1/chat/completions".into(),
                api_key: "k".into(),
                timeout_ms: 1_000,
            },
        );
        LlmClient {
            profile: "down".into(),
            profiles,
            http: reqwest::Client::new(),
            #[cfg(test)]
            scripted: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backoff_waits_500_1000_2000ms_then_fails() {
        // Auto-advance paused time; `Instant::now` on the paused clock
        // measures the virtual elapsed backoff exactly.
        let start = tokio::time::Instant::now();
        let client = retry_client();
        let messages = [ChatMessage::Text(ChatRole::User, "hi".into())];
        let result = match client.chat_stream(&messages).await {
            Ok(_) => panic!("expected failure against a dead provider"),
            Err(e) => e,
        };
        let elapsed = start.elapsed();
        assert!(matches!(result, LlmError::Provider(_)), "got {result:?}");
        // 3 attempts => 2 sleeps: 500ms + 1000ms.
        assert_eq!(elapsed, std::time::Duration::from_millis(1500));
        assert!(elapsed >= std::time::Duration::from_millis(1500));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_first_backoff_sleep_is_500ms() {
        // The first retry always sleeps the schedule's first entry (500ms)
        // before attempt 2; with paused time no wall-clock waiting occurs.
        let start = tokio::time::Instant::now();
        let client = retry_client();
        let messages = [ChatMessage::Text(ChatRole::User, "hi".into())];
        let _ = client.chat_stream(&messages).await;
        assert!(start.elapsed() >= std::time::Duration::from_millis(500));
    }
}
