# Quick Debugging Guide for Image Attachments

## Testing Image Attachments

### 1. Manual Testing Steps

1. Open VS Code with the Copilot Chat extension
2. Open the chat panel or inline chat
3. Try these methods:
   - **Paste**: Copy an image to clipboard, paste in chat input (Ctrl+V / Cmd+V)
   - **Drag & Drop**: Drag an image file from file explorer into chat input
   - **File Reference**: Type `#file` and select an image file

### 2. Verify Image is Detected

Add a breakpoint or log in `src/extension/prompts/node/panel/chatVariables.tsx:230`:

```typescript
if (variableValue instanceof ChatReferenceBinaryData) {
    console.log('✓ Image detected:', {
        name: variableName,
        mimeType: variableValue.mimeType,
        hasReference: !!variableValue.reference
    });
    // Existing code continues...
}
```

**Expected output:**
```
✓ Image detected: {
  name: 'image.png',
  mimeType: 'image/png',
  hasReference: true
}
```

### 3. Check Image Data Retrieval

In `src/extension/prompts/node/panel/image.tsx:79` (Image.render):

```typescript
const variable = await this.props.variableValue;
console.log('✓ Image data retrieved:', {
    sizeBytes: variable.length,
    base64Preview: Buffer.from(variable).toString('base64').substring(0, 50)
});
```

**Expected output:**
```
✓ Image data retrieved: {
  sizeBytes: 45678,
  base64Preview: 'iVBORw0KGgoAAAANSUhEUgAABQAAAALQCAYAAADPfd...'
}
```

### 4. Verify Vision Support

In `src/extension/prompts/node/panel/image.tsx:82`:

```typescript
console.log('Vision support check:', {
    supportsVision: this.promptEndpoint.supportsVision,
    previewFeaturesEnabled: this.authService.copilotToken?.isEditorPreviewFeaturesEnabled(),
    modelName: this.promptEndpoint.model
});
```

**Expected output (for vision-capable model):**
```
Vision support check: {
  supportsVision: true,
  previewFeaturesEnabled: true,
  modelName: 'gpt-4o'
}
```

### 5. Check Upload Decision

In `src/extension/prompts/node/panel/image.tsx:96`:

```typescript
const isChatCompletions = typeof this.promptEndpoint.urlOrRequestMetadata !== 'string' 
    && this.promptEndpoint.urlOrRequestMetadata.type === RequestType.ChatCompletions;
const enabled = this.configurationService.getExperimentBasedConfig(
    ConfigKey.EnableChatImageUpload, 
    this.experimentationService
);

console.log('Upload decision:', {
    isChatCompletions,
    uploadEnabled: enabled,
    canUseURL: modelCanUseImageURL(this.promptEndpoint),
    willUpload: isChatCompletions && enabled && modelCanUseImageURL(this.promptEndpoint)
});
```

### 6. Monitor Upload Process

In `src/extension/prompts/node/panel/image.tsx:99-115`:

```typescript
if (isChatCompletions && enabled && modelCanUseImageURL(this.promptEndpoint)) {
    try {
        console.log('→ Starting image upload...');
        const githubToken = (await this.authService.getGitHubSession('any', { silent: true }))?.accessToken;
        const mimeType = getMimeType(imageSource) ?? imageMimeType;
        const uri = await this.imageService.uploadChatImageAttachment(variable, this.props.variableName, mimeType, githubToken);
        
        if (uri) {
            console.log('✓ Image uploaded:', uri.toString());
            imageSource = uri.toString();
            imageMimeType = mimeType;
        } else {
            console.log('⚠ Upload returned no URI, using base64');
        }
    } catch (error) {
        console.log('✗ Upload failed:', error, '- using base64 fallback');
    }
}
```

### 7. Inspect Final Rendered Output

In `src/extension/prompts/node/panel/image.tsx:117`:

```typescript
console.log('Final image render:', {
    sourceType: imageSource.startsWith('http') ? 'URL' : 'base64',
    sourcePreview: imageSource.substring(0, 100),
    detail: 'high',
    mimeType: imageMimeType
});

return (
    <UserMessage priority={0}>
        <BaseImage src={imageSource} detail='high' mimeType={imageMimeType} />
        // ...
    </UserMessage>
);
```

### 8. Check Message Conversion

In `src/platform/networking/common/openai.ts:147`:

```typescript
export function rawMessageToCAPI(message: Raw.ChatMessage, callback?: RawMessageConversionCallback): CAPIChatMessage {
    const out: CAPIChatMessage = toMode(OutputMode.OpenAI, message);
    
    // Add logging
    if (!Array.isArray(out.content)) {
        console.log('Message content is string (no images)');
    } else {
        const imageUrls = out.content.filter(p => p.type === 'image_url');
        console.log('Message conversion:', {
            totalParts: out.content.length,
            imageParts: imageUrls.length,
            images: imageUrls.map(p => ({
                urlType: p.image_url.url.startsWith('http') ? 'URL' : 'base64',
                detail: p.image_url.detail,
                mediaType: (p.image_url as any).media_type
            }))
        });
    }
    
    // Existing conversion logic...
}
```

### 9. Anthropic/Claude Specific

In `src/extension/agents/claude/node/claudeCodeAgent.ts:139`:

```typescript
for (const ref of request.references) {
    let refValue = ref.value;
    
    if (refValue instanceof ChatReferenceBinaryData) {
        console.log('Processing image for Claude:', {
            name: ref.name,
            mimeType: refValue.mimeType
        });
        
        const mediaType = toAnthropicImageMediaType(refValue.mimeType);
        
        if (mediaType) {
            const data = await refValue.data();
            console.log('✓ Image converted for Claude:', {
                mediaType,
                sizeBytes: data.length,
                base64Length: Buffer.from(data).toString('base64').length
            });
            
            contentBlocks.push({
                type: 'image',
                source: {
                    type: 'base64',
                    data: Buffer.from(data).toString('base64'),
                    media_type: mediaType
                }
            });
        }
    }
}
```

## Common Issues and Solutions

### Issue 1: "Model does not support images"

**Symptoms:**
- Image shows as reference only, not sent to model
- Console shows: `supportsVision: false`

**Solution:**
- Check model name - must be vision-capable (e.g., `gpt-4o`, `gpt-4-vision-preview`, `claude-3-*`)
- Verify endpoint configuration
- Check: `this.authService.copilotToken?.isEditorPreviewFeaturesEnabled()`

### Issue 2: Image not detected

**Symptoms:**
- No log from "Image detected" checkpoint
- `ChatReferenceBinaryData` not in references

**Solution:**
- Verify VS Code version supports proposed API `chatBinaryReferenceData`
- Check mime type is supported: `image/png`, `image/jpeg`, `image/gif`, `image/webp`
- Try different attachment method (paste vs drag vs #file)

### Issue 3: Upload fails, base64 always used

**Symptoms:**
- "Upload failed" or "using base64 fallback" in logs
- Image works but uses base64 encoding

**Solution:**
- Check GitHub authentication: `await this.authService.getGitHubSession('any', { silent: true })`
- Verify `ConfigKey.EnableChatImageUpload` experiment is enabled
- Check `modelCanUseImageURL(this.promptEndpoint)` returns true
- Verify network connectivity to GitHub image service

### Issue 4: Image sent but not processed by model

**Symptoms:**
- Image appears in request
- Model doesn't acknowledge image in response

**Solution:**
- Verify message format matches API requirements
- Check `media_type` field is set correctly (OpenAI uses `media_type`, not `mediaType`)
- Ensure image is in correct content part structure
- Verify base64 encoding is valid
- Check image size isn't exceeding model limits

### Issue 5: Historical images not showing in conversation

**Symptoms:**
- New images work, but history doesn't show images

**Solution:**
- Check `HistoricalImage` component render logic
- Verify `part.type === Raw.ChatCompletionContentPartKind.Image`
- Ensure stored messages include image parts with src/detail/mimeType
- Check vision support is still enabled for current model

## Network Inspection

### Using VS Code DevTools

1. **Open DevTools**: Help → Toggle Developer Tools
2. **Go to Network tab**
3. **Filter**: XHR/Fetch requests
4. **Look for**: POST requests to OpenAI/Anthropic endpoints
5. **Inspect payload**:
   ```json
   {
     "messages": [
       {
         "role": "user",
         "content": [
           { "type": "text", "text": "..." },
           { 
             "type": "image_url", 
             "image_url": { 
               "url": "data:image/png;base64,..." 
             } 
           }
         ]
       }
     ]
   }
   ```

### Using Proxy (mitmproxy/Charles)

1. Configure system proxy
2. Intercept HTTPS traffic
3. Look for requests to:
   - `https://api.openai.com/v1/chat/completions`
   - `https://api.githubcopilot.com/*`
   - `https://api.anthropic.com/v1/messages`

## Verification Checklist

- [ ] Image detected in `ChatReferenceBinaryData`
- [ ] Binary data retrieved successfully
- [ ] Vision support enabled for model
- [ ] Image rendered in `<BaseImage>` component
- [ ] Upload decision made (URL vs base64)
- [ ] Message converted to correct API format
- [ ] Network request includes image in content
- [ ] Model processes and acknowledges image
- [ ] Response references image content

## Performance Monitoring

### Image Size Impact

Log image sizes to understand token/bandwidth impact:

```typescript
const sizeKB = variable.length / 1024;
const base64Size = Buffer.from(variable).toString('base64').length;

console.log('Image metrics:', {
    originalKB: sizeKB.toFixed(2),
    base64KB: (base64Size / 1024).toFixed(2),
    uploadSavings: uploaded ? `${((base64Size - uri.toString().length) / 1024).toFixed(2)} KB` : 'N/A'
});
```

### Timing Measurements

```typescript
const startUpload = performance.now();
const uri = await this.imageService.uploadChatImageAttachment(...);
const uploadTime = performance.now() - startUpload;

console.log(`Upload completed in ${uploadTime.toFixed(2)}ms`);
```

## Related Files

| File | Purpose |
|------|---------|
| `src/extension/vscode.proposed.chatBinaryReferenceData.d.ts` | Type definitions |
| `src/extension/prompts/node/panel/image.tsx` | Image component rendering |
| `src/extension/prompts/node/panel/chatVariables.tsx` | Variable processing |
| `src/extension/prompt/common/chatVariablesCollection.ts` | Variables collection |
| `src/platform/networking/common/openai.ts` | OpenAI format conversion |
| `src/extension/agents/claude/node/claudeCodeAgent.ts` | Claude integration |
| `src/platform/endpoint/common/chatModelCapabilities.ts` | Model capability checks |