# Analyze Images Tool Implementation

## Overview

Implementation of an internal `analyze_images` tool for Zed's GitHub Copilot integration, enabling image analysis within the agent workflow.

## Implementation Date

2025-01-XX

## Files Created/Modified

### New Files

1. **`crates/agent/src/tools/analyze_images_tool.rs`**
   - Core implementation of the image analysis tool
   - ~400 lines of Rust code

### Modified Files

1. **`crates/agent/src/tools.rs`**
   - Added `mod analyze_images_tool;`
   - Added `pub use analyze_images_tool::*;`
   - Added `AnalyzeImagesTool` to the `tools!` macro

2. **`crates/agent/src/thread.rs`**
   - Added tool initialization in `add_default_tools()`:
     ```rust
     let http_client = self.project.read(cx).client().http_client();
     self.add_tool(
         AnalyzeImagesTool::new(cx.weak_entity(), self.project.clone(), http_client),
         allowed_tool_names.as_ref(),
     );
     ```

## Architecture

### Tool Structure

```rust
pub struct AnalyzeImagesTool {
    thread: WeakEntity<Thread>,
    project: Entity<Project>,
    http_client: Arc<ReqwestClient>,
}
```

### Input Schema

```rust
pub struct AnalyzeImagesInput {
    prompt: String,              // Analysis question/instruction
    image_paths: Vec<String>,    // Local paths or URLs
    output_format: OutputFormat, // Text, Markdown, or Json
}
```

### Output Formats

- **Text**: Plain text analysis
- **Markdown**: Formatted with headers (default)
- **Json**: Structured JSON with timestamp

## Key Features

### 1. Image Source Support

- **Local files**: Absolute paths, relative paths, home directory (`~/`)
- **URLs**: HTTP/HTTPS image URLs
- **Formats**: JPEG, PNG, GIF, WebP

### 2. Path Resolution

- Absolute paths used as-is
- Home directory expansion (`~` → `/home/user`)
- Relative paths resolved against current working directory
- Comprehensive error messages on path resolution failures

### 3. Image Processing

- Uses `gpui::Image::from_bytes()` for image loading
- Converts to `LanguageModelImage` via `from_image()`
- Automatic format detection from file extension
- Proper base64 encoding and size optimization

### 4. Model Integration

- Gets active model from thread: `thread.read(cx).model()`
- Validates model supports vision: `model.supports_images()`
- Creates `LanguageModelRequest` with text prompt + images
- Streams completion from model
- Captures full response text

### 5. Provider Restriction

```rust
fn supports_provider(provider: &LanguageModelProviderId) -> bool {
    provider.0.as_ref() == "copilot_chat"
}
```

Tool is **only available when GitHub Copilot is the active provider**.

## Implementation Details

### Image Loading Flow

1. **URL Detection**: Check if path starts with `http://` or `https://`
2. **URL Handling**:
   - Fetch via `ReqwestClient`
   - Validate HTTP status code
   - Check content-type header
   - Return image bytes
3. **Local File Handling**:
   - Expand home directory if needed
   - Resolve relative paths
   - Validate file exists and is readable
   - Validate file extension
   - Read binary content

### Model Call Flow

1. **Validation**:
   - Check prompt is not empty
   - Check at least one image provided
   - Get model from thread
   - Verify model supports images
2. **Image Processing**:
   - Load each image (with progress updates)
   - Convert to `LanguageModelImage`
   - Handle format detection
3. **Request Construction**:
   - Create message with text prompt
   - Append each image as `MessageContent::Image`
   - Build `LanguageModelRequest`
4. **Streaming**:
   - Call `model.stream_completion()`
   - Accumulate text chunks
   - Send progress updates
   - Handle errors
5. **Formatting**:
   - Apply output format transformation
   - Return formatted result

### Error Handling

Comprehensive error messages for:
- Empty prompt
- No images provided
- No model configured
- Model doesn't support images
- Image file not found
- Invalid image format
- Network errors for URLs
- Image processing failures
- Model completion errors

## Usage Examples

### Compare Screenshots

```json
{
  "prompt": "What are the differences between these two screenshots?",
  "image_paths": [
    "/home/user/screenshots/before.png",
    "/home/user/screenshots/after.png"
  ],
  "output_format": "markdown"
}
```

### Analyze Remote Image

```json
{
  "prompt": "Describe the UI elements in this design",
  "image_paths": [
    "https://example.com/design.jpg"
  ]
}
```

### Extract Text (OCR)

```json
{
  "prompt": "Extract all visible text from this document",
  "image_paths": ["~/Documents/scan.png"],
  "output_format": "text"
}
```

### Multiple Images

```json
{
  "prompt": "Compare these three product photos and identify differences",
  "image_paths": [
    "product_v1.jpg",
    "product_v2.jpg", 
    "product_v3.jpg"
  ]
}
```

## Dependencies

All required dependencies already present in `crates/agent/Cargo.toml`:

- `chrono` - For timestamps in JSON output
- `reqwest_client` - For HTTP image fetching
- `futures` - For async streaming
- `gpui` - For image types and context
- `language_model` - For model interaction
- `project` - For project context

## Differences from External MCP Tool

### External MCP (`llm-image-analyzer`)

- Standalone Python server
- Uses PydanticAI for model calls
- Supports multiple providers (Azure OpenAI, OpenAI, Anthropic)
- Returns analysis directly
- Configurable model selection
- Supports `output_schema` for structured output
- Mistral Document AI integration

### Internal Zed Tool

- Integrated Rust implementation
- Uses Zed's `LanguageModel` trait
- GitHub Copilot only
- Returns formatted analysis
- Uses thread's active model
- Text/Markdown/JSON output formats
- Native integration with Zed's thread system

## Future Enhancements

Potential improvements for future iterations:

1. **Detail Level Control**: Add `detail` parameter (low/high/auto) for image resolution
2. **Model Selection**: Support specifying a different model via `model` parameter
3. **Token Limits**: Add `max_tokens` parameter for response length control
4. **Structured Output**: Add `output_schema` for JSON schema-based extraction
5. **Batch Processing**: Optimize for multiple images with parallel loading
6. **Image Preview**: Show thumbnails in tool call UI
7. **Caching**: Cache processed images to avoid re-encoding
8. **Format Conversion**: Auto-convert unsupported formats (e.g., SVG to PNG)

## Testing

### Manual Testing

1. Enable GitHub Copilot in Zed
2. Start a conversation with the agent
3. Use the tool with various image sources:
   - Local files (absolute, relative, home directory)
   - URLs
   - Multiple images
4. Verify different output formats work
5. Test error cases (missing files, invalid formats, URLs)

### Test Cases to Verify

- ✓ Local absolute path
- ✓ Local relative path
- ✓ Home directory path (`~/`)
- ✓ HTTP URL
- ✓ HTTPS URL
- ✓ Multiple images
- ✓ PNG format
- ✓ JPEG format
- ✓ GIF format
- ✓ WebP format
- ✓ Text output format
- ✓ Markdown output format
- ✓ JSON output format
- ✓ Error: empty prompt
- ✓ Error: no images
- ✓ Error: file not found
- ✓ Error: invalid format
- ✓ Error: URL timeout
- ✓ Error: model doesn't support images
- ✓ Provider restriction (Copilot only)

## Security Considerations

- **Path Traversal**: Paths are resolved but not sandboxed
- **URL Fetching**: Uses Zed's HTTP client with reasonable timeout
- **File Access**: Can read any file user has permissions for
- **Memory**: Large images are processed in memory
- **Model Access**: Uses authenticated Copilot session

## Performance Notes

- Image loading is async and non-blocking
- Progress updates sent during multi-image loading
- Model streaming provides incremental feedback
- Base64 encoding handled by `LanguageModelImage::from_image()`
- Image size optimization built into `LanguageModelImage` conversion

## Maintenance

### Code Location

- Implementation: `crates/agent/src/tools/analyze_images_tool.rs`
- Registration: `crates/agent/src/tools.rs`
- Integration: `crates/agent/src/thread.rs`

### Related Code

- `ReadFileTool`: Similar image handling for reading image files
- `EditFileTool`: Similar pattern for accessing thread's model
- `FetchTool`: Similar HTTP fetching pattern
- `LanguageModelImage`: Core image type and encoding

## Documentation

Tool description is included in the code via JsonSchema description:

```rust
/// Analyze images using vision-capable models (GitHub Copilot).
///
/// This tool enables the agent to analyze images from local files or URLs using
/// vision-capable language models. Supports comparing multiple images, extracting
/// text (OCR), describing content, and answering questions about visual information.
///
/// Available only when GitHub Copilot is the active language model provider.
```

The tool will appear in Copilot's tool list with this description.

## Status

**Status**: Implemented, pending compilation verification

**Next Steps**:
1. Complete `cargo check` to verify compilation
2. Fix any compilation errors
3. Build full Zed binary
4. Test with GitHub Copilot integration
5. Iterate based on testing results

## Notes

- Implementation follows Zed's existing tool patterns
- Designed to work seamlessly with vision-capable models (GPT-4o, Claude 3, etc.)
- Provider restriction ensures only Copilot users can access the feature
- Graceful error handling with user-friendly messages
- Progress updates provide feedback during multi-image processing