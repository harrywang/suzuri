//! The settings window's Suzuri page. It holds the fork's own settings in a
//! file Zed never edits, so `page_data.rs` carries one line for it instead of
//! sections woven into Zed's pages.

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
            description: "How many seconds a project's files must stay unchanged before they are committed and pushed. A longer delay makes fewer, larger commits. (min: 1)",
            field: Box::new(SettingField::<u64> {
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
            description: "How many minutes pass between checks for changes made on your other computers. Document > Sync > Sync Now checks at once. (min: 1)",
            field: Box::new(SettingField::<u64> {
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
