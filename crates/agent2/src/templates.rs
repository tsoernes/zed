use anyhow::Result;
use gpui::SharedString;
use handlebars::Handlebars;
use rust_embed::RustEmbed;
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use std::sync::Arc;

#[derive(RustEmbed)]
#[folder = "src/templates"]
#[include = "*.hbs"]
struct Assets;

pub struct Templates(Handlebars<'static>);

impl Templates {
    pub fn new() -> Arc<Self> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        handlebars.register_helper("contains", Box::new(contains));
        handlebars.register_embed_templates::<Assets>().unwrap();
        Arc::new(Self(handlebars))
    }
}

pub trait Template: Sized {
    const TEMPLATE_NAME: &'static str;

    fn render(&self, templates: &Templates) -> Result<String>
    where
        Self: Serialize + Sized,
    {
        Ok(templates.0.render(Self::TEMPLATE_NAME, self)?)
    }
}

pub struct SystemPromptTemplate<'a> {
    pub project: &'a prompt_store::ProjectContext,
    pub available_tools: Vec<SharedString>,

    /// Current approximate or precise active token count (included only when Some)
    pub active_tokens: Option<usize>,
    /// Model max token capacity (included only when Some)
    pub max_tokens: Option<usize>,
    /// Percentage usage (0-100, included only when Some)
    pub usage_pct: Option<f64>,

    /// Number of archived memory segments (included only when Some)
    pub memory_segment_count: Option<usize>,
    /// Total precise token savings from archived segments (sum of per-segment (message_token_count - placeholder_token_count)); included only when Some
    pub memory_saved_tokens: Option<u64>,
}

impl<'a> Serialize for SystemPromptTemplate<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Base dynamic field count; booleans always appended explicitly.
        let mut state = serializer.serialize_struct("SystemPromptTemplate", 18)?;
        // Flatten project context fields (template expects top-level `worktrees`, etc.).
        state.serialize_field("worktrees", &self.project.worktrees)?;
        state.serialize_field("has_rules", &self.project.has_rules)?;
        state.serialize_field("user_rules", &self.project.user_rules)?;
        state.serialize_field("has_user_rules", &self.project.has_user_rules)?;
        state.serialize_field("os", &self.project.os)?;
        state.serialize_field("arch", &self.project.arch)?;
        state.serialize_field("shell", &self.project.shell)?;
        state.serialize_field("available_tools", &self.available_tools)?;
        if let Some(v) = self.active_tokens {
            state.serialize_field("active_tokens", &v)?;
        }
        if let Some(v) = self.max_tokens {
            state.serialize_field("max_tokens", &v)?;
        }
        if let Some(v) = self.usage_pct {
            state.serialize_field("usage_pct", &v)?;
        }
        if let Some(v) = self.memory_segment_count {
            state.serialize_field("memory_segment_count", &v)?;
        }
        if let Some(v) = self.memory_saved_tokens {
            state.serialize_field("memory_saved_tokens", &v)?;
        }
        // Boolean flags to simplify template logic (avoid needing `or`, `gt`, `len` helpers).
        let has_available_tools = !self.available_tools.is_empty();
        let has_active_usage =
            self.active_tokens.is_some() || self.max_tokens.is_some() || self.usage_pct.is_some();
        let has_memory_stats =
            self.memory_segment_count.is_some() || self.memory_saved_tokens.is_some();
        state.serialize_field("has_available_tools", &has_available_tools)?;
        state.serialize_field("has_active_usage", &has_active_usage)?;
        state.serialize_field("has_memory_stats", &has_memory_stats)?;
        state.end()
    }
}

impl Template for SystemPromptTemplate<'_> {
    const TEMPLATE_NAME: &'static str = "system_prompt.hbs";
}

/// Handlebars helper for checking if an item is in a list
fn contains(
    h: &handlebars::Helper,
    _: &handlebars::Handlebars,
    _: &handlebars::Context,
    _: &mut handlebars::RenderContext,
    out: &mut dyn handlebars::Output,
) -> handlebars::HelperResult {
    let list = h
        .param(0)
        .and_then(|v| v.value().as_array())
        .ok_or_else(|| {
            handlebars::RenderError::new("contains: missing or invalid list parameter")
        })?;
    let query = h.param(1).map(|v| v.value()).ok_or_else(|| {
        handlebars::RenderError::new("contains: missing or invalid query parameter")
    })?;

    if list.contains(query) {
        out.write("true")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_prompt_template() {
        let project = prompt_store::ProjectContext::default();
        let template = SystemPromptTemplate {
            project: &project,
            available_tools: vec!["echo".into()],
            active_tokens: Some(1200),
            max_tokens: Some(16000),
            usage_pct: Some(7.5),
            memory_segment_count: Some(2),
            memory_saved_tokens: Some(1234),
        };
        let templates = Templates::new();
        let rendered = template.render(&templates).unwrap();
        assert!(rendered.contains("## Fixing Diagnostics"));
    }
}
