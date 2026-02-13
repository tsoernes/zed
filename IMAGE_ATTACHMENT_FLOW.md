# Image Attachment Flow in GitHub Copilot Chat Extension

This document traces the complete flow of how images are sent to GitHub Copilot, from user interaction (paste/drag) to network transmission.

## Overview

Images can be attached to chat requests through:
1. **Paste** events (`Ctrl+V` / `Cmd+V`)
2. **Drag and drop** events from file explorer or external sources
3. **Variable references** (e.g., `#file:image.png`)

The flow involves several layers:
- **VS Code UI Layer**: Handles paste/drag events and creates attachments
- **Extension API Layer**: Receives `ChatRequest` with binary references
- **Prompt Layer**: Renders images into prompt messages
- **Network Layer**: Converts to OpenAI/Anthropic format and sends to API

---

## 1. UI Layer: Paste/Drag Event Detection

### Paste Events
VS Code's chat input handles paste events and creates `DataTransfer` objects containing image data.

**Key Interfaces** (`src/extension/vscode.d.ts`):
```typescript
export class DataTransfer implements Iterable<[mimeType: string, item: DataTransferItem]> {
    get(mimeType: string): DataTransferItem | undefined;
    // Supports mime types like 'image/png', 'image/jpeg', 'image/webp', etc.
}

export interface DataTransferFile {
    readonly name: string;
    // The actual file data
}

export class DataTransferItem {
    asFile(): DataTransferFile | undefined;
    readonly value: any;
}
```

### Drag and Drop Events
Similar handling using `DataTransfer` API with `onDrop` handlers.

**Example from fixtures** (`src/extension/prompts/node/test/fixtures/codeEditorWidget.ts:370-386`):
```typescript
onDrop: async e => {
    if (!isDropIntoEnabled()) {
        return;
    }
    this.removeDropIndicator();
    if (!e.dataTransfer) {
        return;
    }
    // ... processes DataTransfer for images
}
```

---

## 2. Extension API Layer: ChatReferenceBinaryData

When a user attaches an image, VS Code creates a `ChatReferenceBinaryData` object and includes it in the `ChatRequest.references` array.

### Type Definitions

**ChatPromptReference** (`src/extension/vscode.d.ts`):
```typescript
export interface ChatPromptReference {
    readonly id: string;
    readonly name: string;
    readonly value: string | Uri | Location | ChatReferenceBinaryData | unknown;
    readonly range?: [start: number, end: number];
}
```

**ChatReferenceBinaryData** (`src/extension/vscode.proposed.chatBinaryReferenceData.d.ts`):
```typescript
export class ChatReferenceBinaryData {
    readonly mimeType: string;
    
    /**
     * Retrieves the binary data of the reference.
     * This is primarily used to receive image attachments from the chat.
     */
    data(): Thenable<Uint8Array>;
    
    /**
     * Retrieves a URI reference to the binary data, if available.
     */
    readonly reference?: Uri;
    
    constructor(mimeType: string, data: () => Thenable<Uint8Array>);
}
```

### Chat Request Processing

**ChatParticipantRequestHandler** (`src/extension/prompt/node/chatParticipantRequestHandler.ts:75-92`):
```typescript
export class ChatParticipantRequestHandler {
    constructor(
        private readonly rawHistory: ReadonlyArray<ChatRequestTurn | ChatResponseTurn>,
        private request: ChatRequest, // Contains references with ChatReferenceBinaryData
        stream: ChatResponseStream,
        // ...
    ) {
        // The request.references array contains ChatPromptReference objects
        // which may have ChatReferenceBinaryData as their value
    }
}
```

---

## 3. Prompt Layer: Image Rendering

Images are processed and rendered into the prompt using the `@vscode/prompt-tsx` library.

### ChatVariablesCollection

**Processing References** (`src/extension/prompt/common/chatVariablesCollection.ts`):
```typescript
export interface PromptVariable {
    readonly reference: vscode.ChatPromptReference;
    readonly originalName: string;
    readonly uniqueName: string;
    readonly value: string | vscode.Uri | vscode.Location | unknown;
    // value can be ChatReferenceBinaryData
}

export class ChatVariablesCollection {
    constructor(
        private readonly _source: readonly vscode.ChatPromptReference[] = []
    ) { }
    
    // Provides iteration over variables including images
    public *[Symbol.iterator](): IterableIterator<PromptVariable>
}
```

### Image Component

**Main Image Component** (`src/extension/prompts/node/panel/image.tsx`):

```typescript
export interface ImageProps extends BasePromptElementProps {
    variableName: string;
    variableValue: Uint8Array | Promise<Uint8Array>;
    omitReferences?: boolean;
    reference?: Uri;
}

export class Image extends PromptElement<ImageProps, unknown> {
    override async render(_state: unknown, sizing: PromptSizing) {
        // 1. Check if model supports vision
        if (!this.promptEndpoint.supportsVision || 
            !this.authService.copilotToken?.isEditorPreviewFeaturesEnabled()) {
            // Omit image or show as reference only
            return <references value={[...]} />;
        }
        
        // 2. Get binary data
        const variable = await this.props.variableValue;
        let imageSource = Buffer.from(variable).toString('base64');
        let imageMimeType: string | undefined = undefined;
        
        // 3. Upload to GitHub (for compatible models/endpoints)
        const isChatCompletions = /* check if using ChatCompletions API */;
        const enabled = this.configurationService.getExperimentBasedConfig(
            ConfigKey.EnableChatImageUpload, 
            this.experimentationService
        );
        
        if (isChatCompletions && enabled && modelCanUseImageURL(this.promptEndpoint)) {
            try {
                const githubToken = (await this.authService.getGitHubSession(
                    'any', 
                    { silent: true }
                ))?.accessToken;
                const mimeType = getMimeType(imageSource) ?? imageMimeType;
                
                // Upload to GitHub's image service
                const uri = await this.imageService.uploadChatImageAttachment(
                    variable, 
                    this.props.variableName, 
                    mimeType, 
                    githubToken
                );
                
                if (uri) {
                    imageSource = uri.toString(); // Use URL instead of base64
                    imageMimeType = mimeType;
                }
            } catch (error) {
                this.logService.warn(`Image upload failed, using base64 fallback: ${error}`);
            }
        }
        
        // 4. Render as BaseImage (from @vscode/prompt-tsx)
        return (
            <UserMessage priority={0}>
                <BaseImage 
                    src={imageSource}  // Either base64 or URL
                    detail='high' 
                    mimeType={imageMimeType} 
                />
                {this.props.reference && (
                    <references value={[...]} />
                )}
            </UserMessage>
        );
    }
}
```

### Historical Image (from Conversation History)

**HistoricalImage Component** (`src/extension/prompts/node/panel/image.tsx:44-63`):
```typescript
export interface HistoricalImageProps extends BasePromptElementProps {
    src: string;          // Already processed (base64 or URL)
    detail?: 'auto' | 'low' | 'high';
    mimeType?: string;
}

export class HistoricalImage extends PromptElement<HistoricalImageProps, unknown> {
    override async render(_state: unknown, sizing: PromptSizing) {
        // Check if model supports vision
        if (!this.promptEndpoint.supportsVision || 
            !this.authService.copilotToken?.isEditorPreviewFeaturesEnabled()) {
            return undefined; // Omit from history
        }
        
        return <BaseImage 
            src={this.props.src} 
            detail={this.props.detail} 
            mimeType={this.props.mimeType} 
        />;
    }
}
```

### Usage in Chat Variables

**Rendering Chat Variables** (`src/extension/prompts/node/panel/chatVariables.tsx:230-231`):
```typescript
async function renderChatVariables(...) {
    // ... iterate over variables
    
    if (variableValue instanceof ChatReferenceBinaryData) {
        elements.push(
            <Image 
                variableName={variableName} 
                variableValue={await variableValue.data()} 
                reference={variableValue.reference} 
                omitReferences={omitReferences}
            />
        );
    }
    
    // ... handle other variable types
}
```

### Agent Prompt Integration

**AgentPrompt** (`src/extension/prompts/node/agent/agentPrompt.tsx`):
```typescript
import { HistoricalImage } from '../panel/image';

// When rendering historical messages:
function renderMessageParts(message, enableCacheBreakpoints) {
    return message.map(part => {
        if (part.type === Raw.ChatCompletionContentPartKind.Text) {
            return part.text;
        } else if (part.type === Raw.ChatCompletionContentPartKind.Image) {
            return <HistoricalImage 
                src={part.imageUrl.url} 
                detail={part.imageUrl.detail} 
                mimeType={part.imageUrl.mediaType} 
            />;
        }
        // ...
    }).filter(isDefined);
}
```

---

## 4. Network Layer: OpenAI/Anthropic Format

The prompt-tsx library converts the rendered prompt into API-specific formats.

### OpenAI Format Conversion

**CAPI Message Format** (`src/platform/networking/common/openai.ts:98-184`):
```typescript
export type CAPIChatMessage = OpenAI.ChatMessage & {
    copilot_references?: ICopilotReference[];
    copilot_confirmations?: { state: string; confirmation: any }[];
    copilot_cache_control?: { 'type': 'ephemeral' };
};

export function rawMessageToCAPI(
    message: Raw.ChatMessage, 
    callback?: RawMessageConversionCallback
): CAPIChatMessage {
    const out: CAPIChatMessage = toMode(OutputMode.OpenAI, message);
    
    // Handle string content
    if (typeof out.content === 'string') {
        out.content = out.content.trimEnd();
    } else {
        // Handle content parts (text, images, etc.)
        for (let i = 0; i < out.content.length; i++) {
            const part = out.content[i];
            
            if (part.type === 'text') {
                part.text = part.text.trimEnd();
            } else if (part.type === 'image_url' && 
                       Array.isArray(message.content) && 
                       i < message.content.length) {
                const rawPart = message.content[i] as Raw.ChatCompletionContentPart;
                
                if (rawPart?.type === Raw.ChatCompletionContentPartKind.Image && 
                    rawPart.imageUrl?.mediaType) {
                    // CAPI expects `media_type` instead of `mediaType`
                    const { mediaType, ...rawImageUrl } = rawPart.imageUrl;
                    (part.image_url as ChatCompletionContentPartImage.ImageURL & { 
                        media_type: string 
                    }) = {
                        ...rawImageUrl,
                        media_type: mediaType
                    };
                }
            }
        }
    }
    
    return out;
}
```

### OpenAI API Structure

The final message sent to OpenAI looks like:

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "What's in this image?"
    },
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANS...",
        "detail": "high",
        "media_type": "image/png"
      }
    }
  ]
}
```

Or with uploaded image URL:

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "What's in this image?"
    },
    {
      "type": "image_url",
      "image_url": {
        "url": "https://github-production-user-uploads.s3.amazonaws.com/...",
        "detail": "high",
        "media_type": "image/png"
      }
    }
  ]
}
```

### Anthropic (Claude) Format

**Claude Agent Integration** (`src/extension/agents/claude/node/claudeCodeAgent.ts:139-149`):
```typescript
for (const ref of request.references) {
    let refValue = ref.value;
    
    if (refValue instanceof ChatReferenceBinaryData) {
        const mediaType = toAnthropicImageMediaType(refValue.mimeType);
        
        if (mediaType) {
            const data = await refValue.data();
            contentBlocks.push({
                type: 'image',
                source: {
                    type: 'base64',
                    data: Buffer.from(data).toString('base64'),
                    media_type: mediaType // e.g., 'image/jpeg', 'image/png', 'image/webp'
                }
            });
        }
    }
}
```

Anthropic message structure:

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "What's in this image?"
    },
    {
      "type": "image",
      "source": {
        "type": "base64",
        "media_type": "image/png",
        "data": "iVBORw0KGgoAAAANS..."
      }
    }
  ]
}
```

### Copilot CLI (Agent Mode)

**Copilot CLI Prompt Resolver** (`src/extension/agents/copilotcli/node/copilotcliPromptResolver.ts:67-73`):
```typescript
// Images will be attached using regular attachments via Copilot CLI SDK
if (variableRef.value instanceof ChatReferenceBinaryData) {
    if (!isImageMimeType(variableRef.value.mimeType)) {
        validReferences.push(variableRef);
    }
    fileFolderReferences.push(variableRef);
    return;
}
```

**Image Attachment Construction** (`src/extension/agents/copilotcli/node/copilotcliPromptResolver.ts:120-130`):
```typescript
if (ref.value instanceof ChatReferenceBinaryData) {
    if (!isImageMimeType(ref.value.mimeType)) {
        return;
    }
    // Handle image attachments
    try {
        const buffer = await ref.value.data();
        const uri = await this.imageSupport.storeImage(buffer, ref.value.mimeType);
        attachments.push({
            type: 'file',
            displayName: ref.name,
            path: uri.fsPath // Uses Claude Agent SDK attachment mechanism
        });
    }
}
```

---

## 5. Complete Flow Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. USER INTERACTION                                             │
│    • Paste (Ctrl+V)                                             │
│    • Drag & Drop                                                │
│    • #file:image.png                                            │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. VS CODE UI LAYER                                             │
│    • DataTransfer API captures image data                       │
│    • Creates DataTransferItem with mime type                    │
│    • Stores Uint8Array of image data                            │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. EXTENSION API LAYER                                          │
│    • VS Code creates ChatReferenceBinaryData                    │
│      - mimeType: 'image/png' | 'image/jpeg' | etc.              │
│      - data(): Promise<Uint8Array>                              │
│      - reference?: Uri (optional file path)                     │
│    • Added to ChatRequest.references[]                          │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 4. CHAT PARTICIPANT HANDLER                                     │
│    • ChatParticipantRequestHandler receives request             │
│    • Passes references to ChatVariablesCollection               │
│    • PromptVariable created with ChatReferenceBinaryData value  │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 5. PROMPT LAYER (TSX)                                           │
│    • ChatVariables component detects ChatReferenceBinaryData    │
│    • Renders <Image> component:                                 │
│      a. await variableValue.data() → Uint8Array                 │
│      b. Convert to base64: Buffer.from(data).toString('base64') │
│      c. [Optional] Upload to GitHub image service → URL         │
│      d. Render <BaseImage src={source} detail='high' />         │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 6. PROMPT-TSX LIBRARY (@vscode/prompt-tsx)                      │
│    • <BaseImage> component creates Raw.ChatCompletionContentPart│
│    • Type: ChatCompletionContentPartKind.Image                  │
│    • Structure:                                                 │
│      {                                                           │
│        type: 'Image',                                            │
│        imageUrl: {                                               │
│          url: 'data:image/png;base64,...' OR 'https://...',     │
│          detail: 'high',                                         │
│          mediaType: 'image/png'                                  │
│        }                                                         │
│      }                                                           │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 7. MESSAGE CONVERSION                                           │
│    • rawMessageToCAPI() converts to provider format             │
│                                                                  │
│    OpenAI/CAPI Format:                                          │
│    {                                                             │
│      role: 'user',                                               │
│      content: [                                                  │
│        { type: 'text', text: '...' },                           │
│        {                                                         │
│          type: 'image_url',                                      │
│          image_url: {                                            │
│            url: 'data:image/png;base64,...',                    │
│            detail: 'high',                                       │
│            media_type: 'image/png'  // CAPI specific            │
│          }                                                       │
│        }                                                         │
│      ]                                                           │
│    }                                                             │
│                                                                  │
│    Anthropic/Claude Format:                                     │
│    {                                                             │
│      role: 'user',                                               │
│      content: [                                                  │
│        { type: 'text', text: '...' },                           │
│        {                                                         │
│          type: 'image',                                          │
│          source: {                                               │
│            type: 'base64',                                       │
│            media_type: 'image/png',                              │
│            data: 'iVBORw0KGgoAAAANS...'                         │
│          }                                                       │
│        }                                                         │
│      ]                                                           │
│    }                                                             │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 8. NETWORK LAYER                                                │
│    • HTTP POST to OpenAI/CAPI/Anthropic endpoint                │
│    • Content-Type: application/json                             │
│    • Body: JSON with image in message content                   │
└─────────────────────────────────────────────────────────────────┘
```

---

## 6. Image Upload Optimization

### When Image Upload is Used

**Configuration** (`src/extension/prompts/node/panel/image.tsx:96-98`):
```typescript
const isChatCompletions = typeof this.promptEndpoint.urlOrRequestMetadata !== 'string' 
    && this.promptEndpoint.urlOrRequestMetadata.type === RequestType.ChatCompletions;
    
const enabled = this.configurationService.getExperimentBasedConfig(
    ConfigKey.EnableChatImageUpload, 
    this.experimentationService
);

if (isChatCompletions && enabled && modelCanUseImageURL(this.promptEndpoint)) {
    // Upload to GitHub instead of using base64
}
```

### Upload Process

1. **Get GitHub token**: `await this.authService.getGitHubSession('any', { silent: true })`
2. **Upload to GitHub**: `await this.imageService.uploadChatImageAttachment(data, name, mimeType, token)`
3. **Receive URL**: `https://github-production-user-uploads.s3.amazonaws.com/...`
4. **Use URL in prompt**: Replaces base64 data with URL

### Benefits

- **Reduced token usage**: URLs are much shorter than base64 strings
- **Better caching**: Same image URL can be cached by the model
- **Performance**: Faster prompt processing

---

## 7. Debugging Image Attachments

### Breakpoint Locations

1. **Attachment Detection**:
   - VS Code's chat input handler (internal to VS Code core)
   - Creates `ChatReferenceBinaryData` objects

2. **Extension Reception**:
   - `src/extension/prompt/node/chatParticipantRequestHandler.ts`
   - Constructor receives `request: ChatRequest` with references

3. **Variable Processing**:
   - `src/extension/prompt/common/chatVariablesCollection.ts`
   - Iterate over variables to find `ChatReferenceBinaryData`

4. **Image Rendering**:
   - `src/extension/prompts/node/panel/chatVariables.tsx:230`
   - Check `variableValue instanceof ChatReferenceBinaryData`
   - `src/extension/prompts/node/panel/image.tsx:79`
   - `Image.render()` method

5. **Upload Decision**:
   - `src/extension/prompts/node/panel/image.tsx:96-98`
   - Check if upload is enabled and supported

6. **Format Conversion**:
   - OpenAI: `src/platform/networking/common/openai.ts:rawMessageToCAPI()`
   - Claude: `src/extension/agents/claude/node/claudeCodeAgent.ts:139`

7. **Network Request**:
   - OpenAI fetch layer (various files in `src/platform/openai/`)
   - Inspect actual HTTP request body

### Logging

Add logging at key points:

```typescript
// In chatVariables.tsx
console.log('Processing reference:', {
    type: variableValue.constructor.name,
    mimeType: variableValue instanceof ChatReferenceBinaryData 
        ? variableValue.mimeType 
        : undefined
});

// In image.tsx
console.log('Image render:', {
    supportsVision: this.promptEndpoint.supportsVision,
    uploadEnabled: enabled,
    canUseURL: modelCanUseImageURL(this.promptEndpoint),
    imageSource: imageSource.substring(0, 50) + '...'
});
```

---

## 8. Key Configuration Settings

### Vision Support Check

```typescript
// src/extension/prompts/node/panel/image.tsx
this.promptEndpoint.supportsVision
this.authService.copilotToken?.isEditorPreviewFeaturesEnabled()
```

### Image Upload Feature Flag

```typescript
ConfigKey.EnableChatImageUpload
// Controlled via experimentation service
```

### Model Capabilities

```typescript
// src/platform/endpoint/common/chatModelCapabilities.ts
modelCanUseImageURL(endpoint: IChatEndpoint): boolean
```

---

## Summary

The image attachment flow involves:

1. **UI capture** via `DataTransfer` API
2. **Wrapping** in `ChatReferenceBinaryData` by VS Code
3. **Processing** through `ChatVariablesCollection`
4. **Rendering** with TSX components (`<Image>`, `<BaseImage>`)
5. **Upload optimization** (optional, base64 fallback)
6. **Format conversion** to OpenAI/Anthropic structures
7. **Network transmission** in message content array

Key files to monitor:
- `src/extension/vscode.proposed.chatBinaryReferenceData.d.ts` - Type definitions
- `src/extension/prompts/node/panel/image.tsx` - Image rendering
- `src/extension/prompts/node/panel/chatVariables.tsx` - Variable processing
- `src/platform/networking/common/openai.ts` - Format conversion
- `src/extension/agents/claude/node/claudeCodeAgent.ts` - Claude integration