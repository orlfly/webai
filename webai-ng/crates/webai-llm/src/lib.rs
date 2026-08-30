//! LLM client (OpenAI-compatible HTTP + local llama.cpp server).
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the public
//! `LlmClient` interface (ARCHITECTURE.md §4.3): `from_default_location`,
//! `chat_stream` (OpenAI-compatible) and multimodal message support. A real HTTP
//! transport lands in a later milestone; for now the client exposes the API shape
//! and a fully-testable stub that returns canned deltas.

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
    ToolCall { id: String, name: String, arguments: String },
}

/// LLM errors.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no profile configured: {0}")]
    NoProfile(String),
    #[error("provider request failed: {0}")]
    Provider(String),
    #[error("empty completion")]
    EmptyCompletion,
}

/// The async LLM client (ARCHITECTURE.md §4.3).
pub struct LlmClient {
    profile: String,
}

impl LlmClient {
    /// Construct from the default config location.
    pub async fn from_default_location() -> Result<Self, LlmError> {
        Ok(Self::default_stub("default"))
    }

    /// Synchronous stub constructor for a named provider profile (no I/O;
    /// safe to call without an async runtime).
    pub fn with_profile_stub(profile: &str) -> Self {
        Self::default_stub(profile)
    }

    /// Construct for a named provider profile.
    pub async fn with_profile(profile: impl Into<String>) -> Result<Self, LlmError> {
        Ok(Self::default_stub(&profile.into()))
    }

    fn default_stub(profile: &str) -> Self {
        Self {
            profile: profile.to_owned(),
        }
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Perform a streaming completion. In stub mode returns a canned stream so
    /// the whole upstream chain (ACP/TUI) can be exercised without a live model.
    pub async fn chat_stream<'a>(
        &self,
        _messages: &'a [ChatMessage],
    ) -> Result<impl Stream<Item = Delta> + 'a, LlmError> {
        let mut done = 0usize;
        Ok(futures::stream::poll_fn(move |_cx| {
            if done < 3 {
                done += 1;
                std::task::Poll::Ready(Some(Delta::Text(format!("stub {done}"))))
            } else {
                std::task::Poll::Ready(None)
            }
        }))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn chat_stream_emits_canned_deltas() {
        let client = LlmClient::from_default_location().await.unwrap();
        let messages = [ChatMessage::Text(ChatRole::User, "hi".into())];
        let mut stream = client.chat_stream(&messages).await.unwrap();
        let mut texts = Vec::new();
        while let Some(Delta::Text(t)) = stream.next().await {
            texts.push(t);
        }
        assert_eq!(texts, vec!["stub 1", "stub 2", "stub 3"]);
    }

    #[tokio::test]
    async fn complete_concatenates_stream() {
        let client = LlmClient::with_profile("deepseek-v4-flash").await.unwrap();
        let out = client.complete("hello").await.unwrap();
        assert_eq!(out, "stub 1stub 2stub 3");
    }

    #[test]
    fn roles_serialize_to_names() {
        assert_eq!(ChatRole::System.as_str(), "system");
        assert_eq!(ChatRole::User.as_str(), "user");
        assert_eq!(ChatRole::Assistant.as_str(), "assistant");
    }
}
