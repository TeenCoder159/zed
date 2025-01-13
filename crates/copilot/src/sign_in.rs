use crate::{request::PromptUserDeviceFlow, Copilot, Status};
use gpui::{
    div, AppContext, ClipboardItem, DismissEvent, Element, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, Model, ModelContext, MouseDownEvent,
    ParentElement, Render, Styled, Subscription, Window,
};
use ui::{prelude::*, Button, Label, Vector, VectorName};
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{ModalView, Toast, Workspace};

const COPILOT_SIGN_UP_URL: &str = "https://github.com/features/copilot";

struct CopilotStartingToast;

pub fn initiate_sign_in(window: &mut Window, cx: &mut AppContext) {
    let Some(copilot) = Copilot::global(cx) else {
        return;
    };
    let status = copilot.read(cx).status();
    let Some(workspace) = window.window_handle().downcast::<Workspace>() else {
        return;
    };
    match status {
        Status::Starting { task } => {
            let Some(workspace) = window.window_handle().downcast::<Workspace>() else {
                return;
            };

            let Ok(workspace) = workspace.update(cx, |workspace, window, cx| {
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<CopilotStartingToast>(),
                        "Copilot is starting...",
                    ),
                    cx,
                );
                workspace.weak_handle()
            }) else {
                return;
            };

            cx.spawn(|mut cx| async move {
                task.await;
                if let Some(copilot) = cx.update(|cx| Copilot::global(cx)).ok().flatten() {
                    workspace
                        .update(&mut cx, |workspace, cx| match copilot.read(cx).status() {
                            Status::Authorized => workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<CopilotStartingToast>(),
                                    "Copilot has started!",
                                ),
                                cx,
                            ),
                            _ => {
                                workspace.dismiss_toast(
                                    &NotificationId::unique::<CopilotStartingToast>(),
                                    cx,
                                );
                                copilot
                                    .update(cx, |copilot, cx| copilot.sign_in(cx))
                                    .detach_and_log_err(cx);
                            }
                        })
                        .log_err();
                }
            })
            .detach();
        }
        _ => {
            copilot.update(cx, |this, cx| this.sign_in(cx)).detach();
            workspace
                .update(cx, |this, window, cx| {
                    this.toggle_modal(window, cx, |window, cx| {
                        CopilotCodeVerification::new(&copilot, window, cx)
                    });
                })
                .ok();
        }
    }
}

pub struct CopilotCodeVerification {
    status: Status,
    connect_clicked: bool,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl Focusable for CopilotCodeVerification {
    fn focus_handle(&self, _: &AppContext) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for CopilotCodeVerification {}
impl ModalView for CopilotCodeVerification {}

impl CopilotCodeVerification {
    pub fn new(copilot: &Model<Copilot>, window: &mut Window, cx: &mut ModelContext<Self>) -> Self {
        let status = copilot.read(cx).status();
        Self {
            status,
            connect_clicked: false,
            focus_handle: cx.focus_handle(),
            _subscription: cx.observe(copilot, |this, copilot, cx| {
                let status = copilot.read(cx).status();
                match status {
                    Status::Authorized | Status::Unauthorized | Status::SigningIn { .. } => {
                        this.set_status(status, cx)
                    }
                    _ => cx.emit(DismissEvent),
                }
            }),
        }
    }

    pub fn set_status(&mut self, status: Status, cx: &mut ModelContext<Self>) {
        self.status = status;
        cx.notify();
    }

    fn render_device_code(
        data: &PromptUserDeviceFlow,
        window: &mut Window,
        cx: &mut ModelContext<Self>,
    ) -> impl IntoElement {
        let copied = cx
            .read_from_clipboard()
            .map(|item| item.text().as_ref() == Some(&data.user_code))
            .unwrap_or(false);
        h_flex()
            .w_full()
            .p_1()
            .border_1()
            .border_muted(window, cx)
            .rounded_md()
            .cursor_pointer()
            .justify_between()
            .on_mouse_down(gpui::MouseButton::Left, {
                let user_code = data.user_code.clone();
                move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(user_code.clone()));
                    window.refresh();
                }
            })
            .child(div().flex_1().child(Label::new(data.user_code.clone())))
            .child(div().flex_none().px_1().child(Label::new(if copied {
                "Copied!"
            } else {
                "Copy"
            })))
    }

    fn render_prompting_modal(
        connect_clicked: bool,
        data: &PromptUserDeviceFlow,
        window: &mut Window,
        cx: &mut ModelContext<Self>,
    ) -> impl Element {
        let connect_button_label = if connect_clicked {
            "Waiting for connection..."
        } else {
            "Connect to GitHub"
        };
        v_flex()
            .flex_1()
            .gap_2()
            .items_center()
            .child(Headline::new("Use GitHub Copilot in Zed.").size(HeadlineSize::Large))
            .child(
                Label::new("Using Copilot requires an active subscription on GitHub.")
                    .color(Color::Muted),
            )
            .child(Self::render_device_code(data, window, cx))
            .child(
                Label::new("Paste this code into GitHub after clicking the button below.")
                    .size(ui::LabelSize::Small),
            )
            .child(
                Button::new("connect-button", connect_button_label)
                    .on_click({
                        let verification_uri = data.verification_uri.clone();
                        cx.listener(move |this, _, _window, cx| {
                            cx.open_url(&verification_uri);
                            this.connect_clicked = true;
                        })
                    })
                    .full_width()
                    .style(ButtonStyle::Filled),
            )
            .child(
                Button::new("copilot-enable-cancel-button", "Cancel")
                    .full_width()
                    .on_click(cx.listener(|_, _, window, cx| cx.emit(DismissEvent))),
            )
    }
    fn render_enabled_modal(cx: &mut ModelContext<Self>) -> impl Element {
        v_flex()
            .gap_2()
            .child(Headline::new("Copilot Enabled!").size(HeadlineSize::Large))
            .child(Label::new(
                "You can update your settings or sign out from the Copilot menu in the status bar.",
            ))
            .child(
                Button::new("copilot-enabled-done-button", "Done")
                    .full_width()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
            )
    }

    fn render_unauthorized_modal(cx: &mut ModelContext<Self>) -> impl Element {
        v_flex()
            .child(Headline::new("You must have an active GitHub Copilot subscription.").size(HeadlineSize::Large))

            .child(Label::new(
                "You can enable Copilot by connecting your existing license once you have subscribed or renewed your subscription.",
            ).color(Color::Warning))
            .child(
                Button::new("copilot-subscribe-button", "Subscribe on GitHub")
                    .full_width()
                    .on_click(|_, _, cx| cx.open_url(COPILOT_SIGN_UP_URL)),
            )
            .child(
                Button::new("copilot-subscribe-cancel-button", "Cancel")
                    .full_width()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
            )
    }

    fn render_disabled_modal() -> impl Element {
        v_flex()
            .child(Headline::new("Copilot is disabled").size(HeadlineSize::Large))
            .child(Label::new("You can enable Copilot in your settings."))
    }
}

impl Render for CopilotCodeVerification {
    fn render(&mut self, window: &mut Window, cx: &mut ModelContext<Self>) -> impl IntoElement {
        let prompt = match &self.status {
            Status::SigningIn {
                prompt: Some(prompt),
            } => Self::render_prompting_modal(self.connect_clicked, prompt, window, cx)
                .into_any_element(),
            Status::Unauthorized => {
                self.connect_clicked = false;
                Self::render_unauthorized_modal(cx).into_any_element()
            }
            Status::Authorized => {
                self.connect_clicked = false;
                Self::render_enabled_modal(cx).into_any_element()
            }
            Status::Disabled => {
                self.connect_clicked = false;
                Self::render_disabled_modal().into_any_element()
            }
            _ => div().into_any_element(),
        };

        v_flex()
            .id("copilot code verification")
            .track_focus(&self.focus_handle(cx))
            .elevation_3(window, cx)
            .w_96()
            .items_center()
            .p_4()
            .gap_2()
            .on_action(cx.listener(|_, _: &menu::Cancel, window, cx| {
                cx.emit(DismissEvent);
            }))
            .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                window.focus(&this.focus_handle);
            }))
            .child(
                Vector::new(VectorName::ZedXCopilot, rems(8.), rems(4.))
                    .color(Color::Custom(cx.theme().colors().icon)),
            )
            .child(prompt)
    }
}
