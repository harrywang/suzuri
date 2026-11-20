//! The settings window's Suzuri page. It holds the fork's own settings in a
//! file Zed never edits, so `page_data.rs` carries one line for it instead of
//! sections woven into Zed's pages.

use std::num::NonZeroU32;

use crate::{SettingField, SettingItem, SettingsPage, SettingsPageItem, USER};

pub(crate) fn suzuri_page() -> SettingsPage {
    SettingsPage {
        title: "Suzuri",
        items: Box::new(auto_sync_section()),
    }
}

fn auto_sync_section() -> [SettingsPageItem; 3] {
    [
        SettingsPageItem::SectionHeader("Auto Sync"),
        SettingsPageItem::SettingItem(SettingItem {
            title: "Sync Delay",
            description: "Seconds to wait after your last edit before syncing.",
            field: Box::new(SettingField::<NonZeroU32> {
                organization_override: None,
                json_path: Some("auto_sync.sync_delay_seconds"),
                pick: |content| content.auto_sync.as_ref()?.sync_delay_seconds.as_ref(),
                write: |content, value, _| {
                    content.auto_sync.get_or_insert_default().sync_delay_seconds = value;
                },
            }),
            metadata: None,
            files: USER,
        }),
        SettingsPageItem::SettingItem(SettingItem {
            title: "Pull Interval",
            description: "Minutes between checks for changes from your other computers.",
            field: Box::new(SettingField::<NonZeroU32> {
                organization_override: None,
                json_path: Some("auto_sync.pull_interval_minutes"),
                pick: |content| content.auto_sync.as_ref()?.pull_interval_minutes.as_ref(),
                write: |content, value, _| {
                    content
                        .auto_sync
                        .get_or_insert_default()
                        .pull_interval_minutes = value;
                },
            }),
            metadata: None,
            files: USER,
        }),
    ]
}
