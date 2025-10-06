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
