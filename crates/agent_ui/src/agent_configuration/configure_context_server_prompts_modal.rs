use context_server::{ContextServerId, types::Prompt};
use gpui::{DismissEvent, EventEmitter, FocusHandle, Focusable, ScrollHandle, Window, prelude::*};
use ui::{Divider, DividerColor, Modal, ModalHeader, WithScrollbar, prelude::*};
use workspace::{ModalView, Workspace};

pub struct ConfigureContextServerPromptsModal {
    context_server_id: ContextServerId,
    prompts: Vec<Prompt>,
    focus_handle: FocusHandle,
    expanded_prompts: std::collections::HashMap<String, bool>,
    scroll_handle: ScrollHandle,
}

impl ConfigureContextServerPromptsModal {
    fn new(
        context_server_id: ContextServerId,
        prompts: Vec<Prompt>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            context_server_id,
            prompts,
            focus_handle: cx.focus_handle(),
            expanded_prompts: std::collections::HashMap::new(),
            scroll_handle: ScrollHandle::new(),
        }
    }

    pub fn toggle(
        context_server_id: ContextServerId,
        prompts: Vec<Prompt>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, |window, cx| {
            Self::new(context_server_id, prompts, window, cx)
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
        let prompts = &self.prompts;

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
                    .children(prompts.iter().enumerate().flat_map(|(index, prompt)| {
                        let prompt_name = prompt.name.clone();
                        let description = prompt.description.clone();
                        let is_expanded = self
                            .expanded_prompts
                            .get(&prompt_name)
                            .copied()
                            .unwrap_or(false);

                        let icon = if is_expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        };

                        let mut items = vec![
                            v_flex()
                                .child(
                                    h_flex()
                                        .id(SharedString::from(format!(
                                            "prompt-header-{}",
                                            index
                                        )))
                                        .py_1()
                                        .pl_1()
                                        .pr_2()
                                        .w_full()
                                        .justify_between()
                                        .rounded_sm()
                                        .hover(|s| s.bg(cx.theme().colors().element_hover))
                                        .child(
                                            Label::new(prompt_name.clone())
                                                .buffer_font(cx)
                                                .size(LabelSize::Small),
                                        )
                                        .child(
                                            Icon::new(icon)
                                                .size(IconSize::Small)
                                                .color(Color::Muted),
                                        )
                                        .on_click(cx.listener({
                                            move |this, _event, _window, _cx| {
                                                let current = this
                                                    .expanded_prompts
                                                    .get(&prompt_name)
                                                    .copied()
                                                    .unwrap_or(false);
                                                this.expanded_prompts
                                                    .insert(prompt_name.clone(), !current);
                                                _cx.notify();
                                            }
                                        })),
                                )
                                .when(is_expanded, |this| {
                                    this.child(
                                        v_flex()
                                            .mx_1()
                                            .gap_1()
                                            .when_some(description, |this, desc| {
                                                this.child(
                                                    Label::new(desc)
                                                        .color(Color::Muted),
                                                )
                                            })
                                    )
                                })
                                .into_any_element(),
                        ];

                        if index < prompts.len() - 1 {
                            items.push(
                                h_flex()
                                    .w_full()
                                    .child(Divider::horizontal().color(DividerColor::BorderVariant))
                                    .into_any_element(),
                            );
                        }

                        items
                    })),
            )
            .vertical_scrollbar_for(self.scroll_handle.clone(), window, cx)
            .into_any_element()
    }
}

impl ModalView for ConfigureContextServerPromptsModal {}

impl Focusable for ConfigureContextServerPromptsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ConfigureContextServerPromptsModal {}

impl Render for ConfigureContextServerPromptsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("ContextServerPromptsModal")
            .occlude()
            .elevation_3(cx)
            .w(rems(34.))
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .child(
                Modal::new("configure-context-server-prompts", None::<ScrollHandle>)
                    .header(
                        ModalHeader::new()
                            .headline(format!("Prompts from {}", self.context_server_id.0))
                            .show_dismiss_button(true),
                    )
                    .child(self.render_modal_content(window, cx)),
            )
    }
}
