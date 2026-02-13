use std::sync::Arc;

use agent_client_protocol as acp;
use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt;
use gpui::{App, Entity, SharedString, Task, WeakEntity};
use language_model::{
    LanguageModel, LanguageModelCompletionEvent, LanguageModelImage, LanguageModelProviderId,
    LanguageModelRequest, LanguageModelRequestMessage, LanguageModelToolResultContent,
    MessageContent, Role,
};
use project::Project;
use reqwest_client::ReqwestClient;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, Thread, ToolCallEventStream};

/// Output format for image analysis results
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Plain text analysis
    Text,
    /// Rich content with markdown formatting
    Markdown,
    /// Structured JSON output
    Json,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Markdown
    }
}

/// Analyze images using vision-capable models (GitHub Copilot).
///
/// This tool enables the agent to analyze images from local files or URLs using
/// vision-capable language models. Supports comparing multiple images, extracting
/// text (OCR), describing content, and answering questions about visual information.
///
/// Available only when GitHub Copilot is the active language model provider.
///
/// Common use cases:
/// - Compare two or more screenshots or designs
/// - Extract text from images (OCR)
/// - Describe image content in detail
/// - Identify objects, people, brands, or scenes
/// - Answer specific questions about image content
/// - Analyze diagrams, charts, or infographics
///
/// The tool loads images, encodes them properly, and makes an internal model call
/// to analyze them based on the provided prompt.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeImagesInput {
    /// The question or instruction for analyzing the image(s).
    ///
    /// Be specific about what you want to know or what aspects to analyze.
    ///
    /// Examples:
    /// - "What are the differences between these two screenshots?"
    /// - "Describe the UI elements visible in this design"
    /// - "Extract all text from this document"
    /// - "What brand logos are visible?"
    /// - "Analyze the data shown in this chart"
    pub prompt: String,

    /// Paths to images to analyze.
    ///
    /// Can be:
    /// - Absolute paths: "/home/user/screenshots/image.png"
    /// - Relative paths: "screenshots/image.png" (relative to current directory)
    /// - URLs: "https://example.com/image.jpg"
    /// - Home directory paths: "~/Pictures/photo.png"
    ///
    /// Supported formats: JPEG, PNG, GIF, WebP
    ///
    /// Multiple images can be provided for comparison or multi-image analysis.
    pub image_paths: Vec<String>,

    /// Output format for the analysis results.
    ///
    /// - "text": Plain text analysis
    /// - "markdown": Rich content with markdown formatting (default)
    /// - "json": Structured JSON output
    #[serde(default)]
    pub output_format: OutputFormat,
}

pub struct AnalyzeImagesTool {
    thread: WeakEntity<Thread>,
    project: Entity<Project>,
    http_client: Arc<ReqwestClient>,
}

impl AnalyzeImagesTool {
    pub fn new(thread: WeakEntity<Thread>, project: Entity<Project>, http_client: Arc<ReqwestClient>) -> Self {
        Self { thread, project, http_client }
    }

    async fn load_image_from_path(&self, path: &str) -> Result<Vec<u8>> {
        use std::path::Path;

        // Check if it's a URL
        if path.starts_with("http://") || path.starts_with("https://") {
            // Fetch from URL
            let response = self.http_client
                .get(path, Default::default(), true)
                .await
                .with_context(|| format!("Failed to fetch image from URL: {}", path))?;

            if !response.status().is_success() {
                return Err(anyhow!(
                    "URL returned status {}: {}. Please check that the URL is correct and accessible.",
                    response.status(),
                    path
                ));
            }

            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok());

            if let Some(ct) = content_type {
                if !ct.starts_with("image/") {
                    log::warn!(
                        "URL does not appear to be an image (content-type: {}): {}",
                        ct,
                        path
                    );
                }
            }

            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("Failed to read image bytes from URL: {}", path))?;

            Ok(bytes.to_vec())
        } else {
            // Load from local file
            let path_obj = Path::new(path);
            let resolved_path = if path_obj.is_absolute() {
                path_obj.to_path_buf()
            } else if path.starts_with("~/") {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .context("Could not determine home directory")?;
                Path::new(&home).join(&path[2..])
            } else {
                // Relative path - resolve against current directory
                std::env::current_dir()
                    .context("Could not get current directory")?
                    .join(path_obj)
            };

            if !resolved_path.exists() {
                return Err(anyhow!(
                    "Image file not found: {}.\nResolved to: {}\nPlease check that the file exists and the path is correct.",
                    path,
                    resolved_path.display()
                ));
            }

            if !resolved_path.is_file() {
                return Err(anyhow!(
                    "Path is not a file: {}. Please provide a path to an image file.",
                    path
                ));
            }

            // Validate it's an image by checking extension
            let ext = resolved_path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase());

            match ext.as_deref() {
                Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp") => {}
                _ => {
                    return Err(anyhow!(
                        "Unsupported image format at {}. Supported formats: JPEG, PNG, GIF, WebP",
                        path
                    ));
                }
            }

            std::fs::read(&resolved_path)
                .with_context(|| format!("Failed to read image file: {}", path))
        }
    }

    fn format_output(analysis: &str, format: &OutputFormat) -> String {
        match format {
            OutputFormat::Text => analysis.to_string(),
            OutputFormat::Markdown => {
                // Ensure proper markdown formatting
                if analysis.trim().starts_with('#') {
                    analysis.to_string()
                } else {
                    format!("# Image Analysis\n\n{}", analysis)
                }
            }
            OutputFormat::Json => {
                // Wrap in JSON structure
                serde_json::json!({
                    "analysis": analysis,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
                .to_string()
            }
        }
    }
}

impl AgentTool for AnalyzeImagesTool {
    type Input = AnalyzeImagesInput;
    type Output = String;

    const NAME: &'static str = "analyze_images";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => {
                let count = input.image_paths.len();
                if count == 1 {
                    "Analyzing image".into()
                } else {
                    format!("Analyzing {} images", count).into()
                }
            }
            Err(_) => "Analyze images".into(),
        }
    }

    fn supports_provider(provider: &LanguageModelProviderId) -> bool {
        // Only available for GitHub Copilot
        provider.0.as_ref() == "copilot_chat"
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        cx.spawn(async move |mut cx| {
            // Validate inputs
            if input.prompt.trim().is_empty() {
                return Err(anyhow!(
                    "Prompt cannot be empty. Please provide a question or instruction for analyzing the image(s)."
                ));
            }

            if input.image_paths.is_empty() {
                return Err(anyhow!(
                    "At least one image path is required. Provide local file paths or URLs."
                ));
            }

            // Get the model from the thread
            let model = cx
                .update(|cx| {
                    self.thread
                        .upgrade()
                        .and_then(|thread| thread.read(cx).model().cloned())
                })?
                .ok_or_else(|| anyhow!("No language model configured"))?;

            // Check if the model supports images
            if !model.supports_images() {
                return Err(anyhow!(
                    "The current model does not support image analysis. Please use a vision-capable model like GPT-4o or Claude 3."
                ));
            }

            event_stream
                .send(acp::ToolCallUpdate::progress(
                    "Loading images...",
                    Some(acp::ToolProgress {
                        total: Some(input.image_paths.len() as u64),
                        current: 0,
                    }),
                ))
                .await;

            // Load all images
            let mut images = Vec::new();
            for (idx, path) in input.image_paths.iter().enumerate() {
                let image_data = self.load_image_from_path(path)
                    .await
                    .with_context(|| format!("Failed to load image: {}", path))?;

                // Create gpui::Image from bytes
                let format = if path.ends_with(".png") {
                    gpui::ImageFormat::Png
                } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
                    gpui::ImageFormat::Jpeg
                } else if path.ends_with(".gif") {
                    gpui::ImageFormat::Gif
                } else if path.ends_with(".webp") {
                    gpui::ImageFormat::Webp
                } else {
                    // Default to PNG
                    gpui::ImageFormat::Png
                };

                let image = Arc::new(gpui::Image::from_bytes(format, image_data));

                // Convert to LanguageModelImage
                let language_model_image = cx
                    .update(|cx| LanguageModelImage::from_image(image, cx))?
                    .await
                    .ok_or_else(|| anyhow!("Failed to process image: {}", path))?;

                images.push(language_model_image);

                event_stream
                    .send(acp::ToolCallUpdate::progress(
                        format!("Loaded image {} of {}", idx + 1, input.image_paths.len()),
                        Some(acp::ToolProgress {
                            total: Some(input.image_paths.len() as u64),
                            current: (idx + 1) as u64,
                        }),
                    ))
                    .await;
            }

            event_stream
                .send(acp::ToolCallUpdate::progress("Analyzing images...", None))
                .await;

            // Create the request with images
            let mut message_content = vec![MessageContent::Text {
                text: input.prompt.clone(),
            }];

            for image in images {
                message_content.push(MessageContent::Image(image));
            }

            let request = LanguageModelRequest {
                messages: vec![LanguageModelRequestMessage {
                    role: Role::User,
                    content: message_content,
                    cache: false,
                }],
                tools: vec![],
                stop: vec![],
                temperature: None,
            };

            // Stream the completion
            let mut stream = model.stream_completion(request, &mut cx);
            let mut response_text = String::new();

            while let Some(event) = stream.next().await {
                match event {
                    Ok(LanguageModelCompletionEvent::Text(text)) => {
                        response_text.push_str(&text);
                        // Send progress updates with partial text
                        event_stream
                            .send(acp::ToolCallUpdate::progress(
                                format!("Analyzing... ({} chars)", response_text.len()),
                                None,
                            ))
                            .await;
                    }
                    Ok(LanguageModelCompletionEvent::Stop(_)) => break,
                    Err(e) => {
                        return Err(anyhow!("Error during image analysis: {}", e));
                    }
                    _ => {}
                }
            }

            if response_text.is_empty() {
                return Err(anyhow!(
                    "Model did not return any analysis. This may indicate a problem with the model or the images."
                ));
            }

            event_stream
                .send(acp::ToolCallUpdate::progress("Analysis complete", None))
                .await;

            // Format the output according to the requested format
            let formatted = Self::format_output(&response_text, &input.output_format);

            Ok(formatted)
        })
    }
}
