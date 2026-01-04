use gpui::{
    ClickEvent, ClipboardItem, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    RenderImage, SharedString, Task, WeakEntity, Window, img,
};
use std::sync::Arc;
use ui::{Tooltip, prelude::*};
use workspace::{ModalView, Workspace};
use zed_actions::agent::{StartRemoteServer, StartRemoteServerInternet, StopRemoteServer};

use agent_remote_server::ServerState;

#[allow(dead_code)]
pub struct RemoteServerModal {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    agent_panel: WeakEntity<crate::AgentPanel>,
    server_state: Arc<ServerState>,
    qr_image: Option<Arc<RenderImage>>,
    _poll_task: Option<Task<()>>,
}

impl RemoteServerModal {
    #[allow(dead_code)]
    pub fn new(
        workspace: Entity<Workspace>,
        agent_panel: WeakEntity<crate::AgentPanel>,
        server_state: Arc<ServerState>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Generate QR code for the appropriate URL based on mode
        let qr_url = if server_state.mode == agent_remote_server::ServerMode::Internet {
            // For internet mode, use public URL with token if available
            if let Some(public_url) = &server_state.public_url {
                format!("{}?token={}", public_url, server_state.auth_token)
            } else {
                // Fallback to local URL if public URL not ready yet
                server_state.pairing_url.clone()
            }
        } else {
            // For local mode, use pairing URL
            server_state.pairing_url.clone()
        };

        // Load QR code PNG directly into memory as an image
        let qr_image = agent_remote_server::generate_qr_code(&qr_url)
            .ok()
            .and_then(|png_bytes| {
                // Decode PNG bytes into an image
                image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
                    .ok()
                    .map(|img| {
                        let mut rgba = img.into_rgba8();
                        // Convert from RGBA to BGRA for GPUI
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                        let frame = image::Frame::new(rgba);
                        Arc::new(RenderImage::new(smallvec::smallvec![frame]))
                    })
            });

        let poll_task = {
            let agent_panel = agent_panel.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;

                    let updated_state = agent_panel
                        .update(cx, |panel, cx| panel.remote_server_state(cx))
                        .ok()
                        .flatten();

                    let Some(updated_state) = updated_state else {
                        continue;
                    };

                    let _ = this.update(cx, |this, cx| {
                        let should_update = this.server_state.public_url
                            != updated_state.public_url
                            || this.server_state.pairing_url != updated_state.pairing_url
                            || this.server_state.auth_token != updated_state.auth_token;

                        if !should_update {
                            return;
                        }

                        this.server_state = updated_state.clone();

                        let qr_url = if this.server_state.mode
                            == agent_remote_server::ServerMode::Internet
                        {
                            if let Some(public_url) = &this.server_state.public_url {
                                format!("{}?token={}", public_url, this.server_state.auth_token)
                            } else {
                                this.server_state.pairing_url.clone()
                            }
                        } else {
                            this.server_state.pairing_url.clone()
                        };

                        this.qr_image = agent_remote_server::generate_qr_code(&qr_url)
                            .ok()
                            .and_then(|png_bytes| {
                                image::load_from_memory_with_format(
                                    &png_bytes,
                                    image::ImageFormat::Png,
                                )
                                .ok()
                                .map(|img| {
                                    let mut rgba = img.into_rgba8();
                                    for pixel in rgba.chunks_exact_mut(4) {
                                        pixel.swap(0, 2);
                                    }
                                    let frame = image::Frame::new(rgba);
                                    Arc::new(RenderImage::new(smallvec::smallvec![frame]))
                                })
                            });

                        cx.notify();
                    });
                }
            })
        };

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            agent_panel,
            server_state,
            qr_image,
            _poll_task: Some(poll_task),
        }
    }

    #[allow(dead_code)]
    pub fn toggle(
        workspace: &mut Workspace,
        agent_panel: WeakEntity<crate::AgentPanel>,
        server_state: Arc<ServerState>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let workspace_entity = cx.entity();
        workspace.toggle_modal(window, cx, |_window, cx| {
            Self::new(workspace_entity, agent_panel, server_state, cx)
        });
    }

    #[allow(dead_code)]
    fn copy_url(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let url = self.server_state.pairing_url.clone();
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        cx.notify();
    }

    #[allow(dead_code)]
    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn start_local_server(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let _ = workspace.update(cx, |_workspace, cx| {
            cx.dispatch_action(&StartRemoteServer);
        });
        cx.emit(DismissEvent);
    }

    fn start_internet_server(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let _ = workspace.update(cx, |_workspace, cx| {
            cx.dispatch_action(&StartRemoteServerInternet);
        });
        cx.emit(DismissEvent);
    }

    fn stop_server(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let _ = workspace.update(cx, |_workspace, cx| {
            cx.dispatch_action(&StopRemoteServer);
        });
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for RemoteServerModal {}

impl Focusable for RemoteServerModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for RemoteServerModal {}

impl Render for RemoteServerModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let is_internet_mode = self.server_state.mode == agent_remote_server::ServerMode::Internet;
        let has_public_url = self.server_state.public_url.is_some();

        v_flex()
            .key_context("RemoteServerModal")
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .elevation_3(cx)
            .w(rems(32.))
            .p_4()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                Headline::new(if is_internet_mode {
                                    "Remote Agent Server (Internet)"
                                } else {
                                    "Remote Agent Server (Local Network)"
                                })
                                    .size(HeadlineSize::Small)
                                    .color(Color::Default),
                            )
                            .child(
                                IconButton::new("close", IconName::Close)
                                    .icon_size(IconSize::Small)
                                    .on_click(cx.listener(|_this, _event, _window, cx| {
                                        cx.emit(DismissEvent);
                                    })),
                            ),
                    )
                    .child(
                        Label::new(if is_internet_mode {
                            if has_public_url {
                                "Your agent is accessible over the internet via cloudflared tunnel. Use the public URL below to connect from anywhere."
                            } else {
                                "Starting cloudflared tunnel... This may take a moment."
                            }
                        } else {
                            "Scan the QR code below or use the pairing URL to connect from your mobile device on the same network."
                        })
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Label::new("Server Address")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        h_flex()
                            .p_2()
                            .gap_2()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .child(
                                Label::new(format!("{}", self.server_state.local_addr))
                                    .size(LabelSize::Small)
                                    .color(Color::Default),
                            ),
                    ),
            )
            .when(is_internet_mode && has_public_url, |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(
                            Label::new("Public URL (Internet)")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            h_flex()
                                .p_2()
                                .gap_2()
                                .bg(colors.editor_background)
                                .border_1()
                                .border_color(colors.border)
                                .rounded_md()
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_x_hidden()
                                        .child(
                                            Label::new(SharedString::from(
                                                format!(
                                                    "{}?token={}",
                                                    self.server_state.public_url.clone().unwrap_or_default(),
                                                    self.server_state.auth_token
                                                ),
                                            ))
                                            .size(LabelSize::Small)
                                            .color(Color::Default),
                                        ),
                                )
                                .child(
                                    IconButton::new("copy-public", IconName::Copy)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text("Copy to clipboard"))
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            if let Some(url) = &this.server_state.public_url {
                                                let url_with_token = format!("{}?token={}", url, this.server_state.auth_token);
                                                cx.write_to_clipboard(ClipboardItem::new_string(url_with_token));
                                                cx.notify();
                                            }
                                        })),
                                ),
                        ),
                )
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Label::new(if is_internet_mode { "Local URL" } else { "Pairing URL" })
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        h_flex()
                            .p_2()
                            .gap_2()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_x_hidden()
                                    .child(
                                        Label::new(SharedString::from(
                                            self.server_state.pairing_url.clone(),
                                        ))
                                        .size(LabelSize::Small)
                                        .color(Color::Default),
                                    ),
                            )
                            .child(
                                IconButton::new("copy", IconName::Copy)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Copy to clipboard"))
                                    .on_click(cx.listener(Self::copy_url)),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Label::new("Authentication Token")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        h_flex()
                            .p_2()
                            .gap_2()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_x_hidden()
                                    .child(
                                        Label::new(SharedString::from(
                                            self.server_state.auth_token.to_string(),
                                        ))
                                        .size(LabelSize::Small)
                                        .color(Color::Default),
                                    ),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Label::new("QR Code")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .w_full()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .items_center()
                            .justify_center()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .when_some(self.qr_image.clone(), |this, qr_img| {
                                this.child(
                                    img(qr_img)
                                        .size(px(200.0)),
                                )
                            })
                            .when(self.qr_image.is_none(), |this| {
                                this.child(
                                    Icon::new(IconName::Sparkle)
                                        .size(IconSize::XLarge)
                                        .color(Color::Muted),
                                )
                            })
                            .child(
                                Label::new("Scan with your phone to connect")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("start-local", "Start Local")
                                    .style(ButtonStyle::Subtle)
                                    .tooltip(Tooltip::text("Start server for local network access"))
                                    .on_click(cx.listener(Self::start_local_server)),
                            )
                            .child(
                                Button::new("start-internet", "Start Internet")
                                    .style(ButtonStyle::Subtle)
                                    .tooltip(Tooltip::text("Start server for internet access via cloudflared"))
                                    .on_click(cx.listener(Self::start_internet_server)),
                            )
                            .child(
                                Button::new("stop-server", "Stop")
                                    .style(ButtonStyle::Subtle)
                                    .tooltip(Tooltip::text("Stop the remote server"))
                                    .on_click(cx.listener(Self::stop_server)),
                            ),
                    )
                    .child(
                        Button::new("close-modal", "Close")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|_this, _event, _window, cx| {
                                cx.emit(DismissEvent);
                            })),
                    ),
            )
    }
}
