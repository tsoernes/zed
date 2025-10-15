use anyhow::{Result, anyhow};
use collections::BTreeMap;
use futures::{FutureExt, StreamExt, future, future::BoxFuture};
use gpui::{AnyView, App, AsyncApp, Context, Entity, SharedString, Task, Window};
use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest};
use language_model::{
    AuthenticateError, LanguageModel, LanguageModelCompletionError, LanguageModelCompletionEvent,
    LanguageModelId, LanguageModelName, LanguageModelProvider, LanguageModelProviderId,
    LanguageModelProviderName, LanguageModelProviderState, LanguageModelRequest,
    LanguageModelToolChoice, LanguageModelToolSchemaFormat, RateLimiter,
};
use open_ai::ResponseStreamEvent;
use serde::{Deserialize, Serialize};
use settings::SettingsStore;
use std::sync::{Arc, LazyLock};
use ui::{ElevationIndex, Tooltip, prelude::*};
use ui_input::SingleLineInput;
use util::{ResultExt, truncate_and_trailoff};
use zed_env_vars::{EnvVar, env_var};

use crate::api_key::ApiKeyState;
use crate::provider::open_ai::{OpenAiEventMapper, into_open_ai};

/// Azure AI Foundry provider.
///
/// This implements an OpenAI-compatible streaming interface but authenticates
/// with the `api-key` header instead of `Authorization: Bearer`.
///
/// Note: This provider assumes the endpoint speaks the "v1 chat completions"
/// protocol and will POST to `${api_url}/chat/completions`.
const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("azure-foundry");
const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("Azure AI Foundry");

/// Default API URL when none configured via settings or environment.
/// Azure AI Foundry "Models" endpoints commonly use this base; users may need
/// to customize for their project/region.
const DEFAULT_API_URL: &str = "https://models.inference.azure.com/v1";

/// Environment variable for the API key.
const API_KEY_ENV_VAR_NAME: &str = "AZURE_FOUNDRY_API_KEY";
static API_KEY_ENV_VAR: LazyLock<EnvVar> = env_var!(API_KEY_ENV_VAR_NAME);

/// Optional environment variable to override the API base URL.
const API_URL_ENV_VAR_NAME: &str = "AZURE_FOUNDRY_API_URL";
static API_URL_ENV_VAR: LazyLock<EnvVar> = env_var!(API_URL_ENV_VAR_NAME);

/// Optional environment variable to set the default model id (e.g. "gpt-4o-mini").
const MODEL_ENV_VAR_NAME: &str = "AZURE_FOUNDRY_MODEL";
static MODEL_ENV_VAR: LazyLock<EnvVar> = env_var!(MODEL_ENV_VAR_NAME);

/// Optional environment variable to specify a deployment name for Azure OpenAI-style endpoints.
const DEPLOYMENT_ENV_VAR_NAME: &str = "AZURE_FOUNDRY_DEPLOYMENT";
static DEPLOYMENT_ENV_VAR: LazyLock<EnvVar> = env_var!(DEPLOYMENT_ENV_VAR_NAME);

/// Optional environment variable to specify an API version for Azure OpenAI-style endpoints.
const API_VERSION_ENV_VAR_NAME: &str = "AZURE_FOUNDRY_API_VERSION";
static API_VERSION_ENV_VAR: LazyLock<EnvVar> = env_var!(API_VERSION_ENV_VAR_NAME);

#[derive(Clone, Debug, PartialEq)]
pub struct AzureFoundrySettings {
    pub api_url: String,
    /// Optional deployment name for Azure OpenAI-style endpoints:
    /// {api_url}/openai/deployments/{deployment}/chat/completions?api-version={api_version}
    pub deployment_name: Option<String>,
    /// Optional API version for Azure OpenAI-style endpoints.
    pub api_version: Option<String>,
    pub available_models: Vec<AvailableModel>,
}

/// Minimal available-model specification for Azure Foundry.
///
/// We reuse a simplified shape similar to OpenAI-compatible provider models,
/// but keep capabilities explicit so we can perform feature gating.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AvailableModel {
    pub name: String,
    pub display_name: Option<String>,
    pub max_tokens: u64,
    pub max_output_tokens: Option<u64>,
    pub max_completion_tokens: Option<u64>,
    pub capabilities: ModelCapabilities,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub images: bool,
    pub parallel_tool_calls: bool,
    pub prompt_cache_key: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            tools: true,
            images: false,
            parallel_tool_calls: true,
            prompt_cache_key: true,
        }
    }
}

pub struct AzureFoundryLanguageModelProvider {
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
}

pub struct State {
    api_key_state: ApiKeyState,
    settings: AzureFoundrySettings,
}

impl State {
    fn is_authenticated(&self) -> bool {
        self.api_key_state.has_key()
    }

    fn set_api_key(&mut self, api_key: Option<String>, cx: &mut Context<Self>) -> Task<Result<()>> {
        let api_url = SharedString::new(self.settings.api_url.as_str());
        self.api_key_state
            .store(api_url, api_key, |this| &mut this.api_key_state, cx)
    }

    fn authenticate(&mut self, cx: &mut Context<Self>) -> Task<Result<(), AuthenticateError>> {
        let api_url = SharedString::new(self.settings.api_url.clone());
        self.api_key_state.load_if_needed(
            api_url,
            &API_KEY_ENV_VAR,
            |this| &mut this.api_key_state,
            cx,
        )
    }
}

impl AzureFoundryLanguageModelProvider {
    pub fn new(http_client: Arc<dyn HttpClient>, cx: &mut App) -> Self {
        // Resolve API URL defaults and available models from environment vars,
        // so users can get started without adding settings entries.
        let api_url = API_URL_ENV_VAR
            .value
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_API_URL)
            .to_string();

        // Default model: Use env override if present; otherwise a reasonable default.
        let default_model_name = MODEL_ENV_VAR
            .value
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("gpt-4o-mini")
            .to_string();

        let settings = AzureFoundrySettings {
            api_url: api_url.clone(),
            deployment_name: DEPLOYMENT_ENV_VAR
                .value
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|s| s.to_string()),
            api_version: API_VERSION_ENV_VAR
                .value
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|s| s.to_string()),
            available_models: vec![AvailableModel {
                name: default_model_name,
                display_name: None,
                max_tokens: 128_000,
                max_output_tokens: Some(16_384),
                max_completion_tokens: None,
                capabilities: ModelCapabilities::default(),
            }],
        };

        let state = cx.new(|cx| {
            cx.observe_global::<SettingsStore>(|this: &mut State, cx| {
                let new_settings = crate::AllLanguageModelSettings::get_global(cx)
                    .azure_foundry
                    .clone();
                if this.settings != new_settings {
                    let api_url_ss = SharedString::new(new_settings.api_url.as_str());
                    this.api_key_state.handle_url_change(
                        api_url_ss,
                        &API_KEY_ENV_VAR,
                        |this| &mut this.api_key_state,
                        cx,
                    );
                    this.settings = new_settings;
                    cx.notify();
                }
            })
            .detach();
            State {
                api_key_state: ApiKeyState::new(SharedString::new(api_url)),
                settings,
            }
        });

        Self { http_client, state }
    }

    fn create_language_model(&self, model: AvailableModel) -> Arc<dyn LanguageModel> {
        Arc::new(AzureFoundryLanguageModel {
            id: LanguageModelId::from(model.name.clone()),
            provider_id: PROVIDER_ID,
            provider_name: PROVIDER_NAME,
            model,
            state: self.state.clone(),
            http_client: self.http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }

    fn settings(&self, cx: &App) -> AzureFoundrySettings {
        // In the absence of dedicated settings integration, read from state.
        self.state.read(cx).settings.clone()
    }
}

impl LanguageModelProviderState for AzureFoundryLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for AzureFoundryLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn icon(&self) -> IconName {
        // No dedicated Azure icon present; reuse OpenAI-compatible icon.
        IconName::AiOpenAiCompat
    }

    fn default_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        self.settings(cx)
            .available_models
            .first()
            .map(|m| self.create_language_model(m.clone()))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        None
    }

    fn provided_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        let settings = self.settings(cx);
        // De-duplicate by name, with deterministic order.
        let mut models = BTreeMap::default();
        for model in settings.available_models {
            models.insert(model.name.clone(), model);
        }
        models
            .into_values()
            .map(|m| self.create_language_model(m))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state.update(cx, |state, cx| state.authenticate(cx))
    }

    fn configuration_view(
        &self,
        _target_agent: language_model::ConfigurationViewTargetAgent,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        cx.new(|cx| ConfigurationView::new(self.state.clone(), window, cx))
            .into()
    }

    fn reset_credentials(&self, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.set_api_key(None, cx))
    }
}

pub struct AzureFoundryLanguageModel {
    id: LanguageModelId,
    provider_id: LanguageModelProviderId,
    provider_name: LanguageModelProviderName,
    model: AvailableModel,
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

impl AzureFoundryLanguageModel {
    fn stream_completion_inner(
        &self,
        request: open_ai::Request,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<futures::stream::BoxStream<'static, Result<ResponseStreamEvent>>>>
    {
        let http_client = self.http_client.clone();

        // Read key and API URL atomically from state.
        let Ok((api_key, api_url, deployment_name, api_version)) =
            self.state.read_with(cx, |state, _cx| {
                (
                    state.api_key_state.key(&state.settings.api_url),
                    state.settings.api_url.clone(),
                    state.settings.deployment_name.clone(),
                    state.settings.api_version.clone(),
                )
            })
        else {
            return future::ready(Err(anyhow!("App state dropped"))).boxed();
        };

        let provider = self.provider_name.clone();
        let future = self.request_limiter.stream(async move {
            let Some(api_key) = api_key else {
                return Err(LanguageModelCompletionError::NoApiKey { provider });
            };
            let response = stream_completion_azure(
                http_client.as_ref(),
                &api_url,
                &api_key,
                deployment_name,
                api_version,
                request,
            )
            .await?;
            Ok(response)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }
}

impl LanguageModel for AzureFoundryLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(
            self.model
                .display_name
                .clone()
                .unwrap_or_else(|| self.model.name.clone()),
        )
    }

    fn provider_id(&self) -> LanguageModelProviderId {
        self.provider_id.clone()
    }

    fn provider_name(&self) -> LanguageModelProviderName {
        self.provider_name.clone()
    }

    fn supports_tools(&self) -> bool {
        self.model.capabilities.tools
    }

    fn tool_input_format(&self) -> LanguageModelToolSchemaFormat {
        // Azure Foundry "Models" responses API is OpenAI-compatible; use JSON Schema subset
        // when tools are supported to align with providers that constrain schema features.
        LanguageModelToolSchemaFormat::JsonSchemaSubset
    }

    fn supports_images(&self) -> bool {
        self.model.capabilities.images
    }

    fn supports_tool_choice(&self, choice: LanguageModelToolChoice) -> bool {
        match choice {
            LanguageModelToolChoice::Auto => self.model.capabilities.tools,
            LanguageModelToolChoice::Any => self.model.capabilities.tools,
            LanguageModelToolChoice::None => true,
        }
    }

    fn telemetry_id(&self) -> String {
        format!("azure_foundry/{}", self.model.name)
    }

    fn max_token_count(&self) -> u64 {
        self.model.max_tokens
    }

    fn max_output_tokens(&self) -> Option<u64> {
        self.model.max_output_tokens
    }

    fn count_tokens(
        &self,
        request: LanguageModelRequest,
        cx: &App,
    ) -> BoxFuture<'static, Result<u64>> {
        // Heuristic mapping: Use gpt-4o tokenizer for large contexts, else gpt-4.
        let max_token_count = self.max_token_count();
        cx.background_spawn(async move {
            let messages = crate::provider::open_ai::collect_tiktoken_messages(request);
            let model = if max_token_count >= 100_000 {
                "gpt-4o"
            } else {
                "gpt-4"
            };
            tiktoken_rs::num_tokens_from_messages(model, &messages).map(|tokens| tokens as u64)
        })
        .boxed()
    }

    fn stream_completion(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        let request = into_open_ai(
            request,
            &self.model.name,
            self.model.capabilities.parallel_tool_calls,
            self.model.capabilities.prompt_cache_key,
            self.max_output_tokens(),
            None,
        );
        let completions = self.stream_completion_inner(request, cx);
        async move {
            let mapper = OpenAiEventMapper::new();
            Ok(mapper.map_stream(completions.await?).boxed())
        }
        .boxed()
    }
}

struct ConfigurationView {
    api_key_editor: Entity<SingleLineInput>,
    state: Entity<State>,
    load_credentials_task: Option<Task<()>>,
}

impl ConfigurationView {
    fn new(state: Entity<State>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key_editor = cx.new(|cx| {
            SingleLineInput::new(
                window,
                cx,
                "000000000000000000000000000000000000000000000000000",
            )
        });

        cx.observe(&state, |_, _, cx| cx.notify()).detach();

        let load_credentials_task = Some(cx.spawn_in(window, {
            let state = state.clone();
            async move |this, cx| {
                if let Some(task) = state
                    .update(cx, |state, cx| state.authenticate(cx))
                    .log_err()
                {
                    let _ = task.await;
                }
                this.update(cx, |this, cx| {
                    this.load_credentials_task = None;
                    cx.notify();
                })
                .log_err();
            }
        }));

        Self {
            api_key_editor,
            state,
            load_credentials_task,
        }
    }

    fn save_api_key(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let api_key = self.api_key_editor.read(cx).text(cx).trim().to_string();
        if api_key.is_empty() {
            return;
        }

        self.api_key_editor
            .update(cx, |editor, cx| editor.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(Some(api_key), cx))?
                .await
        })
        .detach_and_log_err(cx);
    }

    fn reset_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.api_key_editor
            .update(cx, |input, cx| input.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(None, cx))?
                .await
        })
        .detach_and_log_err(cx);
    }

    fn should_render_editor(&self, cx: &Context<Self>) -> bool {
        !self.state.read(cx).is_authenticated()
    }
}

impl Render for ConfigurationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let env_var_set = state.api_key_state.is_from_env_var();

        let api_key_section = if self.should_render_editor(cx) {
            v_flex()
                .on_action(cx.listener(Self::save_api_key))
                .child(Label::new("To use Zed's agent with Azure AI Foundry, you need to add an API key. You can configure the API URL, deployment name, and API version in Settings (Language Models → Azure Foundry) or via environment variables: AZURE_FOUNDRY_API_URL, AZURE_FOUNDRY_DEPLOYMENT, AZURE_FOUNDRY_API_VERSION."))
                .child(
                    div()
                        .pt(DynamicSpacing::Base04.rems(cx))
                        .child(self.api_key_editor.clone()),
                )
                .child(
                    Label::new(format!(
                        "You can also assign the {API_KEY_ENV_VAR_NAME} environment variable and restart Zed. The default API URL is {DEFAULT_API_URL}. Optionally set AZURE_FOUNDRY_DEPLOYMENT and AZURE_FOUNDRY_API_VERSION for Azure OpenAI deployment-style endpoints."
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
                .into_any()
        } else {
            h_flex()
                .mt_1()
                .p_1()
                .justify_between()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().background)
                .child(
                    h_flex()
                        .gap_1()
                        .child(Icon::new(IconName::Check).color(Color::Success))
                        .child(Label::new(if env_var_set {
                            format!("API key set in {API_KEY_ENV_VAR_NAME} environment variable")
                        } else {
                            format!(
                                "API key configured for {}",
                                truncate_and_trailoff(&state.settings.api_url, 32)
                            )
                        })),
                )
                .child(
                    Button::new("reset-api-key", "Reset API Key")
                        .label_size(LabelSize::Small)
                        .icon(IconName::Undo)
                        .icon_size(IconSize::Small)
                        .icon_position(IconPosition::Start)
                        .layer(ElevationIndex::ModalSurface)
                        .when(env_var_set, |this| {
                            this.tooltip(Tooltip::text(format!(
                                "To reset your API key, unset the {API_KEY_ENV_VAR_NAME} environment variable."
                            )))
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.reset_api_key(window, cx))),
                )
                .into_any()
        };

        if self.load_credentials_task.is_some() {
            div().child(Label::new("Loading credentials…")).into_any()
        } else {
            v_flex().size_full().child(api_key_section).into_any()
        }
    }
}

/// Streaming request to Azure AI Foundry using `api-key` header.
///
/// This mirrors the OpenAI "chat/completions" streaming protocol, but uses
/// the `api-key` header instead of `Authorization: Bearer`.
async fn stream_completion_azure(
    client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
    deployment_name: Option<String>,
    api_version: Option<String>,
    request: open_ai::Request,
) -> Result<futures::stream::BoxStream<'static, Result<ResponseStreamEvent>>> {
    use futures::{AsyncBufReadExt, io::BufReader};
    use serde::Deserialize;

    // Compose the endpoint path; support base and Azure OpenAI deployment-style endpoints.
    let base = api_url.trim_end_matches('/');
    let uri = if let Some(deployment) = deployment_name.filter(|d| !d.is_empty()) {
        let ver = api_version.as_deref().unwrap_or("2024-06-01");
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            base, deployment, ver
        )
    } else {
        format!("{}/chat/completions", base)
    };

    let request_builder = HttpRequest::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Content-Type", "application/json")
        .header("api-key", api_key.trim());

    let request = request_builder.body(AsyncBody::from(serde_json::to_string(&request)?))?;
    let mut response = client.send(request).await?;
    if response.status().is_success() {
        let reader = BufReader::new(response.into_body());
        Ok(reader
            .lines()
            .filter_map(|line| async move {
                match line {
                    Ok(line) => {
                        let line = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"))?;
                        if line == "[DONE]" {
                            None
                        } else {
                            match serde_json::from_str::<ResponseStreamResult>(&line) {
                                Ok(ResponseStreamResult::Ok(response)) => Some(Ok(response)),
                                Ok(ResponseStreamResult::Err { error }) => {
                                    Some(Err(anyhow!(error.message)))
                                }
                                Err(error) => {
                                    log::error!(
                                        "Failed to parse Azure Foundry response into ResponseStreamResult: `{}`\nResponse: `{}`",
                                        error,
                                        line,
                                    );
                                    Some(Err(anyhow!(error)))
                                }
                            }
                        }
                    }
                    Err(error) => Some(Err(anyhow!(error))),
                }
            })
            .boxed())
    } else {
        let mut body_bytes = Vec::new();
        futures::io::AsyncReadExt::read_to_end(response.body_mut(), &mut body_bytes).await?;
        let body = String::from_utf8(body_bytes).unwrap_or_default();
        #[derive(Deserialize)]
        struct ErrorWrapper {
            error: OpenAiError,
        }
        match serde_json::from_str::<ErrorWrapper>(&body) {
            Ok(response) if !response.error.message.is_empty() => Err(anyhow!(
                "API request to {} failed: {}",
                api_url,
                response.error.message,
            )),
            _ => anyhow::bail!(
                "API request to {} failed with status {}: {}",
                api_url,
                response.status(),
                body,
            ),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum ResponseStreamResult {
    Ok(ResponseStreamEvent),
    Err { error: OpenAiError },
}

#[derive(Serialize, Deserialize, Debug)]
struct OpenAiError {
    message: String,
}
