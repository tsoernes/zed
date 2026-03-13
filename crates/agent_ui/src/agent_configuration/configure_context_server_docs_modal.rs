use context_server::{
    ContextServerId,
    protocol::ServerCapability,
    types::{Prompt, Resource},
};
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, Task, Window, prelude::*};
use project::context_server_store::ContextServerStore;
use ui::{Divider, DividerColor, Modal, ModalHeader, WithScrollbar, prelude::*};
use workspace::{ModalView, Workspace};

enum DocsState {
    Loading,
    Loaded {
        prompts: Vec<Prompt>,
        resources: Vec<Resource>,
    },
    Error(String),
}

pub struct ConfigureContextServerDocsModal {
    context_server_id: ContextServerId,
    focus_handle: FocusHandle,
    expanded_items: std::collections::HashMap<String, bool>,
    scroll_handle: ScrollHandle,
    state: DocsState,
    _load_task: Task<()>,
}

impl ConfigureContextServerDocsModal {
    fn new(
        context_server_id: ContextServerId,
        context_server_store: Entity<ContextServerStore>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let load_task = cx.spawn({
            let context_server_id = context_server_id.clone();
            async move |this, cx| {
                let server = cx
                    .read_entity(&context_server_store, |store, _cx| {
                        store.get_running_server(&context_server_id)
                    })
                    .ok()
                    .flatten();

                let Some(server) = server else {
                    this.update(cx, |this, cx| {
                        this.state = DocsState::Error("Server is not running.".to_string());
                        cx.notify();
                    })
                    .ok();
                    return;
                };

                let Some(protocol) = server.client() else {
                    this.update(cx, |this, cx| {
                        this.state =
                            DocsState::Error("Server is not initialized.".to_string());
                        cx.notify();
                    })
                    .ok();
                    return;
                };

                let mut prompts: Vec<Prompt> = Vec::new();
                let mut resources: Vec<Resource> = Vec::new();

                if protocol.capable(ServerCapability::Prompts) {
                    match protocol
                        .request::<context_server::types::requests::PromptsList>(())
                        .await
                    {
                        Ok(response) => prompts = response.prompts,
                        Err(err) => {
                            log::warn!("Failed to list prompts from MCP server: {err}");
                        }
                    }
                }

                if protocol.capable(ServerCapability::Resources) {
                    match protocol
                        .request::<context_server::types::requests::ResourcesList>(())
                        .await
                    {
                        Ok(response) => resources = response.resources,
                        Err(err) => {
                            log::warn!("Failed to list resources from MCP server: {err}");
                        }
                    }
                }

                this.update(cx, |this, cx| {
                    this.state = DocsState::Loaded { prompts, resources };
                    cx.notify();
                })
                .ok();
            }
        });

        Self {
            context_server_id,
            focus_handle: cx.focus_handle(),
            expanded_items: std::collections::HashMap::new(),
            scroll_handle: ScrollHandle::new(),
            state: DocsState::Loading,
            _load_task: load_task,
        }
    }

    pub fn toggle(
        context_server_id: ContextServerId,
        context_server_store: Entity<ContextServerStore>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, |window, cx| {
            Self::new(context_server_id, context_server_store, window, cx)
        });
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent)
    }

    fn render_modal_content(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match &self.state {
            DocsState::Loading => div()
                .size_full()
                .pb_2()
                .child(
                    h_flex()
                        .px_2()
                        .py_4()
                        .justify_center()
                        .child(Label::new("Loading…").color(Color::Muted)),
                )
                .into_any_element(),

            DocsState::Error(message) => div()
                .size_full()
                .pb_2()
                .child(
                    h_flex()
                        .px_2()
                        .py_4()
                        .justify_center()
                        .child(Label::new(message.clone()).color(Color::Error)),
                )
                .into_any_element(),

            DocsState::Loaded { prompts, resources } => {
                let no_docs = prompts.is_empty() && resources.is_empty();

                div()
                    .size_full()
                    .pb_2()
                    .child(
                        v_flex()
                            .id("modal_content")
                            .px_2()
                            .gap_1()
                            .max_h_128()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .when(no_docs, |this| {
                                this.child(
                                    h_flex()
                                        .py_4()
                                        .justify_center()
                                        .child(
                                            Label::new(
                                                "No prompts or resources exposed by this server.",
                                            )
                                            .color(Color::Muted),
                                        ),
                                )
                            })
                            .when(!prompts.is_empty(), |this| {
                                this.child(self.render_section(
                                    "Prompts",
                                    prompts
                                        .iter()
                                        .map(|p| DocItem {
                                            key: format!("prompt:{}", p.name),
                                            name: p.name.clone(),
                                            description: p.description.clone(),
                                            detail: p.arguments.as_ref().map(|args| {
                                                if args.is_empty() {
                                                    "No arguments".to_string()
                                                } else {
                                                    let arg_names: Vec<_> =
                                                        args.iter().map(|a| a.name.as_str()).collect();
                                                    format!("Arguments: {}", arg_names.join(", "))
                                                }
                                            }),
                                        })
                                        .collect(),
                                    cx,
                                ))
                            })
                            .when(!prompts.is_empty() && !resources.is_empty(), |this| {
                                this.child(
                                    h_flex().w_full().child(
                                        Divider::horizontal()
                                            .color(DividerColor::BorderVariant),
                                    ),
                                )
                            })
                            .when(!resources.is_empty(), |this| {
                                this.child(self.render_section(
                                    "Resources",
                                    resources
                                        .iter()
                                        .map(|r| DocItem {
                                            key: format!("resource:{}", r.uri),
                                            name: r.name.clone(),
                                            description: r.description.clone(),
                                            detail: Some(r.uri.to_string()),
                                        })
                                        .collect(),
                                    cx,
                                ))
                            }),
                    )
                    .vertical_scrollbar_for(self.scroll_handle.clone(), window, cx)
                    .into_any_element()
            }
        }
    }

    fn render_section(
        &self,
        title: &'static str,
        items: Vec<DocItem>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                Label::new(title)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .px_1()
                    .pt_1(),
            )
            .children(items.into_iter().enumerate().flat_map(|(index, item)| {
                let is_expanded = self.expanded_items.get(&item.key).copied().unwrap_or(false);
                let key = item.key;
                let name = item.name;
                let description = item.description;
                let detail = item.detail;

                let icon = if is_expanded {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                };

                let has_extra = description.is_some() || detail.is_some();

                let mut row_items: Vec<AnyElement> = vec![
                    v_flex()
                        .child(
                            h_flex()
                                .id(SharedString::from(format!(
                                    "doc-header-{}-{}",
                                    title, index
                                )))
                                .py_1()
                                .pl_1()
                                .pr_2()
                                .w_full()
                                .justify_between()
                                .rounded_sm()
                                .when(has_extra, |this| {
                                    this.hover(|s| s.bg(cx.theme().colors().element_hover))
                                        .on_click(cx.listener({
                                            let key = key.clone();
                                            move |this, _event, _window, cx| {
                                                let current = this
                                                    .expanded_items
                                                    .get(&key)
                                                    .copied()
                                                    .unwrap_or(false);
                                                this.expanded_items
                                                    .insert(key.clone(), !current);
                                                cx.notify();
                                            }
                                        }))
                                })
                                .child(
                                    Label::new(name)
                                        .buffer_font(cx)
                                        .size(LabelSize::Small),
                                )
                                .when(has_extra, |this| {
                                    this.child(
                                        Icon::new(icon)
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        )
                        .when(is_expanded, |this| {
                            this.when_some(description, |this, desc| {
                                this.child(Label::new(desc).color(Color::Muted).mx_1())
                            })
                            .when_some(detail, |this, detail| {
                                this.child(
                                    Label::new(detail)
                                        .color(Color::Muted)
                                        .size(LabelSize::Small)
                                        .mx_1()
                                        .italic(),
                                )
                            })
                        })
                        .into_any_element(),
                ];

                if index > 0 {
                    row_items.insert(
                        0,
                        h_flex()
                            .w_full()
                            .child(
                                Divider::horizontal().color(DividerColor::BorderVariant),
                            )
                            .into_any_element(),
                    );
                }

                row_items
            }))
    }
}

struct DocItem {
    key: String,
    name: String,
    description: Option<String>,
    detail: Option<String>,
}

impl ModalView for ConfigureContextServerDocsModal {}

impl Focusable for ConfigureContextServerDocsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ConfigureContextServerDocsModal {}

impl Render for ConfigureContextServerDocsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("ContextServerDocsModal")
            .occlude()
            .elevation_3(cx)
            .w(rems(34.))
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .child(
                Modal::new("configure-context-server-docs", None::<ScrollHandle>)
                    .header(
                        ModalHeader::new()
                            .headline(format!("Docs from {}", self.context_server_id.0))
                            .show_dismiss_button(true),
                    )
                    .child(self.render_modal_content(window, cx)),
            )
    }
}
