use std::io;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};
use futures::{AsyncBufReadExt, AsyncReadExt, StreamExt, io::BufReader, stream::BoxStream};
use http_client::http::{self, HeaderMap, HeaderValue};
use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest, StatusCode};
use serde::{Deserialize, Serialize};
pub use settings::{AnthropicAvailableModel as AvailableModel, ModelMode};
use strum::{EnumIter, EnumString};
use thiserror::Error;

pub const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AnthropicModelCacheConfiguration {
    pub min_total_token: u64,
    pub should_speculate: bool,
    pub max_cache_anchors: usize,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum AnthropicModelMode {
    #[default]
    Default,
    Thinking {
        budget_tokens: Option<u32>,
    },
}

impl From<ModelMode> for AnthropicModelMode {
    fn from(value: ModelMode) -> Self {
        match value {
            ModelMode::Default => AnthropicModelMode::Default,
            ModelMode::Thinking { budget_tokens } => AnthropicModelMode::Thinking { budget_tokens },
        }
    }
}

impl From<AnthropicModelMode> for ModelMode {
    fn from(value: AnthropicModelMode) -> Self {
        match value {
            AnthropicModelMode::Default => ModelMode::Default,
            AnthropicModelMode::Thinking { budget_tokens } => ModelMode::Thinking { budget_tokens },
        }
    }
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, EnumIter)]
pub enum Model {
    #[serde(rename = "claude-opus-4", alias = "claude-opus-4-latest")]
    ClaudeOpus4,
    #[serde(rename = "claude-opus-4-1", alias = "claude-opus-4-1-latest")]
    ClaudeOpus4_1,
    #[serde(
        rename = "claude-opus-4-thinking",
        alias = "claude-opus-4-thinking-latest"
    )]
    ClaudeOpus4Thinking,
    #[serde(
        rename = "claude-opus-4-1-thinking",
        alias = "claude-opus-4-1-thinking-latest"
    )]
    ClaudeOpus4_1Thinking,
    #[serde(rename = "claude-sonnet-4", alias = "claude-sonnet-4-latest")]
    ClaudeSonnet4,
    #[serde(
        rename = "claude-sonnet-4-thinking",
        alias = "claude-sonnet-4-thinking-latest"
    )]
    ClaudeSonnet4Thinking,
    #[default]
    #[serde(rename = "claude-sonnet-4-5", alias = "claude-sonnet-4-5-latest")]
    ClaudeSonnet4_5,
    #[serde(
        rename = "claude-sonnet-4-5-thinking",
        alias = "claude-sonnet-4-5-thinking-latest"
    )]
    ClaudeSonnet4_5Thinking,
    #[serde(rename = "claude-3-7-sonnet", alias = "claude-3-7-sonnet-latest")]
    Claude3_7Sonnet,
    #[serde(
        rename = "claude-3-7-sonnet-thinking",
        alias = "claude-3-7-sonnet-thinking-latest"
    )]
    Claude3_7SonnetThinking,
    #[serde(rename = "claude-3-5-sonnet", alias = "claude-3-5-sonnet-latest")]
    Claude3_5Sonnet,
    #[serde(rename = "claude-3-5-haiku", alias = "claude-3-5-haiku-latest")]
    Claude3_5Haiku,
    #[serde(rename = "claude-3-opus", alias = "claude-3-opus-latest")]
    Claude3Opus,
    #[serde(rename = "claude-3-sonnet", alias = "claude-3-sonnet-latest")]
    Claude3Sonnet,
    #[serde(rename = "claude-3-haiku", alias = "claude-3-haiku-latest")]
    Claude3Haiku,
    #[serde(rename = "custom")]
    Custom {
        name: String,
        max_tokens: u64,
        /// The name displayed in the UI, such as in the assistant panel model dropdown menu.
        display_name: Option<String>,
        /// Override this model with a different Anthropic model for tool calls.
        tool_override: Option<String>,
        /// Indicates whether this custom model supports caching.
        cache_configuration: Option<AnthropicModelCacheConfiguration>,
        max_output_tokens: Option<u64>,
        default_temperature: Option<f32>,
        #[serde(default)]
        extra_beta_headers: Vec<String>,
        #[serde(default)]
        mode: AnthropicModelMode,
    },
}

impl Model {
    pub fn default_fast() -> Self {
        Self::Claude3_5Haiku
    }

    pub fn from_id(id: &str) -> Result<Self> {
        if id.starts_with("claude-opus-4-1-thinking") {
            return Ok(Self::ClaudeOpus4_1Thinking);
        }

        if id.starts_with("claude-opus-4-thinking") {
            return Ok(Self::ClaudeOpus4Thinking);
        }

        if id.starts_with("claude-opus-4-1") {
            return Ok(Self::ClaudeOpus4_1);
        }

        if id.starts_with("claude-opus-4") {
            return Ok(Self::ClaudeOpus4);
        }

        if id.starts_with("claude-sonnet-4-5-thinking") {
            return Ok(Self::ClaudeSonnet4_5Thinking);
        }

        if id.starts_with("claude-sonnet-4-5") {
            return Ok(Self::ClaudeSonnet4_5);
        }

        if id.starts_with("claude-sonnet-4-thinking") {
            return Ok(Self::ClaudeSonnet4Thinking);
        }

        if id.starts_with("claude-sonnet-4") {
            return Ok(Self::ClaudeSonnet4);
        }

        if id.starts_with("claude-3-7-sonnet-thinking") {
            return Ok(Self::Claude3_7SonnetThinking);
        }

        if id.starts_with("claude-3-7-sonnet") {
            return Ok(Self::Claude3_7Sonnet);
        }

        if id.starts_with("claude-3-5-sonnet") {
            return Ok(Self::Claude3_5Sonnet);
        }

        if id.starts_with("claude-3-5-haiku") {
            return Ok(Self::Claude3_5Haiku);
        }

        if id.starts_with("claude-3-opus") {
            return Ok(Self::Claude3Opus);
        }

        if id.starts_with("claude-3-sonnet") {
            return Ok(Self::Claude3Sonnet);
        }

        if id.starts_with("claude-3-haiku") {
            return Ok(Self::Claude3Haiku);
        }

        Err(anyhow!("invalid model ID: {id}"))
    }

    pub fn id(&self) -> &str {
        match self {
            Self::ClaudeOpus4 => "claude-opus-4-latest",
            Self::ClaudeOpus4_1 => "claude-opus-4-1-latest",
            Self::ClaudeOpus4Thinking => "claude-opus-4-thinking-latest",
            Self::ClaudeOpus4_1Thinking => "claude-opus-4-1-thinking-latest",
            Self::ClaudeSonnet4 => "claude-sonnet-4-latest",
            Self::ClaudeSonnet4Thinking => "claude-sonnet-4-thinking-latest",
            Self::ClaudeSonnet4_5 => "claude-sonnet-4-5-latest",
            Self::ClaudeSonnet4_5Thinking => "claude-sonnet-4-5-thinking-latest",
            Self::Claude3_5Sonnet => "claude-3-5-sonnet-latest",
            Self::Claude3_7Sonnet => "claude-3-7-sonnet-latest",
            Self::Claude3_7SonnetThinking => "claude-3-7-sonnet-thinking-latest",
            Self::Claude3_5Haiku => "claude-3-5-haiku-latest",
            Self::Claude3Opus => "claude-3-opus-latest",
            Self::Claude3Sonnet => "claude-3-sonnet-20240229",
            Self::Claude3Haiku => "claude-3-haiku-20240307",
            Self::Custom { name, .. } => name,
        }
    }

    /// The id of the model that should be used for making API requests
    pub fn request_id(&self) -> &str {
        // If using alternate model IDs is enabled, this will override the model mapping
        let alternate_model_map = std::env::var("ANTHROPIC_USE_ALTERNATE_MODEL_IDS").is_ok();

        let model_id = match self {
            Self::ClaudeOpus4 | Self::ClaudeOpus4Thinking => "claude-opus-4-20250514",
            Self::ClaudeOpus4_1 | Self::ClaudeOpus4_1Thinking => "claude-opus-4-1-20250805",
            Self::ClaudeSonnet4 | Self::ClaudeSonnet4Thinking => "claude-sonnet-4-20250514",
            Self::ClaudeSonnet4_5 | Self::ClaudeSonnet4_5Thinking => "claude-sonnet-4-5-20250929",
            Self::Claude3_5Sonnet => "claude-3-5-sonnet-latest",
            Self::Claude3_7Sonnet => "claude-3-7-sonnet-latest",
            Self::Claude3_7SonnetThinking => {
                if alternate_model_map {
                    log::info!(
                        "Using alternate model ID for Claude3_7SonnetThinking: claude-3-7-sonnet-latest"
                    );
                    "claude-3-7-sonnet-latest" // Use non-thinking variant as fallback
                } else {
                    "claude-3-7-sonnet-thinking-latest"
                }
            }
            Self::Claude3_5Haiku => "claude-3-5-haiku-latest",
            Self::Claude3Opus => "claude-3-opus-latest",
            Self::Claude3Sonnet => "claude-3-sonnet-20240229",
            Self::Claude3Haiku => "claude-3-haiku-20240307",
            Self::Custom { name, .. } => name,
        };

        log::info!(
            "Anthropic model requested={:?}, using model_id={}",
            self,
            model_id
        );
        model_id
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::ClaudeOpus4 => "Claude Opus 4",
            Self::ClaudeOpus4_1 => "Claude Opus 4.1",
            Self::ClaudeOpus4Thinking => "Claude Opus 4 Thinking",
            Self::ClaudeOpus4_1Thinking => "Claude Opus 4.1 Thinking",
            Self::ClaudeSonnet4 => "Claude Sonnet 4",
            Self::ClaudeSonnet4Thinking => "Claude Sonnet 4 Thinking",
            Self::ClaudeSonnet4_5 => "Claude Sonnet 4.5",
            Self::ClaudeSonnet4_5Thinking => "Claude Sonnet 4.5 Thinking",
            Self::Claude3_7Sonnet => "Claude 3.7 Sonnet",
            Self::Claude3_5Sonnet => "Claude 3.5 Sonnet",
            Self::Claude3_7SonnetThinking => "Claude 3.7 Sonnet Thinking",
            Self::Claude3_5Haiku => "Claude 3.5 Haiku",
            Self::Claude3Opus => "Claude 3 Opus",
            Self::Claude3Sonnet => "Claude 3 Sonnet",
            Self::Claude3Haiku => "Claude 3 Haiku",
            Self::Custom {
                name, display_name, ..
            } => display_name.as_ref().unwrap_or(name),
        }
    }

    pub fn cache_configuration(&self) -> Option<AnthropicModelCacheConfiguration> {
        match self {
            Self::ClaudeOpus4
            | Self::ClaudeOpus4_1
            | Self::ClaudeOpus4Thinking
            | Self::ClaudeOpus4_1Thinking
            | Self::ClaudeSonnet4
            | Self::ClaudeSonnet4Thinking
            | Self::ClaudeSonnet4_5
            | Self::ClaudeSonnet4_5Thinking
            | Self::Claude3_5Sonnet
            | Self::Claude3_5Haiku
            | Self::Claude3_7Sonnet
            | Self::Claude3_7SonnetThinking
            | Self::Claude3Haiku => Some(AnthropicModelCacheConfiguration {
                min_total_token: 2_048,
                should_speculate: true,
                max_cache_anchors: 4,
            }),
            Self::Custom {
                cache_configuration,
                ..
            } => cache_configuration.clone(),
            _ => None,
        }
    }

    pub fn max_token_count(&self) -> u64 {
        match self {
            Self::ClaudeOpus4
            | Self::ClaudeOpus4_1
            | Self::ClaudeOpus4Thinking
            | Self::ClaudeOpus4_1Thinking
            | Self::ClaudeSonnet4
            | Self::ClaudeSonnet4Thinking
            | Self::ClaudeSonnet4_5
            | Self::ClaudeSonnet4_5Thinking
            | Self::Claude3_5Sonnet
            | Self::Claude3_5Haiku
            | Self::Claude3_7Sonnet
            | Self::Claude3_7SonnetThinking
            | Self::Claude3Opus
            | Self::Claude3Sonnet
            | Self::Claude3Haiku => 200_000,
            Self::Custom { max_tokens, .. } => *max_tokens,
        }
    }

    pub fn max_output_tokens(&self) -> u64 {
        match self {
            Self::ClaudeOpus4
            | Self::ClaudeOpus4_1
            | Self::ClaudeOpus4Thinking
            | Self::ClaudeOpus4_1Thinking
            | Self::ClaudeSonnet4
            | Self::ClaudeSonnet4Thinking
            | Self::ClaudeSonnet4_5
            | Self::ClaudeSonnet4_5Thinking
            | Self::Claude3_5Sonnet
            | Self::Claude3_7Sonnet
            | Self::Claude3_7SonnetThinking
            | Self::Claude3_5Haiku => 8_192,
            Self::Claude3Opus | Self::Claude3Sonnet | Self::Claude3Haiku => 4_096,
            Self::Custom {
                max_output_tokens, ..
            } => max_output_tokens.unwrap_or(4_096),
        }
    }

    pub fn default_temperature(&self) -> f32 {
        match self {
            Self::ClaudeOpus4
            | Self::ClaudeOpus4_1
            | Self::ClaudeOpus4Thinking
            | Self::ClaudeOpus4_1Thinking
            | Self::ClaudeSonnet4
            | Self::ClaudeSonnet4Thinking
            | Self::ClaudeSonnet4_5
            | Self::ClaudeSonnet4_5Thinking
            | Self::Claude3_5Sonnet
            | Self::Claude3_7Sonnet
            | Self::Claude3_7SonnetThinking
            | Self::Claude3_5Haiku
            | Self::Claude3Opus
            | Self::Claude3Sonnet
            | Self::Claude3Haiku => 1.0,
            Self::Custom {
                default_temperature,
                ..
            } => default_temperature.unwrap_or(1.0),
        }
    }

    pub fn mode(&self) -> AnthropicModelMode {
        match self {
            Self::ClaudeOpus4
            | Self::ClaudeOpus4_1
            | Self::ClaudeSonnet4
            | Self::ClaudeSonnet4_5
            | Self::Claude3_5Sonnet
            | Self::Claude3_7Sonnet
            | Self::Claude3_5Haiku
            | Self::Claude3Opus
            | Self::Claude3Sonnet
            | Self::Claude3Haiku => AnthropicModelMode::Default,
            Self::ClaudeOpus4Thinking
            | Self::ClaudeOpus4_1Thinking
            | Self::ClaudeSonnet4Thinking
            | Self::ClaudeSonnet4_5Thinking
            | Self::Claude3_7SonnetThinking => AnthropicModelMode::Thinking {
                budget_tokens: Some(4_096),
            },
            Self::Custom { mode, .. } => mode.clone(),
        }
    }

    pub const DEFAULT_BETA_HEADERS: &[&str] = &[];

    pub fn beta_headers(&self) -> String {
        let mut headers = Self::DEFAULT_BETA_HEADERS
            .iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>();

        match self {
            Self::Claude3_7Sonnet | Self::Claude3_7SonnetThinking => {
                // Include token-efficient tools beta header only if opt-in env var is set.
                if std::env::var("ANTHROPIC_ENABLE_TOKEN_EFFICIENT").is_ok() {
                    // https://docs.anthropic.com/en/docs/build-with-claude/tool-use/token-efficient-tool-use
                    headers.push("token-efficient-tools-2025-02-19".to_string());
                }

                // Don't use any beta headers if explicitly disabled
                if std::env::var("ANTHROPIC_DISABLE_BETA_HEADERS").is_ok() {
                    log::info!("Anthropic beta headers disabled by environment variable");
                    headers.clear();
                }
            }
            Self::Custom {
                extra_beta_headers, ..
            } => {
                headers.extend(
                    extra_beta_headers
                        .iter()
                        .filter(|header| !header.trim().is_empty())
                        .cloned(),
                );
            }
            _ => {}
        }

        let result = headers.join(",");
        log::info!("Using Anthropic beta headers: {}", result);
        result
    }

    pub fn tool_model_id(&self) -> &str {
        if let Self::Custom {
            tool_override: Some(tool_override),
            ..
        } = self
        {
            tool_override
        } else {
            self.request_id()
        }
    }
}

pub async fn complete(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
    request: Request,
    beta_headers: String,
) -> Result<Response, AnthropicError> {
    let uri = format!("{api_url}/v1/messages");
    let request_builder = HttpRequest::builder()
        .method(Method::POST)
        .uri(&uri)
        .header("Anthropic-Version", "2023-06-01")
        .header("Anthropic-Beta", &beta_headers)
        .header("X-Api-Key", api_key.trim())
        .header("Content-Type", "application/json");

    let serialized_request =
        serde_json::to_string(&request).map_err(AnthropicError::SerializeRequest)?;

    // Log comprehensive request details
    log::info!(
        "Anthropic complete request uri={} beta_headers={} model={} max_tokens={} tools={} thinking={}",
        uri,
        beta_headers,
        request.model,
        request.max_tokens,
        request.tools.len(),
        request.thinking.is_some()
    );
    log::trace!(
        "Anthropic complete request full body (len={}): {}",
        serialized_request.len(),
        serialized_request
    );

    let http_request = request_builder
        .body(AsyncBody::from(serialized_request.clone()))
        .map_err(AnthropicError::BuildRequestBody)?;

    let mut response = client
        .send(http_request)
        .await
        .map_err(AnthropicError::HttpSend)?;
    let status_code = response.status();
    let mut body = String::new();
    response
        .body_mut()
        .read_to_string(&mut body)
        .await
        .map_err(AnthropicError::ReadResponse)?;

    if status_code.is_success() {
        Ok(serde_json::from_str(&body).map_err(AnthropicError::DeserializeResponse)?)
    } else {
        // Try to parse as ApiError first for more detailed error information
        let api_error = serde_json::from_str::<ApiError>(&body);
        if let Ok(error) = api_error {
            log::error!(
                "Anthropic API error: status={} type={} message={}",
                status_code,
                error.error_type,
                error.message
            );
        } else {
            log::error!(
                "Anthropic non-success response status={} body_len={} body_preview={}",
                status_code,
                body.len(),
                &body.chars().take(1000).collect::<String>()
            );
        }

        if status_code == StatusCode::BAD_REQUEST {
            // Dump full serialized request (may include thinking/tool blocks) for diagnostics.
            log::error!(
                "Anthropic 400 request dump: serialized_request_len={} serialized_request={}",
                serialized_request.len(),
                serialized_request
            );
            log::error!(
                "Anthropic 400 request meta: model={} max_tokens={} messages={} tools={} thinking_present={}",
                request.model,
                request.max_tokens,
                request.messages.len(),
                request.tools.len(),
                request.thinking.is_some()
            );
        }

        Err(AnthropicError::HttpResponseError {
            status_code,
            message: body,
        })
    }
}

pub async fn stream_completion(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
    request: Request,
    beta_headers: String,
) -> Result<BoxStream<'static, Result<Event, AnthropicError>>, AnthropicError> {
    stream_completion_with_rate_limit_info(client, api_url, api_key, request, beta_headers)
        .await
        .map(|output| output.0)
}

/// An individual rate limit.
#[derive(Debug)]
pub struct RateLimit {
    pub limit: usize,
    pub remaining: usize,
    pub reset: DateTime<Utc>,
}

impl RateLimit {
    fn from_headers(resource: &str, headers: &HeaderMap<HeaderValue>) -> Result<Self> {
        let limit =
            get_header(&format!("anthropic-ratelimit-{resource}-limit"), headers)?.parse()?;
        let remaining = get_header(
            &format!("anthropic-ratelimit-{resource}-remaining"),
            headers,
        )?
        .parse()?;
        let reset = DateTime::parse_from_rfc3339(get_header(
            &format!("anthropic-ratelimit-{resource}-reset"),
            headers,
        )?)?
        .to_utc();

        Ok(Self {
            limit,
            remaining,
            reset,
        })
    }
}

/// <https://docs.anthropic.com/en/api/rate-limits#response-headers>
#[derive(Debug)]
pub struct RateLimitInfo {
    pub retry_after: Option<Duration>,
    pub requests: Option<RateLimit>,
    pub tokens: Option<RateLimit>,
    pub input_tokens: Option<RateLimit>,
    pub output_tokens: Option<RateLimit>,
}

impl RateLimitInfo {
    fn from_headers(headers: &HeaderMap<HeaderValue>) -> Self {
        // Check if any rate limit headers exist
        let has_rate_limit_headers = headers
            .keys()
            .any(|k| k == "retry-after" || k.as_str().starts_with("anthropic-ratelimit-"));

        if !has_rate_limit_headers {
            return Self {
                retry_after: None,
                requests: None,
                tokens: None,
                input_tokens: None,
                output_tokens: None,
            };
        }

        Self {
            retry_after: parse_retry_after(headers),
            requests: RateLimit::from_headers("requests", headers).ok(),
            tokens: RateLimit::from_headers("tokens", headers).ok(),
            input_tokens: RateLimit::from_headers("input-tokens", headers).ok(),
            output_tokens: RateLimit::from_headers("output-tokens", headers).ok(),
        }
    }
}

/// Parses the Retry-After header value as an integer number of seconds (anthropic always uses
/// seconds). Note that other services might specify an HTTP date or some other format for this
/// header. Returns `None` if the header is not present or cannot be parsed.
pub fn parse_retry_after(headers: &HeaderMap<HeaderValue>) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn get_header<'a>(key: &str, headers: &'a HeaderMap) -> anyhow::Result<&'a str> {
    Ok(headers
        .get(key)
        .with_context(|| format!("missing header `{key}`"))?
        .to_str()?)
}

pub async fn stream_completion_with_rate_limit_info(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
    request: Request,
    beta_headers: String,
) -> Result<
    (
        BoxStream<'static, Result<Event, AnthropicError>>,
        Option<RateLimitInfo>,
    ),
    AnthropicError,
> {
    let request = StreamingRequest {
        base: request,
        stream: true,
    };
    let uri = format!("{api_url}/v1/messages");

    let request_builder = HttpRequest::builder()
        .method(Method::POST)
        .uri(&uri)
        .header("Anthropic-Version", "2023-06-01")
        .header("Anthropic-Beta", &beta_headers)
        .header("X-Api-Key", api_key.trim())
        .header("Content-Type", "application/json");
    let serialized_request =
        serde_json::to_string(&request).map_err(AnthropicError::SerializeRequest)?;

    // Log detailed request information for debugging
    log::info!(
        "Anthropic streaming request uri={} beta_headers={}",
        uri,
        beta_headers
    );
    log::info!(
        "Anthropic request model={} thinking={} tool_count={}",
        request.base.model,
        request.base.thinking.is_some(),
        request.base.tools.len()
    );
    log::debug!("Anthropic request full body: {}", serialized_request);

    let http_request = request_builder
        .body(AsyncBody::from(serialized_request))
        .map_err(AnthropicError::BuildRequestBody)?;

    let mut response = client
        .send(http_request)
        .await
        .map_err(AnthropicError::HttpSend)?;
    let rate_limits = RateLimitInfo::from_headers(response.headers());
    if response.status().is_success() {
        let reader = BufReader::new(response.into_body());
        let stream = reader
            .lines()
            .filter_map(|line| async move {
                match line {
                    Ok(line) => {
                        let line = line.strip_prefix("data: ")?;
                        match serde_json::from_str(line) {
                            Ok(response) => Some(Ok(response)),
                            Err(error) => Some(Err(AnthropicError::DeserializeResponse(error))),
                        }
                    }
                    Err(error) => Some(Err(AnthropicError::ReadResponse(error))),
                }
            })
            .boxed();
        Ok((stream, Some(rate_limits)))
    } else if response.status().as_u16() == 529 {
        Err(AnthropicError::ServerOverloaded {
            retry_after: rate_limits.retry_after,
        })
    } else if let Some(retry_after) = rate_limits.retry_after {
        Err(AnthropicError::RateLimit { retry_after })
    } else {
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .map_err(AnthropicError::ReadResponse)?;

        // Try to parse error as ApiError first
        let api_error = serde_json::from_str::<ApiError>(&body);
        if let Ok(error) = api_error {
            log::error!(
                "Anthropic streaming API error: status={} type={} message={}",
                response.status(),
                error.error_type,
                error.message
            );
        } else {
            log::error!(
                "Anthropic streaming error: status={} body={}",
                response.status(),
                body
            );

            // Try to extract more specific error information
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(error) = json.get("error") {
                    log::error!("Anthropic API error details: {:?}", error);
                }
            }
        }

        // Attempt richer parsing path.
        let event_result = serde_json::from_str::<Event>(&body);
        match event_result {
            Ok(Event::Error { error }) => Err(AnthropicError::ApiError(error)),
            Ok(_) => {
                if let Ok(api_error) = serde_json::from_str::<ApiError>(&body) {
                    Err(AnthropicError::ApiError(api_error))
                } else {
                    Err(AnthropicError::HttpResponseError {
                        status_code: response.status(),
                        message: body,
                    })
                }
            }
            Err(_) => {
                if let Ok(api_error) = serde_json::from_str::<ApiError>(&body) {
                    Err(AnthropicError::ApiError(api_error))
                } else {
                    Err(AnthropicError::HttpResponseError {
                        status_code: response.status(),
                        message: body,
                    })
                }
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlType {
    Ephemeral,
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: CacheControlType,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: Role,
    pub content: Vec<RequestContent>,
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum RequestContent {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
    #[serde(rename = "image")]
    Image {
        source: ImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        content: ToolResultContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum ToolResultContent {
    Plain(String),
    Multipart(Vec<ToolResultPart>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultPart {
    Text { text: String },
    Image { source: ImageSource },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
    None,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Thinking {
    Enabled { budget_tokens: Option<u32> },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum StringOrContents {
    String(String),
    Content(Vec<RequestContent>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Request {
    pub model: String,
    pub max_tokens: u64,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<StringOrContents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StreamingRequest {
    #[serde(flatten)]
    pub base: Request,
    pub stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Metadata {
    pub user_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: Role,
    pub content: Vec<ResponseContent>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "message_start")]
    MessageStart { message: Response },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ResponseContent,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: MessageDelta, usage: Usage },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: ApiError },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[derive(Debug)]
pub enum AnthropicError {
    /// Failed to serialize the HTTP request body to JSON
    SerializeRequest(serde_json::Error),

    /// Failed to construct the HTTP request body
    BuildRequestBody(http::Error),

    /// Failed to send the HTTP request
    HttpSend(anyhow::Error),

    /// Failed to deserialize the response from JSON
    DeserializeResponse(serde_json::Error),

    /// Failed to read from response stream
    ReadResponse(io::Error),

    /// HTTP error response from the API
    HttpResponseError {
        status_code: StatusCode,
        message: String,
    },

    /// Rate limit exceeded
    RateLimit { retry_after: Duration },

    /// Server overloaded
    ServerOverloaded { retry_after: Option<Duration> },

    /// API returned an error response
    ApiError(ApiError),
}

#[derive(Debug, Serialize, Deserialize, Error)]
#[error("Anthropic API Error: {error_type}: {message}")]
pub struct ApiError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

/// An Anthropic API error code.
/// <https://docs.anthropic.com/en/api/errors#http-errors>
#[derive(Debug, PartialEq, Eq, Clone, Copy, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum ApiErrorCode {
    /// 400 - `invalid_request_error`: There was an issue with the format or content of your request.
    InvalidRequestError,
    /// 401 - `authentication_error`: There's an issue with your API key.
    AuthenticationError,
    /// 403 - `permission_error`: Your API key does not have permission to use the specified resource.
    PermissionError,
    /// 404 - `not_found_error`: The requested resource was not found.
    NotFoundError,
    /// 413 - `request_too_large`: Request exceeds the maximum allowed number of bytes.
    RequestTooLarge,
    /// 429 - `rate_limit_error`: You have exceeded your rate limit.
    RateLimitError,
    /// 500 - `api_error`: An unexpected error has occurred internal to Anthropic's systems.
    ApiError,
    /// 529 - `overloaded_error`: Anthropic's API is temporarily overloaded.
    OverloadedError,
    /// 400 - `model_not_found_error`: The specified model was not found.
    ModelNotFoundError,
    /// 400 - `invalid_tool_use_error`: There was an issue with the tool use format.
    InvalidToolUseError,
    /// 400 - `unsupported_feature_error`: Feature not supported for this model or account.
    UnsupportedFeatureError,
}

impl ApiError {
    pub fn code(&self) -> Option<ApiErrorCode> {
        ApiErrorCode::from_str(&self.error_type).ok()
    }

    pub fn is_rate_limit_error(&self) -> bool {
        matches!(self.code(), Some(ApiErrorCode::RateLimitError))
    }

    pub fn match_window_exceeded(&self) -> Option<u64> {
        let Some(ApiErrorCode::InvalidRequestError) = self.code() else {
            return None;
        };

        parse_prompt_too_long(&self.message)
    }

    pub fn is_thinking_related(&self) -> bool {
        self.message.to_lowercase().contains("thinking")
            || (self.message.to_lowercase().contains("feature")
                && self.message.to_lowercase().contains("support"))
    }

    pub fn is_tool_related(&self) -> bool {
        self.message.to_lowercase().contains("tool")
            || self.message.to_lowercase().contains("function")
            || self.message.to_lowercase().contains("schema")
    }

    pub fn is_model_related(&self) -> bool {
        self.message.to_lowercase().contains("model")
            && (self.message.to_lowercase().contains("not found")
                || self.message.to_lowercase().contains("invalid")
                || self.message.to_lowercase().contains("unsupported"))
    }

    pub fn has_debug_info(&self) -> bool {
        !self.message.is_empty() && self.message.len() > 10
    }
}

pub fn parse_prompt_too_long(message: &str) -> Option<u64> {
    message
        .strip_prefix("prompt is too long: ")?
        .split_once(" tokens")?
        .0
        .parse()
        .ok()
}

#[test]
fn test_match_window_exceeded() {
    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "prompt is too long: 220000 tokens > 200000".to_string(),
    };
    assert_eq!(error.match_window_exceeded(), Some(220_000));

    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "prompt is too long: 1234953 tokens".to_string(),
    };
    assert_eq!(error.match_window_exceeded(), Some(1234953));

    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "not a prompt length error".to_string(),
    };
    assert_eq!(error.match_window_exceeded(), None);

    let error = ApiError {
        error_type: "rate_limit_error".to_string(),
        message: "prompt is too long: 12345 tokens".to_string(),
    };
    assert_eq!(error.match_window_exceeded(), None);

    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "prompt is too long: invalid tokens".to_string(),
    };
    assert_eq!(error.match_window_exceeded(), None);

    // Test the new error detection helpers
    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "The thinking feature is not supported for this model".to_string(),
    };
    assert!(error.is_thinking_related());
    assert!(!error.is_tool_related());

    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "Invalid tool schema: required property 'type' missing".to_string(),
    };
    assert!(error.is_tool_related());
    assert!(!error.is_thinking_related());

    let error = ApiError {
        error_type: "invalid_request_error".to_string(),
        message: "Model claude-3-7-sonnet-thinking-latest not found".to_string(),
    };
    assert!(error.is_model_related());
}
