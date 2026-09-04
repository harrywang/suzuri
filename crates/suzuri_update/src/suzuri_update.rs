//! Update checks for Suzuri.
//!
//! Suzuri ships on the `dev` release channel, where Zed's own auto-updater never
//! polls (see `ReleaseChannel::poll_for_updates`) and whose release endpoints
//! serve Zed's builds rather than this fork's. This crate is the fork's
//! replacement: it asks GitHub for the newest `suzuri-v*` release and, when one
//! is newer than the running build, shows a notification linking to the
//! download.
//!
//! It deliberately stops at notifying. Installing an update means replacing a
//! running `.app` in place, which needs signing and rollback guarantees this
//! fork does not make yet; a broken installer would strand every user running
//! the version that shipped it.
//!
//! The version being compared is [`SUZURI_VERSION`], not the crate version —
//! see that constant for why.

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, AsyncApp, Context, DismissEvent, Entity, Global, Task, WeakEntity,
    actions,
};
use http_client::{
    HttpClient,
    github::{GithubRelease, GithubReleaseAsset, latest_github_release},
};
use semver::Version;
use settings::{RegisterSetting, Settings, SettingsStore};
use std::{
    env::{
        self,
        consts::{ARCH, OS},
    },
    sync::Arc,
    time::Duration,
};
use workspace::{
    Workspace,
    notifications::{
        NotificationId, show_app_notification, simple_message_notification::MessageNotification,
    },
};

/// The version of Suzuri itself, and the only place it is written down.
///
/// Deliberately *not* `CARGO_PKG_VERSION`: `crates/zed/Cargo.toml` carries
/// upstream Zed's version, which upstream bumps on its own release cadence and
/// every merge brings along. Comparing a Suzuri release tag against that number
/// would compare two unrelated sequences. This constant lives in a file
/// upstream never touches, so a merge can never move it.
///
/// Bump it in the same commit that gets tagged `suzuri-v<this version>`; the
/// `check-version` job in `.github/workflows/suzuri-release.yml` enforces that
/// the two agree.
pub const SUZURI_VERSION: &str = "0.4.4";

/// The repository whose releases describe newer Suzuri builds.
const RELEASE_REPOSITORY: &str = "harrywang/suzuri";
/// `.github/workflows/suzuri-release.yml` publishes one release per `suzuri-v*` tag.
const RELEASE_TAG_PREFIX: &str = "suzuri-v";

/// Long enough that a launch never competes with the rest of startup for network.
const INITIAL_POLL_DELAY: Duration = Duration::from_secs(5 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Set to any value to stop background polling, for working on the fork itself
/// without the running debug build reporting itself out of date.
const DISABLE_ENV_VAR: &str = "SUZURI_NO_UPDATE_CHECK";

actions!(
    suzuri,
    [
        /// Checks whether a newer Suzuri release is available.
        CheckForUpdates
    ]
);

/// Reuses Zed's `auto_update` setting: a user who turned update checks off
/// means it for the app they are running, whichever updater implements them.
#[derive(Clone, Copy, Debug, RegisterSetting)]
struct UpdateCheckSetting(bool);

impl Settings for UpdateCheckSetting {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self(content.auto_update.unwrap())
    }
}

pub fn init(http_client: Arc<dyn HttpClient>, cx: &mut App) {
    // `test_suzuri_version_is_valid_semver` keeps this from ever failing, so a
    // typo costs a red build rather than a release nobody is told about.
    let current_version = match SUZURI_VERSION.parse::<Version>() {
        Ok(version) => version,
        Err(error) => {
            log::error!("SUZURI_VERSION {SUZURI_VERSION:?} is not valid semver: {error}");
            return;
        }
    };

    let checker = cx.new(|cx| UpdateChecker::new(http_client, current_version, cx));
    cx.set_global(GlobalUpdateChecker(checker));

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_, _: &CheckForUpdates, _window, cx| {
            check_for_updates(cx);
        });
    })
    .detach();
}

/// Runs a check the user asked for: it reports its outcome either way, where a
/// background check stays silent unless there is something to install.
pub fn check_for_updates(cx: &mut App) {
    let Some(checker) = UpdateChecker::global(cx) else {
        return;
    };
    checker.update(cx, |checker, cx| checker.check(CheckKind::Manual, cx));
}

struct GlobalUpdateChecker(Entity<UpdateChecker>);

impl Global for GlobalUpdateChecker {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CheckKind {
    Manual,
    Automatic,
}

struct UpdateChecker {
    http_client: Arc<dyn HttpClient>,
    current_version: Version,
    /// The version a background check last raised, so that leaving Suzuri open
    /// does not re-notify about the same release every `POLL_INTERVAL`.
    last_notified_version: Option<Version>,
    check_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
}

impl UpdateChecker {
    fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalUpdateChecker>()
            .map(|checker| checker.0.clone())
    }

    fn new(
        http_client: Arc<dyn HttpClient>,
        current_version: Version,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            http_client,
            current_version,
            last_notified_version: None,
            check_task: None,
            poll_task: None,
        };

        cx.observe_global::<SettingsStore>(|this, cx| this.sync_polling(cx))
            .detach();
        this.sync_polling(cx);

        this
    }

    fn sync_polling(&mut self, cx: &mut Context<Self>) {
        if env::var(DISABLE_ENV_VAR).is_ok() || !UpdateCheckSetting::get_global(cx).0 {
            self.poll_task = None;
            return;
        }

        if self.poll_task.is_some() {
            return;
        }

        self.poll_task = Some(cx.spawn(async move |this, cx| {
            let mut delay = INITIAL_POLL_DELAY;
            loop {
                cx.background_executor().timer(delay).await;
                delay = POLL_INTERVAL;

                if this
                    .update(cx, |this, cx| this.check(CheckKind::Automatic, cx))
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn check(&mut self, kind: CheckKind, cx: &mut Context<Self>) {
        self.check_task = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = Self::perform_check(&this, kind, cx).await {
                log::warn!("Suzuri update check failed: {error:#}");
                if kind == CheckKind::Manual {
                    cx.update(|cx| show_check_failed_notification(&error, cx));
                }
            }
        }));
    }

    async fn perform_check(
        this: &WeakEntity<Self>,
        kind: CheckKind,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let (http_client, current_version, last_notified_version) =
            this.read_with(cx, |this, _| {
                (
                    this.http_client.clone(),
                    this.current_version.clone(),
                    this.last_notified_version.clone(),
                )
            })?;

        match fetch_outcome(http_client, &current_version, OS, ARCH).await? {
            CheckOutcome::UpToDate => {
                if kind == CheckKind::Manual {
                    cx.update(|cx| show_up_to_date_notification(&current_version, cx));
                }
            }
            CheckOutcome::UpdateAvailable {
                version,
                download_url,
                release_url,
            } => {
                if kind == CheckKind::Automatic && last_notified_version.as_ref() == Some(&version)
                {
                    return Ok(());
                }

                this.update(cx, |this, _| {
                    this.last_notified_version = Some(version.clone());
                })?;

                cx.update(|cx| {
                    show_update_available_notification(version, download_url, release_url, cx)
                });
            }
        }

        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CheckOutcome {
    UpToDate,
    UpdateAvailable {
        version: Version,
        download_url: String,
        release_url: String,
    },
}

/// Everything a check decides before any of it reaches the screen, kept free of
/// `App` so the decision can be tested against a fake GitHub.
async fn fetch_outcome(
    http_client: Arc<dyn HttpClient>,
    current_version: &Version,
    os: &str,
    arch: &str,
) -> Result<CheckOutcome> {
    let release = latest_github_release(RELEASE_REPOSITORY, true, false, http_client)
        .await
        .context("could not reach GitHub to look for a newer Suzuri release")?;

    let latest_version = parse_release_version(&release.tag_name).with_context(|| {
        format!(
            "the newest release of {RELEASE_REPOSITORY} is tagged {:?}, \
             which is not a {RELEASE_TAG_PREFIX}<version> tag",
            release.tag_name
        )
    })?;

    if !is_newer(&latest_version, current_version) {
        return Ok(CheckOutcome::UpToDate);
    }

    let release_url = release_page_url(&release.tag_name);
    let download_url = download_url_for_host(&release, os, arch)
        .map(str::to_owned)
        .unwrap_or_else(|| release_url.clone());

    Ok(CheckOutcome::UpdateAvailable {
        version: latest_version,
        download_url,
        release_url,
    })
}

/// Turns `suzuri-v1.18.0` into `1.18.0`.
fn parse_release_version(tag_name: &str) -> Option<Version> {
    tag_name.strip_prefix(RELEASE_TAG_PREFIX)?.parse().ok()
}

/// Compares release numbers only, so that a `0.3.0-rc1` build is not told that
/// `0.3.0` — the release it is a candidate for — is an update, which is what
/// plain semver ordering would claim.
fn is_newer(latest: &Version, current: &Version) -> bool {
    (latest.major, latest.minor, latest.patch) > (current.major, current.minor, current.patch)
}

fn release_page_url(tag_name: &str) -> String {
    format!("https://github.com/{RELEASE_REPOSITORY}/releases/tag/{tag_name}")
}

/// Picks the asset a user on this OS and CPU should download, matching on shape
/// rather than exact filenames so that renaming an artifact in the release
/// workflow degrades to the release page instead of linking to a 404.
fn download_url_for_host<'a>(release: &'a GithubRelease, os: &str, arch: &str) -> Option<&'a str> {
    let matches = |asset: &GithubReleaseAsset| match os {
        "macos" => asset.name.ends_with(".dmg") && asset.name.contains(arch),
        "windows" => asset.name.ends_with(".exe") && asset.name.contains("windows"),
        "linux" => asset.name.ends_with(".tar.gz") && asset.name.contains(arch),
        _ => false,
    };

    release
        .assets
        .iter()
        .find(|asset| matches(asset))
        .map(|asset| asset.browser_download_url.as_str())
}

fn show_update_available_notification(
    version: Version,
    download_url: String,
    release_url: String,
    cx: &mut App,
) {
    struct UpdateAvailable;

    show_app_notification(NotificationId::unique::<UpdateAvailable>(), cx, move |cx| {
        let download_url = download_url.clone();
        let release_url = release_url.clone();
        cx.new(|cx| {
            MessageNotification::new(format!("Suzuri {version} is available"), cx)
                .primary_message("Download")
                .primary_on_click(move |_, cx| {
                    cx.open_url(&download_url);
                    cx.emit(DismissEvent);
                })
                .secondary_message("Release Notes")
                .secondary_on_click(move |_, cx| {
                    cx.open_url(&release_url);
                    cx.emit(DismissEvent);
                })
                .show_suppress_button(false)
        })
    });
}

fn show_up_to_date_notification(current_version: &Version, cx: &mut App) {
    struct UpToDate;

    let message = format!("Suzuri {current_version} is up to date");
    show_app_notification(NotificationId::unique::<UpToDate>(), cx, move |cx| {
        let message = message.clone();
        cx.new(|cx| MessageNotification::new(message, cx).show_suppress_button(false))
    });
}

fn show_check_failed_notification(error: &anyhow::Error, cx: &mut App) {
    struct CheckFailed;

    let message = format!("Couldn't check for Suzuri updates: {error}");
    let release_url = format!("https://github.com/{RELEASE_REPOSITORY}/releases/latest");
    show_app_notification(NotificationId::unique::<CheckFailed>(), cx, move |cx| {
        let message = message.clone();
        let release_url = release_url.clone();
        cx.new(|cx| {
            MessageNotification::new(message, cx)
                .primary_message("View Releases")
                .primary_on_click(move |_, cx| {
                    cx.open_url(&release_url);
                    cx.emit(DismissEvent);
                })
                .show_suppress_button(false)
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::{AsyncBody, FakeHttpClient, Response};

    /// Shaped like a real response from `/repos/{owner}/{repo}/releases`, so
    /// that a change to the fields `GithubRelease` requires — an upstream merge
    /// narrowing them, or GitHub dropping one — fails here rather than in the
    /// hands of users who then never hear about a release.
    const RELEASES_JSON: &str = r#"[
      {
        "tag_name": "suzuri-v0.3.0",
        "prerelease": false,
        "tarball_url": "https://api.github.test/repos/harrywang/suzuri/tarball/suzuri-v0.3.0",
        "zipball_url": "https://api.github.test/repos/harrywang/suzuri/zipball/suzuri-v0.3.0",
        "assets": [
          {
            "name": "suzuri-aarch64.dmg",
            "browser_download_url": "https://github.test/suzuri-aarch64.dmg",
            "digest": "sha256:0123456789abcdef"
          },
          {
            "name": "suzuri-x86_64.dmg",
            "browser_download_url": "https://github.test/suzuri-x86_64.dmg",
            "digest": null
          },
          {
            "name": "suzuri-windows-x86_64.exe",
            "browser_download_url": "https://github.test/suzuri-windows-x86_64.exe",
            "digest": null
          }
        ]
      }
    ]"#;

    fn fake_github(body: &'static str) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |_| async move {
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body))?)
        })
    }

    fn version(text: &str) -> Version {
        text.parse().expect("test version should be valid semver")
    }

    fn asset(name: &str) -> GithubReleaseAsset {
        GithubReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        }
    }

    fn release(tag_name: &str, asset_names: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.to_string(),
            pre_release: false,
            assets: asset_names.iter().copied().map(asset).collect(),
            tarball_url: String::new(),
            zipball_url: String::new(),
        }
    }

    #[test]
    fn test_parse_release_version() {
        assert_eq!(
            parse_release_version("suzuri-v1.18.0"),
            Some(Version::new(1, 18, 0))
        );
        // Zed's own tags share this repository's history but not its releases;
        // reading one as a Suzuri version would advertise the wrong download.
        assert_eq!(parse_release_version("v0.180.0"), None);
        assert_eq!(parse_release_version("suzuri-v"), None);
        assert_eq!(parse_release_version("suzuri-vnightly"), None);
    }

    #[test]
    fn test_suzuri_version_is_valid_semver() {
        // `init` gives up on an unparseable version, which would silently leave
        // every user of that build with no update checks at all.
        assert!(SUZURI_VERSION.parse::<Version>().is_ok());
    }

    #[test]
    fn test_is_newer_ignores_pre_release_and_build_metadata() {
        let current: Version = "0.2.0-rc1+abc123".parse().unwrap();

        assert!(is_newer(&Version::new(0, 3, 0), &current));
        assert!(is_newer(&Version::new(1, 0, 0), &current));
        // The release this build is a candidate for is not an update to it,
        // though plain semver ordering would say otherwise.
        assert!(!is_newer(&Version::new(0, 2, 0), &current));
        assert!(!is_newer(&Version::new(0, 1, 9), &current));
    }

    #[test]
    fn test_download_url_matches_host_asset() {
        let release = release(
            "suzuri-v1.18.0",
            &[
                "suzuri-aarch64.dmg",
                "suzuri-x86_64.dmg",
                "suzuri-windows-x86_64.exe",
                "suzuri-linux-x86_64.tar.gz",
                "suzuri-linux-aarch64.tar.gz",
            ],
        );

        assert_eq!(
            download_url_for_host(&release, "macos", "aarch64"),
            Some("https://example.test/suzuri-aarch64.dmg")
        );
        assert_eq!(
            download_url_for_host(&release, "macos", "x86_64"),
            Some("https://example.test/suzuri-x86_64.dmg")
        );
        assert_eq!(
            download_url_for_host(&release, "windows", "x86_64"),
            Some("https://example.test/suzuri-windows-x86_64.exe")
        );
        assert_eq!(
            download_url_for_host(&release, "linux", "x86_64"),
            Some("https://example.test/suzuri-linux-x86_64.tar.gz")
        );
        assert_eq!(
            download_url_for_host(&release, "linux", "aarch64"),
            Some("https://example.test/suzuri-linux-aarch64.tar.gz")
        );
    }

    #[test]
    fn test_download_url_is_absent_when_assets_do_not_match() {
        let release = release("suzuri-v1.18.0", &["suzuri-x86_64.dmg"]);

        assert_eq!(download_url_for_host(&release, "macos", "aarch64"), None);
    }

    #[test]
    fn test_release_page_url() {
        assert_eq!(
            release_page_url("suzuri-v1.18.0"),
            "https://github.com/harrywang/suzuri/releases/tag/suzuri-v1.18.0"
        );
    }

    #[test]
    fn test_fetch_outcome_offers_the_host_download_for_a_newer_release() {
        let outcome = gpui::block_on(fetch_outcome(
            fake_github(RELEASES_JSON),
            &version("0.2.0"),
            "macos",
            "aarch64",
        ))
        .expect("a well-formed release listing should be understood");

        assert_eq!(
            outcome,
            CheckOutcome::UpdateAvailable {
                version: version("0.3.0"),
                download_url: "https://github.test/suzuri-aarch64.dmg".to_string(),
                release_url: "https://github.com/harrywang/suzuri/releases/tag/suzuri-v0.3.0"
                    .to_string(),
            }
        );
    }

    #[test]
    fn test_fetch_outcome_is_up_to_date_on_the_newest_release() {
        for current in ["0.3.0", "0.4.0"] {
            let outcome = gpui::block_on(fetch_outcome(
                fake_github(RELEASES_JSON),
                &version(current),
                "macos",
                "aarch64",
            ))
            .expect("a well-formed release listing should be understood");

            assert_eq!(outcome, CheckOutcome::UpToDate, "running {current}");
        }
    }

    #[test]
    fn test_fetch_outcome_falls_back_to_the_release_page_without_a_host_asset() {
        let outcome = gpui::block_on(fetch_outcome(
            fake_github(RELEASES_JSON),
            &version("0.2.0"),
            "linux",
            "x86_64",
        ))
        .expect("a well-formed release listing should be understood");

        // Sending a Linux user to the release page beats sending them a .dmg.
        assert_eq!(
            outcome,
            CheckOutcome::UpdateAvailable {
                version: version("0.3.0"),
                download_url: "https://github.com/harrywang/suzuri/releases/tag/suzuri-v0.3.0"
                    .to_string(),
                release_url: "https://github.com/harrywang/suzuri/releases/tag/suzuri-v0.3.0"
                    .to_string(),
            }
        );
    }

    #[test]
    fn test_fetch_outcome_rejects_a_release_that_is_not_suzuris() {
        const ZED_RELEASE_JSON: &str = r#"[
          {
            "tag_name": "v0.180.0",
            "prerelease": false,
            "tarball_url": "https://api.github.test/tarball/v0.180.0",
            "zipball_url": "https://api.github.test/zipball/v0.180.0",
            "assets": [
              {
                "name": "Zed.dmg",
                "browser_download_url": "https://github.test/Zed.dmg",
                "digest": null
              }
            ]
          }
        ]"#;

        let error = gpui::block_on(fetch_outcome(
            fake_github(ZED_RELEASE_JSON),
            &version("0.2.0"),
            "macos",
            "aarch64",
        ))
        .expect_err("a tag that is not a Suzuri release must not become an update");

        assert!(
            format!("{error:#}").contains("v0.180.0"),
            "the error should name the tag it could not read: {error:#}"
        );
    }

    #[test]
    fn test_fetch_outcome_surfaces_an_unreachable_github() {
        let unreachable = FakeHttpClient::with_404_response();

        let error = gpui::block_on(fetch_outcome(
            unreachable,
            &version("0.2.0"),
            "macos",
            "aarch64",
        ))
        .expect_err("a failed request must not be reported as being up to date");

        assert!(
            format!("{error:#}").contains("GitHub"),
            "the error should say what could not be reached: {error:#}"
        );
    }
}
