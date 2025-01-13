use std::path::PathBuf;

use gpui::{ClickEvent, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, WeakModel};
use project::project_settings::ProjectSettings;
use remote::SshConnectionOptions;
use settings::Settings;
use ui::{
    div, h_flex, rems, Button, ButtonCommon, ButtonStyle, Clickable, ElevationIndex, FluentBuilder,
    Headline, HeadlineSize, IconName, IconPosition, InteractiveElement, IntoElement, Label, Modal,
    ModalFooter, ModalHeader, ModelContext, ParentElement, Section, Styled, StyledExt, Window,
};
use workspace::{notifications::DetachAndPromptErr, ModalView, OpenOptions, Workspace};

use crate::open_ssh_project;

enum Host {
    RemoteProject,
    SshRemoteProject(SshConnectionOptions),
}

pub struct DisconnectedOverlay {
    workspace: WeakModel<Workspace>,
    host: Host,
    focus_handle: FocusHandle,
    finished: bool,
}

impl EventEmitter<DismissEvent> for DisconnectedOverlay {}
impl Focusable for DisconnectedOverlay {
    fn focus_handle(&self, _cx: &gpui::AppContext) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
impl ModalView for DisconnectedOverlay {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        _: &mut ModelContext<Self>,
    ) -> workspace::DismissDecision {
        return workspace::DismissDecision::Dismiss(self.finished);
    }
    fn fade_out_background(&self) -> bool {
        true
    }
}

impl DisconnectedOverlay {
    pub fn register(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut ModelContext<Workspace>,
    ) {
        cx.subscribe_in(
            workspace.project(),
            window,
            |workspace, project, event, window, cx| {
                if !matches!(
                    event,
                    project::Event::DisconnectedFromHost
                        | project::Event::DisconnectedFromSshRemote
                ) {
                    return;
                }
                let handle = cx.model().downgrade();

                let ssh_connection_options = project.read(cx).ssh_connection_options(cx);
                let host = if let Some(ssh_connection_options) = ssh_connection_options {
                    Host::SshRemoteProject(ssh_connection_options)
                } else {
                    Host::RemoteProject
                };

                workspace.toggle_modal(window, cx, |window, cx| DisconnectedOverlay {
                    finished: false,
                    workspace: handle,
                    host,
                    focus_handle: cx.focus_handle(),
                });
            },
        )
        .detach();
    }

    fn handle_reconnect(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut ModelContext<Self>,
    ) {
        self.finished = true;
        cx.emit(DismissEvent);

        match &self.host {
            Host::SshRemoteProject(ssh_connection_options) => {
                self.reconnect_to_ssh_remote(ssh_connection_options.clone(), window, cx);
            }
            _ => {}
        }
    }

    fn reconnect_to_ssh_remote(
        &self,
        connection_options: SshConnectionOptions,
        window: &mut Window,
        cx: &mut ModelContext<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let Some(ssh_project) = workspace.read(cx).serialized_ssh_project() else {
            return;
        };

        let Some(window_handle) = window.window_handle().downcast::<Workspace>() else {
            return;
        };

        let app_state = workspace.read(cx).app_state().clone();

        let paths = ssh_project.paths.iter().map(PathBuf::from).collect();

        cx.spawn_in(window, move |_, mut cx| async move {
            open_ssh_project(
                connection_options,
                paths,
                app_state,
                OpenOptions {
                    replace_window: Some(window_handle),
                    ..Default::default()
                },
                &mut cx,
            )
            .await?;
            Ok(())
        })
        .detach_and_prompt_err("Failed to reconnect", window, cx, |_, _, _| None);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut ModelContext<Self>) {
        self.finished = true;
        cx.emit(DismissEvent)
    }
}

impl Render for DisconnectedOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut ModelContext<Self>) -> impl IntoElement {
        let can_reconnect = matches!(self.host, Host::SshRemoteProject(_));

        let message = match &self.host {
            Host::RemoteProject => {
                "Your connection to the remote project has been lost.".to_string()
            }
            Host::SshRemoteProject(options) => {
                let autosave = if ProjectSettings::get_global(cx)
                    .session
                    .restore_unsaved_buffers
                {
                    "\nUnsaved changes are stored locally."
                } else {
                    ""
                };
                format!(
                    "Your connection to {} has been lost.{}",
                    options.host, autosave
                )
            }
        };

        div()
            .track_focus(&self.focus_handle(cx))
            .elevation_3(window, cx)
            .on_action(cx.listener(Self::cancel))
            .occlude()
            .w(rems(24.))
            .max_h(rems(40.))
            .child(
                Modal::new("disconnected", None)
                    .header(
                        ModalHeader::new()
                            .show_dismiss_button(true)
                            .child(Headline::new("Disconnected").size(HeadlineSize::Small)),
                    )
                    .section(Section::new().child(Label::new(message)))
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("close-window", "Close Window")
                                        .style(ButtonStyle::Filled)
                                        .layer(ElevationIndex::ModalSurface)
                                        .on_click(cx.listener(move |_, _, window, cx| {
                                            window.remove_window();
                                        })),
                                )
                                .when(can_reconnect, |el| {
                                    el.child(
                                        Button::new("reconnect", "Reconnect")
                                            .style(ButtonStyle::Filled)
                                            .layer(ElevationIndex::ModalSurface)
                                            .icon(IconName::ArrowCircle)
                                            .icon_position(IconPosition::Start)
                                            .on_click(cx.listener(Self::handle_reconnect)),
                                    )
                                }),
                        ),
                    ),
            )
    }
}
