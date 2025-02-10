mod appearance_settings_controls;

use std::any::TypeId;

use command_palette_hooks::CommandPaletteFilter;
use editor::EditorSettingsControls;
use feature_flags::{FeatureFlag, FeatureFlagViewExt};
use gpui::{actions, App, Entity, EventEmitter, FocusHandle, Focusable};
use ui::prelude::*;
use workspace::item::{Item, ItemEvent};
use workspace::Workspace;

use crate::appearance_settings_controls::AppearanceSettingsControls;

pub struct SettingsUiFeatureFlag;

impl FeatureFlag for SettingsUiFeatureFlag {
    const NAME: &'static str = "settings-ui";
}

actions!(zed, [OpenSettingsEditor]);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };

        workspace.register_action(|workspace, _: &OpenSettingsEditor, window, cx| {
            let existing = workspace
                .active_pane()
                .read(cx)
                .items()
                .find_map(|item| item.downcast::<SettingsPage>());

            if let Some(existing) = existing {
                workspace.activate_item(&existing, true, true, window, cx);
            } else {
                let settings_page = SettingsPage::new(workspace, cx);
                workspace.add_item_to_active_pane(Box::new(settings_page), None, true, window, cx)
            }
        });

        let settings_ui_actions = [TypeId::of::<OpenSettingsEditor>()];

        CommandPaletteFilter::update_global(cx, |filter, _cx| {
            filter.hide_action_types(&settings_ui_actions);
        });

        cx.observe_flag::<SettingsUiFeatureFlag, _>(
            window,
            move |is_enabled, _workspace, _, cx| {
                if is_enabled {
                    CommandPaletteFilter::update_global(cx, |filter, _cx| {
                        filter.show_action_types(settings_ui_actions.iter());
                    });
                } else {
                    CommandPaletteFilter::update_global(cx, |filter, _cx| {
                        filter.hide_action_types(&settings_ui_actions);
                    });
                }
            },
        )
        .detach();
    })
    .detach();
}

pub struct SettingsPage {
    focus_handle: FocusHandle,
}

impl SettingsPage {
    pub fn new(_workspace: &Workspace, cx: &mut Context<Workspace>) -> Entity<Self> {
        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
        })
    }
}

impl EventEmitter<ItemEvent> for SettingsPage {}

impl Focusable for SettingsPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for SettingsPage {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Settings))
    }

    fn tab_content_text(&self, _window: &Window, _cx: &App) -> Option<SharedString> {
        Some("Settings".into())
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, mut f: impl FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("settings-ui")
            .overflow_y_scroll()
            .bg(cx.theme().colors().editor_background)
            .size_full()
            .items_center()
            .p_8()
            .child(
                v_flex()
                    .elevation_2(cx)
                    .p_8()
                    .max_w(px(800.))
                    .gap_4()
                    .child(
                        v_group()
                            .unfilled()
                            .gap_2()
                            .child(
                                div().max_w(px(580.)).p_1()
                                    .child(Headline::new("Welcome to the settings UI alpha!").size(HeadlineSize::Small))
                                    .child(Label::new("We have a lot to build, and many settings to cover.")
                                        .italic(true).color(Color::Muted))
                                    .child(Label::new("Help us out by giving feedback, and contributing to coverage of visual settings.")
                                        .italic(true).color(Color::Muted)))
                            .child(
                                // TODO: Update URLs
                                h_flex()
                                    .gap_3()
                                    .child(Button::new("give-feedback", "Give Feedback")
                                        .layer(ui::ElevationIndex::Surface)
                                        .on_click(cx.listener(|_, _, _, cx| {
                                        cx.open_url("https://github.com/zed-industries/zed/discussions");
                                })))
                                .child(Button::new("contribute", "Contribute")
                                    .layer(ui::ElevationIndex::Surface)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                    cx.open_url("https://github.com/zed-industries/zed");
                                })))
                            )
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Headline::new("Appearance").size(HeadlineSize::Small))
                            .child(AppearanceSettingsControls::new()),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Headline::new("Editor").size(HeadlineSize::Small))
                            .child(EditorSettingsControls::new()),
                    ),
            )
    }
}
