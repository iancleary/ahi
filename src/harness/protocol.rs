//! The wire protocol a provider speaks. Independent of who the provider is and
//! of how a request is authenticated.

use serde::{Deserialize, Serialize};

/// One of the three request/stream grammars ahi can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    /// OpenAI Chat Completions: `POST <baseUrl>/chat/completions`.
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    /// OpenAI Responses: `POST <baseUrl>/responses`.
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    /// Anthropic Messages: `POST <baseUrl>/messages`.
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

/// Where a provider's model catalog is fetched from on explicit refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogRoute {
    /// The gateway-specific `GET <origin>/agent/models.json` convention.
    GatewayConvention,
    /// The provider's own `GET <baseUrl>/models` list.
    StandardModels,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [
        Protocol::OpenAiCompletions,
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
    ];

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|protocol| protocol.as_str() == text)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    pub(crate) fn inference_path(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "/chat/completions",
            Self::OpenAiResponses => "/responses",
            Self::AnthropicMessages => "/messages",
        }
    }

    pub(crate) fn catalog_route(self) -> CatalogRoute {
        match self {
            Self::OpenAiCompletions => CatalogRoute::GatewayConvention,
            Self::OpenAiResponses | Self::AnthropicMessages => CatalogRoute::StandardModels,
        }
    }

    /// The header that carries a configured API key for this protocol.
    pub(crate) fn api_key_header(self) -> (&'static str, &'static str) {
        match self {
            Self::OpenAiCompletions | Self::OpenAiResponses => ("authorization", "Bearer "),
            Self::AnthropicMessages => ("x-api-key", ""),
        }
    }

    /// Headers every request to this protocol must carry besides authentication.
    pub(crate) fn fixed_headers(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::AnthropicMessages => &[("anthropic-version", "2023-06-01")],
            _ => &[],
        }
    }

    pub fn supported_list() -> String {
        Self::ALL
            .iter()
            .map(|protocol| protocol.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_round_trip_their_spelling() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::parse(protocol.as_str()), Some(protocol));
            let json = serde_json::to_string(&protocol).unwrap();
            assert_eq!(json, format!("\"{}\"", protocol.as_str()));
        }
        assert_eq!(Protocol::parse("openai"), None);
        assert_eq!(
            Protocol::AnthropicMessages.api_key_header(),
            ("x-api-key", "")
        );
        assert_eq!(
            Protocol::OpenAiCompletions.inference_path(),
            "/chat/completions"
        );
    }
}
