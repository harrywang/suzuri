use std::sync::Arc;

use anyhow::anyhow;
use auto_update::{AutoUpdateStatus, AutoUpdater, UpdateCheckType};
use gpui::{Empty, Render};
use semver::Version;
use settings::localization::text;
use ui::{Tooltip, UpdateButton, prelude::*};

pub struct UpdateVersion {
    status: AutoUpdateStatus,
    update_check_type: UpdateCheckType,
    dismissed_status: Option<AutoUpdateStatus>,
}

impl UpdateVersion {
    pub fn new(cx: &mut Context<Self>) -> Self {
        if let Some(auto_updater) = AutoUpdater::get(cx) {
            cx.observe(&auto_updater, |this, auto_update, cx| {
                let auto_update = auto_update.read(cx);
                this.status = auto_update.status();
                this.update_check_type = auto_update.update_check_type();
                this.dismissed_status = auto_update.dismissed_status();
                cx.notify();
            })
            .detach();
            Self {
                status: auto_updater.read(cx).status(),
                update_check_type: UpdateCheckType::Automatic,
                dismissed_status: auto_updater.read(cx).dismissed_status(),
            }
        } else {
            Self {
                status: AutoUpdateStatus::Idle,
                update_check_type: UpdateCheckType::Automatic,
                dismissed_status: None,
            }
        }
    }

    pub fn update_simulation(&mut self, cx: &mut Context<Self>) {
        let next_state = match self.status {
            AutoUpdateStatus::Idle => AutoUpdateStatus::Checking,
            AutoUpdateStatus::Checking => AutoUpdateStatus::Downloading {
                version: Version::new(1, 99, 0),
                progress: Some(0.5),
            },
            AutoUpdateStatus::Downloading { .. } => AutoUpdateStatus::Installing {
                version: Version::new(1, 99, 0),
            },
            AutoUpdateStatus::Installing { .. } => AutoUpdateStatus::Updated {
                version: Version::new(1, 99, 0),
            },
            AutoUpdateStatus::Updated { .. } => AutoUpdateStatus::Errored {
                error: Arc::new(anyhow!("Network timeout")),
            },
            AutoUpdateStatus::Errored { .. } => AutoUpdateStatus::Idle,
        };

        self.status = next_state;
        self.update_check_type = UpdateCheckType::Manual;
        self.dismissed_status = None;
        cx.notify()
    }

    pub fn show_update_in_menu_bar(&self) -> bool {
        self.is_dismissed() && self.status.is_updated()
    }

    fn is_dismissed(&self) -> bool {
        self.dismissed_status.as_ref() == Some(&self.status)
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.dismissed_status = Some(self.status.clone());
        if let Some(auto_updater) = AutoUpdater::get(cx) {
            let status = self.status.clone();
            auto_updater.update(cx, |auto_updater, cx| {
                auto_updater.dismiss_status(status, cx)
            });
        }
        cx.notify()
    }

    fn version_tooltip_message(version: &Version, cx: &App) -> String {
        format!("{} {version}", text("update.version", cx))
    }

    fn downloading_tooltip_message(version: &Version, progress: Option<f32>, cx: &App) -> String {
        let message = Self::version_tooltip_message(version, cx);
        match progress {
            Some(progress) => format!(
                "{message} ({:.0}% {})",
                progress.clamp(0.0, 1.0) * 100.0,
                text("update.downloaded", cx)
            ),
            None => message,
        }
    }
}

impl Render for UpdateVersion {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_dismissed() {
            return Empty.into_any_element();
        }
        match &self.status {
            AutoUpdateStatus::Checking if self.update_check_type.is_manual() => {
                UpdateButton::checking()
                    .message(text("update.checking", cx))
                    .into_any_element()
            }
            AutoUpdateStatus::Downloading { version, progress } => {
                let rendered_version = version.clone();
                let tooltip = Tooltip::element(move |_, cx| {
                    let status = AutoUpdater::get(cx).map(|updater| updater.read(cx).status());
                    let message = match &status {
                        Some(AutoUpdateStatus::Downloading { version, progress }) => {
                            Self::downloading_tooltip_message(version, *progress, cx)
                        }
                        _ => Self::version_tooltip_message(&rendered_version, cx),
                    };
                    Label::new(message).into_any_element()
                });
                UpdateButton::downloading(*progress)
                    .message(text("update.downloading", cx))
                    .tooltip_fn(tooltip)
                    .into_any_element()
            }
            AutoUpdateStatus::Installing { version } => {
                let version = Self::version_tooltip_message(version, cx);
                UpdateButton::installing(version)
                    .message(text("update.installing", cx))
                    .into_any_element()
            }
            AutoUpdateStatus::Updated { version } => {
                let version = Self::version_tooltip_message(version, cx);
                UpdateButton::updated(version)
                    .message(text("update.restart", cx))
                    .dismiss_label(text("update.dismiss", cx))
                    .on_click(|_, _, cx| {
                        workspace::reload(cx);
                    })
                    .on_dismiss(cx.listener(|this, _, _window, cx| this.dismiss(cx)))
                    .into_any_element()
            }
            AutoUpdateStatus::Errored { error } => {
                let error_str = error.to_string();
                UpdateButton::errored(error_str)
                    .message(text("update.failed", cx))
                    .dismiss_label(text("update.dismiss", cx))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(workspace::OpenLog), cx);
                    })
                    .on_dismiss(cx.listener(|this, _, _window, cx| this.dismiss(cx)))
                    .into_any_element()
            }
            AutoUpdateStatus::Idle | AutoUpdateStatus::Checking { .. } => Empty.into_any_element(),
        }
    }
}
#[cfg(test)]
mod tests {
    use gpui::UpdateGlobal;

    use super::*;

    #[gpui::test]
    fn update_tooltips_follow_the_session_language(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            settings::localization::init(cx);
            let version = Version::new(1, 0, 0);
            assert_eq!(
                UpdateVersion::version_tooltip_message(&version, cx),
                "Update to Version: 1.0.0"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, Some(0.454), cx),
                "Update to Version: 1.0.0 (45% downloaded)"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, None, cx),
                "Update to Version: 1.0.0"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, Some(1.5), cx),
                "Update to Version: 1.0.0 (100% downloaded)"
            );
            let nightly: Version = "1.0.0+nightly.14d9a4189f058d8736339b06ff2340101eaea5af"
                .parse()
                .expect("valid nightly version");
            assert_eq!(
                UpdateVersion::version_tooltip_message(&nightly, cx),
                "Update to Version: 1.0.0+nightly.14d9a4189f058d8736339b06ff2340101eaea5af"
            );
            settings::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"ui_language":"zh-CN"}"#, cx)
                    .expect("valid language setting");
            });
            settings::localization::init(cx);
            assert_eq!(
                UpdateVersion::version_tooltip_message(&version, cx),
                "更新至版本： 1.0.0"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, None, cx),
                "更新至版本： 1.0.0"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, Some(1.5), cx),
                "更新至版本： 1.0.0 (100% 已下载)"
            );
            assert_eq!(
                UpdateVersion::downloading_tooltip_message(&version, Some(-0.5), cx),
                "更新至版本： 1.0.0 (0% 已下载)"
            );
        });
    }
}
