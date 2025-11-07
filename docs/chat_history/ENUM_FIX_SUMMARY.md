# ChatHistoryOperation Enum Serialization Fix

## Problem

The `ChatHistoryOperation` enum in `crates/assistant_tools/src/context_management/chat_history_tool.rs` had a **mismatch between its Rust serialization format and the JSON schema advertised to language models**.

### Root Cause

1. **Original Rust Code**: Used `#[serde(rename_all = "snake_case")]` without a tag attribute, making it an **untagged enum**
   - Expected JSON format: `{"similar": {"n": 5}}`
   
2. **Generated JSON Schema**: When using `schemars` with OpenAPI 3.0 settings, untagged enums are automatically converted to **internally-tagged** format with a `type` discriminator
   - Advertised JSON format: `{"type": "similar", "n": 5}`

3. **Result**: Language models would send internally-tagged JSON (matching the schema), but Rust would fail to deserialize it because it expected untagged format

### Error Messages

When the tool was called with the schema-compliant format:
```
invalid type: string "{\"type\": \"similar\", \"n\": 5}", expected internally tagged enum ChatHistoryOperation
```

Or when called with untagged format:
```
unknown variant `{"similar": {"n": 5}}`, expected one of `append`, `search`, `answer`, `similar`, ...
```

## Solution

Changed the enum to **explicitly use internal tagging** to match the generated schema:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]  // Added tag = "type"
pub enum ChatHistoryOperation {
    Similar {
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        n: Option<usize>,
        // ...
    },
    // ... other variants
}
```

## Changes Made

1. **Enum Attribute**: Added `tag = "type"` to `#[serde(...)]` attribute
2. **Documentation**: Updated all examples to use internally-tagged format
3. **Tests**: Added deserialization test to verify the fix works
4. **Examples**: Updated documentation with comprehensive usage examples

## Correct Usage

When calling `ctx_chat_history`, use this format:

```json
{
  "operation": {
    "type": "similar",
    "n": 5
  }
}
```

NOT:

```json
{
  "operation": {
    "similar": {"n": 5}
  }
}
```

## Why Internal Tagging?

Internal tagging (with `tag = "type"`) is the standard approach for discriminated unions in:
- OpenAPI 3.0 specifications
- Many JSON schema validators
- TypeScript's discriminated unions
- Most API design guidelines

The format `{"type": "variant_name", ...fields}` is more intuitive and easier to work with than untagged `{"variant_name": {...fields}}`.

## Testing

Run the deserialization test to verify:

```bash
cargo test --package assistant_tools test_chat_history_operation_deserialization
```

Expected output:
```
Deserializing similar: Ok(Similar { chat_id: None, n: Some(5), project_scoped: None })
Deserializing list: Ok(List { project_id: None, limit: Some(10), offset: None })
Deserializing wrapped: Ok(ChatHistoryToolInput { operation: Similar { ... } })
```

## Related

- Similar fix may be needed for other enums that use OpenAPI schema generation
- `MemoryOperation` uses untagged format but has no data in variants (unit-only enum), so it serializes as simple strings which works fine
- This pattern should be followed for all new tools with complex enum parameters