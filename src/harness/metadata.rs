use super::*;
use std::num::NonZeroU64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenParameter {
    MaxTokens,
    MaxCompletionTokens,
}

impl TokenParameter {
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TokenPrice(f64);

impl Eq for TokenPrice {}

impl<'de> Deserialize<'de> for TokenPrice {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        if !value.is_finite() || value < 0.0 {
            return Err(serde::de::Error::custom(
                "expected a finite nonnegative price",
            ));
        }
        Ok(Self(value))
    }
}

macro_rules! optional_fields {
    ($name:ident { $($field:ident: $kind:ty),* $(,)? }) => {
        #[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name {
            $(#[serde(default, skip_serializing_if = "Option::is_none")]
            pub $field: Option<$kind>,)*
        }
        impl $name {
            pub fn overlay(&self, specified: &Self) -> Self {
                Self { $($field: specified.$field.as_ref().or(self.$field.as_ref()).cloned(),)* }
            }
        }
    };
}

optional_fields!(CompletionCompatibility {
    supports_developer_role: bool,
    supports_reasoning_effort: bool,
    supports_store: bool,
    supports_usage_in_streaming: bool,
    supports_finish_reason: bool,
    max_tokens_field: TokenParameter,
    requires_tool_result_name: bool,
    requires_assistant_after_tool_result: bool,
    requires_thinking_as_text: bool,
    requires_reasoning_content_on_assistant_messages: bool,
});
optional_fields!(ModelCost {
    input: TokenPrice,
    output: TokenPrice,
    cache_read: TokenPrice,
    cache_write: TokenPrice,
});
optional_fields!(InputLimits {
    max_request_bytes: NonZeroU64
});

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<InputModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub input_limits: InputLimits,
    #[serde(default)]
    pub cost: ModelCost,
    #[serde(default)]
    pub compat: CompletionCompatibility,
}

impl ModelMetadata {
    pub fn overlay(&self, specified: &Self) -> Self {
        Self {
            name: specified.name.clone().or_else(|| self.name.clone()),
            context_window: specified.context_window.or(self.context_window),
            max_tokens: specified.max_tokens.or(self.max_tokens),
            input: specified.input.clone().or_else(|| self.input.clone()),
            reasoning: specified.reasoning.or(self.reasoning),
            input_limits: self.input_limits.overlay(&specified.input_limits),
            cost: self.cost.overlay(&specified.cost),
            compat: self.compat.overlay(&specified.compat),
        }
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = &self.name {
            parts.push(name.clone());
        }
        if let Some(window) = self.context_window {
            parts.push(format!("context {window}"));
        }
        if let Some(limit) = self.max_tokens {
            parts.push(format!("output {limit}"));
        }
        if let Some(limit) = self.input_limits.max_request_bytes {
            parts.push(format!("request {limit} bytes"));
        }
        if self
            .input
            .as_ref()
            .is_some_and(|input| input.contains(&InputModality::Image))
        {
            parts.push("image capable (text input only)".to_string());
        }
        if self.reasoning == Some(true) {
            parts.push("reasoning capable".to_string());
        }
        parts.join(" | ")
    }

    pub(crate) fn usage_cost(&self, input: u64, output: u64, cached: Option<u64>) -> Option<f64> {
        let input_rate = self.cost.input?.0;
        let output_rate = self.cost.output?.0;
        let input_cost = match cached {
            Some(cached) if cached <= input => {
                (input - cached) as f64 * input_rate
                    + if cached == 0 {
                        0.0
                    } else {
                        cached as f64 * self.cost.cache_read?.0
                    }
            }
            Some(_) => return None,
            None if self
                .cost
                .cache_read
                .is_some_and(|rate| rate.0 == input_rate) =>
            {
                input as f64 * input_rate
            }
            None => return None,
        };
        let cost = (input_cost + output as f64 * output_rate) / 1_000_000.0;
        cost.is_finite().then_some(cost)
    }
}

pub(super) fn parse_metadata(value: &Value) -> Result<ModelMetadata> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("model must be an object"))?;
    let mut metadata = serde_json::Map::new();
    for (field, value) in object {
        if field == "id" {
            continue;
        }
        if ![
            "name",
            "contextWindow",
            "maxTokens",
            "input",
            "reasoning",
            "inputLimits",
            "cost",
            "compat",
        ]
        .contains(&field.as_str())
        {
            return Err(Error::new("unsupported model metadata field"));
        }
        let mut single = serde_json::Map::new();
        single.insert(field.clone(), value.clone());
        serde_json::from_value::<ModelMetadata>(Value::Object(single)).map_err(|_| {
            Error::new(format!(
                "invalid model {field}: expected supported typed metadata"
            ))
        })?;
        metadata.insert(field.clone(), value.clone());
    }
    let metadata: ModelMetadata = serde_json::from_value(Value::Object(metadata))
        .map_err(|_| Error::new("invalid model metadata"))?;
    if metadata
        .name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty() || name.chars().any(char::is_control))
    {
        return Err(Error::new(
            "invalid model name: expected nonempty display text without control characters",
        ));
    }
    Ok(metadata)
}
