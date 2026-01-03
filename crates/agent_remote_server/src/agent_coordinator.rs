//! Agent Coordinator - Bridges WebSocket clients and AcpThread
//!
//! This module provides a GPUI entity that coordinates communication between
//! WebSocket clients and the agent thread. It owns the message channels and
//! spawns tasks with proper GPUI context, avoiding lifetime issues.

use crate::agent_bridge::{AgentToClientMessage, ClientToAgentMessage};
use acp_thread::{AcpThread, AgentThreadEntry};
use agent_client_protocol as acp;
use anyhow::Result;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{Context, EventEmitter, Subscription, Task, WeakEntity};
use log::{error, info, warn};
use parking_lot::Mutex;

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
        // TODO: Add event subscription for real-time updates
        // For now, focus on basic message flow
        // Will add proper subscription pattern in next iteration

        let mut coordinator = Self {
            acp_thread,
            to_agent_rx: Mutex::new(Some(to_agent_rx)),
            from_agent_tx,
            _task: None,
            _subscription: None,
        };

        // Start processing messages
        coordinator.start_processing(cx);

        coordinator
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
            info!("AgentCoordinator: Started message processing");

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

            info!("AgentCoordinator: Stopped message processing");
        });

        self._task = Some(task);
    }

    /// Handle a chat message
    async fn handle_chat(
        this: WeakEntity<Self>,
        content: String,
        cx: &mut gpui::AsyncApp,
    ) -> Result<()> {
        info!("AgentCoordinator: Forwarding chat message to agent");

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
                Ok(_) => {
                    info!("AgentCoordinator: Message sent to agent successfully");
                }
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
        info!("AgentCoordinator: Fetching conversation history");

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
        info!("AgentCoordinator: Cancelling agent operation");

        let acp_thread = this.update(cx, |coordinator, _cx| coordinator.acp_thread.clone())?;

        acp_thread.update(cx, |thread, cx| {
            let cancel_task = thread.cancel(cx);

            cx.spawn(async move |_thread, _cx| {
                cancel_task.await;
                info!("AgentCoordinator: Agent operation cancelled");
            })
            .detach();
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
