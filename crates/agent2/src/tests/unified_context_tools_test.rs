#![cfg(test)]

use std::collections::HashSet;

use crate::tools::default_tool_names;

/// Ensures the unified context-management tools (`memory`, `list_history`)
/// are no longer part of the agent2 crate's built-in default tool list.
/// They are now expected to be provided by the canonical assistant_tools
/// registry to avoid duplicate / divergent schema export issues that caused
/// external (Claude) 400 errors.
///
/// If this test fails, it means a regression reintroduced the placeholder
/// implementations into the agent2 default registry.
#[test]
fn unified_context_tools_not_in_agent2_default_registry() {
    let names: Vec<&'static str> = default_tool_names().collect();

    assert!(
        !names.contains(&"memory"),
        "memory tool should NOT be in agent2::default_tool_names(); it must be supplied only by assistant_tools"
    );
    assert!(
        !names.contains(&"list_history"),
        "list_history tool should NOT be in agent2::default_tool_names(); it must be supplied only by assistant_tools"
    );
}

/// Sanity check that the default tool name set has no duplicates.
/// (A duplicate could silently mask a removed tool when collected into a Set.)
#[test]
fn default_tool_names_are_unique() {
    let names: Vec<&'static str> = default_tool_names().collect();
    let set: HashSet<&'static str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        set.len(),
        "default_tool_names() contained duplicates: {:?}",
        names
    );
}
