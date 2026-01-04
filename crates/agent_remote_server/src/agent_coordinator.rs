//! Agent Coordinator - Bridges WebSocket clients and AcpThread
//!
//! This module provides a GPUI entity that coordinates communication between
//! WebSocket clients and the agent thread. It owns the message channels and
//! spawns tasks with proper GPUI context, avoiding lifetime issues.

use crate::agent_bridge::{AgentToClientMessage, ClientToAgentMessage};
use acp_thread::{AcpThread, AcpThreadEvent, AgentThreadEntry, AssistantMessageChunk};
use agent_client_protocol as acp;
use anyhow::Result;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Context, EventEmitter, Subscription, Task, WeakEntity};
use log::{error, warn};
use parking_lot::Mutex;
use std::collections::HashMap;

/// Coordinator entity that manages agent communication
pub struct AgentCoordinator {
    /// Reference to the agent thread
    acp_thread: WeakEntity<AcpThread>,
    /// Receive messages from WebSocket clients
    to_agent_rx: Mutex<Option<mpsc::UnboundedReceiver<ClientToAgentMessage>>>,
    /// Send messages to WebSocket clients (via bridge)
    from_agent_tx: mpsc::UnboundedSender<AgentToClientMessage>,
    /// Active processing task
    _task: Option<Task<()>>,
    /// Subscription to agent thread events
    _subscription: Option<Subscription>,
    /// Track last sent length per entry index to send only deltas
    last_sent_lengths: Mutex<HashMap<usize, usize>>,
    /// Track which tool calls we've already sent to avoid duplicates
    ///
    /// `false` => sent start, but not final result yet
    /// `true`  => sent final result (or we decided there's nothing else to send)
    sent_tool_calls: Mutex<HashMap<usize, bool>>,
}

/// Events emitted by the coordinator
#[derive(Debug, Clone)]
pub enum CoordinatorEvent {
    /// A message was processed
    MessageProcessed,
    /// An error occurred
    Error(String),
}

impl EventEmitter<CoordinatorEvent> for AgentCoordinator {}

impl AgentCoordinator {
    /// Create a new coordinator
    pub fn new(
        acp_thread: WeakEntity<AcpThread>,
        to_agent_rx: mpsc::UnboundedReceiver<ClientToAgentMessage>,
        from_agent_tx: mpsc::UnboundedSender<AgentToClientMessage>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Subscribe to agent thread events
        let subscription = if let Some(thread) = acp_thread.upgrade() {
            Some(cx.subscribe(&thread, Self::handle_thread_event))
        } else {
            None
        };

        let mut coordinator = Self {
            acp_thread: acp_thread.clone(),
            to_agent_rx: Mutex::new(Some(to_agent_rx)),
            from_agent_tx: from_agent_tx.clone(),
            _task: None,
            _subscription: subscription,
            last_sent_lengths: Mutex::new(HashMap::new()),
            sent_tool_calls: Mutex::new(HashMap::new()),
        };

        // Send initial state (token usage and model info) to client
        coordinator.send_initial_state(cx);

        // Start processing messages
        coordinator.start_processing(cx);

        coordinator
    }

    /// Send initial state (token usage and model info) to a newly connected client
    fn send_initial_state(&self, cx: &mut Context<Self>) {
        if let Some(thread) = self.acp_thread.upgrade() {
            log::info!("AgentCoordinator: Sending initial state to client");

            // Send token usage
            thread.read_with(cx, |thread, _cx| {
                if let Some(usage) = thread.token_usage() {
                    log::info!(
                        "AgentCoordinator: Sending initial token usage: {} / {}",
                        usage.used_tokens,
                        usage.max_tokens
                    );
                    let _ =
                        self.from_agent_tx
                            .unbounded_send(AgentToClientMessage::TokenUsageUpdate {
                                used_tokens: usage.used_tokens,
                                max_tokens: usage.max_tokens,
                            });
                } else {
                    log::info!("AgentCoordinator: No token usage available yet");
                }
            });

            // Send model info
            let connection = thread.read(cx).connection();
            let session_id = thread.read(cx).session_id();
            if let Some(model_selector) = connection.model_selector(&session_id) {
                log::info!("AgentCoordinator: Model selector available, fetching model info");
                let from_agent_tx = self.from_agent_tx.clone();
                let model_task = model_selector.selected_model(cx);
                cx.spawn(async move |_this, _cx| match model_task.await {
                    Ok(model_info) => {
                        log::info!(
                            "AgentCoordinator: Sending model info: {} ({})",
                            model_info.name,
                            model_info.id
                        );
                        let _ =
                            from_agent_tx.unbounded_send(AgentToClientMessage::ModelInfoUpdate {
                                model_id: model_info.id.to_string(),
                                model_name: model_info.name.to_string(),
                            });
                    }
                    Err(e) => {
                        log::error!("AgentCoordinator: Failed to get model info: {:?}", e);
                    }
                })
                .detach();
            } else {
                log::warn!("AgentCoordinator: No model selector available");
            }
        } else {
            log::warn!("AgentCoordinator: ACP thread no longer available");
        }
    }

    /// Handle events from the agent thread
    fn handle_thread_event(
        &mut self,
        thread: gpui::Entity<AcpThread>,
        event: &AcpThreadEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            AcpThreadEvent::TokenUsageUpdated => {
                // Send token usage update to clients
                thread.read_with(cx, |thread, _cx| {
                    if let Some(usage) = thread.token_usage() {
                        let _ = self.from_agent_tx.unbounded_send(
                            AgentToClientMessage::TokenUsageUpdate {
                                used_tokens: usage.used_tokens,
                                max_tokens: usage.max_tokens,
                            },
                        );
                    }
                });
            }
            AcpThreadEvent::EntryUpdated(index) => {
                // Read the updated entry and stream it to clients
                if let Some(thread) = self.acp_thread.upgrade() {
                    thread.read_with(cx, |thread, _cx| {
                        let entries = thread.entries();
                        if let Some(entry) = entries.get(*index) {
                            self.stream_entry_to_clients(*index, entry, cx);
                        }
                    });
                }
            }
            AcpThreadEvent::NewEntry => {
                // New entry added, stream it to clients
                if let Some(thread) = self.acp_thread.upgrade() {
                    thread.read_with(cx, |thread, _cx| {
                        let entries = thread.entries();
                        if let Some(entry) = entries.last() {
                            let index = entries.len() - 1;
                            self.stream_entry_to_clients(index, entry, cx);
                        }
                    });
                }
            }
            AcpThreadEvent::Stopped => {
                // Treat "stopped" as the completion signal for the most recent assistant entry.
                let entry_index =
                    self.acp_thread
                        .upgrade()
                        .and_then(|thread| {
                            thread.read_with(cx, |thread, _cx| {
                                thread.entries().iter().rposition(|e| {
                                    matches!(e, AgentThreadEntry::AssistantMessage(_))
                                })
                            })
                        })
                        .unwrap_or(0);

                let _ = self
                    .from_agent_tx
                    .unbounded_send(AgentToClientMessage::ResponseComplete { entry_index });
            }
            AcpThreadEvent::Error => {
                let _ = self
                    .from_agent_tx
                    .unbounded_send(AgentToClientMessage::Error {
                        message: "Agent encountered an error".to_string(),
                    });
            }
            _ => {}
        }
    }

    /// Stream an agent entry to all connected clients
    fn stream_entry_to_clients(&self, entry_index: usize, entry: &AgentThreadEntry, cx: &App) {
        match entry {
            AgentThreadEntry::AssistantMessage(msg) => {
                let mut full_content = String::new();
                for chunk in &msg.chunks {
                    match chunk {
                        AssistantMessageChunk::Message { block }
                        | AssistantMessageChunk::Thought { block } => {
                            full_content.push_str(block.to_markdown(cx));
                        }
                    }
                }

                let mut lengths = self.last_sent_lengths.lock();
                let last_sent = lengths.get(&entry_index).copied().unwrap_or(0);

                if full_content.len() > last_sent {
                    let delta = &full_content[last_sent..];

                    let _ = self
                        .from_agent_tx
                        .unbounded_send(AgentToClientMessage::TextChunk {
                            entry_index,
                            content: delta.to_string(),
                        });

                    lengths.insert(entry_index, full_content.len());
                }
            }
            AgentThreadEntry::ToolCall(tool_call) => {
                // We key tool-call streaming by the *entry index* (stable for this thread history).
                // Track whether we've already sent the start and whether we've already sent the final output.
                let mut sent_tools = self.sent_tool_calls.lock();

                let tool_call_id = tool_call.id.to_string();
                let tool_name = tool_call.label.read(cx).source().to_string();

                let have_raw_input = tool_call.raw_input.is_some();
                let have_raw_output = tool_call.raw_output.is_some();
                let have_rendered_content = !tool_call.content.is_empty();

                let already_sent_final = sent_tools.get(&entry_index).copied().unwrap_or(false);

                if !sent_tools.contains_key(&entry_index) {
                    let _ = self
                        .from_agent_tx
                        .unbounded_send(AgentToClientMessage::ToolStart {
                            entry_index,
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            tool_input: tool_call.raw_input.clone().unwrap_or_default(),
                        });

                    // start sent; final not yet
                    sent_tools.insert(entry_index, false);
                }

                // Prefer raw_output if present; otherwise fall back to rendered content.
                if !already_sent_final && (have_raw_output || have_rendered_content) {
                    let result_text = if let Some(raw_output) = &tool_call.raw_output {
                        serde_json::to_string_pretty(raw_output)
                            .unwrap_or_else(|_| raw_output.to_string())
                    } else {
                        tool_call
                            .content
                            .iter()
                            .map(|content| content.to_markdown(cx))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };

                    let _ = self
                        .from_agent_tx
                        .unbounded_send(AgentToClientMessage::ToolResult {
                            entry_index,
                            tool_call_id,
                            tool_name,
                            result: result_text,
                            error: None,
                        });

                    sent_tools.insert(entry_index, true);
                } else if !already_sent_final
                    && have_raw_input
                    && !have_raw_output
                    && !have_rendered_content
                {
                    // Still running; nothing else to send yet.
                }
            }
            AgentThreadEntry::UserMessage(_) => {}
        }
    }

    /// Start processing messages from clients
    fn start_processing(&mut self, cx: &mut Context<Self>) {
        // Take the receiver out of the mutex so we can move it into the task
        let mut to_agent_rx = self.to_agent_rx.lock().take();

        if to_agent_rx.is_none() {
            warn!("Coordinator already processing messages");
            return;
        }

        let task = cx.spawn(async move |this: WeakEntity<Self>, mut cx| {
            while let Some(msg) = to_agent_rx.as_mut().unwrap().next().await {
                let result = match msg {
                    ClientToAgentMessage::Chat { content } => {
                        Self::handle_chat(this.clone(), content, &mut cx).await
                    }
                    ClientToAgentMessage::GetHistory => {
                        Self::handle_get_history(this.clone(), &mut cx).await
                    }
                    ClientToAgentMessage::Cancel => {
                        Self::handle_cancel(this.clone(), &mut cx).await
                    }
                    ClientToAgentMessage::GetInitialState => {
                        Self::handle_get_initial_state(this.clone(), &mut cx).await
                    }
                };

                if let Err(e) = result {
                    error!("AgentCoordinator: Error handling message: {:?}", e);
                    let _ = this.update(cx, |coordinator: &mut Self, cx| {
                        let _ =
                            coordinator
                                .from_agent_tx
                                .unbounded_send(AgentToClientMessage::Error {
                                    message: format!("Error: {}", e),
                                });
                        cx.emit(CoordinatorEvent::Error(e.to_string()));
                    });
                }
            }
        });

        self._task = Some(task);
    }

    /// Handle a chat message
    async fn handle_chat(
        this: WeakEntity<Self>,
        content: String,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        let (acp_thread, from_agent_tx) = this.update(cx, |coordinator, _cx| {
            (
                coordinator.acp_thread.clone(),
                coordinator.from_agent_tx.clone(),
            )
        })?;

        // Send to agent thread
        acp_thread.update(cx, |thread, cx| {
            // Create content blocks
            let blocks = vec![content.into()];

            // Send to agent
            let send_future = thread.send(blocks, cx);

            // Spawn task to wait for completion
            cx.spawn(async move |_thread, _cx| match send_future.await {
                Ok(_) => {}
                Err(e) => {
                    error!("AgentCoordinator: Failed to send message: {:?}", e);
                    let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                        message: format!("Failed to send message: {}", e),
                    });
                }
            })
            .detach();
        })?;

        Ok(())
    }

    /// Handle get history request
    async fn handle_get_history(this: WeakEntity<Self>, cx: &mut gpui::AsyncApp) -> Result<()> {
        let (acp_thread, from_agent_tx) = this.update(cx, |coordinator, _cx| {
            (
                coordinator.acp_thread.clone(),
                coordinator.from_agent_tx.clone(),
            )
        })?;

        acp_thread.read_with(cx, |thread, _cx| {
            let entries = thread.entries();

            for entry in entries {
                let msg = match entry {
                    AgentThreadEntry::UserMessage(msg) => {
                        // Extract text content
                        let content = msg
                            .chunks
                            .iter()
                            .filter_map(|block| {
                                if let acp::ContentBlock::Text(text) = block {
                                    Some(text.text.clone())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        Some(AgentToClientMessage::HistoryEntry {
                            role: "user".to_string(),
                            content,
                        })
                    }
                    AgentThreadEntry::AssistantMessage(_msg) => {
                        // TODO: Extract assistant message content properly
                        Some(AgentToClientMessage::HistoryEntry {
                            role: "assistant".to_string(),
                            content: "Assistant response".to_string(),
                        })
                    }
                    _ => None,
                };

                if let Some(msg) = msg {
                    let _ = from_agent_tx.unbounded_send(msg);
                }
            }
        })?;

        Ok(())
    }

    /// Handle cancel request
    async fn handle_cancel(this: WeakEntity<Self>, cx: &mut gpui::AsyncApp) -> Result<()> {
        let acp_thread = this.update(cx, |coordinator, _cx| coordinator.acp_thread.clone())?;

        acp_thread.update(cx, |thread, cx| {
            let cancel_task = thread.cancel(cx);

            cx.spawn(async move |_thread, _cx| {
                cancel_task.await;
            })
            .detach();
        })?;

        Ok(())
    }

    /// Handle get initial state request
    async fn handle_get_initial_state(
        this: WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        this.update(cx, |coordinator, cx| {
            coordinator.send_initial_state(cx);
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_coordinator_module_compiles() {
        // Basic test to ensure module compiles
        // Full integration tests require a real AcpThread with GPUI context
        assert!(true);
    }
}
