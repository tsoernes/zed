use schemars::schema_for;
use serde_json::Value;

/// Import the public input struct for the list_history tool.
use agent2::tools::ListHistoryToolInput;
use agent2::tools::MemoryToolInput;

/// Integration-style schema test for the context compaction executor's
/// `list_history` tool. This does not execute the tool (full execution
/// requires a running `Thread` instance and foreground GPUI context),
/// but it ensures the JSON schema that will be exported during dynamic
/// tool registration contains the expected shape and fields.
///
/// This provides early failure if refactors accidentally hide / rename
/// fields that downstream registration logic (or the model's prompting
/// instructions) depend upon.
#[test]
fn list_history_input_schema_contains_expected_properties() -> anyhow::Result<()> {
    // Generate the schema using schemars the same way the registration
    // code would.
    let schema = schema_for!(ListHistoryToolInput);

    // Convert to generic JSON for simple introspection.
    let schema_json = serde_json::to_value(&schema)?;
    let obj = match schema_json {
        Value::Object(map) => map,
        other => {
            return Err(anyhow::anyhow!(
                "Expected root schema object, got: {other:?}"
            ));
        }
    };

    // Navigate to "properties" of the main schema (draft07 layout).
    let Some(Value::Object(root_schema_obj)) = obj.get("schema").cloned() else {
        return Err(anyhow::anyhow!(
            "Missing 'schema' field in generated root schema"
        ));
    };

    let Some(Value::Object(main_obj)) = root_schema_obj.get("properties").cloned() else {
        return Err(anyhow::anyhow!("Root schema missing 'properties' object"));
    };

    // Expected property keys we rely on externally.
    let expected = [
        "start",
        "limit",
        "max_chars_per_message",
        "include_full_markdown",
    ];

    for key in expected {
        if !main_obj.contains_key(key) {
            return Err(anyhow::anyhow!(
                "Expected property '{key}' not found in ListHistoryToolInput schema. Present keys: {:?}",
                main_obj.keys().collect::<Vec<_>>()
            ));
        }
    }

    Ok(())
}

/// Placeholder (ignored) test for future full integration that will
/// exercise the ContextCompactionExecutor end-to-end by actually
/// invoking the exported `list_history` tool through the dynamic tool
/// infrastructure.
///
/// Steps (to be implemented when a convenient test harness for creating
/// a `Thread` + registering dynamic tools is available):
/// 1. Spin up a minimal Thread with a handful of synthetic messages.
/// 2. Construct a `ContextCompactionExecutor` with the thread's WeakEntity.
/// 3. Build schemas & call `register_core_context_tools` with the executor.
/// 4. Invoke the dynamically registered `list_history` tool with various
///    arguments (default, custom range, include_full_markdown).
/// 5. Assert the returned markdown & JSON summary contain expected indices,
///    counts, and truncation behavior.
/// 6. Confirm no panics and that large limits are clamped as specified.
///
/// Keeping this ignored avoids spurious failures until the harness is
/// in place.
#[test]
fn memory_input_schema_contains_expected_properties() -> anyhow::Result<()> {
    let schema = schema_for!(MemoryToolInput);
    let schema_json = serde_json::to_value(&schema)?;
    let root = schema_json
        .get("schema")
        .and_then(|v| v.get("properties"))
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("Missing root properties in MemoryToolInput schema"))?;

    // Expected top-level property keys in MemoryToolInput
    let expected = [
        "operation",
        "start_index",
        "end_index",
        "memory_handle",
        "summary",
        "auto",
        "max_preview_chars",
        "restore_insert_index",
        "remove_placeholder",
        "replace_placeholder_with",
    ];

    for key in expected {
        if !root.contains_key(key) {
            return Err(anyhow::anyhow!(
                "Expected property '{key}' not found in MemoryToolInput schema. Present: {:?}",
                root.keys().collect::<Vec<_>>()
            ));
        }
    }

    Ok(())
}

#[test]
#[ignore]
fn list_history_full_execution_placeholder() {
    // Intentionally left blank; see doc comment above.
    // Returning () implicitly; the ignore attribute prevents running.
}

/// The following memory tool tests are integration placeholders. They are marked
/// #[ignore] until a lightweight harness for constructing a Thread + App context
/// (with foreground executor and project entities) is available. Each test
/// documents the intended assertions for durable file-based memory archives.

/// Store + List round‑trip:
/// Steps:
/// 1. Create thread with 3 user + 2 agent messages (text + thinking + tool use).
/// 2. Invoke memory store (range covering first 3 messages).
/// 3. Assert:
///    * Placeholder inserted at original start index.
///    * Archived directory exists with metadata + messages.zst.
/// 4. Run memory list; assert handle present, count matches, placeholder=yes.
/// 5. (Optional) Deserialize metadata.json and verify summary not empty.
#[test]
#[ignore]
fn memory_store_and_list_roundtrip_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}

/// Load excerpt:
/// Steps:
/// 1. Reuse archive from store test.
/// 2. Invoke memory load with large max_preview_chars.
/// 3. Assert markdown contains handle + "Messages:" line + at least one "### [0]".
#[test]
#[ignore]
fn memory_load_excerpt_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}

/// Restore with placeholder retained, then removed:
/// Steps:
/// 1. Store archive (range 0..1).
/// 2. Restore without remove_placeholder => placeholder stays; restored=yes.
/// 3. Restore again with remove_placeholder=true should:
///    * Insert messages again only if placeholder still there (single restore rule)
///    * Or (preferred) return an error after first remove (once implemented).
#[test]
#[ignore]
fn memory_restore_retained_then_removed_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}

/// Prune after restore + placeholder removal:
/// Steps:
/// 1. Store + restore with remove_placeholder=true.
/// 2. Run prune => archive directory deleted.
/// 3. Subsequent list omits handle.
#[test]
#[ignore]
fn memory_prune_after_restore_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}

/// Reject user message with mention/image:
/// Steps:
/// 1. Insert a user message containing a mention (or image segment).
/// 2. Attempt store over that index => expect error matching
///    "unsupported user content (mention/image)".
/// 3. Ensure no archive directory created.
#[test]
#[ignore]
fn memory_store_rejects_user_mention_or_image_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}

/// Store + restore with tool uses:
/// Steps:
/// 1. Agent message containing ToolUse + Text segs.
/// 2. Store range including that message.
/// 3. Restore at end of thread.
/// 4. Assert reconstructed AgentMessage contains ToolUse segment (raw_input preserved)
///    and text segments appear in original order.
#[test]
#[ignore]
fn memory_store_and_restore_tool_use_placeholder() {
    // Placeholder – see doc comment for intended implementation steps.
}
