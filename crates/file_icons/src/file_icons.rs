use std::{path::Path, str};

use collections::HashMap;

use gpui::{AppContext, AssetSource, Global, SharedString};
use serde_derive::Deserialize;
use settings::Settings;
use theme::ThemeSettings;
use util::{maybe, paths::PathExt};

#[derive(Deserialize, Debug)]
pub struct FileIcons {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
}

impl Global for FileIcons {}

pub const FILE_TYPES_ASSET: &str = "icons/file_icons/file_types.json";

pub fn init(assets: impl AssetSource, cx: &mut AppContext) {
    cx.set_global(FileIcons::new(assets))
}

impl FileIcons {
    pub fn get(cx: &AppContext) -> &Self {
        cx.global::<FileIcons>()
    }

    pub fn new(assets: impl AssetSource) -> Self {
        assets
            .load(FILE_TYPES_ASSET)
            .ok()
            .flatten()
            .and_then(|file| serde_json::from_str::<FileIcons>(str::from_utf8(&file).unwrap()).ok())
            .unwrap_or_else(|| FileIcons {
                stems: HashMap::default(),
                suffixes: HashMap::default(),
            })
    }

    pub fn get_icon(path: &Path, cx: &AppContext) -> Option<SharedString> {
        let this = cx.try_global::<Self>()?;

        // TODO: Associate a type with the languages and have the file's language
        //       override these associations
        maybe!({
            let suffix = path.icon_stem_or_suffix()?;

            if let Some(type_str) = this.stems.get(suffix) {
                return this.get_icon_for_type(type_str, cx);
            }

            this.suffixes
                .get(suffix)
                .and_then(|type_str| this.get_icon_for_type(type_str, cx))
        })
        .or_else(|| this.get_icon_for_type("default", cx))
    }

    pub fn get_icon_for_type(&self, typ: &str, cx: &AppContext) -> Option<SharedString> {
        let theme_settings = ThemeSettings::get_global(cx);

        theme_settings
            .active_icon_theme
            .file_icons
            .get(typ)
            .map(|icon_definition| icon_definition.path.clone())
    }

    pub fn get_folder_icon(expanded: bool, cx: &AppContext) -> Option<SharedString> {
        let icon_theme = &ThemeSettings::get_global(cx).active_icon_theme;

        if expanded {
            icon_theme.directory_icons.expanded.clone()
        } else {
            icon_theme.directory_icons.collapsed.clone()
        }
    }

    pub fn get_chevron_icon(expanded: bool, cx: &AppContext) -> Option<SharedString> {
        let icon_theme = &ThemeSettings::get_global(cx).active_icon_theme;

        if expanded {
            icon_theme.chevron_icons.expanded.clone()
        } else {
            icon_theme.chevron_icons.collapsed.clone()
        }
    }
}
