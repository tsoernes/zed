use gpui::{
    ClickEvent, ClipboardItem, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render,
    RenderImage, SharedString, img,
};
use std::sync::Arc;
use ui::{Tooltip, prelude::*};
use workspace::{ModalView, Workspace};

use agent_remote_server::ServerState;

#[allow(dead_code)]
pub struct RemoteServerModal {
    focus_handle: FocusHandle,
    _workspace: Entity<Workspace>,
    server_state: Arc<ServerState>,
    qr_image: Option<Arc<RenderImage>>,
}

impl RemoteServerModal {
    #[allow(dead_code)]
    pub fn new(
        workspace: Entity<Workspace>,
        server_state: Arc<ServerState>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Load QR code PNG directly into memory as an image
        let qr_image = agent_remote_server::generate_qr_code(&server_state.pairing_url)
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

        Self {
            focus_handle: cx.focus_handle(),
            _workspace: workspace,
            server_state,
            qr_image,
        }
    }

    #[allow(dead_code)]
    pub fn toggle(
        workspace: &mut Workspace,
        server_state: Arc<ServerState>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let workspace_entity = cx.entity();
        workspace.toggle_modal(window, cx, |_window, cx| {
            Self::new(workspace_entity, server_state, cx)
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
                    .justify_end()
                    .child(
                        Button::new("close-modal", "Close")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|_this, _event, __window, cx| {
                                cx.emit(DismissEvent);
                            })),
                    ),
            )
    }
}
