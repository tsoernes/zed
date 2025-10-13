#![cfg(test)]

use std::collections::HashSet;

use crate::tools::default_tool_names;

/// Ensures the unified context-management tools (`memory`, `list_history`)
/// are now part of the agent2 crate's built-in default tool list after
/// integrating a native memory tool implementation. If this test fails,
/// a regression may have removed one of the tools from the registry.
#[test]
fn unified_context_tools_present_in_agent2_default_registry() {
    let names: Vec<&'static str> = default_tool_names().collect();

    assert!(
        names.contains(&"memory"),
        "memory tool should be in agent2::default_tool_names() after native integration"
    );
    assert!(
        names.contains(&"list_history"),
        "list_history tool should be in agent2::default_tool_names()"
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
