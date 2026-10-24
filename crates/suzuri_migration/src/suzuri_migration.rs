//! Separating Suzuri's user data from Zed's.
//!
//! Suzuri used to keep upstream's `paths::APP_NAME` of "Zed", so it read and
//! wrote Zed's own settings, extensions, and agent threads, and the two apps
//! could not be configured independently (suzuri#140). `APP_NAME` is now
//! "Suzuri", and this crate covers the move:
//!
//! 1. [`migrate_from_zed`] copies an existing Suzuri user's state out of the
//!    Zed folders once, on the first launch that finds no Suzuri folders.
//! 2. [`ImportZedSettings`] copies Zed's settings, keymap, tasks, themes, and
//!    snippets on request, the way VS Code and Cursor settings are imported.
//! 3. A shipped build stores credentials in the system keychain instead of the
//!    plain-text `development_credentials` file Zed uses for its own dev
//!    builds, moving whatever that file holds into the keychain.
//!
//! Zed's folders are only ever read, never changed: Zed may still be installed,
//! and an older Suzuri build must keep working after a rollback.

use anyhow::{Context as _, Result};
use credentials_provider::CredentialsProvider;
use futures::lock::Mutex;
use gpui::{Action, App, AsyncApp, AsyncWindowContext, Global, WeakEntity};
use notifications::status_toast::StatusToast;
use release_channel::ReleaseChannel;
use schemars::JsonSchema;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, OnceLock},
};
use ui::prelude::*;
use workspace::Workspace;
use zed_credentials_provider::ZedCredentialsProvider;

/// Imports settings, keymap, tasks, themes, and snippets from Zed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ImportZedSettings {
    #[serde(default)]
    pub skip_prompt: bool,
}

const LEGACY_APP_NAME: &str = "Zed";
const LEGACY_APP_NAME_LOWERCASE: &str = "zed";

/// What a Suzuri user had in Zed's data folder that is worth keeping. Language
/// servers, Node, and external agents are left behind: they download again on
/// demand, and copying them would double a gigabyte or more of disk use.
/// Only Suzuri's own `dev` database comes along, not Zed's channels.
const MIGRATED_DATA_ENTRIES: &[&str] = &[
    "db/0-dev",
    "db/0-global",
    "threads",
    "extensions",
    "typeset_compilers",
    "prompts",
    "suzuri-recovery.json",
];

const IMPORTED_CONFIG_FILES: &[&str] =
    &["settings.json", "keymap.json", "tasks.json", "debug.json"];
const IMPORTED_CONFIG_DIRECTORIES: &[&str] = &["themes", "snippets"];
const IMPORT_BACKUP_SUFFIX: &str = ".before-zed-import";

const DEVELOPMENT_CREDENTIALS_FILE_NAME: &str = "development_credentials";

/// Messages from [`migrate_from_zed`], which runs before logging is set up.
static MIGRATION_REPORT: OnceLock<Vec<(log::Level, String)>> = OnceLock::new();

/// The Zed counterpart of one of Suzuri's top-level folders: the sibling with
/// Zed's name in place of Suzuri's. `None` for a folder not named after the
/// app, such as one under `--user-data-dir`.
fn legacy_dir(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    if name == paths::APP_NAME {
        Some(dir.with_file_name(LEGACY_APP_NAME))
    } else if name == paths::APP_NAME_LOWERCASE {
        Some(dir.with_file_name(LEGACY_APP_NAME_LOWERCASE))
    } else {
        None
    }
}

/// Copies an existing Suzuri user's state out of Zed's folders.
///
/// Must run before anything creates Suzuri's folders: a config folder that
/// already exists means Suzuri has been set up and nothing is copied. Logging
/// is not available yet, so the outcome is logged later by [`init`].
pub fn migrate_from_zed() {
    let config_dir = paths::config_dir();
    let data_dir = paths::data_dir();
    let report = match (legacy_dir(config_dir), legacy_dir(data_dir)) {
        (Some(legacy_config_dir), Some(legacy_data_dir)) => {
            migrate(&legacy_config_dir, config_dir, &legacy_data_dir, data_dir)
        }
        _ => Vec::new(),
    };
    MIGRATION_REPORT.set(report).ok();
}

fn migrate(
    legacy_config_dir: &Path,
    config_dir: &Path,
    legacy_data_dir: &Path,
    data_dir: &Path,
) -> Vec<(log::Level, String)> {
    let mut report = Vec::new();
    if config_dir.exists() || !used_suzuri(legacy_data_dir) {
        return report;
    }

    // The config folder goes last because its existence marks the migration as
    // done. A launch that dies in between finds the data in place and copies
    // only the config.
    if !data_dir.exists() {
        report.push(copy_into_place(
            legacy_data_dir,
            data_dir,
            Some(MIGRATED_DATA_ENTRIES),
        ));
    }
    if legacy_config_dir.is_dir() {
        report.push(copy_into_place(legacy_config_dir, config_dir, None));
    }
    report
}

/// Whether Zed's data folder holds state written by Suzuri rather than by Zed
/// alone. Released Zed runs on other channels, so it never writes `db/0-dev`,
/// and only Suzuri writes the recovery file. A Zed-only user installing Suzuri
/// starts clean and can import Zed's settings from the welcome page instead.
fn used_suzuri(legacy_data_dir: &Path) -> bool {
    legacy_data_dir.join("db/0-dev").is_dir()
        || legacy_data_dir.join("suzuri-recovery.json").is_file()
}

/// Copies `source` (or only `entries` of it) to `destination` through a
/// staging folder renamed into place, so an interrupted copy never leaves a
/// partial folder that would pass for a finished migration.
fn copy_into_place(
    source: &Path,
    destination: &Path,
    entries: Option<&[&str]>,
) -> (log::Level, String) {
    let mut staging_name = destination.as_os_str().to_owned();
    staging_name.push(".migrating");
    let staging = PathBuf::from(staging_name);

    let result = (|| -> Result<()> {
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .with_context(|| format!("removing {}", staging.display()))?;
        }
        fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
        match entries {
            Some(entries) => {
                for entry in entries {
                    let entry_source = source.join(entry);
                    if fs::symlink_metadata(&entry_source).is_ok() {
                        copy_recursive(&entry_source, &staging.join(entry))?;
                    }
                }
            }
            None => copy_directory_contents(source, &staging)?,
        }
        fs::rename(&staging, destination).with_context(|| {
            format!(
                "renaming {} to {}",
                staging.display(),
                destination.display()
            )
        })
    })();

    match result {
        Ok(()) => (
            log::Level::Info,
            format!(
                "copied Suzuri's data from {} to {}",
                source.display(),
                destination.display()
            ),
        ),
        Err(error) => {
            if let Err(cleanup_error) = fs::remove_dir_all(&staging)
                && cleanup_error.kind() != io::ErrorKind::NotFound
            {
                eprintln!("could not remove {}: {cleanup_error}", staging.display());
            }
            (
                log::Level::Error,
                format!(
                    "could not copy Suzuri's data from {} to {}: {error:#}",
                    source.display(),
                    destination.display()
                ),
            )
        }
    }
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source).with_context(|| format!("reading {}", source.display()))? {
        let entry = entry?;
        copy_recursive(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_recursive(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let metadata =
        fs::symlink_metadata(source).with_context(|| format!("reading {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return copy_symlink(source, destination);
    }
    if metadata.is_dir() {
        fs::create_dir(destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        copy_directory_contents(source, destination)
    } else {
        fs::copy(source, destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
        Ok(())
    }
}

/// Recreates a symlink rather than following it. A TeX distribution in
/// `typeset_compilers` links its binaries this way, and a user's dotfiles may
/// link `settings.json`.
#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source).with_context(|| format!("reading {}", source.display()))?;
    std::os::unix::fs::symlink(&target, destination)
        .with_context(|| format!("linking {}", destination.display()))
}

/// Creating symlinks on Windows needs a privilege most users lack, so the
/// link's target is copied instead.
#[cfg(not(unix))]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::metadata(source).with_context(|| format!("reading {}", source.display()))?;
    if metadata.is_dir() {
        fs::create_dir(destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        copy_directory_contents(source, destination)
    } else {
        fs::copy(source, destination).with_context(|| format!("copying {}", source.display()))?;
        Ok(())
    }
}

/// Copies Zed's settings, keymap, tasks, themes, and snippets into Suzuri's
/// config folder. Replaced files are kept beside the new ones with
/// [`IMPORT_BACKUP_SUFFIX`]. Returns how many files were imported.
fn import_zed_config(legacy_config_dir: &Path, config_dir: &Path) -> Result<usize> {
    let mut imported = 0;
    for name in IMPORTED_CONFIG_FILES {
        let source = legacy_config_dir.join(name);
        if source.is_file() {
            import_file(&source, &config_dir.join(name))?;
            imported += 1;
        }
    }
    for name in IMPORTED_CONFIG_DIRECTORIES {
        let source_dir = legacy_config_dir.join(name);
        if !source_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&source_dir)
            .with_context(|| format!("reading {}", source_dir.display()))?
        {
            let entry = entry?;
            let source = entry.path();
            if source.is_file() {
                import_file(&source, &config_dir.join(name).join(entry.file_name()))?;
                imported += 1;
            }
        }
    }
    Ok(imported)
}

fn import_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if destination.exists() {
        let mut backup = destination.as_os_str().to_owned();
        backup.push(IMPORT_BACKUP_SUFFIX);
        fs::copy(destination, &backup)
            .with_context(|| format!("backing up {}", destination.display()))?;
    }
    // `fs::copy` follows a symlinked source, so a settings file Zed reads from
    // dotfiles arrives as a plain copy instead of a link Suzuri would share.
    fs::copy(source, destination)
        .with_context(|| format!("copying {} to {}", source.display(), destination.display()))?;
    Ok(())
}

#[derive(Default)]
struct ZedSettingsImported(bool);

impl Global for ZedSettingsImported {}

/// Whether Zed's settings were imported in this session, for the welcome page.
pub fn zed_settings_imported(cx: &App) -> bool {
    cx.try_global::<ZedSettingsImported>()
        .is_some_and(|imported| imported.0)
}

pub fn init(cx: &mut App) {
    for (level, message) in MIGRATION_REPORT.get().into_iter().flatten() {
        log::log!(*level, "{message}");
    }

    // `ZED_BUNDLE` is set by every bundle script and never by `cargo run`, the
    // same test `ReleaseChannel::poll_for_updates` uses. A source build keeps
    // Zed's file-based provider to avoid keychain prompts on every rebuild.
    if ReleaseChannel::global(cx) == ReleaseChannel::Dev && option_env!("ZED_BUNDLE").is_some() {
        cx.set_global(ZedCredentialsProvider(Arc::new(KeychainCredentials::new(
            paths::config_dir().join(DEVELOPMENT_CREDENTIALS_FILE_NAME),
        ))));
    }

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_workspace, action: &ImportZedSettings, window, cx| {
            let workspace = cx.weak_entity();
            let skip_prompt = action.skip_prompt;
            window
                .spawn(cx, async move |cx| {
                    handle_import_zed_settings(workspace, skip_prompt, cx).await
                })
                .detach();
        });
    })
    .detach();
}

async fn handle_import_zed_settings(
    workspace: WeakEntity<Workspace>,
    skip_prompt: bool,
    cx: &mut AsyncWindowContext,
) {
    let config_dir = paths::config_dir().clone();
    let Some(legacy_config_dir) =
        legacy_dir(&config_dir).filter(|dir| dir.join("settings.json").is_file())
    else {
        show_toast(
            &workspace,
            "No Zed settings were found to import.",
            false,
            cx,
        );
        return;
    };

    if !skip_prompt {
        let answer = cx
            .prompt(
                gpui::PromptLevel::Warning,
                "Import settings from Zed?",
                Some(&format!(
                    "Suzuri's settings, keymap, and tasks will be replaced with Zed's from {}. \
                    Replaced files are kept with a {IMPORT_BACKUP_SUFFIX} suffix.",
                    legacy_config_dir.display()
                )),
                &["Import", "Cancel"],
            )
            .await;
        if answer != Ok(0) {
            return;
        }
    }

    let result = cx
        .background_spawn({
            let legacy_config_dir = legacy_config_dir.clone();
            async move { import_zed_config(&legacy_config_dir, &config_dir) }
        })
        .await;
    match result {
        Ok(imported) => {
            log::info!(
                "imported {imported} files from Zed's config folder {}",
                legacy_config_dir.display()
            );
            cx.update(|_, cx| cx.set_global(ZedSettingsImported(true)))
                .ok();
            show_toast(
                &workspace,
                "Your Zed settings were successfully imported.",
                true,
                cx,
            );
        }
        Err(error) => {
            log::error!("could not import Zed settings: {error:#}");
            show_toast(
                &workspace,
                "Failed to import Zed settings. See log for details",
                false,
                cx,
            );
        }
    }
}

fn show_toast(
    workspace: &WeakEntity<Workspace>,
    message: &'static str,
    success: bool,
    cx: &mut AsyncWindowContext,
) {
    workspace
        .update_in(cx, |workspace, _, cx| {
            let toast = StatusToast::new(message, cx, move |this, _| {
                let this = if success {
                    this.icon(
                        Icon::new(IconName::Check)
                            .size(IconSize::Small)
                            .color(Color::Success),
                    )
                } else {
                    this.icon(
                        Icon::new(IconName::Close)
                            .size(IconSize::Small)
                            .color(Color::Error),
                    )
                    .action("Open Log", |window, cx| {
                        window.dispatch_action(workspace::OpenLog.boxed_clone(), cx)
                    })
                };
                this.dismiss_button(true)
            });
            workspace.toggle_status_toast(toast, cx);
        })
        .ok();
}

type StoredCredentials = HashMap<String, (String, Vec<u8>)>;

/// The system keychain, plus a one-time move of the credentials an earlier
/// Suzuri build left in `development_credentials`.
///
/// Suzuri ships on the `dev` channel, where Zed's provider writes credentials,
/// API keys included, to that plain-text file so that Zed's own developers are
/// spared keychain prompts. The first credential access moves the file's
/// entries into the keychain and deletes the file. Entries the keychain refuses
/// stay in the file, and are still served from it, so nothing is lost on a
/// machine without a working keychain.
struct KeychainCredentials {
    legacy_file: PathBuf,
    /// `None` until the first access has moved the file's entries; afterwards,
    /// the entries the keychain refused.
    unmoved: Mutex<Option<StoredCredentials>>,
}

impl KeychainCredentials {
    fn new(legacy_file: PathBuf) -> Self {
        Self {
            legacy_file,
            unmoved: Mutex::new(None),
        }
    }

    async fn unmoved_credentials(
        &self,
        cx: &AsyncApp,
    ) -> futures::lock::MutexGuard<'_, Option<StoredCredentials>> {
        let mut unmoved = self.unmoved.lock().await;
        if unmoved.is_none() {
            *unmoved = Some(self.move_into_keychain(cx).await);
        }
        unmoved
    }

    async fn move_into_keychain(&self, cx: &AsyncApp) -> StoredCredentials {
        let stored = match fs::read(&self.legacy_file) {
            Ok(contents) => match serde_json::from_slice::<StoredCredentials>(&contents) {
                Ok(stored) => stored,
                Err(error) => {
                    log::error!("could not parse {}: {error}", self.legacy_file.display());
                    return StoredCredentials::default();
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return StoredCredentials::default();
            }
            Err(error) => {
                log::error!("could not read {}: {error}", self.legacy_file.display());
                return StoredCredentials::default();
            }
        };

        let total = stored.len();
        let mut unmoved = StoredCredentials::default();
        for (url, (username, password)) in stored {
            // An existing keychain entry is left alone: on macOS it may belong
            // to Zed, which keys credentials by the same URLs.
            let existing = cx.update(|cx| cx.read_credentials(&url)).await;
            let result = match existing {
                Ok(Some(_)) => Ok(()),
                Ok(None) => {
                    cx.update(|cx| cx.write_credentials(&url, &username, &password))
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                log::error!(
                    "could not move the credentials for {url} into the keychain: {error:#}"
                );
                unmoved.insert(url, (username, password));
            }
        }

        if let Err(error) = self.persist(&unmoved) {
            log::error!("{error:#}");
        } else if total > 0 {
            log::info!(
                "moved {} of {total} credentials from {} into the keychain",
                total - unmoved.len(),
                self.legacy_file.display()
            );
        }
        unmoved
    }

    fn persist(&self, unmoved: &StoredCredentials) -> Result<()> {
        if unmoved.is_empty() {
            match fs::remove_file(&self.legacy_file) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    Err(error).with_context(|| format!("removing {}", self.legacy_file.display()))
                }
                _ => Ok(()),
            }
        } else {
            fs::write(&self.legacy_file, serde_json::to_vec(unmoved)?)
                .with_context(|| format!("writing {}", self.legacy_file.display()))
        }
    }
}

impl CredentialsProvider for KeychainCredentials {
    fn read_credentials<'a>(
        &'a self,
        url: &'a str,
        cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>> {
        Box::pin(async move {
            let unmoved = self.unmoved_credentials(cx).await;
            if let Some(credentials) = unmoved.as_ref().and_then(|unmoved| unmoved.get(url)) {
                return Ok(Some(credentials.clone()));
            }
            drop(unmoved);
            cx.update(|cx| cx.read_credentials(url)).await
        })
    }

    fn write_credentials<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a [u8],
        cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let mut unmoved = self.unmoved_credentials(cx).await;
            cx.update(|cx| cx.write_credentials(url, username, password))
                .await?;
            if let Some(unmoved) = unmoved.as_mut()
                && unmoved.remove(url).is_some()
            {
                self.persist(unmoved)?;
            }
            Ok(())
        })
    }

    fn delete_credentials<'a>(
        &'a self,
        url: &'a str,
        cx: &'a AsyncApp,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let mut unmoved = self.unmoved_credentials(cx).await;
            if let Some(unmoved) = unmoved.as_mut()
                && unmoved.remove(url).is_some()
            {
                self.persist(unmoved)?;
            }
            cx.update(|cx| cx.delete_credentials(url)).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folders {
        _root: tempfile::TempDir,
        legacy_config: PathBuf,
        config: PathBuf,
        legacy_data: PathBuf,
        data: PathBuf,
    }

    impl Folders {
        fn new() -> Folders {
            let root = tempfile::tempdir().expect("temp dir");
            let path = root.path().to_path_buf();
            Folders {
                legacy_config: path.join("config/zed"),
                config: path.join("config/suzuri"),
                legacy_data: path.join("data/Zed"),
                data: path.join("data/Suzuri"),
                _root: root,
            }
        }

        fn migrate(&self) -> Vec<(log::Level, String)> {
            migrate(
                &self.legacy_config,
                &self.config,
                &self.legacy_data,
                &self.data,
            )
        }
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, contents).expect("write");
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    #[test]
    fn legacy_dir_swaps_the_app_name() {
        assert_eq!(
            legacy_dir(Path::new("/home/me/.config/suzuri")),
            Some(PathBuf::from("/home/me/.config/zed"))
        );
        assert_eq!(
            legacy_dir(Path::new("/Users/me/Library/Application Support/Suzuri")),
            Some(PathBuf::from("/Users/me/Library/Application Support/Zed"))
        );
        assert_eq!(legacy_dir(Path::new("/tmp/custom-data-dir/config")), None);
    }

    #[test]
    fn migrates_a_suzuri_users_state() {
        let folders = Folders::new();
        write(&folders.legacy_config.join("settings.json"), "{\"a\": 1}");
        write(&folders.legacy_config.join("themes/mine.json"), "theme");
        write(&folders.legacy_data.join("db/0-dev/db.sqlite"), "suzuri db");
        write(
            &folders.legacy_data.join("db/0-global/db.sqlite"),
            "global db",
        );
        write(&folders.legacy_data.join("db/0-stable/db.sqlite"), "zed db");
        write(&folders.legacy_data.join("threads/threads.db"), "threads");
        write(&folders.legacy_data.join("extensions/index.json"), "{}");
        write(&folders.legacy_data.join("node/bin/node"), "node");

        let report = folders.migrate();
        assert!(
            report.iter().all(|(level, _)| *level == log::Level::Info),
            "{report:?}"
        );

        assert_eq!(read(&folders.config.join("settings.json")), "{\"a\": 1}");
        assert_eq!(read(&folders.config.join("themes/mine.json")), "theme");
        assert_eq!(read(&folders.data.join("db/0-dev/db.sqlite")), "suzuri db");
        assert_eq!(
            read(&folders.data.join("db/0-global/db.sqlite")),
            "global db"
        );
        assert_eq!(read(&folders.data.join("threads/threads.db")), "threads");
        assert_eq!(read(&folders.data.join("extensions/index.json")), "{}");
        assert!(!folders.data.join("db/0-stable").exists());
        assert!(!folders.data.join("node").exists());

        assert_eq!(
            read(&folders.legacy_config.join("settings.json")),
            "{\"a\": 1}",
            "Zed's folders must be left as they were"
        );
        assert!(folders.legacy_data.join("node/bin/node").exists());
    }

    #[test]
    fn leaves_a_zed_only_user_alone() {
        let folders = Folders::new();
        write(&folders.legacy_config.join("settings.json"), "{}");
        write(&folders.legacy_data.join("db/0-stable/db.sqlite"), "zed db");

        assert!(folders.migrate().is_empty());
        assert!(!folders.config.exists());
        assert!(!folders.data.exists());
    }

    #[test]
    fn runs_only_until_suzuri_has_a_config_folder() {
        let folders = Folders::new();
        write(&folders.legacy_config.join("settings.json"), "zed");
        write(&folders.legacy_data.join("suzuri-recovery.json"), "{}");
        write(&folders.config.join("settings.json"), "suzuri");

        assert!(folders.migrate().is_empty());
        assert_eq!(read(&folders.config.join("settings.json")), "suzuri");
        assert!(!folders.data.exists());
    }

    #[test]
    fn finishes_a_migration_interrupted_before_the_config_folder() {
        let folders = Folders::new();
        write(&folders.legacy_config.join("settings.json"), "zed");
        write(&folders.legacy_data.join("db/0-dev/db.sqlite"), "zed db");
        write(&folders.data.join("db/0-dev/db.sqlite"), "already copied");
        let stale_staging = PathBuf::from(format!("{}.migrating", folders.config.display()));
        write(&stale_staging.join("half-copied"), "stale");

        folders.migrate();
        assert_eq!(
            read(&folders.data.join("db/0-dev/db.sqlite")),
            "already copied"
        );
        assert_eq!(read(&folders.config.join("settings.json")), "zed");
        assert!(!folders.config.join("half-copied").exists());
        assert!(!stale_staging.exists());
    }

    #[cfg(unix)]
    #[test]
    fn keeps_symlinks_as_symlinks() {
        let folders = Folders::new();
        write(&folders.legacy_data.join("db/0-dev/db.sqlite"), "db");
        write(
            &folders.legacy_data.join("typeset_compilers/tex/bin/real"),
            "binary",
        );
        std::os::unix::fs::symlink(
            "real",
            folders.legacy_data.join("typeset_compilers/tex/bin/link"),
        )
        .expect("symlink");
        fs::create_dir_all(&folders.legacy_config).expect("config");

        folders.migrate();
        let link = folders.data.join("typeset_compilers/tex/bin/link");
        assert_eq!(
            fs::read_link(&link).expect("still a link"),
            Path::new("real")
        );
        assert_eq!(read(&link), "binary");
    }

    #[test]
    fn import_replaces_config_files_and_keeps_backups() {
        let folders = Folders::new();
        write(&folders.legacy_config.join("settings.json"), "zed settings");
        write(&folders.legacy_config.join("keymap.json"), "zed keymap");
        write(
            &folders.legacy_config.join("themes/zed-theme.json"),
            "theme",
        );
        write(
            &folders.legacy_config.join("development_credentials"),
            "secret",
        );
        write(&folders.config.join("settings.json"), "suzuri settings");
        write(&folders.config.join("themes/suzuri-theme.json"), "mine");

        let imported = import_zed_config(&folders.legacy_config, &folders.config).expect("import");
        assert_eq!(imported, 3);
        assert_eq!(read(&folders.config.join("settings.json")), "zed settings");
        assert_eq!(
            read(&folders.config.join("settings.json.before-zed-import")),
            "suzuri settings"
        );
        assert_eq!(read(&folders.config.join("keymap.json")), "zed keymap");
        assert!(
            !folders
                .config
                .join("keymap.json.before-zed-import")
                .exists()
        );
        assert_eq!(read(&folders.config.join("themes/zed-theme.json")), "theme");
        assert_eq!(
            read(&folders.config.join("themes/suzuri-theme.json")),
            "mine"
        );
        assert!(!folders.config.join("development_credentials").exists());
    }
}
