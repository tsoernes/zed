use crate::{
    ContextServerRegistry, CopyPathTool, CreateDirectoryTool, DbLanguageModel, DbThread,
    DeletePathTool, DiagnosticsTool, EditFileTool, EnhancedTerminalTool, FetchTool, FindPathTool,
    GrepTool, ListDirectoryTool, MemoryAgentTool, MovePathTool, NowTool, OpenTool, ReadFileTool,
    ShellDetectorTool, SystemPromptTemplate, Template, Templates, TerminalTool, ThinkingTool,
    TokenUsageTool, WebSearchTool,
};
use acp_thread::{MentionUri, UserMessageId};
use action_log::ActionLog;
use agent::thread::{GitState, ProjectSnapshot, WorktreeSnapshot};
use agent_client_protocol as acp;
use agent_settings::{
    AgentProfileId, AgentProfileSettings, AgentSettings, CompletionMode,
    SUMMARIZE_THREAD_DETAILED_PROMPT, SUMMARIZE_THREAD_PROMPT,
};
use anyhow::{Context as _, Result, anyhow};
use assistant_tool::adapt_schema_to_format;

use chrono::{DateTime, Utc};
use client::{ModelRequestUsage, RequestUsage};
use cloud_llm_client::{CompletionIntent, CompletionRequestStatus, UsageLimit};
use collections::{HashMap, HashSet, IndexMap};
use fs::Fs;
use futures::{
    FutureExt,
    channel::{mpsc, oneshot},
    future::Shared,
    stream::FuturesUnordered,
};
use git::repository::DiffType;
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, SharedString, Task, WeakEntity,
};
use language_model::{
    LanguageModel, LanguageModelCompletionError, LanguageModelCompletionEvent, LanguageModelExt,
    LanguageModelImage, LanguageModelProviderId, LanguageModelRegistry, LanguageModelRequest,
    LanguageModelRequestMessage, LanguageModelRequestTool, LanguageModelToolResult,
    LanguageModelToolResultContent, LanguageModelToolSchemaFormat, LanguageModelToolUse,
    LanguageModelToolUseId, Role, SelectedModel, StopReason, TokenUsage,
};
use project::{
    Project,
    git_store::{GitStore, RepositoryState},
};
use prompt_store::ProjectContext;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use settings::{Settings, update_settings_file};
use smol::stream::StreamExt;
use std::{
    collections::BTreeMap,
    ops::RangeInclusive,
    path::Path,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use std::{fmt::Write, path::PathBuf};
use util::{ResultExt, debug_panic, markdown::MarkdownCodeBlock};
use uuid::Uuid;

const TOOL_CANCELED_MESSAGE: &str = "Tool canceled by user";
pub const MAX_TOOL_NAME_LENGTH: usize = 64;

/// The ID of the user prompt that initiated a request.
///
/// This equates to the user physically submitting a message to the model (e.g., by pressing the Enter key).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Serialize, Deserialize)]
pub struct PromptId(Arc<str>);

impl PromptId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string().into())
    }
}

impl std::fmt::Display for PromptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub(crate) const MAX_RETRY_ATTEMPTS: u8 = 4;
pub(crate) const BASE_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
enum RetryStrategy {
    ExponentialBackoff {
        initial_delay: Duration,
        max_attempts: u8,
    },
    Fixed {
        delay: Duration,
        max_attempts: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    User(UserMessage),
    Agent(AgentMessage),
    Resume,
}

// Internal thread-scoped archived memory segment.
// Not exposed publicly; used for context compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ThreadMemorySegment {
    pub(crate) id: u64,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) summary: SharedString,
    pub(crate) message_char_count: usize,
    pub(crate) message_count: usize,
    pub(crate) stored_epoch_ms: u128,
    pub(crate) placeholder_char_count: usize,
    // Token counts captured at archive time for reconstructing full context size.
    pub(crate) message_token_count: usize,
    pub(crate) placeholder_token_count: usize,
    pub(crate) messages: Vec<Message>,
}

// On-disk persisted representation of thread memory segments.
// Stored as JSON: { "segments": [ ThreadMemorySegment, ... ] }
#[derive(Serialize, Deserialize)]
struct PersistedMemorySegments {
    segments: Vec<ThreadMemorySegment>,
}

// Auxiliary impl block providing persistence helpers.
// These are separated to keep core logic above uncluttered.
impl Thread {
    fn memory_segments_file_path(&self) -> std::path::PathBuf {
        // contexts_dir()/memory_segments/<thread_id>.json
        paths::contexts_dir()
            .join("memory_segments")
            .join(format!("{}.json", self.id))
    }

    fn ensure_memory_dir(path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }

    fn persist_memory_segments(&self) -> anyhow::Result<()> {
        // Do not write empty (avoid churn); if none exist and file present, remove it.
        let path = self.memory_segments_file_path();
        if self.memory_segments.is_empty() {
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            return Ok(());
        }
        Self::ensure_memory_dir(&path)?;
        let data = PersistedMemorySegments {
            segments: self.memory_segments.clone(),
        };
        let json = serde_json::to_vec_pretty(&data)?;
        // Atomic write: write to temp then rename.
        let mut tmp = path.clone();
        tmp.set_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn load_memory_segments_from_disk(&mut self) -> anyhow::Result<()> {
        let path = self.memory_segments_file_path();
        if !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&path)?;
        let persisted: PersistedMemorySegments = serde_json::from_slice(&bytes)?;
        // Assign and recompute next id
        let max_id = persisted.segments.iter().map(|s| s.id).max().unwrap_or(0);
        self.memory_segments = persisted.segments;
        self.memory_next_id = max_id.saturating_add(1);
        Ok(())
    }
}

impl Message {
    pub fn as_agent_message(&self) -> Option<&AgentMessage> {
        match self {
            Message::Agent(agent_message) => Some(agent_message),
            _ => None,
        }
    }

    pub fn to_request(&self) -> Vec<LanguageModelRequestMessage> {
        match self {
            Message::User(message) => vec![message.to_request()],
            Message::Agent(message) => message.to_request(),
            Message::Resume => vec![LanguageModelRequestMessage {
                role: Role::User,
                content: vec!["Continue where you left off".into()],
                cache: false,
            }],
        }
    }

    pub fn to_markdown(&self) -> String {
        match self {
            Message::User(message) => message.to_markdown(),
            Message::Agent(message) => message.to_markdown(),
            Message::Resume => "[resume]\n".into(),
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Message::User(_) | Message::Resume => Role::User,
            Message::Agent(_) => Role::Assistant,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    pub id: UserMessageId,
    pub content: Vec<UserMessageContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserMessageContent {
    Text(String),
    Mention { uri: MentionUri, content: String },
    Image(LanguageModelImage),
}

impl UserMessage {
    pub fn to_markdown(&self) -> String {
        let mut markdown = String::from("## User\n\n");

        for content in &self.content {
            match content {
                UserMessageContent::Text(text) => {
                    markdown.push_str(text);
                    markdown.push('\n');
                }
                UserMessageContent::Image(_) => {
                    markdown.push_str("<image />\n");
                }
                UserMessageContent::Mention { uri, content } => {
                    if !content.is_empty() {
                        let _ = writeln!(&mut markdown, "{}\n\n{}", uri.as_link(), content);
                    } else {
                        let _ = writeln!(&mut markdown, "{}", uri.as_link());
                    }
                }
            }
        }

        markdown
    }

    fn to_request(&self) -> LanguageModelRequestMessage {
        let mut message = LanguageModelRequestMessage {
            role: Role::User,
            content: Vec::with_capacity(self.content.len()),
            cache: false,
        };

        const OPEN_CONTEXT: &str = "<context>\n\
            The following items were attached by the user. \
            They are up-to-date and don't need to be re-read.\n\n";

        const OPEN_FILES_TAG: &str = "<files>";
        const OPEN_DIRECTORIES_TAG: &str = "<directories>";
        const OPEN_SYMBOLS_TAG: &str = "<symbols>";
        const OPEN_SELECTIONS_TAG: &str = "<selections>";
        const OPEN_THREADS_TAG: &str = "<threads>";
        const OPEN_FETCH_TAG: &str = "<fetched_urls>";
        const OPEN_RULES_TAG: &str =
            "<rules>\nThe user has specified the following rules that should be applied:\n";

        let mut file_context = OPEN_FILES_TAG.to_string();
        let mut directory_context = OPEN_DIRECTORIES_TAG.to_string();
        let mut symbol_context = OPEN_SYMBOLS_TAG.to_string();
        let mut selection_context = OPEN_SELECTIONS_TAG.to_string();
        let mut thread_context = OPEN_THREADS_TAG.to_string();
        let mut fetch_context = OPEN_FETCH_TAG.to_string();
        let mut rules_context = OPEN_RULES_TAG.to_string();

        for chunk in &self.content {
            let chunk = match chunk {
                UserMessageContent::Text(text) => {
                    language_model::MessageContent::Text(text.clone())
                }
                UserMessageContent::Image(value) => {
                    language_model::MessageContent::Image(value.clone())
                }
                UserMessageContent::Mention { uri, content } => {
                    match uri {
                        MentionUri::File { abs_path } => {
                            write!(
                                &mut file_context,
                                "\n{}",
                                MarkdownCodeBlock {
                                    tag: &codeblock_tag(abs_path, None),
                                    text: &content.to_string(),
                                }
                            )
                            .ok();
                        }
                        MentionUri::PastedImage => {
                            debug_panic!("pasted image URI should not be used in mention content")
                        }
                        MentionUri::Directory { .. } => {
                            write!(&mut directory_context, "\n{}\n", content).ok();
                        }
                        MentionUri::Symbol {
                            abs_path: path,
                            line_range,
                            ..
                        } => {
                            write!(
                                &mut symbol_context,
                                "\n{}",
                                MarkdownCodeBlock {
                                    tag: &codeblock_tag(path, Some(line_range)),
                                    text: content
                                }
                            )
                            .ok();
                        }
                        MentionUri::Selection {
                            abs_path: path,
                            line_range,
                            ..
                        } => {
                            write!(
                                &mut selection_context,
                                "\n{}",
                                MarkdownCodeBlock {
                                    tag: &codeblock_tag(
                                        path.as_deref().unwrap_or("Untitled".as_ref()),
                                        Some(line_range)
                                    ),
                                    text: content
                                }
                            )
                            .ok();
                        }
                        MentionUri::Thread { .. } => {
                            write!(&mut thread_context, "\n{}\n", content).ok();
                        }
                        MentionUri::TextThread { .. } => {
                            write!(&mut thread_context, "\n{}\n", content).ok();
                        }
                        MentionUri::Rule { .. } => {
                            write!(
                                &mut rules_context,
                                "\n{}",
                                MarkdownCodeBlock {
                                    tag: "",
                                    text: content
                                }
                            )
                            .ok();
                        }
                        MentionUri::Fetch { url } => {
                            write!(&mut fetch_context, "\nFetch: {}\n\n{}", url, content).ok();
                        }
                    }

                    language_model::MessageContent::Text(uri.as_link().to_string())
                }
            };

            message.content.push(chunk);
        }

        let len_before_context = message.content.len();

        if file_context.len() > OPEN_FILES_TAG.len() {
            file_context.push_str("</files>\n");
            message
                .content
                .push(language_model::MessageContent::Text(file_context));
        }

        if directory_context.len() > OPEN_DIRECTORIES_TAG.len() {
            directory_context.push_str("</directories>\n");
            message
                .content
                .push(language_model::MessageContent::Text(directory_context));
        }

        if symbol_context.len() > OPEN_SYMBOLS_TAG.len() {
            symbol_context.push_str("</symbols>\n");
            message
                .content
                .push(language_model::MessageContent::Text(symbol_context));
        }

        if selection_context.len() > OPEN_SELECTIONS_TAG.len() {
            selection_context.push_str("</selections>\n");
            message
                .content
                .push(language_model::MessageContent::Text(selection_context));
        }

        if thread_context.len() > OPEN_THREADS_TAG.len() {
            thread_context.push_str("</threads>\n");
            message
                .content
                .push(language_model::MessageContent::Text(thread_context));
        }

        if fetch_context.len() > OPEN_FETCH_TAG.len() {
            fetch_context.push_str("</fetched_urls>\n");
            message
                .content
                .push(language_model::MessageContent::Text(fetch_context));
        }

        if rules_context.len() > OPEN_RULES_TAG.len() {
            rules_context.push_str("</user_rules>\n");
            message
                .content
                .push(language_model::MessageContent::Text(rules_context));
        }

        if message.content.len() > len_before_context {
            message.content.insert(
                len_before_context,
                language_model::MessageContent::Text(OPEN_CONTEXT.into()),
            );
            message
                .content
                .push(language_model::MessageContent::Text("</context>".into()));
        }

        message
    }
}

fn codeblock_tag(full_path: &Path, line_range: Option<&RangeInclusive<u32>>) -> String {
    let mut result = String::new();

    if let Some(extension) = full_path.extension().and_then(|ext| ext.to_str()) {
        let _ = write!(result, "{} ", extension);
    }

    let _ = write!(result, "{}", full_path.display());

    if let Some(range) = line_range {
        if range.start() == range.end() {
            let _ = write!(result, ":{}", range.start() + 1);
        } else {
            let _ = write!(result, ":{}-{}", range.start() + 1, range.end() + 1);
        }
    }

    result
}

impl AgentMessage {
    pub fn to_markdown(&self) -> String {
        let mut markdown = String::from("## Assistant\n\n");

        for content in &self.content {
            match content {
                AgentMessageContent::Text(text) => {
                    markdown.push_str(text);
                    markdown.push('\n');
                }
                AgentMessageContent::Thinking { text, .. } => {
                    markdown.push_str("<think>");
                    markdown.push_str(text);
                    markdown.push_str("</think>\n");
                }
                AgentMessageContent::RedactedThinking(_) => {
                    markdown.push_str("<redacted_thinking />\n")
                }
                AgentMessageContent::ToolUse(tool_use) => {
                    markdown.push_str(&format!(
                        "**Tool Use**: {} (ID: {})\n",
                        tool_use.name, tool_use.id
                    ));
                    markdown.push_str(&format!(
                        "{}\n",
                        MarkdownCodeBlock {
                            tag: "json",
                            text: &format!("{:#}", tool_use.input)
                        }
                    ));
                }
            }
        }

        for tool_result in self.tool_results.values() {
            markdown.push_str(&format!(
                "**Tool Result**: {} (ID: {})\n\n",
                tool_result.tool_name, tool_result.tool_use_id
            ));
            if tool_result.is_error {
                markdown.push_str("**ERROR:**\n");
            }

            match &tool_result.content {
                LanguageModelToolResultContent::Text(text) => {
                    writeln!(markdown, "{text}\n").ok();
                }
                LanguageModelToolResultContent::Image(_) => {
                    writeln!(markdown, "<image />\n").ok();
                }
            }

            if let Some(output) = tool_result.output.as_ref() {
                writeln!(
                    markdown,
                    "**Debug Output**:\n\n```json\n{}\n```\n",
                    serde_json::to_string_pretty(output).unwrap()
                )
                .unwrap();
            }
        }

        markdown
    }

    pub fn to_request(&self) -> Vec<LanguageModelRequestMessage> {
        let mut assistant_message = LanguageModelRequestMessage {
            role: Role::Assistant,
            content: Vec::with_capacity(self.content.len()),
            cache: false,
        };
        for chunk in &self.content {
            match chunk {
                AgentMessageContent::Text(text) => {
                    assistant_message
                        .content
                        .push(language_model::MessageContent::Text(text.clone()));
                }
                AgentMessageContent::Thinking { text, signature } => {
                    assistant_message
                        .content
                        .push(language_model::MessageContent::Thinking {
                            text: text.clone(),
                            signature: signature.clone(),
                        });
                }
                AgentMessageContent::RedactedThinking(value) => {
                    assistant_message.content.push(
                        language_model::MessageContent::RedactedThinking(value.clone()),
                    );
                }
                AgentMessageContent::ToolUse(tool_use) => {
                    if self.tool_results.contains_key(&tool_use.id) {
                        assistant_message
                            .content
                            .push(language_model::MessageContent::ToolUse(tool_use.clone()));
                    }
                }
            };
        }

        let mut user_message = LanguageModelRequestMessage {
            role: Role::User,
            content: Vec::new(),
            cache: false,
        };

        for tool_result in self.tool_results.values() {
            let mut tool_result = tool_result.clone();
            // Surprisingly, the API fails if we return an empty string here.
            // It thinks we are sending a tool use without a tool result.
            if tool_result.content.is_empty() {
                tool_result.content = "<Tool returned an empty string>".into();
            }
            user_message
                .content
                .push(language_model::MessageContent::ToolResult(tool_result));
        }

        let mut messages = Vec::new();
        if !assistant_message.content.is_empty() {
            messages.push(assistant_message);
        }
        if !user_message.content.is_empty() {
            messages.push(user_message);
        }
        messages
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub content: Vec<AgentMessageContent>,
    pub tool_results: IndexMap<LanguageModelToolUseId, LanguageModelToolResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMessageContent {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    ToolUse(LanguageModelToolUse),
}

pub trait TerminalHandle {
    fn id(&self, cx: &AsyncApp) -> Result<acp::TerminalId>;
    fn current_output(&self, cx: &AsyncApp) -> Result<acp::TerminalOutputResponse>;
    fn wait_for_exit(&self, cx: &AsyncApp) -> Result<Shared<Task<acp::TerminalExitStatus>>>;
}

pub trait ThreadEnvironment {
    fn create_terminal(
        &self,
        command: String,
        cwd: Option<PathBuf>,
        output_byte_limit: Option<u64>,
        cx: &mut AsyncApp,
    ) -> Task<Result<Rc<dyn TerminalHandle>>>;
}

#[derive(Debug)]
pub enum ThreadEvent {
    UserMessage(UserMessage),
    AgentText(String),
    AgentThinking(String),
    ToolCall(acp::ToolCall),
    ToolCallUpdate(acp_thread::ToolCallUpdate),
    ToolCallAuthorization(ToolCallAuthorization),
    Retry(acp_thread::RetryStatus),
    Stop(acp::StopReason),
}

#[derive(Debug)]
pub struct NewTerminal {
    pub command: String,
    pub output_byte_limit: Option<u64>,
    pub cwd: Option<PathBuf>,
    pub response: oneshot::Sender<Result<Entity<acp_thread::Terminal>>>,
}

#[derive(Debug)]
pub struct ToolCallAuthorization {
    pub tool_call: acp::ToolCallUpdate,
    pub options: Vec<acp::PermissionOption>,
    pub response: oneshot::Sender<acp::PermissionOptionId>,
}

#[derive(Debug, thiserror::Error)]
enum CompletionError {
    #[error("max tokens")]
    MaxTokens,
    #[error("refusal")]
    Refusal,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct Thread {
    id: acp::SessionId,
    prompt_id: PromptId,
    updated_at: DateTime<Utc>,
    title: Option<SharedString>,
    pending_title_generation: Option<Task<()>>,
    summary: Option<SharedString>,
    messages: Vec<Message>,
    // Thread-scoped memory archive segments (non-public).
    memory_segments: Vec<ThreadMemorySegment>,
    memory_next_id: u64,
    completion_mode: CompletionMode,
    /// Holds the task that handles agent interaction until the end of the turn.
    /// Survives across multiple requests as the model performs tool calls and
    /// we run tools, report their results.
    running_turn: Option<RunningTurn>,
    pending_message: Option<AgentMessage>,
    tools: BTreeMap<SharedString, Arc<dyn AnyAgentTool>>,
    tool_use_limit_reached: bool,
    request_token_usage: HashMap<UserMessageId, language_model::TokenUsage>,
    #[allow(unused)]
    cumulative_token_usage: TokenUsage,
    #[allow(unused)]
    initial_project_snapshot: Shared<Task<Option<Arc<ProjectSnapshot>>>>,
    context_server_registry: Entity<ContextServerRegistry>,
    profile_id: AgentProfileId,
    project_context: Entity<ProjectContext>,
    templates: Arc<Templates>,
    model: Option<Arc<dyn LanguageModel>>,
    summarization_model: Option<Arc<dyn LanguageModel>>,
    prompt_capabilities_tx: watch::Sender<acp::PromptCapabilities>,
    pub(crate) prompt_capabilities_rx: watch::Receiver<acp::PromptCapabilities>,
    pub(crate) project: Entity<Project>,
    pub(crate) action_log: Entity<ActionLog>,
    /// Precise active token count for current conversation messages (if computed).
    pub(crate) precise_active_tokens: Option<u64>,
    /// Precise model max token capacity (cached when token count is computed).
    pub(crate) precise_max_tokens: Option<u64>,
    /// Precise per-message token counts aligned with current request messages, if computed.
    pub(crate) precise_per_message_tokens: Option<Vec<usize>>,
}

impl Thread {
    fn prompt_capabilities(model: Option<&dyn LanguageModel>) -> acp::PromptCapabilities {
        let image = model.map_or(true, |model| model.supports_images());
        acp::PromptCapabilities {
            meta: None,
            image,
            audio: false,
            embedded_context: true,
        }
    }

    pub fn new(
        project: Entity<Project>,
        project_context: Entity<ProjectContext>,
        context_server_registry: Entity<ContextServerRegistry>,
        templates: Arc<Templates>,
        model: Option<Arc<dyn LanguageModel>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let profile_id = AgentSettings::get_global(cx).default_profile.clone();
        let action_log = cx.new(|_cx| ActionLog::new(project.clone()));
        let (prompt_capabilities_tx, prompt_capabilities_rx) =
            watch::channel(Self::prompt_capabilities(model.as_deref()));
        Self {
            id: acp::SessionId(uuid::Uuid::new_v4().to_string().into()),
            prompt_id: PromptId::new(),
            updated_at: Utc::now(),
            title: None,
            pending_title_generation: None,
            summary: None,
            messages: Vec::new(),
            memory_segments: Vec::new(),
            memory_next_id: 0,
            completion_mode: AgentSettings::get_global(cx).preferred_completion_mode,
            running_turn: None,
            pending_message: None,
            tools: BTreeMap::default(),
            tool_use_limit_reached: false,
            request_token_usage: HashMap::default(),
            cumulative_token_usage: TokenUsage::default(),
            initial_project_snapshot: {
                let project_snapshot = Self::project_snapshot(project.clone(), cx);
                cx.foreground_executor()
                    .spawn(async move { Some(project_snapshot.await) })
                    .shared()
            },
            context_server_registry,
            profile_id,
            project_context,
            templates,
            precise_active_tokens: None,
            precise_max_tokens: None,
            precise_per_message_tokens: None,
            model,
            summarization_model: None,
            prompt_capabilities_tx,
            prompt_capabilities_rx,
            project,
            action_log,
        }
    }

    pub fn id(&self) -> &acp::SessionId {
        &self.id
    }

    /// Returns an immutable slice of all messages in the thread.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Archive (store) a contiguous inclusive range of messages, replacing them
    /// with a single placeholder summary message. Returns the new memory segment id.
    ///
    /// Backwards-compatible wrapper that does not allow a custom summary. Calls
    /// `store_memory_segment_with_summary` with `None`.
    pub fn store_memory_segment(
        &mut self,
        start: usize,
        end: usize,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<u64> {
        self.store_memory_segment_with_summary(start, end, None, cx)
    }

    /// Archive (store) a contiguous inclusive range of messages with an optional
    /// caller-provided custom summary. If `custom_summary` is `None` or empty
    /// after trimming, an automatic summary is synthesized (previous behavior).
    ///
    /// Custom summary handling:
    /// * Trim whitespace
    /// * Collapse internal newlines to spaces
    /// * Enforce a maximum length (96 chars); truncate with an ellipsis if exceeded
    pub fn store_memory_segment_with_summary(
        &mut self,
        start: usize,
        end: usize,
        custom_summary: Option<&str>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<u64> {
        if start > end {
            return Err(anyhow::anyhow!("start index greater than end index"));
        }
        if end >= self.messages.len() {
            return Err(anyhow::anyhow!(
                "range {}..={} out of bounds (len={})",
                start,
                end,
                self.messages.len()
            ));
        }
        if self.memory_range_overlaps(start, end) {
            return Err(anyhow::anyhow!(
                "range {}..={} overlaps an existing archived memory segment",
                start,
                end
            ));
        }

        // Extract messages
        let removed = self.extract_messages(start..=end, cx)?;
        if removed.is_empty() {
            return Err(anyhow::anyhow!("empty range cannot be archived"));
        }

        // Utility: truncate helper
        fn truncate(s: &str, max: usize) -> String {
            if s.len() <= max {
                s.to_string()
            } else {
                let mut out = s.chars().take(max).collect::<String>();
                out.push('…');
                out
            }
        }

        let mut char_total = 0usize;
        let mut rendered: Vec<String> = Vec::with_capacity(removed.len());
        for m in &removed {
            let md = m.to_markdown();
            char_total += md.len();
            rendered.push(md);
        }

        // Auto summary (legacy behavior)
        let auto_summary = if rendered.len() == 1 {
            format!("Single message: {}", truncate(&rendered[0], 48))
        } else {
            let first = truncate(&rendered.first().unwrap(), 48);
            let last = truncate(&rendered.last().unwrap(), 48);
            format!(
                "{} msgs | first: {} | last: {}",
                rendered.len(),
                first,
                last
            )
        };

        // Prepare a sanitized custom summary if provided.
        let summary = custom_summary
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                // Replace internal newlines / tabs with single spaces and collapse runs of whitespace.
                let cleaned = s
                    .chars()
                    .map(|c| {
                        if c == '\n' || c == '\r' || c == '\t' {
                            ' '
                        } else {
                            c
                        }
                    })
                    .collect::<String>();
                // Collapse multiple spaces
                let mut collapsed = String::with_capacity(cleaned.len());
                let mut last_space = false;
                for ch in cleaned.chars() {
                    if ch.is_whitespace() {
                        if !last_space {
                            collapsed.push(' ');
                        }
                        last_space = true;
                    } else {
                        collapsed.push(ch);
                        last_space = false;
                    }
                }
                // Enforce max length
                let max_len = 96;
                if collapsed.chars().count() > max_len {
                    let mut truncated = collapsed.chars().take(max_len).collect::<String>();
                    truncated.push('…');
                    truncated
                } else {
                    collapsed
                }
            })
            .unwrap_or(auto_summary);

        let id = self.memory_next_id;

        // Construct placeholder text now (need it for token counting)
        let placeholder_text = format!("[memory:{}] {}", id, summary);

        // Heuristic token counts for removed messages vs placeholder
        let removed_req: Vec<_> = removed.iter().flat_map(|m| m.to_request()).collect();
        let message_token_count = crate::token_usage::heuristic_token_count(&removed_req);

        // Build placeholder message for token counting
        let placeholder_message = Message::Agent(AgentMessage {
            content: vec![AgentMessageContent::Text(placeholder_text.clone())],
            tool_results: Default::default(),
        });
        let placeholder_req: Vec<_> = placeholder_message.to_request();
        let placeholder_token_count = crate::token_usage::heuristic_token_count(&placeholder_req);

        self.memory_next_id = self.memory_next_id.saturating_add(1);

        // Insert placeholder at start index
        self.insert_messages(start, vec![placeholder_message], cx)?;

        // Record segment
        let seg = ThreadMemorySegment {
            id,
            start,
            end,
            summary: summary.clone().into(),
            message_char_count: char_total,
            message_count: removed.len(),
            stored_epoch_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default(),
            placeholder_char_count: placeholder_text.len(),
            message_token_count,
            placeholder_token_count,
            messages: removed,
        };
        self.memory_segments.push(seg);
        // Persist archive state (log errors, do not abort user-facing operation).
        self.persist_memory_segments().log_err();

        // Invalidate cached precise token usage & schedule recompute for updated “active” usage
        self.invalidate_and_schedule_token_recount(cx);

        Ok(id)
    }

    /// Restore a previously archived memory segment by id:
    /// - Removes the placeholder if still present at the original start index.
    /// - Reinserts the original messages in their prior order.
    /// Segment remains archived (not pruned) after restore.
    pub fn restore_memory_segment(
        &mut self,
        id: u64,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let seg_index = self
            .memory_segments
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("no memory segment with id {}", id))?;

        let seg_start;
        {
            let seg = &self.memory_segments[seg_index];
            seg_start = seg.start;
        }

        // If placeholder still present at seg_start and matches id, remove it.
        if seg_start < self.messages.len() {
            let is_placeholder = match &self.messages[seg_start] {
                Message::Agent(agent_msg) => agent_msg
                    .content
                    .iter()
                    .any(|c| matches!(c, AgentMessageContent::Text(t) if t.starts_with(&format!("[memory:{}]", id)))),
                _ => false,
            };
            if is_placeholder {
                // Remove placeholder directly
                self.messages.remove(seg_start);
            }
        }

        // Reinsert archived messages at original start index
        let archived = self.memory_segments[seg_index].messages.clone(); // clone to keep archive intact
        self.insert_messages(seg_start, archived, cx)?;

        // Restoring changes active context size
        self.invalidate_and_schedule_token_recount(cx);
        // Persist after restore to capture removal of placeholder and maintain archive continuity on disk.
        self.persist_memory_segments().log_err();
        Ok(())
    }

    /// Restore all archived memory segments, generate a detailed summary over the full
    /// expanded message set, then re-archive the segments preserving their original
    /// summaries. This ensures summaries are created from the complete, uncompressed
    /// conversation while retaining memory compaction afterward.
    pub fn summarize_with_full_restore_and_rearchive(
        &mut self,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        // Snapshot original segments (clone so we can rebuild after).
        let original_segments = self.memory_segments.clone();

        // Restore all segments (ascending start order to keep index math predictable).
        let mut sorted = original_segments.clone();
        sorted.sort_by_key(|s| s.start);
        for seg in &sorted {
            // Ignore failures (e.g., already restored) but log them.
            if let Err(e) = self.restore_memory_segment(seg.id, cx) {
                log::debug!(
                    "restore_memory_segment({}) failed during full restore: {}",
                    seg.id,
                    e
                );
            }
        }

        // Build detailed summarization request using the fully restored messages.
        let Some(model) = self.summarization_model.clone() else {
            // No model available; nothing further to do. Re-archive segments immediately.
            self.rearchive_segments_after_full_restore(original_segments, cx)?;
            return Ok(());
        };

        let mut request = LanguageModelRequest {
            intent: Some(CompletionIntent::ThreadSummarization),
            temperature: AgentSettings::temperature_for_model(&model, cx),
            ..Default::default()
        };

        for (i, message) in self.messages.iter().enumerate() {
            let mut reqs = message.to_request();
            if let Some(first) = reqs.get_mut(0) {
                first
                    .content
                    .insert(0, language_model::MessageContent::Text(format!("[@{}]", i)));
            }
            request.messages.extend(reqs);
        }

        // Use the detailed prompt (same as existing summary generation path).
        request.messages.push(LanguageModelRequestMessage {
            role: Role::User,
            content: vec![SUMMARIZE_THREAD_DETAILED_PROMPT.into()],
            cache: false,
        });

        // Capture segments for re-archiving after async completion.
        let segments_for_rearchive: Vec<_> = original_segments
            .into_iter()
            .map(|s| (s.start, s.end, s.message_count, s.summary.to_string()))
            .collect();

        // Spawn async summarization; once complete re-archive segments.
        cx.spawn(async move |this, cx| {
            let mut summary_accum = String::new();
            let mut stream = model.stream_completion(request, cx).await?;
            while let Some(evt) = stream.next().await {
                let evt = evt?;
                match evt {
                    LanguageModelCompletionEvent::Text(text) => {
                        let mut lines = text.lines();
                        summary_accum.extend(lines.next());
                    }
                    LanguageModelCompletionEvent::StatusUpdate(
                        CompletionRequestStatus::UsageUpdated { amount, limit },
                    ) => {
                        this.update(cx, |thread, cx| {
                            thread.update_model_request_usage(amount, limit, cx);
                        })?;
                    }
                    _ => {}
                }
            }

            let final_summary = SharedString::from(summary_accum);
            // Set summary
            this.update(cx, |thread, cx| {
                thread.summary = Some(final_summary.clone());
                cx.notify();
            })?;

            // Re-archive segments after summary generation.
            this.update(cx, |thread, cx| {
                thread.rearchive_segments_after_full_restore(
                    segments_for_rearchive
                        .iter()
                        .map(|(start, end, count, summary)| ThreadMemorySegment {
                            // id will be reassigned; placeholder metadata below
                            id: 0,
                            start: *start,
                            end: *end,
                            summary: summary.clone().into(),
                            message_char_count: 0,
                            message_count: *count,
                            stored_epoch_ms: 0,
                            placeholder_char_count: 0,
                            message_token_count: 0,
                            placeholder_token_count: 0,
                            messages: Vec::new(), // not needed here
                        })
                        .collect(),
                    cx,
                )
            })??;

            Ok(())
        })
        .detach();

        Ok(())
    }

    /// Internal helper: re-archive previously restored segments.
    /// Accepts the original segments (with original start/end/message_count/summary).
    fn rearchive_segments_after_full_restore(
        &mut self,
        original_segments: Vec<ThreadMemorySegment>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        // Sort by original start to compute index shifts deterministically.
        let mut segs = original_segments;
        segs.sort_by_key(|s| s.start);

        // After full restore, each segment's messages occupy [start, start + message_count - 1]
        // but earlier restorations expanded the message vector, shifting later start indices.
        // Compute cumulative shift and re-archive each segment using its original summary.
        let mut cumulative_shift: isize = 0;

        // Clear existing memory_segments; they will be rebuilt by store calls.
        self.memory_segments.clear();

        for seg in segs {
            let adjusted_start = (seg.start as isize + cumulative_shift) as usize;
            let adjusted_end = adjusted_start + seg.message_count.saturating_sub(1);
            if let Err(e) = self.store_memory_segment_with_summary(
                adjusted_start,
                adjusted_end,
                Some(seg.summary.as_ref()),
                cx,
            ) {
                log::debug!(
                    "Failed to re-archive segment (orig_id={}, start={}, end={}): {}",
                    seg.id,
                    adjusted_start,
                    adjusted_end,
                    e
                );
                continue;
            }
            // Archiving replaces message_count messages with one placeholder -> shift increases by (message_count - 1)
            cumulative_shift += (seg.message_count as isize).saturating_sub(1);
        }

        Ok(())
    }

    /// Load (inspect) an archived memory segment by id without modifying the thread.
    /// Returns a tuple of (metadata_json, messages_markdown).
    /// The metadata includes a token_savings_estimate computed as (archived_chars - placeholder_chars).
    fn invalidate_and_schedule_token_recount(&mut self, cx: &mut Context<Self>) {
        self.precise_active_tokens = None;
        self.precise_per_message_tokens = None;
        self.spawn_compute_precise_usage(cx);
    }

    /// Returns (active_usage, full_usage):
    /// active = current messages (with placeholders)
    /// full   = hypothetical if all archived segments were expanded
    pub fn active_and_full_token_usage(
        &self,
    ) -> Option<(acp_thread::TokenUsage, acp_thread::TokenUsage)> {
        let model = self.model.clone()?;
        let max_tokens = model.max_token_count_for_mode(self.completion_mode.into());

        // Active token count: precise if available else heuristic
        let active_used = if let Some(precise) = self.precise_active_tokens {
            precise
        } else {
            let req: Vec<_> = self
                .messages
                .iter()
                .enumerate()
                .flat_map(|(i, m)| {
                    let mut reqs = m.to_request();
                    if let Some(first) = reqs.get_mut(0) {
                        first
                            .content
                            .insert(0, language_model::MessageContent::Text(format!("[@{}]", i)));
                    }
                    reqs
                })
                .collect();
            crate::token_usage::heuristic_token_count(&req) as u64
        };

        // Sum deltas (archived - placeholder)
        let mut delta: i64 = 0;
        for seg in &self.memory_segments {
            let extra = seg
                .message_token_count
                .saturating_sub(seg.placeholder_token_count);
            delta += extra as i64;
        }
        let full_used = (active_used as i64 + delta).max(0) as u64;

        let active = acp_thread::TokenUsage {
            max_tokens,
            used_tokens: active_used,
        };
        let full = acp_thread::TokenUsage {
            max_tokens,
            used_tokens: full_used,
        };
        Some((active, full))
    }

    pub fn load_memory_segment(&self, id: u64) -> anyhow::Result<(serde_json::Value, Vec<String>)> {
        let seg = self
            .memory_segments
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("no memory segment with id {}", id))?;

        let token_savings_estimate = seg
            .message_char_count
            .saturating_sub(seg.placeholder_char_count);

        let meta = serde_json::json!({
            "id": seg.id,
            "start": seg.start,
            "end": seg.end,
            "count": seg.message_count,
            "chars": seg.message_char_count,
            "summary": seg.summary.as_ref(),
            "stored_epoch_ms": seg.stored_epoch_ms,
            "placeholder_chars": seg.placeholder_char_count,
            "token_savings_estimate": token_savings_estimate
        });

        let messages_markdown: Vec<String> = seg.messages.iter().map(|m| m.to_markdown()).collect();

        Ok((meta, messages_markdown))
    }

    /// List all archived memory segments (thread-scoped).
    #[allow(dead_code)]
    pub(crate) fn list_memory_segments(&self) -> &[ThreadMemorySegment] {
        &self.memory_segments
    }

    /// Return metadata for all archived memory segments without exposing the internal
    /// `ThreadMemorySegment` type. Each tuple contains:
    /// (id, start, end, message_count, message_char_count, placeholder_char_count,
    ///  token_savings_estimate, summary, stored_epoch_ms)
    pub fn memory_segment_metas(
        &self,
    ) -> Vec<(u64, usize, usize, usize, usize, usize, usize, String, u128)> {
        self.memory_segments
            .iter()
            .map(|seg| {
                (
                    seg.id,
                    seg.start,
                    seg.end,
                    seg.message_count,
                    seg.message_char_count,
                    seg.placeholder_char_count,
                    seg.message_char_count
                        .saturating_sub(seg.placeholder_char_count),
                    seg.summary.to_string(),
                    seg.stored_epoch_ms,
                )
            })
            .collect()
    }

    /// Returns true if the inclusive range [start, end] overlaps any stored memory segment.
    /// Overlap logic: two closed intervals [a,b] and [c,d] overlap if not (b < c || d < a).
    pub fn memory_range_overlaps(&self, start: usize, end: usize) -> bool {
        if start > end {
            return false;
        }
        self.memory_segments
            .iter()
            .any(|seg| !(end < seg.start || start > seg.end))
    }

    /// Extract (remove) a contiguous inclusive range of messages from the thread,
    /// returning the removed messages in their original order.
    /// "Why": Needed by memory/context compaction to archive messages while shrinking
    /// the active context. This is exposed (rather than direct field access) to
    /// centralize bounds checking and notification logic.
    pub fn extract_messages(
        &mut self,
        range: std::ops::RangeInclusive<usize>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Vec<Message>> {
        let start = *range.start();
        let end = *range.end();
        if start > end {
            return Err(anyhow::anyhow!("start index greater than end index"));
        }
        if end >= self.messages.len() {
            return Err(anyhow::anyhow!(
                "range {}..={} out of bounds (len={})",
                start,
                end,
                self.messages.len()
            ));
        }
        let count = end - start + 1;
        let mut removed = Vec::with_capacity(count);
        // Remove in-place by repeatedly removing at 'start'
        for _ in 0..count {
            removed.push(self.messages.remove(start));
        }
        // Any cached summary may now be invalid
        self.summary = None;
        cx.notify();
        Ok(removed)
    }

    /// Insert a sequence of messages starting at the given index (clamped to len).
    /// "Why": Allows restoration of archived messages (memory restore) at an arbitrary
    /// point without exposing internal vector operations elsewhere.
    pub fn insert_messages(
        &mut self,
        index: usize,
        mut msgs: Vec<Message>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let insert_at = index.min(self.messages.len());
        // Preserve order: insert by extending via splice pattern
        if insert_at == self.messages.len() {
            self.messages.extend(msgs.drain(..));
        } else {
            for (offset, msg) in msgs.drain(..).enumerate() {
                self.messages.insert(insert_at + offset, msg);
            }
        }
        self.summary = None;
        cx.notify();
        Ok(())
    }

    /// Replace a single message at `index` with an agent placeholder message containing
    /// the provided text. Fails if index is out of bounds.
    /// "Why": Memory archiving uses placeholders to retain a compact semantic summary
    /// and a handle reference in place of original verbose content.
    pub fn set_placeholder(
        &mut self,
        index: usize,
        placeholder_text: String,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if index >= self.messages.len() {
            return Err(anyhow::anyhow!(
                "placeholder index {} out of bounds (len={})",
                index,
                self.messages.len()
            ));
        }
        self.messages[index] = Message::Agent(AgentMessage {
            content: vec![AgentMessageContent::Text(placeholder_text)],
            tool_results: Default::default(),
        });
        self.summary = None;
        cx.notify();
        Ok(())
    }

    /// Remove a single message at index (used when eliminating a placeholder after restore).
    /// "Why": Keeps placeholder removal logic consistent and bounds-checked.
    pub fn remove_message(&mut self, index: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
        if index >= self.messages.len() {
            return Err(anyhow::anyhow!(
                "remove index {} out of bounds (len={})",
                index,
                self.messages.len()
            ));
        }
        self.messages.remove(index);
        self.summary = None;
        cx.notify();
        Ok(())
    }

    pub fn replay(
        &mut self,
        cx: &mut Context<Self>,
    ) -> mpsc::UnboundedReceiver<Result<ThreadEvent>> {
        let (tx, rx) = mpsc::unbounded();
        let stream = ThreadEventStream(tx);
        for message in &self.messages {
            match message {
                Message::User(user_message) => stream.send_user_message(user_message),
                Message::Agent(assistant_message) => {
                    for content in &assistant_message.content {
                        match content {
                            AgentMessageContent::Text(text) => stream.send_text(text),
                            AgentMessageContent::Thinking { text, .. } => {
                                stream.send_thinking(text)
                            }
                            AgentMessageContent::RedactedThinking(_) => {}
                            AgentMessageContent::ToolUse(tool_use) => {
                                self.replay_tool_call(
                                    tool_use,
                                    assistant_message.tool_results.get(&tool_use.id),
                                    &stream,
                                    cx,
                                );
                            }
                        }
                    }
                }
                Message::Resume => {}
            }
        }
        rx
    }

    fn replay_tool_call(
        &self,
        tool_use: &LanguageModelToolUse,
        tool_result: Option<&LanguageModelToolResult>,
        stream: &ThreadEventStream,
        cx: &mut Context<Self>,
    ) {
        let tool = self.tools.get(tool_use.name.as_ref()).cloned().or_else(|| {
            self.context_server_registry
                .read(cx)
                .servers()
                .find_map(|(_, tools)| {
                    if let Some(tool) = tools.get(tool_use.name.as_ref()) {
                        Some(tool.clone())
                    } else {
                        None
                    }
                })
        });

        let Some(tool) = tool else {
            stream
                .0
                .unbounded_send(Ok(ThreadEvent::ToolCall(acp::ToolCall {
                    meta: None,
                    id: acp::ToolCallId(tool_use.id.to_string().into()),
                    title: tool_use.name.to_string(),
                    kind: acp::ToolKind::Other,
                    status: acp::ToolCallStatus::Failed,
                    content: Vec::new(),
                    locations: Vec::new(),
                    raw_input: Some(tool_use.input.clone()),
                    raw_output: None,
                })))
                .ok();
            return;
        };

        let title = tool.initial_title(tool_use.input.clone(), cx);
        let kind = tool.kind();
        stream.send_tool_call(&tool_use.id, title, kind, tool_use.input.clone());

        let output = tool_result
            .as_ref()
            .and_then(|result| result.output.clone());
        if let Some(output) = output.clone() {
            let tool_event_stream = ToolCallEventStream::new(
                tool_use.id.clone(),
                stream.clone(),
                Some(self.project.read(cx).fs().clone()),
            );
            tool.replay(tool_use.input.clone(), output, tool_event_stream, cx)
                .log_err();
        }

        stream.update_tool_call_fields(
            &tool_use.id,
            acp::ToolCallUpdateFields {
                status: Some(
                    tool_result
                        .as_ref()
                        .map_or(acp::ToolCallStatus::Failed, |result| {
                            if result.is_error {
                                acp::ToolCallStatus::Failed
                            } else {
                                acp::ToolCallStatus::Completed
                            }
                        }),
                ),
                raw_output: output,
                ..Default::default()
            },
        );
    }

    pub fn from_db(
        id: acp::SessionId,
        db_thread: DbThread,
        project: Entity<Project>,
        project_context: Entity<ProjectContext>,
        context_server_registry: Entity<ContextServerRegistry>,
        action_log: Entity<ActionLog>,
        templates: Arc<Templates>,
        cx: &mut Context<Self>,
    ) -> Self {
        let profile_id = db_thread
            .profile
            .unwrap_or_else(|| AgentSettings::get_global(cx).default_profile.clone());
        let model = LanguageModelRegistry::global(cx).update(cx, |registry, cx| {
            db_thread
                .model
                .and_then(|model| {
                    let model = SelectedModel {
                        provider: model.provider.clone().into(),
                        model: model.model.into(),
                    };
                    registry.select_model(&model, cx)
                })
                .or_else(|| registry.default_model())
                .map(|model| model.model)
        });
        let (prompt_capabilities_tx, prompt_capabilities_rx) =
            watch::channel(Self::prompt_capabilities(model.as_deref()));

        let mut thread = Self {
            id,
            prompt_id: PromptId::new(),
            title: if db_thread.title.is_empty() {
                None
            } else {
                Some(db_thread.title.clone())
            },
            pending_title_generation: None,
            summary: db_thread.detailed_summary,
            messages: db_thread.messages,
            memory_segments: Vec::new(),
            memory_next_id: 0,
            completion_mode: db_thread.completion_mode.unwrap_or_default(),
            running_turn: None,
            pending_message: None,
            tools: BTreeMap::default(),
            tool_use_limit_reached: false,
            request_token_usage: db_thread.request_token_usage.clone(),
            cumulative_token_usage: db_thread.cumulative_token_usage,
            initial_project_snapshot: Task::ready(db_thread.initial_project_snapshot).shared(),
            context_server_registry,
            profile_id,
            project_context,
            templates,
            // Newly added precise token usage cache fields (were missing causing E0063)
            precise_active_tokens: None,
            precise_max_tokens: None,
            precise_per_message_tokens: None,
            model,
            summarization_model: None,
            project,
            action_log,
            updated_at: db_thread.updated_at,
            prompt_capabilities_tx,
            prompt_capabilities_rx,
        };

        if let Err(err) = thread.load_memory_segments_from_disk() {
            log::warn!(
                "failed to load memory segments for thread {}: {err:#}",
                thread.id
            );
        }

        thread
    }

    pub fn to_db(&self, cx: &App) -> Task<DbThread> {
        let initial_project_snapshot = self.initial_project_snapshot.clone();
        let mut thread = DbThread {
            title: self.title(),
            messages: self.messages.clone(),
            updated_at: self.updated_at,
            detailed_summary: self.summary.clone(),
            initial_project_snapshot: None,
            cumulative_token_usage: self.cumulative_token_usage,
            request_token_usage: self.request_token_usage.clone(),
            model: self.model.as_ref().map(|model| DbLanguageModel {
                provider: model.provider_id().to_string(),
                model: model.name().0.to_string(),
            }),
            completion_mode: Some(self.completion_mode),
            profile: Some(self.profile_id.clone()),
        };

        cx.background_spawn(async move {
            let initial_project_snapshot = initial_project_snapshot.await;
            thread.initial_project_snapshot = initial_project_snapshot;
            thread
        })
    }

    /// Create a snapshot of the current project state including git information and unsaved buffers.
    fn project_snapshot(
        project: Entity<Project>,
        cx: &mut Context<Self>,
    ) -> Task<Arc<agent::thread::ProjectSnapshot>> {
        let git_store = project.read(cx).git_store().clone();
        let worktree_snapshots: Vec<_> = project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| Self::worktree_snapshot(worktree, git_store.clone(), cx))
            .collect();

        cx.spawn(async move |_, _| {
            let worktree_snapshots = futures::future::join_all(worktree_snapshots).await;

            Arc::new(ProjectSnapshot {
                worktree_snapshots,
                timestamp: Utc::now(),
            })
        })
    }

    fn worktree_snapshot(
        worktree: Entity<project::Worktree>,
        git_store: Entity<GitStore>,
        cx: &App,
    ) -> Task<agent::thread::WorktreeSnapshot> {
        cx.spawn(async move |cx| {
            // Get worktree path and snapshot
            let worktree_info = cx.update(|app_cx| {
                let worktree = worktree.read(app_cx);
                let path = worktree.abs_path().to_string_lossy().into_owned();
                let snapshot = worktree.snapshot();
                (path, snapshot)
            });

            let Ok((worktree_path, _snapshot)) = worktree_info else {
                return WorktreeSnapshot {
                    worktree_path: String::new(),
                    git_state: None,
                };
            };

            let git_state = git_store
                .update(cx, |git_store, cx| {
                    git_store
                        .repositories()
                        .values()
                        .find(|repo| {
                            repo.read(cx)
                                .abs_path_to_repo_path(&worktree.read(cx).abs_path())
                                .is_some()
                        })
                        .cloned()
                })
                .ok()
                .flatten()
                .map(|repo| {
                    repo.update(cx, |repo, _| {
                        let current_branch =
                            repo.branch.as_ref().map(|branch| branch.name().to_owned());
                        repo.send_job(None, |state, _| async move {
                            let RepositoryState::Local { backend, .. } = state else {
                                return GitState {
                                    remote_url: None,
                                    head_sha: None,
                                    current_branch,
                                    diff: None,
                                };
                            };

                            let remote_url = backend.remote_url("origin");
                            let head_sha = backend.head_sha().await;
                            let diff = backend.diff(DiffType::HeadToWorktree).await.ok();

                            GitState {
                                remote_url,
                                head_sha,
                                current_branch,
                                diff,
                            }
                        })
                    })
                });

            let git_state = match git_state {
                Some(git_state) => match git_state.ok() {
                    Some(git_state) => git_state.await.ok(),
                    None => None,
                },
                None => None,
            };

            WorktreeSnapshot {
                worktree_path,
                git_state,
            }
        })
    }

    pub fn project_context(&self) -> &Entity<ProjectContext> {
        &self.project_context
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn action_log(&self) -> &Entity<ActionLog> {
        &self.action_log
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.title.is_none()
    }

    pub fn model(&self) -> Option<&Arc<dyn LanguageModel>> {
        self.model.as_ref()
    }

    pub fn set_model(&mut self, model: Arc<dyn LanguageModel>, cx: &mut Context<Self>) {
        let old_usage = self.latest_token_usage();
        self.model = Some(model);
        let new_caps = Self::prompt_capabilities(self.model.as_deref());
        let new_usage = self.latest_token_usage();
        if old_usage != new_usage {
            cx.emit(TokenUsageUpdated(new_usage));
        }
        self.prompt_capabilities_tx.send(new_caps).log_err();
        cx.notify()
    }

    pub fn summarization_model(&self) -> Option<&Arc<dyn LanguageModel>> {
        self.summarization_model.as_ref()
    }

    pub fn set_summarization_model(
        &mut self,
        model: Option<Arc<dyn LanguageModel>>,
        cx: &mut Context<Self>,
    ) {
        self.summarization_model = model;
        cx.notify()
    }

    pub fn completion_mode(&self) -> CompletionMode {
        self.completion_mode
    }

    pub fn set_completion_mode(&mut self, mode: CompletionMode, cx: &mut Context<Self>) {
        let old_usage = self.latest_token_usage();
        self.completion_mode = mode;
        let new_usage = self.latest_token_usage();
        if old_usage != new_usage {
            cx.emit(TokenUsageUpdated(new_usage));
        }
        cx.notify()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn last_message(&self) -> Option<Message> {
        if let Some(message) = self.pending_message.clone() {
            Some(Message::Agent(message))
        } else {
            self.messages.last().cloned()
        }
    }

    pub fn add_default_tools(
        &mut self,
        environment: Rc<dyn ThreadEnvironment>,
        cx: &mut Context<Self>,
    ) {
        let language_registry = self.project.read(cx).languages().clone();
        self.add_tool(CopyPathTool::new(self.project.clone()));
        self.add_tool(CreateDirectoryTool::new(self.project.clone()));
        self.add_tool(DeletePathTool::new(
            self.project.clone(),
            self.action_log.clone(),
        ));
        self.add_tool(DiagnosticsTool::new(self.project.clone()));
        self.add_tool(EditFileTool::new(
            self.project.clone(),
            cx.weak_entity(),
            language_registry,
        ));
        self.add_tool(FetchTool::new(self.project.read(cx).client().http_client()));
        self.add_tool(FindPathTool::new(self.project.clone()));
        self.add_tool(GrepTool::new(self.project.clone()));
        self.add_tool(ListDirectoryTool::new(self.project.clone()));
        // self.add_tool(ListHistoryTool::new(cx.weak_entity())); // disabled by default
        // ChatHistoryAgentTool removed
        self.add_tool(MemoryAgentTool::new(cx.weak_entity()));
        self.add_tool(TokenUsageTool::new(cx.weak_entity()));

        self.add_tool(MovePathTool::new(self.project.clone()));
        self.add_tool(NowTool);
        self.add_tool(OpenTool::new(self.project.clone()));
        self.add_tool(ReadFileTool::new(
            self.project.clone(),
            self.action_log.clone(),
        ));
        self.add_tool(TerminalTool::new(self.project.clone(), environment.clone()));
        self.add_tool(EnhancedTerminalTool::new(
            self.project.clone(),
            environment.clone(),
        ));
        self.add_tool(ShellDetectorTool::new());
        self.add_tool(ThinkingTool);
        self.add_tool(WebSearchTool);
    }

    pub fn add_tool<T: AgentTool>(&mut self, tool: T) {
        self.tools.insert(T::name().into(), tool.erase());
    }

    pub fn remove_tool(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    /// Return a cloned tool handle by name if it is currently registered and enabled.
    pub fn tool(&self, name: &str) -> Option<Arc<dyn AnyAgentTool>> {
        self.tools.get(name).cloned()
    }

    pub fn profile(&self) -> &AgentProfileId {
        &self.profile_id
    }

    pub fn set_profile(&mut self, profile_id: AgentProfileId) {
        self.profile_id = profile_id;
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(running_turn) = self.running_turn.take() {
            running_turn.cancel();
        }
        self.flush_pending_message(cx);
    }

    fn update_token_usage(&mut self, update: language_model::TokenUsage, cx: &mut Context<Self>) {
        let Some(last_user_message) = self.last_user_message() else {
            return;
        };

        self.request_token_usage
            .insert(last_user_message.id.clone(), update);
        cx.emit(TokenUsageUpdated(self.latest_token_usage()));
        cx.notify();
    }

    pub fn truncate(&mut self, message_id: UserMessageId, cx: &mut Context<Self>) -> Result<()> {
        self.cancel(cx);
        let Some(position) = self.messages.iter().position(
            |msg| matches!(msg, Message::User(UserMessage { id, .. }) if id == &message_id),
        ) else {
            return Err(anyhow!("Message not found"));
        };

        for message in self.messages.drain(position..) {
            match message {
                Message::User(message) => {
                    self.request_token_usage.remove(&message.id);
                }
                Message::Agent(_) | Message::Resume => {}
            }
        }
        self.summary = None;
        cx.notify();
        Ok(())
    }

    pub fn latest_token_usage(&self) -> Option<acp_thread::TokenUsage> {
        let last_user_message = self.last_user_message()?;
        let tokens = self.request_token_usage.get(&last_user_message.id)?;
        let model = self.model.clone()?;

        Some(acp_thread::TokenUsage {
            max_tokens: model.max_token_count_for_mode(self.completion_mode.into()),
            used_tokens: tokens.total_tokens(),
        })
    }

    pub fn resume(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<mpsc::UnboundedReceiver<Result<ThreadEvent>>> {
        self.messages.push(Message::Resume);
        cx.notify();

        log::trace!("Total messages in thread: {}", self.messages.len());
        self.run_turn(cx)
    }

    /// Sending a message results in the model streaming a response, which could include tool calls.
    /// After calling tools, the model will stops and waits for any outstanding tool calls to be completed and their results sent.
    /// The returned channel will report all the occurrences in which the model stops before erroring or ending its turn.
    pub fn send<T>(
        &mut self,
        id: UserMessageId,
        content: impl IntoIterator<Item = T>,
        cx: &mut Context<Self>,
    ) -> Result<mpsc::UnboundedReceiver<Result<ThreadEvent>>>
    where
        T: Into<UserMessageContent>,
    {
        let model = self.model().context("No language model configured")?;

        log::info!("Thread::send called with model: {}", model.name().0);
        self.advance_prompt_id();

        let content = content.into_iter().map(Into::into).collect::<Vec<_>>();
        log::trace!("Thread::send content: {:?}", content);

        self.messages
            .push(Message::User(UserMessage { id, content }));
        cx.notify();

        log::trace!("Total messages in thread: {}", self.messages.len());
        // Kick off (non-blocking) precise token usage computation.
        self.spawn_compute_precise_usage(cx);
        self.run_turn(cx)
    }

    fn run_turn(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<mpsc::UnboundedReceiver<Result<ThreadEvent>>> {
        self.cancel(cx);

        let model = self.model.clone().context("No language model configured")?;
        let profile = AgentSettings::get_global(cx)
            .profiles
            .get(&self.profile_id)
            .context("Profile not found")?;
        let (events_tx, events_rx) = mpsc::unbounded::<Result<ThreadEvent>>();
        let event_stream = ThreadEventStream(events_tx);
        let message_ix = self.messages.len().saturating_sub(1);
        self.tool_use_limit_reached = false;
        self.summary = None;
        self.running_turn = Some(RunningTurn {
            event_stream: event_stream.clone(),
            tools: self.enabled_tools(profile, &model, cx),
            _task: cx.spawn(async move |this, cx| {
                log::trace!("Starting agent turn execution");

                let turn_result = Self::run_turn_internal(&this, model, &event_stream, cx).await;
                _ = this.update(cx, |this, cx| this.flush_pending_message(cx));

                match turn_result {
                    Ok(()) => {
                        log::trace!("Turn execution completed");
                        event_stream.send_stop(acp::StopReason::EndTurn);
                    }
                    Err(error) => {
                        log::error!("Turn execution failed: {:?}", error);
                        match error.downcast::<CompletionError>() {
                            Ok(CompletionError::Refusal) => {
                                event_stream.send_stop(acp::StopReason::Refusal);
                                _ = this.update(cx, |this, _| this.messages.truncate(message_ix));
                            }
                            Ok(CompletionError::MaxTokens) => {
                                event_stream.send_stop(acp::StopReason::MaxTokens);
                            }
                            Ok(CompletionError::Other(error)) | Err(error) => {
                                event_stream.send_error(error);
                            }
                        }
                    }
                }

                _ = this.update(cx, |this, _| this.running_turn.take());
            }),
        });
        Ok(events_rx)
    }

    async fn run_turn_internal(
        this: &WeakEntity<Self>,
        model: Arc<dyn LanguageModel>,
        event_stream: &ThreadEventStream,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let mut attempt = 0;
        let mut intent = CompletionIntent::UserPrompt;
        loop {
            let request =
                this.update(cx, |this, cx| this.build_completion_request(intent, cx))??;

            telemetry::event!(
                "Agent Thread Completion",
                thread_id = this.read_with(cx, |this, _| this.id.to_string())?,
                prompt_id = this.read_with(cx, |this, _| this.prompt_id.to_string())?,
                model = model.telemetry_id(),
                model_provider = model.provider_id().to_string(),
                attempt
            );

            log::trace!("Calling model.stream_completion, attempt {}", attempt);
            let mut events = model
                .stream_completion(request, cx)
                .await
                .map_err(|error| anyhow!(error))?;
            let mut tool_results = FuturesUnordered::new();
            let mut error = None;
            while let Some(event) = events.next().await {
                log::trace!("Received completion event: {:?}", event);
                match event {
                    Ok(event) => {
                        tool_results.extend(this.update(cx, |this, cx| {
                            this.handle_completion_event(event, event_stream, cx)
                        })??);
                    }
                    Err(err) => {
                        error = Some(err);
                        break;
                    }
                }
            }

            let end_turn = tool_results.is_empty();
            while let Some(tool_result) = tool_results.next().await {
                log::trace!("Tool finished {:?}", tool_result);

                event_stream.update_tool_call_fields(
                    &tool_result.tool_use_id,
                    acp::ToolCallUpdateFields {
                        status: Some(if tool_result.is_error {
                            acp::ToolCallStatus::Failed
                        } else {
                            acp::ToolCallStatus::Completed
                        }),
                        raw_output: tool_result.output.clone(),
                        ..Default::default()
                    },
                );
                this.update(cx, |this, _cx| {
                    this.pending_message()
                        .tool_results
                        .insert(tool_result.tool_use_id.clone(), tool_result);
                })?;
            }

            this.update(cx, |this, cx| {
                this.flush_pending_message(cx);
                if this.title.is_none() && this.pending_title_generation.is_none() {
                    this.generate_title(cx);
                }
            })?;

            if let Some(error) = error {
                attempt += 1;
                let retry =
                    this.update(cx, |this, _| this.handle_completion_error(error, attempt))??;
                let timer = cx.background_executor().timer(retry.duration);
                event_stream.send_retry(retry);
                timer.await;
                this.update(cx, |this, _cx| {
                    if let Some(Message::Agent(message)) = this.messages.last() {
                        if message.tool_results.is_empty() {
                            intent = CompletionIntent::UserPrompt;
                            this.messages.push(Message::Resume);
                        }
                    }
                })?;
            } else if this.read_with(cx, |this, _| this.tool_use_limit_reached)? {
                return Err(language_model::ToolUseLimitReachedError.into());
            } else if end_turn {
                return Ok(());
            } else {
                intent = CompletionIntent::ToolResults;
                attempt = 0;
            }
        }
    }

    fn handle_completion_error(
        &mut self,
        error: LanguageModelCompletionError,
        attempt: u8,
    ) -> Result<acp_thread::RetryStatus> {
        if self.completion_mode == CompletionMode::Normal {
            return Err(anyhow!(error));
        }

        let Some(strategy) = Self::retry_strategy_for(&error) else {
            return Err(anyhow!(error));
        };

        let max_attempts = match &strategy {
            RetryStrategy::ExponentialBackoff { max_attempts, .. } => *max_attempts,
            RetryStrategy::Fixed { max_attempts, .. } => *max_attempts,
        };

        if attempt > max_attempts {
            return Err(anyhow!(error));
        }

        let delay = match &strategy {
            RetryStrategy::ExponentialBackoff { initial_delay, .. } => {
                let delay_secs = initial_delay.as_secs() * 2u64.pow((attempt - 1) as u32);
                Duration::from_secs(delay_secs)
            }
            RetryStrategy::Fixed { delay, .. } => *delay,
        };
        log::trace!("Retry attempt {attempt} with delay {delay:?}");

        Ok(acp_thread::RetryStatus {
            last_error: error.to_string().into(),
            attempt: attempt as usize,
            max_attempts: max_attempts as usize,
            started_at: Instant::now(),
            duration: delay,
        })
    }

    /// A helper method that's called on every streamed completion event.
    /// Returns an optional tool result task, which the main agentic loop will
    /// send back to the model when it resolves.
    fn handle_completion_event(
        &mut self,
        event: LanguageModelCompletionEvent,
        event_stream: &ThreadEventStream,
        cx: &mut Context<Self>,
    ) -> Result<Option<Task<LanguageModelToolResult>>> {
        log::trace!("Handling streamed completion event: {:?}", event);
        use LanguageModelCompletionEvent::*;

        match event {
            StartMessage { .. } => {
                self.flush_pending_message(cx);
                self.pending_message = Some(AgentMessage::default());
            }
            Text(new_text) => self.handle_text_event(new_text, event_stream, cx),
            Thinking { text, signature } => {
                self.handle_thinking_event(text, signature, event_stream, cx)
            }
            RedactedThinking { data } => self.handle_redacted_thinking_event(data, cx),
            ToolUse(tool_use) => {
                return Ok(self.handle_tool_use_event(tool_use, event_stream, cx));
            }
            ToolUseJsonParseError {
                id,
                tool_name,
                raw_input,
                json_parse_error,
            } => {
                return Ok(Some(Task::ready(
                    self.handle_tool_use_json_parse_error_event(
                        id,
                        tool_name,
                        raw_input,
                        json_parse_error,
                    ),
                )));
            }
            UsageUpdate(usage) => {
                telemetry::event!(
                    "Agent Thread Completion Usage Updated",
                    thread_id = self.id.to_string(),
                    prompt_id = self.prompt_id.to_string(),
                    model = self.model.as_ref().map(|m| m.telemetry_id()),
                    model_provider = self.model.as_ref().map(|m| m.provider_id().to_string()),
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    cache_creation_input_tokens = usage.cache_creation_input_tokens,
                    cache_read_input_tokens = usage.cache_read_input_tokens,
                );
                self.update_token_usage(usage, cx);
            }
            StatusUpdate(CompletionRequestStatus::UsageUpdated { amount, limit }) => {
                self.update_model_request_usage(amount, limit, cx);
            }
            StatusUpdate(
                CompletionRequestStatus::Started
                | CompletionRequestStatus::Queued { .. }
                | CompletionRequestStatus::Failed { .. },
            ) => {}
            StatusUpdate(CompletionRequestStatus::ToolUseLimitReached) => {
                self.tool_use_limit_reached = true;
            }
            Stop(StopReason::Refusal) => return Err(CompletionError::Refusal.into()),
            Stop(StopReason::MaxTokens) => return Err(CompletionError::MaxTokens.into()),
            Stop(StopReason::ToolUse | StopReason::EndTurn) => {}
        }

        Ok(None)
    }

    fn handle_text_event(
        &mut self,
        new_text: String,
        event_stream: &ThreadEventStream,
        cx: &mut Context<Self>,
    ) {
        event_stream.send_text(&new_text);

        let last_message = self.pending_message();
        if let Some(AgentMessageContent::Text(text)) = last_message.content.last_mut() {
            text.push_str(&new_text);
        } else {
            last_message
                .content
                .push(AgentMessageContent::Text(new_text));
        }

        cx.notify();
    }

    fn handle_thinking_event(
        &mut self,
        new_text: String,
        new_signature: Option<String>,
        event_stream: &ThreadEventStream,
        cx: &mut Context<Self>,
    ) {
        event_stream.send_thinking(&new_text);

        let last_message = self.pending_message();
        if let Some(AgentMessageContent::Thinking { text, signature }) =
            last_message.content.last_mut()
        {
            text.push_str(&new_text);
            *signature = new_signature.or(signature.take());
        } else {
            last_message.content.push(AgentMessageContent::Thinking {
                text: new_text,
                signature: new_signature,
            });
        }

        cx.notify();
    }

    fn handle_redacted_thinking_event(&mut self, data: String, cx: &mut Context<Self>) {
        let last_message = self.pending_message();
        last_message
            .content
            .push(AgentMessageContent::RedactedThinking(data));
        cx.notify();
    }

    fn handle_tool_use_event(
        &mut self,
        tool_use: LanguageModelToolUse,
        event_stream: &ThreadEventStream,
        cx: &mut Context<Self>,
    ) -> Option<Task<LanguageModelToolResult>> {
        cx.notify();

        let tool = self.tool(tool_use.name.as_ref());
        let mut title = SharedString::from(&tool_use.name);
        let mut kind = acp::ToolKind::Other;
        if let Some(tool) = tool.as_ref() {
            title = tool.initial_title(tool_use.input.clone(), cx);
            kind = tool.kind();
        }

        // Ensure the last message ends in the current tool use
        let last_message = self.pending_message();
        let push_new_tool_use = last_message.content.last_mut().is_none_or(|content| {
            if let AgentMessageContent::ToolUse(last_tool_use) = content {
                if last_tool_use.id == tool_use.id {
                    *last_tool_use = tool_use.clone();
                    false
                } else {
                    true
                }
            } else {
                true
            }
        });

        if push_new_tool_use {
            event_stream.send_tool_call(&tool_use.id, title, kind, tool_use.input.clone());
            last_message
                .content
                .push(AgentMessageContent::ToolUse(tool_use.clone()));
        } else {
            event_stream.update_tool_call_fields(
                &tool_use.id,
                acp::ToolCallUpdateFields {
                    title: Some(title.into()),
                    kind: Some(kind),
                    raw_input: Some(tool_use.input.clone()),
                    ..Default::default()
                },
            );
        }

        if !tool_use.is_input_complete {
            return None;
        }

        let Some(tool) = tool else {
            let content = format!("No tool named {} exists", tool_use.name);
            return Some(Task::ready(LanguageModelToolResult {
                content: LanguageModelToolResultContent::Text(Arc::from(content)),
                tool_use_id: tool_use.id,
                tool_name: tool_use.name,
                is_error: true,
                output: None,
            }));
        };

        let fs = self.project.read(cx).fs().clone();
        let tool_event_stream =
            ToolCallEventStream::new(tool_use.id.clone(), event_stream.clone(), Some(fs));
        tool_event_stream.update_fields(acp::ToolCallUpdateFields {
            status: Some(acp::ToolCallStatus::InProgress),
            ..Default::default()
        });
        let supports_images = self.model().is_some_and(|model| model.supports_images());
        let tool_result = tool.run(tool_use.input, tool_event_stream, cx);
        log::trace!("Running tool {}", tool_use.name);
        Some(cx.foreground_executor().spawn(async move {
            let tool_result = tool_result.await.and_then(|output| {
                if let LanguageModelToolResultContent::Image(_) = &output.llm_output
                    && !supports_images
                {
                    return Err(anyhow!(
                        "Attempted to read an image, but this model doesn't support it.",
                    ));
                }
                Ok(output)
            });

            match tool_result {
                Ok(output) => LanguageModelToolResult {
                    tool_use_id: tool_use.id,
                    tool_name: tool_use.name,
                    is_error: false,
                    content: output.llm_output,
                    output: Some(output.raw_output),
                },
                Err(error) => LanguageModelToolResult {
                    tool_use_id: tool_use.id,
                    tool_name: tool_use.name,
                    is_error: true,
                    content: LanguageModelToolResultContent::Text(Arc::from(error.to_string())),
                    output: Some(error.to_string().into()),
                },
            }
        }))
    }

    fn handle_tool_use_json_parse_error_event(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        tool_name: Arc<str>,
        raw_input: Arc<str>,
        json_parse_error: String,
    ) -> LanguageModelToolResult {
        let tool_output = format!("Error parsing input JSON: {json_parse_error}");
        LanguageModelToolResult {
            tool_use_id,
            tool_name,
            is_error: true,
            content: LanguageModelToolResultContent::Text(tool_output.into()),
            output: Some(serde_json::Value::String(raw_input.to_string())),
        }
    }

    fn update_model_request_usage(&self, amount: usize, limit: UsageLimit, cx: &mut Context<Self>) {
        self.project
            .read(cx)
            .user_store()
            .update(cx, |user_store, cx| {
                user_store.update_model_request_usage(
                    ModelRequestUsage(RequestUsage {
                        amount: amount as i32,
                        limit,
                    }),
                    cx,
                )
            });
    }

    pub fn title(&self) -> SharedString {
        self.title.clone().unwrap_or("New Thread".into())
    }

    pub fn summary(&mut self, cx: &mut Context<Self>) -> Task<Result<SharedString>> {
        if let Some(summary) = self.summary.as_ref() {
            return Task::ready(Ok(summary.clone()));
        }
        let Some(model) = self.summarization_model.clone() else {
            return Task::ready(Err(anyhow!("No summarization model available")));
        };
        // BEFORE summarization: restore all archived memory segments so summary covers full conversation.
        let original_segments_snapshot = self.memory_segments.clone();
        let mut sorted_restore = original_segments_snapshot.clone();
        sorted_restore.sort_by_key(|s| s.start);
        for seg in &sorted_restore {
            if let Err(e) = self.restore_memory_segment(seg.id, cx) {
                log::debug!(
                    "summary: restore_memory_segment({}) failed prior to summarization: {}",
                    seg.id,
                    e
                );
            }
        }

        // Build full request (may be split if token count exceeds model limit) over restored messages.
        let mut full_request = LanguageModelRequest {
            intent: Some(CompletionIntent::ThreadContextSummarization),
            temperature: AgentSettings::temperature_for_model(&model, cx),
            ..Default::default()
        };
        for (i, message) in self.messages.iter().enumerate() {
            let mut reqs = message.to_request();
            if let Some(first) = reqs.get_mut(0) {
                first
                    .content
                    .insert(0, language_model::MessageContent::Text(format!("[@{}]", i)));
            }
            full_request.messages.extend(reqs);
        }
        full_request.messages.push(LanguageModelRequestMessage {
            role: Role::User,
            content: vec![SUMMARIZE_THREAD_DETAILED_PROMPT.into()],
            cache: false,
        });

        // Capture original segments for re-archiving inside async closure.
        let original_segments_for_rearchive = original_segments_snapshot.clone();

        cx.spawn(async move |this, cx| {
            // Helper: stream a summary for a request (single-line accumulation).
            async fn run_summary_request(
                this: &Entity<Thread>,
                model: &Arc<dyn LanguageModel>,
                request: LanguageModelRequest,
                cx: &mut AsyncApp,
            ) -> Result<String> {
                let mut acc = String::new();
                let mut stream = model.stream_completion(request, cx).await?;
                while let Some(event) = stream.next().await {
                    let event = event?;
                    let text = match event {
                        LanguageModelCompletionEvent::Text(t) => t,
                        LanguageModelCompletionEvent::StatusUpdate(
                            CompletionRequestStatus::UsageUpdated { amount, limit },
                        ) => {
                            this.update(cx, |thread, cx| {
                                thread.update_model_request_usage(amount, limit, cx);
                            })?;
                            continue;
                        }
                        _ => continue,
                    };
                    let mut lines = text.lines();
                    acc.extend(lines.next());
                    if lines.next().is_some() {
                        break;
                    }
                }
                Ok(acc)
            }

            log::debug!(
                "agent2 summary: preparing token count (messages={}, max_tokens={})",
                full_request.messages.len(),
                model.max_token_count()
            );

            // Decide if we need multi-pass splitting.
            let need_split = {
                let max_tokens = model.max_token_count() as u64;
                if let Ok(fut) = cx.update(|app| model.count_tokens(full_request.clone(), app)) {
                    match fut.await {
                        Ok(token_count) => {
                            log::debug!(
                                "agent2 summary: token_count={} max_tokens={} messages={}",
                                token_count,
                                max_tokens,
                                full_request.messages.len()
                            );
                            token_count > max_tokens
                        }
                        Err(e) => {
                            log::debug!("agent2 summary: token counting failed: {e}");
                            false
                        }
                    }
                } else {
                    log::debug!("agent2 summary: token counting future creation failed");
                    false
                }
            };

            let final_summary = if need_split {
                // Remove final prompt for splitting; we will append it per chunk.
                let prompt_msg = full_request.messages.pop();
                let all = &full_request.messages;
                if all.is_empty() {
                    let mut req = full_request.clone();
                    if let Some(p) = prompt_msg.clone() {
                        req.messages.push(p);
                    }
                    run_summary_request(this, &model, req, cx)?
                } else {
                    let max_tokens = model.max_token_count() as u64;
                    let overlap = (all.len() / 20).clamp(1, 4);
                    log::debug!(
                        "agent2 summary: hierarchical splitting start total_messages={} overlap={}",
                        all.len(),
                        overlap
                    );

                    // Greedy chunk construction within token limit.
                    let mut chunks: Vec<(usize, usize)> = Vec::new(); // (start, end_exclusive)
                    let mut start = 0;
                    while start < all.len() {
                        let mut end = start;
                        let mut last_fit_end = start;
                        while end < all.len() {
                            let slice = &all[start..=end];
                            let mut req = LanguageModelRequest {
                                intent: Some(CompletionIntent::ThreadContextSummarization),
                                temperature: full_request.temperature,
                                ..Default::default()
                            };
                            req.messages.extend_from_slice(slice);
                            if let Some(p) = prompt_msg.clone() {
                                req.messages.push(p.clone());
                            }
                            let fits = if let Ok(fut) =
                                cx.update(|app| model.count_tokens(req.clone(), app))
                            {
                                match fut.await {
                                    Ok(token_count) => {
                                        if token_count <= max_tokens {
                                            last_fit_end = end + 1;
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                    Err(e) => {
                                        log::debug!(
                                            "agent2 summary: token count error during chunk build: {e}"
                                        );
                                        false
                                    }
                                }
                            } else {
                                log::debug!(
                                    "agent2 summary: failed to create token count future for chunk"
                                );
                                false
                            };
                            if !fits {
                                break;
                            }
                            end += 1;
                        }

                        if last_fit_end == start {
                            last_fit_end = (start + 1).min(all.len());
                        }

                        chunks.push((start, last_fit_end));
                        log::debug!(
                            "agent2 summary: chunk {} range=({},{}) size={}",
                            chunks.len(),
                            start,
                            last_fit_end,
                            last_fit_end - start
                        );

                        if last_fit_end >= all.len() {
                            break;
                        }
                        start = last_fit_end.saturating_sub(overlap);
                    }

                    if chunks.is_empty() {
                        return Err(anyhow!("agent2 summary: no chunks generated"));
                    }

                    // Build requests per chunk.
                    let build_chunk_request = |slice: &[LanguageModelRequestMessage]| {
                        let mut req = LanguageModelRequest {
                            intent: Some(CompletionIntent::ThreadContextSummarization),
                            temperature: full_request.temperature,
                            ..Default::default()
                        };
                        req.messages.extend_from_slice(slice);
                        if let Some(p) = prompt_msg.clone() {
                            req.messages.push(p);
                        }
                        req
                    };

                    let mut chunk_reqs: Vec<LanguageModelRequest> =
                        Vec::with_capacity(chunks.len());
                    for (s, e) in &chunks {
                        chunk_reqs.push(build_chunk_request(&all[*s..*e]));
                    }

                    let partial_futs = chunk_reqs
                        .into_iter()
                        .map(|req| async { run_summary_request(this, &model, req, cx).await.ok() });

                    let partial_results: Vec<Option<String>> =
                        futures::future::join_all(partial_futs).await;
                    let mut partial_summaries: Vec<String> = Vec::new();
                    for (i, opt) in partial_results.into_iter().enumerate() {
                        let len = opt.as_ref().map(|s| s.len()).unwrap_or(0);
                        log::debug!("agent2 summary: partial {} length={}", i + 1, len);
                        if let Some(s) = opt {
                            partial_summaries.push(s);
                        }
                    }

                    if partial_summaries.is_empty() {
                        return Err(anyhow!("agent2 summary: all chunk summaries failed"));
                    }

                    // Hierarchical merge passes.
                    let mut layer = partial_summaries;
                    let mut pass = 0;
                    loop {
                        pass += 1;
                        let mut merge_block = String::new();
                        for (i, s) in layer.iter().enumerate() {
                            let _ = write!(merge_block, "Partial summary {}:\n{}\n\n", i + 1, s);
                        }
                        merge_block.push_str("Combine these into one concise, comprehensive summary without duplication.");

                        let mut merge_req = LanguageModelRequest {
                            intent: Some(CompletionIntent::ThreadContextSummarization),
                            temperature: full_request.temperature,
                            ..Default::default()
                        };
                        merge_req.messages.push(LanguageModelRequestMessage {
                            role: Role::User,
                            content: vec![merge_block.clone().into()],
                            cache: false,
                        });

                        let merge_fits = if let Ok(fut) =
                            cx.update(|app| model.count_tokens(merge_req.clone(), app))
                        {
                            match fut.await {
                                Ok(token_count) => {
                                    log::debug!(
                                        "agent2 summary: merge pass {} candidate token_count={} layer_size={}",
                                        pass,
                                        token_count,
                                        layer.len()
                                    );
                                    token_count <= max_tokens
                                }
                                Err(e) => {
                                    log::debug!(
                                        "agent2 summary: merge pass {} token count error: {e}",
                                        pass
                                    );
                                    true
                                }
                            }
                        } else {
                            log::debug!(
                                "agent2 summary: merge pass {} failed to create token count future",
                                pass
                            );
                            true
                        };

                        if merge_fits {
                            let merged = run_summary_request(this, &model, merge_req, cx)?;
                            log::debug!(
                                "agent2 summary: hierarchical merge success pass={} final_len={}",
                                pass,
                                merged.len()
                            );
                            break merged;
                        } else {
                            if layer.len() == 1 {
                                let merged = run_summary_request(this, &model, merge_req, cx)?;
                                break merged;
                            }
                            let mut next_layer = Vec::new();
                            let mut i = 0;
                            while i < layer.len() {
                                if i + 1 < layer.len() {
                                    next_layer.push(format!("{}\n\n{}", layer[i], layer[i + 1]));
                                    i += 2;
                                } else {
                                    next_layer.push(layer[i].clone());
                                    i += 1;
                                }
                            }
                            log::debug!(
                                "agent2 summary: merge pass {} reducing layer {} -> {}",
                                pass,
                                layer.len(),
                                next_layer.len()
                            );
                            layer = next_layer;
                        }
                    }
                }
            } else {
                run_summary_request(this, &model, full_request.clone(), cx)?
            };

            log::trace!("Setting summary: {}", final_summary);
            let summary_shared = SharedString::from(final_summary);

            // RE-ARCHIVE segments after full restoration summary generation.
            this.update(cx, |thread, cx| {
                if let Err(e) =
                    thread.rearchive_segments_after_full_restore(original_segments_for_rearchive.clone(), cx)
                {
                    log::debug!("summary: rearchive after full restore failed: {}", e);
                }
                thread.summary = Some(summary_shared.clone());
                cx.notify()
            })?;

            Ok(summary_shared)
        })
    }

    fn generate_title(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.summarization_model.clone() else {
            return;
        };

        log::trace!(
            "Generating title with model: {:?}",
            self.summarization_model.as_ref().map(|model| model.name())
        );

        // Generate a detailed summary from the fully restored message history (then re-archive)
        // prior to title generation if we don't already have one.
        if self.summary.is_none() {
            if let Err(e) = self.summarize_with_full_restore_and_rearchive(cx) {
                log::debug!(
                    "summarize_with_full_restore_and_rearchive failed in generate_title: {e}"
                );
            }
        }
        let mut request = LanguageModelRequest {
            intent: Some(CompletionIntent::ThreadSummarization),
            temperature: AgentSettings::temperature_for_model(&model, cx),
            ..Default::default()
        };

        for (i, message) in self.messages.iter().enumerate() {
            let mut reqs = message.to_request();
            if let Some(first) = reqs.get_mut(0) {
                first
                    .content
                    .insert(0, language_model::MessageContent::Text(format!("[@{}]", i)));
            }
            request.messages.extend(reqs);
        }

        request.messages.push(LanguageModelRequestMessage {
            role: Role::User,
            content: vec![SUMMARIZE_THREAD_PROMPT.into()],
            cache: false,
        });
        self.pending_title_generation = Some(cx.spawn(async move |this, cx| {
            let mut title = String::new();

            let generate = async {
                let mut messages = model.stream_completion(request, cx).await?;
                while let Some(event) = messages.next().await {
                    let event = event?;
                    let text = match event {
                        LanguageModelCompletionEvent::Text(text) => text,
                        LanguageModelCompletionEvent::StatusUpdate(
                            CompletionRequestStatus::UsageUpdated { amount, limit },
                        ) => {
                            this.update(cx, |thread, cx| {
                                thread.update_model_request_usage(amount, limit, cx);
                            })?;
                            continue;
                        }
                        _ => continue,
                    };

                    let mut lines = text.lines();
                    title.extend(lines.next());

                    // Stop if the LLM generated multiple lines.
                    if lines.next().is_some() {
                        break;
                    }
                }
                anyhow::Ok(())
            };

            if generate.await.context("failed to generate title").is_ok() {
                _ = this.update(cx, |this, cx| this.set_title(title.into(), cx));
            }
            _ = this.update(cx, |this, _| this.pending_title_generation = None);
        }));
    }

    pub fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.pending_title_generation = None;
        if Some(&title) != self.title.as_ref() {
            self.title = Some(title);
            cx.emit(TitleUpdated);
            cx.notify();
        }
    }

    fn last_user_message(&self) -> Option<&UserMessage> {
        self.messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User(user_message) => Some(user_message),
                Message::Agent(_) => None,
                Message::Resume => None,
            })
    }

    fn pending_message(&mut self) -> &mut AgentMessage {
        self.pending_message.get_or_insert_default()
    }

    fn flush_pending_message(&mut self, cx: &mut Context<Self>) {
        let Some(mut message) = self.pending_message.take() else {
            return;
        };

        if message.content.is_empty() {
            return;
        }

        for content in &message.content {
            let AgentMessageContent::ToolUse(tool_use) = content else {
                continue;
            };

            if !message.tool_results.contains_key(&tool_use.id) {
                message.tool_results.insert(
                    tool_use.id.clone(),
                    LanguageModelToolResult {
                        tool_use_id: tool_use.id.clone(),
                        tool_name: tool_use.name.clone(),
                        is_error: true,
                        content: LanguageModelToolResultContent::Text(TOOL_CANCELED_MESSAGE.into()),
                        output: None,
                    },
                );
            }
        }

        self.messages.push(Message::Agent(message));
        self.updated_at = Utc::now();
        self.summary = None;
        cx.notify()
    }

    pub(crate) fn build_completion_request(
        &self,
        completion_intent: CompletionIntent,
        cx: &App,
    ) -> Result<LanguageModelRequest> {
        let model = self.model().context("No language model configured")?;
        let tools = if let Some(turn) = self.running_turn.as_ref() {
            turn.tools
                .iter()
                .filter_map(|(tool_name, tool)| {
                    log::trace!("Including tool: {}", tool_name);
                    Some(LanguageModelRequestTool {
                        name: tool_name.to_string(),
                        description: tool.description().to_string(),
                        input_schema: tool.input_schema(model.tool_input_format()).log_err()?,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        log::trace!("Building completion request");
        log::trace!("Completion intent: {:?}", completion_intent);
        log::trace!("Completion mode: {:?}", self.completion_mode);

        let messages = self.build_request_messages(cx);
        log::trace!("Request will include {} messages", messages.len());
        log::trace!("Request includes {} tools", tools.len());

        // Enumerate tool names for debugging (visibility into which tools, including "memory", are offered)
        if log::log_enabled!(log::Level::Debug) {
            let tool_names: Vec<&String> = tools.iter().map(|t| &t.name).collect();
            log::debug!("Thread {} including tools: {:?}", self.id, tool_names);
        }

        let request = LanguageModelRequest {
            thread_id: Some(self.id.to_string()),
            prompt_id: Some(self.prompt_id.to_string()),
            intent: Some(completion_intent),
            mode: Some(self.completion_mode.into()),
            messages,
            tools,
            tool_choice: None,
            stop: Vec::new(),
            temperature: AgentSettings::temperature_for_model(model, cx),
            thinking_allowed: true,
        };

        log::debug!("Completion request built successfully");
        Ok(request)
    }

    fn enabled_tools(
        &self,
        profile: &AgentProfileSettings,
        model: &Arc<dyn LanguageModel>,
        cx: &App,
    ) -> BTreeMap<SharedString, Arc<dyn AnyAgentTool>> {
        fn truncate(tool_name: &SharedString) -> SharedString {
            if tool_name.len() > MAX_TOOL_NAME_LENGTH {
                let mut truncated = tool_name.to_string();
                truncated.truncate(MAX_TOOL_NAME_LENGTH);
                truncated.into()
            } else {
                tool_name.clone()
            }
        }

        let mut tools = self
            .tools
            .iter()
            .filter_map(|(tool_name, tool)| {
                if tool.supported_provider(&model.provider_id())
                    && profile.is_tool_enabled(tool_name)
                {
                    Some((truncate(tool_name), tool.clone()))
                } else {
                    None
                }
            })
            .collect::<BTreeMap<_, _>>();

        let mut context_server_tools = Vec::new();
        let mut seen_tools = tools.keys().cloned().collect::<HashSet<_>>();
        let mut duplicate_tool_names = HashSet::default();
        for (server_id, server_tools) in self.context_server_registry.read(cx).servers() {
            for (tool_name, tool) in server_tools {
                if profile.is_context_server_tool_enabled(&server_id.0, &tool_name) {
                    let tool_name = truncate(tool_name);
                    if !seen_tools.insert(tool_name.clone()) {
                        duplicate_tool_names.insert(tool_name.clone());
                    }
                    context_server_tools.push((server_id.clone(), tool_name, tool.clone()));
                }
            }
        }

        // When there are duplicate tool names, disambiguate by prefixing them
        // with the server ID. In the rare case there isn't enough space for the
        // disambiguated tool name, keep only the last tool with this name.
        for (server_id, tool_name, tool) in context_server_tools {
            if duplicate_tool_names.contains(&tool_name) {
                let available = MAX_TOOL_NAME_LENGTH.saturating_sub(tool_name.len());
                if available >= 2 {
                    let mut disambiguated = server_id.0.to_string();
                    disambiguated.truncate(available - 1);
                    disambiguated.push('_');
                    disambiguated.push_str(&tool_name);
                    tools.insert(disambiguated.into(), tool.clone());
                } else {
                    tools.insert(tool_name, tool.clone());
                }
            } else {
                tools.insert(tool_name, tool.clone());
            }
        }

        tools
    }

    // ----------------------------------------------------------------------------
    // Debug helpers (not exposed outside tests / diagnostics)
    // ----------------------------------------------------------------------------

    #[cfg(any(test, feature = "test-support"))]
    pub fn debug_memory_store(
        &mut self,
        start: usize,
        end: usize,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<u64> {
        self.store_memory_segment(start, end, cx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn debug_memory_list(
        &self,
    ) -> Vec<(u64, usize, usize, usize, usize, usize, usize, String, u128)> {
        self.memory_segment_metas()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn debug_memory_restore(&mut self, id: u64, cx: &mut Context<Self>) -> anyhow::Result<()> {
        self.restore_memory_segment(id, cx)
    }

    // Removed duplicate private tool() accessor; public tool() earlier in impl now used universally.

    /// Build the current system prompt string (excluding any pending user input).
    /// This is factored out so that other components (e.g. token usage tooling) can
    /// separately account for "overhead" tokens contributed by the static / rule /
    /// guidance material vs the dynamic conversation messages.
    pub fn build_system_prompt(&self, cx: &App) -> String {
        // Token usage (precise only): only surface when available; otherwise omit.
        // Prefix these with underscores to indicate intentional unused bindings
        // when the precise values are not needed by the template rendering.
        let (active_tokens_opt, max_tokens_opt, usage_pct_opt) = if let (Some(precise), Some(max)) =
            (self.precise_active_tokens, self.precise_max_tokens)
        {
            let pct = if max > 0 {
                (precise as f64 / max as f64) * 100.0
            } else {
                0.0
            };
            (
                Some(precise as usize),
                Some(max as usize),
                Some((pct * 100.0).round() / 100.0),
            )
        } else {
            (None, None, None)
        };
        SystemPromptTemplate {
            project: self.project_context.read(cx),
            available_tools: self.tools.keys().cloned().collect(),
            active_tokens: active_tokens_opt,
            max_tokens: max_tokens_opt,
            usage_pct: usage_pct_opt,
            memory_segment_count: Some(self.memory_segments.len()),
            memory_saved_tokens: None,
        }
        .render(&self.templates)
        .context("failed to build system prompt")
        .expect("Invalid template")
    }

    /// Heuristic token count for the system prompt alone (no conversation messages).
    /// This lets UIs distinguish between user/assistant exchange tokens and fixed overhead.
    pub fn system_prompt_token_count_heuristic(&self, cx: &App) -> usize {
        let prompt = self.build_system_prompt(cx);
        let msg = LanguageModelRequestMessage {
            role: Role::System,
            content: vec![prompt.into()],
            cache: false,
        };
        crate::token_usage::heuristic_token_count(std::slice::from_ref(&msg))
    }

    fn build_request_messages(&self, cx: &App) -> Vec<LanguageModelRequestMessage> {
        log::trace!(
            "Building request messages from {} thread messages",
            self.messages.len()
        );

        let system_prompt = self.build_system_prompt(cx);
        let mut messages = vec![LanguageModelRequestMessage {
            role: Role::System,
            content: vec![system_prompt.into()],
            cache: false,
        }];

        for (i, message) in self.messages.iter().enumerate() {
            let mut reqs = message.to_request();
            if let Some(first) = reqs.get_mut(0) {
                first
                    .content
                    .insert(0, language_model::MessageContent::Text(format!("[@{}]", i)));
            }
            messages.extend(reqs);
        }

        if let Some(last_message) = messages.last_mut() {
            last_message.cache = true;
        }

        if let Some(message) = self.pending_message.as_ref() {
            let mut reqs = message.to_request();
            if let Some(first) = reqs.get_mut(0) {
                first.content.insert(
                    0,
                    language_model::MessageContent::Text(format!("[@{}]", messages.len())),
                );
            }
            messages.extend(reqs);
        }

        messages
    }

    /// Spawn an async task to compute precise token usage (total + per-message) and
    /// cache the aggregate values in the thread so subsequent prompt builds can
    /// surface accurate usage data in the system prompt. This runs best-effort:
    /// failures (e.g. provider not supporting counting) fall back silently to
    /// heuristic-only behavior. It only updates when values change to limit
    /// unnecessary UI events.
    fn spawn_compute_precise_usage(&mut self, cx: &mut Context<Self>) {
        if self.model.is_none() {
            return;
        }
        let model = self.model.clone();
        let prompt_id = self.prompt_id.clone();
        let completion_mode = self.completion_mode;
        let messages_snapshot: Vec<_> = self.messages.iter().flat_map(|m| m.to_request()).collect();

        let base_request = LanguageModelRequest {
            thread_id: Some(self.id.to_string()),
            prompt_id: Some(prompt_id.to_string()),
            intent: None,
            mode: Some(completion_mode.into()),
            messages: messages_snapshot.clone(),
            tools: Vec::new(),
            tool_choice: None,
            stop: Vec::new(),
            temperature: Some(0.0),
            thinking_allowed: true,
        };

        let _ = cx.spawn({
            let thread = cx.weak_entity();
            async move |_, cx| {
                let Some(model) = model else {
                    return;
                };

                // Attempt full per-message precise counting first (can be expensive).
                let per_message_result = crate::token_usage::precise_per_message_tokens(
                    &model,
                    &base_request,
                    &messages_snapshot,
                    cx, // AsyncApp implements TokenCountApp
                )
                .await;

                // Fallback to heuristic if precise per-message fails.
                let (per_message, total_precise) = match per_message_result {
                    Ok((per, total)) => (Some(per), total),
                    Err(_) => {
                        let total = crate::token_usage::precise_tokens_for_slice(
                            &model,
                            &base_request,
                            &messages_snapshot,
                            cx,
                        )
                        .await;
                        // If precise total also failed (unlikely), heuristic fallback.
                        let final_total = if total == 0 {
                            crate::token_usage::heuristic_token_count(&messages_snapshot)
                        } else {
                            total
                        };
                        let per = crate::token_usage::heuristic_per_message(&messages_snapshot);
                        (Some(per), final_total)
                    }
                };

                let max_tokens = model.max_token_count();

                let _ = thread.update(cx, |this, cx| {
                    let total_changed = this
                        .precise_active_tokens
                        .map(|v| v as usize != total_precise)
                        .unwrap_or(true);
                    let max_changed = this
                        .precise_max_tokens
                        .map(|v| v != max_tokens)
                        .unwrap_or(true);
                    let per_changed = match (&this.precise_per_message_tokens, &per_message) {
                        (None, Some(_)) => true,
                        (Some(old), Some(new)) => {
                            // Explicitly annotate the vector types to satisfy the compiler's type inference (fixes E0282).
                            let (old, new): (&Vec<usize>, &Vec<usize>) = (old, new);
                            old.len() != new.len() || old != new
                        }
                        (Some(_), None) => false, // keep existing if new unavailable
                        (None, None) => false,
                    };

                    if total_changed || max_changed || per_changed {
                        this.precise_active_tokens = Some(total_precise as u64);
                        this.precise_max_tokens = Some(max_tokens);
                        if per_message.is_some() {
                            this.precise_per_message_tokens = per_message;
                        }
                        cx.notify();
                    }
                });
            }
        });
    }

    pub fn to_markdown(&self) -> String {
        let mut markdown = String::new();
        for (ix, message) in self.messages.iter().enumerate() {
            if ix > 0 {
                markdown.push('\n');
            }
            markdown.push_str(&message.to_markdown());
        }

        if let Some(message) = self.pending_message.as_ref() {
            markdown.push('\n');
            markdown.push_str(&message.to_markdown());
        }

        markdown
    }

    fn advance_prompt_id(&mut self) {
        self.prompt_id = PromptId::new();
    }

    fn retry_strategy_for(error: &LanguageModelCompletionError) -> Option<RetryStrategy> {
        use LanguageModelCompletionError::*;
        use http_client::StatusCode;

        // General strategy here:
        // - If retrying won't help (e.g. invalid API key or payload too large), return None so we don't retry at all.
        // - If it's a time-based issue (e.g. server overloaded, rate limit exceeded), retry up to 4 times with exponential backoff.
        // - If it's an issue that *might* be fixed by retrying (e.g. internal server error), retry up to 3 times.
        match error {
            HttpResponseError {
                status_code: StatusCode::TOO_MANY_REQUESTS,
                ..
            } => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: BASE_RETRY_DELAY,
                max_attempts: MAX_RETRY_ATTEMPTS,
            }),
            ServerOverloaded { retry_after, .. } | RateLimitExceeded { retry_after, .. } => {
                Some(RetryStrategy::Fixed {
                    delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                    max_attempts: MAX_RETRY_ATTEMPTS,
                })
            }
            UpstreamProviderError {
                status,
                retry_after,
                ..
            } => match *status {
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    Some(RetryStrategy::Fixed {
                        delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                        max_attempts: MAX_RETRY_ATTEMPTS,
                    })
                }
                StatusCode::INTERNAL_SERVER_ERROR => Some(RetryStrategy::Fixed {
                    delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                    // Internal Server Error could be anything, retry up to 3 times.
                    max_attempts: 3,
                }),
                status => {
                    // There is no StatusCode variant for the unofficial HTTP 529 ("The service is overloaded"),
                    // but we frequently get them in practice. See https://http.dev/529
                    if status.as_u16() == 529 {
                        Some(RetryStrategy::Fixed {
                            delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                            max_attempts: MAX_RETRY_ATTEMPTS,
                        })
                    } else {
                        Some(RetryStrategy::Fixed {
                            delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                            max_attempts: 2,
                        })
                    }
                }
            },
            ApiInternalServerError { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 3,
            }),
            ApiReadResponseError { .. }
            | HttpSend { .. }
            | DeserializeResponse { .. }
            | BadRequestFormat { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 3,
            }),
            // Retrying these errors definitely shouldn't help.
            HttpResponseError {
                status_code:
                    StatusCode::PAYLOAD_TOO_LARGE | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED,
                ..
            }
            | AuthenticationError { .. }
            | PermissionError { .. }
            | NoApiKey { .. }
            | ApiEndpointNotFound { .. }
            | PromptTooLarge { .. } => None,
            // These errors might be transient, so retry them
            SerializeRequest { .. } | BuildRequestBody { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 1,
            }),
            // Retry all other 4xx and 5xx errors once.
            HttpResponseError { status_code, .. }
                if status_code.is_client_error() || status_code.is_server_error() =>
            {
                Some(RetryStrategy::Fixed {
                    delay: BASE_RETRY_DELAY,
                    max_attempts: 3,
                })
            }
            Other(err)
                if err.is::<language_model::PaymentRequiredError>()
                    || err.is::<language_model::ModelRequestLimitReachedError>() =>
            {
                // Retrying won't help for Payment Required or Model Request Limit errors (where
                // the user must upgrade to usage-based billing to get more requests, or else wait
                // for a significant amount of time for the request limit to reset).
                None
            }
            // Conservatively assume that any other errors are non-retryable
            HttpResponseError { .. } | Other(..) => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 2,
            }),
        }
    }
}

struct RunningTurn {
    /// Holds the task that handles agent interaction until the end of the turn.
    /// Survives across multiple requests as the model performs tool calls and
    /// we run tools, report their results.
    _task: Task<()>,
    /// The current event stream for the running turn. Used to report a final
    /// cancellation event if we cancel the turn.
    event_stream: ThreadEventStream,
    /// The tools that were enabled for this turn.
    tools: BTreeMap<SharedString, Arc<dyn AnyAgentTool>>,
}

impl RunningTurn {
    fn cancel(self) {
        log::debug!("Cancelling in progress turn");
        self.event_stream.send_canceled();
    }
}

pub struct TokenUsageUpdated(pub Option<acp_thread::TokenUsage>);

impl EventEmitter<TokenUsageUpdated> for Thread {}

pub struct TitleUpdated;

impl EventEmitter<TitleUpdated> for Thread {}

pub trait AgentTool
where
    Self: 'static + Sized,
{
    type Input: for<'de> Deserialize<'de> + Serialize + JsonSchema;
    type Output: for<'de> Deserialize<'de> + Serialize + Into<LanguageModelToolResultContent>;

    fn name() -> &'static str;

    fn description(&self) -> SharedString {
        let schema = schemars::schema_for!(Self::Input);
        SharedString::new(
            schema
                .get("description")
                .and_then(|description| description.as_str())
                .unwrap_or_default(),
        )
    }

    fn kind() -> acp::ToolKind;

    /// The initial tool title to display. Can be updated during the tool run.
    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        cx: &mut App,
    ) -> SharedString;

    /// Returns the JSON schema that describes the tool's input.
    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Schema {
        crate::tool_schema::root_schema_for::<Self::Input>(format)
    }

    /// Some tools rely on a provider for the underlying billing or other reasons.
    /// Allow the tool to check if they are compatible, or should be filtered out.
    fn supported_provider(&self, _provider: &LanguageModelProviderId) -> bool {
        true
    }

    /// Runs the tool with the provided input.
    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>>;

    /// Emits events for a previous execution of the tool.
    fn replay(
        &self,
        _input: Self::Input,
        _output: Self::Output,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        Ok(())
    }

    fn erase(self) -> Arc<dyn AnyAgentTool> {
        Arc::new(Erased(Arc::new(self)))
    }
}

pub struct Erased<T>(T);

pub struct AgentToolOutput {
    pub llm_output: LanguageModelToolResultContent,
    pub raw_output: serde_json::Value,
}

pub trait AnyAgentTool {
    fn name(&self) -> SharedString;
    fn description(&self) -> SharedString;
    fn kind(&self) -> acp::ToolKind;
    fn initial_title(&self, input: serde_json::Value, _cx: &mut App) -> SharedString;
    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value>;
    fn supported_provider(&self, _provider: &LanguageModelProviderId) -> bool {
        true
    }
    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<AgentToolOutput>>;
    fn replay(
        &self,
        input: serde_json::Value,
        output: serde_json::Value,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Result<()>;
}

impl<T> AnyAgentTool for Erased<Arc<T>>
where
    T: AgentTool,
{
    fn name(&self) -> SharedString {
        T::name().into()
    }

    fn description(&self) -> SharedString {
        self.0.description()
    }

    fn kind(&self) -> agent_client_protocol::ToolKind {
        T::kind()
    }

    fn initial_title(&self, input: serde_json::Value, _cx: &mut App) -> SharedString {
        let parsed_input = serde_json::from_value(input.clone()).map_err(|_| input);
        self.0.initial_title(parsed_input, _cx)
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        let mut json = serde_json::to_value(self.0.input_schema(format))?;
        adapt_schema_to_format(&mut json, format)?;
        Ok(json)
    }

    fn supported_provider(&self, provider: &LanguageModelProviderId) -> bool {
        self.0.supported_provider(provider)
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<AgentToolOutput>> {
        cx.spawn(async move |cx| {
            let input = serde_json::from_value(input)?;
            let output = cx
                .update(|cx| self.0.clone().run(input, event_stream, cx))?
                .await?;
            let raw_output = serde_json::to_value(&output)?;
            Ok(AgentToolOutput {
                llm_output: output.into(),
                raw_output,
            })
        })
    }

    fn replay(
        &self,
        input: serde_json::Value,
        output: serde_json::Value,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Result<()> {
        let input = serde_json::from_value(input)?;
        let output = serde_json::from_value(output)?;
        self.0.replay(input, output, event_stream, cx)
    }
}

#[derive(Clone)]
struct ThreadEventStream(mpsc::UnboundedSender<Result<ThreadEvent>>);

impl ThreadEventStream {
    fn send_user_message(&self, message: &UserMessage) {
        self.0
            .unbounded_send(Ok(ThreadEvent::UserMessage(message.clone())))
            .ok();
    }

    fn send_text(&self, text: &str) {
        self.0
            .unbounded_send(Ok(ThreadEvent::AgentText(text.to_string())))
            .ok();
    }

    fn send_thinking(&self, text: &str) {
        self.0
            .unbounded_send(Ok(ThreadEvent::AgentThinking(text.to_string())))
            .ok();
    }

    fn send_tool_call(
        &self,
        id: &LanguageModelToolUseId,
        title: SharedString,
        kind: acp::ToolKind,
        input: serde_json::Value,
    ) {
        self.0
            .unbounded_send(Ok(ThreadEvent::ToolCall(Self::initial_tool_call(
                id,
                title.to_string(),
                kind,
                input,
            ))))
            .ok();
    }

    fn initial_tool_call(
        id: &LanguageModelToolUseId,
        title: String,
        kind: acp::ToolKind,
        input: serde_json::Value,
    ) -> acp::ToolCall {
        acp::ToolCall {
            meta: None,
            id: acp::ToolCallId(id.to_string().into()),
            title,
            kind,
            status: acp::ToolCallStatus::Pending,
            content: vec![],
            locations: vec![],
            raw_input: Some(input),
            raw_output: None,
        }
    }

    fn update_tool_call_fields(
        &self,
        tool_use_id: &LanguageModelToolUseId,
        fields: acp::ToolCallUpdateFields,
    ) {
        self.0
            .unbounded_send(Ok(ThreadEvent::ToolCallUpdate(
                acp::ToolCallUpdate {
                    meta: None,
                    id: acp::ToolCallId(tool_use_id.to_string().into()),
                    fields,
                }
                .into(),
            )))
            .ok();
    }

    fn send_retry(&self, status: acp_thread::RetryStatus) {
        self.0.unbounded_send(Ok(ThreadEvent::Retry(status))).ok();
    }

    fn send_stop(&self, reason: acp::StopReason) {
        self.0.unbounded_send(Ok(ThreadEvent::Stop(reason))).ok();
    }

    fn send_canceled(&self) {
        self.0
            .unbounded_send(Ok(ThreadEvent::Stop(acp::StopReason::Cancelled)))
            .ok();
    }

    fn send_error(&self, error: impl Into<anyhow::Error>) {
        self.0.unbounded_send(Err(error.into())).ok();
    }
}

#[derive(Clone)]
pub struct ToolCallEventStream {
    tool_use_id: LanguageModelToolUseId,
    stream: ThreadEventStream,
    fs: Option<Arc<dyn Fs>>,
}

impl ToolCallEventStream {
    #[cfg(test)]
    pub fn test() -> (Self, ToolCallEventStreamReceiver) {
        let (events_tx, events_rx) = mpsc::unbounded::<Result<ThreadEvent>>();

        let stream = ToolCallEventStream::new("test_id".into(), ThreadEventStream(events_tx), None);

        (stream, ToolCallEventStreamReceiver(events_rx))
    }

    fn new(
        tool_use_id: LanguageModelToolUseId,
        stream: ThreadEventStream,
        fs: Option<Arc<dyn Fs>>,
    ) -> Self {
        Self {
            tool_use_id,
            stream,
            fs,
        }
    }

    pub fn update_fields(&self, fields: acp::ToolCallUpdateFields) {
        self.stream
            .update_tool_call_fields(&self.tool_use_id, fields);
    }

    pub fn update_diff(&self, diff: Entity<acp_thread::Diff>) {
        self.stream
            .0
            .unbounded_send(Ok(ThreadEvent::ToolCallUpdate(
                acp_thread::ToolCallUpdateDiff {
                    id: acp::ToolCallId(self.tool_use_id.to_string().into()),
                    diff,
                }
                .into(),
            )))
            .ok();
    }

    pub fn authorize(&self, title: impl Into<String>, cx: &mut App) -> Task<Result<()>> {
        if agent_settings::AgentSettings::get_global(cx).always_allow_tool_actions {
            return Task::ready(Ok(()));
        }

        let (response_tx, response_rx) = oneshot::channel();
        self.stream
            .0
            .unbounded_send(Ok(ThreadEvent::ToolCallAuthorization(
                ToolCallAuthorization {
                    tool_call: acp::ToolCallUpdate {
                        meta: None,
                        id: acp::ToolCallId(self.tool_use_id.to_string().into()),
                        fields: acp::ToolCallUpdateFields {
                            title: Some(title.into()),
                            ..Default::default()
                        },
                    },
                    options: vec![
                        acp::PermissionOption {
                            id: acp::PermissionOptionId("always_allow".into()),
                            name: "Always Allow".into(),
                            kind: acp::PermissionOptionKind::AllowAlways,
                            meta: None,
                        },
                        acp::PermissionOption {
                            id: acp::PermissionOptionId("allow".into()),
                            name: "Allow".into(),
                            kind: acp::PermissionOptionKind::AllowOnce,
                            meta: None,
                        },
                        acp::PermissionOption {
                            id: acp::PermissionOptionId("deny".into()),
                            name: "Deny".into(),
                            kind: acp::PermissionOptionKind::RejectOnce,
                            meta: None,
                        },
                    ],
                    response: response_tx,
                },
            )))
            .ok();
        let fs = self.fs.clone();
        cx.spawn(async move |cx| match response_rx.await?.0.as_ref() {
            "always_allow" => {
                if let Some(fs) = fs.clone() {
                    cx.update(|cx| {
                        update_settings_file(fs, cx, |settings, _| {
                            settings
                                .agent
                                .get_or_insert_default()
                                .set_always_allow_tool_actions(true);
                        });
                    })?;
                }

                Ok(())
            }
            "allow" => Ok(()),
            _ => Err(anyhow!("Permission to run tool denied by user")),
        })
    }
}

#[cfg(test)]
pub struct ToolCallEventStreamReceiver(mpsc::UnboundedReceiver<Result<ThreadEvent>>);

#[cfg(test)]
impl ToolCallEventStreamReceiver {
    pub async fn expect_authorization(&mut self) -> ToolCallAuthorization {
        let event = self.0.next().await;
        if let Some(Ok(ThreadEvent::ToolCallAuthorization(auth))) = event {
            auth
        } else {
            panic!("Expected ToolCallAuthorization but got: {:?}", event);
        }
    }

    pub async fn expect_update_fields(&mut self) -> acp::ToolCallUpdateFields {
        let event = self.0.next().await;
        if let Some(Ok(ThreadEvent::ToolCallUpdate(acp_thread::ToolCallUpdate::UpdateFields(
            update,
        )))) = event
        {
            update.fields
        } else {
            panic!("Expected update fields but got: {:?}", event);
        }
    }

    pub async fn expect_diff(&mut self) -> Entity<acp_thread::Diff> {
        let event = self.0.next().await;
        if let Some(Ok(ThreadEvent::ToolCallUpdate(acp_thread::ToolCallUpdate::UpdateDiff(
            update,
        )))) = event
        {
            update.diff
        } else {
            panic!("Expected diff but got: {:?}", event);
        }
    }

    pub async fn expect_terminal(&mut self) -> Entity<acp_thread::Terminal> {
        let event = self.0.next().await;
        if let Some(Ok(ThreadEvent::ToolCallUpdate(acp_thread::ToolCallUpdate::UpdateTerminal(
            update,
        )))) = event
        {
            update.terminal
        } else {
            panic!("Expected terminal but got: {:?}", event);
        }
    }
}

#[cfg(test)]
impl std::ops::Deref for ToolCallEventStreamReceiver {
    type Target = mpsc::UnboundedReceiver<Result<ThreadEvent>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
impl std::ops::DerefMut for ToolCallEventStreamReceiver {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<&str> for UserMessageContent {
    fn from(text: &str) -> Self {
        Self::Text(text.into())
    }
}

impl From<acp::ContentBlock> for UserMessageContent {
    fn from(value: acp::ContentBlock) -> Self {
        match value {
            acp::ContentBlock::Text(text_content) => Self::Text(text_content.text),
            acp::ContentBlock::Image(image_content) => Self::Image(convert_image(image_content)),
            acp::ContentBlock::Audio(_) => {
                // TODO
                Self::Text("[audio]".to_string())
            }
            acp::ContentBlock::ResourceLink(resource_link) => {
                match MentionUri::parse(&resource_link.uri) {
                    Ok(uri) => Self::Mention {
                        uri,
                        content: String::new(),
                    },
                    Err(err) => {
                        log::error!("Failed to parse mention link: {}", err);
                        Self::Text(format!("[{}]({})", resource_link.name, resource_link.uri))
                    }
                }
            }
            acp::ContentBlock::Resource(resource) => match resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                    match MentionUri::parse(&resource.uri) {
                        Ok(uri) => Self::Mention {
                            uri,
                            content: resource.text,
                        },
                        Err(err) => {
                            log::error!("Failed to parse mention link: {}", err);
                            Self::Text(
                                MarkdownCodeBlock {
                                    tag: &resource.uri,
                                    text: &resource.text,
                                }
                                .to_string(),
                            )
                        }
                    }
                }
                acp::EmbeddedResourceResource::BlobResourceContents(_) => {
                    // TODO
                    Self::Text("[blob]".to_string())
                }
            },
        }
    }
}

impl From<UserMessageContent> for acp::ContentBlock {
    fn from(content: UserMessageContent) -> Self {
        match content {
            UserMessageContent::Text(text) => acp::ContentBlock::Text(acp::TextContent {
                text,
                annotations: None,
                meta: None,
            }),
            UserMessageContent::Image(image) => acp::ContentBlock::Image(acp::ImageContent {
                data: image.source.to_string(),
                mime_type: "image/png".to_string(),
                meta: None,
                annotations: None,
                uri: None,
            }),
            UserMessageContent::Mention { uri, content } => {
                acp::ContentBlock::Resource(acp::EmbeddedResource {
                    meta: None,
                    resource: acp::EmbeddedResourceResource::TextResourceContents(
                        acp::TextResourceContents {
                            meta: None,
                            mime_type: None,
                            text: content,
                            uri: uri.to_uri().to_string(),
                        },
                    ),
                    annotations: None,
                })
            }
        }
    }
}

fn convert_image(image_content: acp::ImageContent) -> LanguageModelImage {
    LanguageModelImage {
        source: image_content.data.into(),
        // TODO: make this optional?
        size: gpui::Size::new(0.into(), 0.into()),
    }
}
