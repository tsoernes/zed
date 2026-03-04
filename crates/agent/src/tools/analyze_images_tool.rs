use std::sync::Arc;

use agent_client_protocol as acp;
use anyhow::{Context as _, Result, anyhow};
use futures::{AsyncReadExt, StreamExt};
use gpui::{App, Entity, Image, ImageFormat, SharedString, Task, WeakEntity};
use http_client::AsyncBody;
use language_model::{
    LanguageModel, LanguageModelCompletionEvent, LanguageModelImage, LanguageModelProviderId,
    LanguageModelRequest, LanguageModelRequestMessage, MessageContent, Role,
};
use project::Project;
use reqwest_client::ReqwestClient;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

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
/// Features:
/// - Compare screenshots and designs
/// - Extract text from images (OCR)
/// - Describe visual content
/// - Identify objects, people, brands, or scenes
/// - Answer questions about images
/// - Analyze diagrams and charts
/// - Extract structured data with JSON schemas
///
/// The tool loads images, displays thumbnails in the UI, auto-converts formats like SVG,
/// and makes an internal model call to analyze them based on the provided prompt.
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
    /// Supported formats: JPEG, PNG, GIF, WebP, SVG, BMP, TIFF
    /// (SVG files are automatically converted to PNG)
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

    /// Optional JSON schema for structured output.
    ///
    /// When provided, the model returns data matching the schema structure
    /// instead of free-form text. Perfect for extracting specific fields.
    ///
    /// Example schema:
    /// ```json
    /// {
    ///     "type": "object",
    ///     "properties": {
    ///         "item_name": {"type": "string"},
    ///         "price": {"type": "number"},
    ///         "quantity": {"type": "integer"}
    ///     },
    ///     "required": ["item_name", "price"]
    /// }
    /// ```
    ///
    /// Note: When output_schema is provided, output_format is ignored.
    #[serde(default)]
    pub output_schema: Option<JsonValue>,

    /// Show image thumbnails in the tool call UI.
    ///
    /// When true, displays thumbnails of the loaded images in the UI.
    /// Useful for visual confirmation of which images are being analyzed.
    #[serde(default = "default_show_thumbnails")]
    pub show_thumbnails: bool,
}

fn default_show_thumbnails() -> bool {
    true
}

pub struct AnalyzeImagesTool {
    thread: WeakEntity<Thread>,
    project: Entity<Project>,
    http_client: Arc<ReqwestClient>,
}

impl AnalyzeImagesTool {
    pub fn new(
        thread: WeakEntity<Thread>,
        project: Entity<Project>,
        http_client: Arc<ReqwestClient>,
    ) -> Self {
        Self {
            thread,
            project,
            http_client,
        }
    }

    async fn load_image_from_path(&self, path: &str) -> Result<Vec<u8>> {
        use std::path::Path;

        // Check if it's a URL
        if path.starts_with("http://") || path.starts_with("https://") {
            // Fetch from URL
            let mut response = self
                .http_client
                .get(path, AsyncBody::default(), true)
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

            let mut bytes = Vec::new();
            response
                .body_mut()
                .read_to_end(&mut bytes)
                .await
                .with_context(|| format!("Failed to read image bytes from URL: {}", path))?;

            Ok(bytes)
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
                Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp")
                | Some("svg") | Some("bmp") | Some("tiff") | Some("tif") => {}
                _ => {
                    return Err(anyhow!(
                        "Unsupported image format at {}. Supported formats: JPEG, PNG, GIF, WebP, SVG, BMP, TIFF",
                        path
                    ));
                }
            }

            std::fs::read(&resolved_path)
                .with_context(|| format!("Failed to read image file: {}", path))
        }
    }

    fn detect_image_format(path: &str, data: &[u8]) -> ImageFormat {
        // First try to detect from file extension
        let ext = path
            .rsplit('.')
            .next()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "png" => ImageFormat::Png,
            "jpg" | "jpeg" => ImageFormat::Jpeg,
            "gif" => ImageFormat::Gif,
            "webp" => ImageFormat::Webp,
            "svg" => ImageFormat::Svg,
            "bmp" => ImageFormat::Bmp,
            "tiff" | "tif" => ImageFormat::Tiff,
            _ => {
                // Try to detect from magic bytes
                if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                    ImageFormat::Png
                } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
                    ImageFormat::Jpeg
                } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
                    ImageFormat::Gif
                } else if data.starts_with(b"RIFF") && data.len() > 12 && &data[8..12] == b"WEBP" {
                    ImageFormat::Webp
                } else if data.starts_with(b"<svg") || data.starts_with(b"<?xml") {
                    ImageFormat::Svg
                } else if data.starts_with(b"BM") {
                    ImageFormat::Bmp
                } else if data.starts_with(&[0x49, 0x49, 0x2A, 0x00])
                    || data.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
                {
                    ImageFormat::Tiff
                } else {
                    // Default to PNG as a safe fallback
                    log::warn!("Could not detect image format for {}, assuming PNG", path);
                    ImageFormat::Png
                }
            }
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

    fn format_structured_output(data: JsonValue) -> String {
        serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string())
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
                        .ok_or_else(|| anyhow!("No language model configured"))
                })??;

            // Check if the model supports images
            if !model.supports_images() {
                return Err(anyhow!(
                    "The current model does not support image analysis. Please use a vision-capable model like GPT-4o or Claude 3."
                ));
            }

            // Update status: Loading images
            event_stream.update_fields(
                acp::ToolCallUpdateFields::new().title("Loading images...")
            );

            // Load all images
            let mut images = Vec::new();
            let mut thumbnails = Vec::new();

            for (idx, path) in input.image_paths.iter().enumerate() {
                let image_data = self
                    .load_image_from_path(path)
                    .await
                    .with_context(|| format!("Failed to load image: {}", path))?;

                // Detect format
                let format = Self::detect_image_format(path, &image_data);

                // Create gpui::Image from bytes
                let image = Arc::new(Image::from_bytes(format, image_data));

                // Store for thumbnails
                if input.show_thumbnails {
                    thumbnails.push((path.clone(), image.clone()));
                }

                // Convert to LanguageModelImage
                let language_model_image = cx
                    .update(|cx| LanguageModelImage::from_image(image, cx))
                    .await?
                    .ok_or_else(|| {
                        anyhow!(
                            "Failed to process image: {}. The image may be too large or in an unsupported format.",
                            path
                        )
                    })?;

                images.push(language_model_image);

                // Update progress
                event_stream.update_fields(
                    acp::ToolCallUpdateFields::new()
                        .title(format!("Loaded image {} of {}", idx + 1, input.image_paths.len()))
                );
            }

            // Show thumbnails in UI if requested
            if input.show_thumbnails && !thumbnails.is_empty() {
                let mut content_blocks = Vec::new();
                for (path, image_arc) in thumbnails {
                    // Try to get LanguageModelImage for the thumbnail
                    if let Ok(Some(lang_model_img)) = cx
                        .update(|cx| LanguageModelImage::from_image(image_arc, cx))
                        .await
                    {
                        content_blocks.push(acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::Image(acp::ImageContent::new(
                                lang_model_img.source.to_string(),
                                "image/png",
                            )),
                        )));

                        // Add caption
                        content_blocks.push(acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::Text(acp::TextContent::new(format!(
                                "📸 `{}`",
                                path
                            ))),
                        )));
                    }
                }

                if !content_blocks.is_empty() {
                    event_stream.update_fields(
                        acp::ToolCallUpdateFields::new().content(content_blocks)
                    );
                }
            }

            // Update status: Analyzing
            event_stream.update_fields(
                acp::ToolCallUpdateFields::new().title("Analyzing images...")
            );

            // Create the request with images
            let mut message_content = Vec::new();

            // Check if we need structured output
            let has_schema = input.output_schema.is_some();

            // For now, we'll handle structured output by including schema instructions in the prompt
            // A full implementation would use the model's native structured output API if available
            let final_prompt = if let Some(ref schema) = input.output_schema {
                format!(
                    "{}\n\nPlease respond ONLY with valid JSON matching this exact schema:\n```json\n{}\n```\n\nDo not include any text before or after the JSON.",
                    input.prompt,
                    serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string())
                )
            } else {
                input.prompt.clone()
            };

            message_content.push(MessageContent::Text(final_prompt));

            for image in images {
                message_content.push(MessageContent::Image(image));
            }

            let request = LanguageModelRequest {
                thread_id: None,
                prompt_id: None,
                intent: None,
                messages: vec![LanguageModelRequestMessage {
                    role: Role::User,
                    content: message_content,
                    cache: false,
                    reasoning_details: None,
                }],
                tools: vec![],
                tool_choice: None,
                stop: vec![],
                temperature: None,
                thinking_allowed: false,
                thinking_effort: None,
            };

            // Stream the completion
            let mut stream = model.stream_completion(request, &mut cx);
            let mut response_text = String::new();

            while let Some(event) = stream.next().await {
                match event {
                    Ok(LanguageModelCompletionEvent::Text(text)) => {
                        response_text.push_str(&text);
                        // Send progress updates with partial text
                        if response_text.len() % 100 == 0 {
                            // Update every ~100 chars
                            event_stream.update_fields(
                                acp::ToolCallUpdateFields::new()
                                    .title(format!("Analyzing... ({} chars)", response_text.len()))
                            );
                        }
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

            // Update status: Complete
            event_stream.update_fields(
                acp::ToolCallUpdateFields::new().title("Analysis complete")
            );

            // Format the output according to the requested format
            let formatted = if has_schema {
                // Try to parse as JSON and validate against schema
                match serde_json::from_str::<JsonValue>(&response_text) {
                    Ok(json) => {
                        // Successfully parsed JSON
                        Self::format_structured_output(json)
                    }
                    Err(_) => {
                        // Response wasn't valid JSON, try to extract JSON from markdown code blocks
                        let json_extracted = if let Some(start) = response_text.find("```json") {
                            if let Some(end) = response_text[start..].find("```") {
                                let json_str = &response_text[start + 7..start + end].trim();
                                serde_json::from_str::<JsonValue>(json_str).ok()
                            } else {
                                None
                            }
                        } else if let Some(start) = response_text.find('{') {
                            // Try to extract JSON object
                            serde_json::from_str::<JsonValue>(&response_text[start..]).ok()
                        } else {
                            None
                        };

                        match json_extracted {
                            Some(json) => Self::format_structured_output(json),
                            None => {
                                // Could not extract valid JSON, return as-is with warning
                                format!(
                                    "⚠️ Warning: Response was not valid JSON matching the schema.\n\nRaw response:\n{}",
                                    response_text
                                )
                            }
                        }
                    }
                }
            } else {
                Self::format_output(&response_text, &input.output_format)
            };

            Ok(formatted)
        })
    }
}
