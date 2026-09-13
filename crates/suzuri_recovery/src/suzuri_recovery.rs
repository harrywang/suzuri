//! Crash recovery for Suzuri's release channel.
//!
//! Suzuri ships on Zed's `dev` channel, where Zed installs no crash handler: a
//! panic prints to stderr and exits. Launched from the Dock, stderr goes
//! nowhere, so nothing records why the app quit. The next launch then restores
//! the same session, and a file that panics during layout makes the app die a
//! fraction of a second after every launch, with no clue which file did it.
//!
//! Two pieces close that loop:
//!
//! 1. [`install_panic_hook`] writes the panic and its backtrace into the Zed
//!    log before exiting, and stamps the recovery file so the next launch knows
//!    the previous one died.
//! 2. Every pane activation records the activated file as the current
//!    *suspect*. A panic that follows within [`BLAME_WINDOW`] blames that file.
//!    On the next launch the blamed file opens with live preview off (one
//!    strike), and is left out of session restore altogether once it has
//!    crashed two launches in a row. A launch that ends without a panic clears
//!    all strikes, so a fixed build gets a clean retry.
//!
//! The suspect is recorded from a `Pane` observer registered when the pane is
//! built, which puts its subscriber ahead of the workspace's own pane handler.
//! That order matters: the status bar reads the newly active editor's display
//! map synchronously inside that handler, which is where a layout panic fires.

use anyhow::{Context as _, Result};
use gpui::{App, AppContext as _, Context, DismissEvent, Global, Window};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use util::ResultExt as _;
use workspace::{
    AppState, ItemHandle, Pane, Workspace,
    notifications::{
        NotificationId, show_app_notification, simple_message_notification::MessageNotification,
    },
    pane::Event as PaneEvent,
};

/// How long after a file is activated a panic is still blamed on it. Layout of
/// a freshly activated file happens within a frame, so a panic further out is
/// treated as unrelated rather than quarantining a file that was merely open.
const BLAME_WINDOW: Duration = Duration::from_secs(30);

const RECOVERY_FILE_NAME: &str = "suzuri-recovery.json";

#[derive(Default, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct RecoveryFile {
    /// The file most recently activated in a pane, and when.
    #[serde(default)]
    suspect: Option<Suspect>,
    /// Stamped by the panic hook; consumed by the next launch.
    #[serde(default)]
    panicked_at: Option<u64>,
    /// Consecutive launches that panicked right after activating each file.
    #[serde(default)]
    strikes: BTreeMap<PathBuf, u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct Suspect {
    path: PathBuf,
    noted_at: u64,
}

/// The file the previous launch's panic was blamed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blamed {
    pub path: PathBuf,
    pub strikes: u32,
}

struct GlobalRecovery {
    blamed: Option<Blamed>,
    pending_notification: Option<Blamed>,
}

impl Global for GlobalRecovery {}

fn recovery_path() -> PathBuf {
    paths::data_dir().join(RECOVERY_FILE_NAME)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn read_file(path: &Path) -> RecoveryFile {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|error| {
            log::warn!(
                "ignoring unreadable recovery file {}: {error}",
                path.display()
            );
            RecoveryFile::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RecoveryFile::default(),
        Err(error) => {
            log::warn!("could not read recovery file {}: {error}", path.display());
            RecoveryFile::default()
        }
    }
}

fn write_file(path: &Path, file: &RecoveryFile) -> Result<()> {
    let contents = serde_json::to_string_pretty(file)?;
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Installs the panic hook Suzuri uses in place of `crashes::force_backtrace`.
///
/// Besides printing to stderr and exiting like the original, it writes the
/// panic and its backtrace into the Zed log, where a Dock launch can still be
/// diagnosed, and stamps the recovery file so the next launch can blame the
/// file that was activated just before.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let path = recovery_path();
        let mut file = read_file(&path);
        let suspect = file
            .suspect
            .as_ref()
            .map(|suspect| format!(" (last activated file: {})", suspect.path.display()))
            .unwrap_or_default();
        log::error!("Suzuri panicked{suspect}: {info}\n{backtrace}");
        zlog::flush();

        file.panicked_at = Some(unix_now());
        if let Err(error) = write_file(&path, &file) {
            eprintln!("could not record the panic in {}: {error}", path.display());
        }

        // The rest mirrors `crashes::force_backtrace`: a terminal launch still
        // gets the backtrace on stderr, and exiting keeps macOS from raising
        // its own crash dialog.
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
        previous(info);
        if cfg!(target_os = "macos") {
            std::process::exit(1);
        }
    }));
}

/// Consumes the previous launch's outcome from the recovery file.
///
/// A panic within [`BLAME_WINDOW`] of the last activation adds a strike to that
/// file and returns it. A launch that did not panic clears every strike.
fn settle(file: &mut RecoveryFile, now: u64) -> Option<Blamed> {
    let panicked_at = file.panicked_at.take();
    let suspect = file.suspect.take();

    let Some(panicked_at) = panicked_at else {
        file.strikes.clear();
        return None;
    };

    // `now` only guards against a clock that ran backwards between launches:
    // a suspect noted after the panic it is being blamed for is not a suspect.
    let suspect = suspect.filter(|suspect| {
        suspect.noted_at <= panicked_at
            && suspect.noted_at <= now
            && panicked_at - suspect.noted_at <= BLAME_WINDOW.as_secs()
    })?;

    let strikes = file.strikes.entry(suspect.path.clone()).or_insert(0);
    *strikes += 1;
    Some(Blamed {
        path: suspect.path,
        strikes: *strikes,
    })
}

fn note_suspect(path: &Path, suspect: &Path, now: u64) {
    let mut file = read_file(path);
    file.suspect = Some(Suspect {
        path: suspect.to_path_buf(),
        noted_at: now,
    });
    write_file(path, &file).log_err();
}

pub fn init(cx: &mut App) {
    let path = recovery_path();
    let mut file = read_file(&path);
    let blamed = settle(&mut file, unix_now());
    write_file(&path, &file).log_err();

    if let Some(blamed) = &blamed {
        log::warn!(
            "the previous launch panicked within {}s of activating {}; strike {}",
            BLAME_WINDOW.as_secs(),
            blamed.path.display(),
            blamed.strikes
        );
    }
    cx.set_global(GlobalRecovery {
        blamed: blamed.clone(),
        pending_notification: blamed,
    });

    cx.observe_new(
        |_: &mut Pane, _window: Option<&mut Window>, cx: &mut Context<Pane>| {
            cx.subscribe_self(|pane, event: &PaneEvent, cx| {
                let item = match event {
                    PaneEvent::AddItem { item } => item.boxed_clone(),
                    PaneEvent::ActivateItem { .. } => match pane.active_item() {
                        Some(item) => item,
                        None => return,
                    },
                    _ => return,
                };
                if let Some(abs_path) = abs_path_of(cx.entity_id(), item.as_ref(), cx) {
                    note_suspect(&recovery_path(), &abs_path, unix_now());
                }
            })
            .detach();
        },
    )
    .detach();

    cx.observe_new(
        |_: &mut Workspace, _window: Option<&mut Window>, cx: &mut Context<Workspace>| {
            let Some(blamed) = cx
                .global_mut::<GlobalRecovery>()
                .pending_notification
                .take()
            else {
                return;
            };
            // The workspace being built is mid-update here, and the notification
            // is delivered by updating every workspace.
            cx.defer(move |cx| show_recovery_notification(blamed, cx));
        },
    )
    .detach();
}

/// Resolves an item's file through the workspace that owns `pane`.
///
/// The pane keeps its project private, and the window's root view is not set
/// yet when the first pane is built, so the workspace is found through the
/// store instead.
fn abs_path_of(pane: gpui::EntityId, item: &dyn ItemHandle, cx: &App) -> Option<PathBuf> {
    let project_path = item.project_path(cx)?;
    let app_state = AppState::global(cx);
    let store = app_state.workspace_store.read(cx);
    store.workspaces().find_map(|workspace| {
        let workspace = workspace.upgrade()?;
        let workspace = workspace.read(cx);
        if !workspace
            .panes()
            .iter()
            .any(|candidate| candidate.entity_id() == pane)
        {
            return None;
        }
        workspace
            .project()
            .read(cx)
            .absolute_path(&project_path, cx)
    })
}

/// Strikes against `path` from the previous launch's panic, if it was blamed.
pub fn strikes_against(path: &Path, cx: &App) -> Option<u32> {
    cx.try_global::<GlobalRecovery>()?
        .blamed
        .as_ref()
        .filter(|blamed| blamed.path == path)
        .map(|blamed| blamed.strikes)
}

/// One strike: the file opens, but without live preview, since the fork's
/// rendering is the likeliest home of a file-specific panic.
pub fn should_disable_live_preview(path: &Path, cx: &App) -> bool {
    strikes_against(path, cx).is_some()
}

/// Two strikes: the file crashed even without live preview, so session restore
/// leaves it out and the user gets a working window to open it from by hand.
pub fn should_skip_restore(path: &Path, cx: &App) -> bool {
    strikes_against(path, cx).is_some_and(|strikes| strikes >= 2)
}

fn show_recovery_notification(blamed: Blamed, cx: &mut App) {
    struct CrashRecovery;

    let name = blamed
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| blamed.path.display().to_string());
    let message = if blamed.strikes >= 2 {
        format!(
            "Suzuri crashed again right after opening {name}, so it was not reopened. The panic is in the log."
        )
    } else {
        format!(
            "Suzuri crashed right after opening {name}, so it was reopened with live preview off. The panic is in the log."
        )
    };

    show_app_notification(NotificationId::unique::<CrashRecovery>(), cx, move |cx| {
        cx.new(|cx| {
            MessageNotification::new(message.clone(), cx)
                .primary_message("Open Log")
                .primary_on_click(|_, cx| {
                    cx.open_with_system(paths::log_file());
                    cx.emit(DismissEvent);
                })
                .show_suppress_button(false)
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suspect(path: &str, noted_at: u64) -> Option<Suspect> {
        Some(Suspect {
            path: PathBuf::from(path),
            noted_at,
        })
    }

    #[test]
    fn a_clean_launch_clears_strikes_and_blames_nobody() {
        let mut file = RecoveryFile {
            suspect: suspect("/notes/a.md", 100),
            panicked_at: None,
            strikes: BTreeMap::from([(PathBuf::from("/notes/a.md"), 1)]),
        };
        assert_eq!(settle(&mut file, 200), None);
        assert_eq!(file, RecoveryFile::default());
    }

    #[test]
    fn a_panic_right_after_activation_blames_the_file_and_counts_strikes() {
        let mut file = RecoveryFile {
            suspect: suspect("/notes/a.md", 100),
            panicked_at: Some(101),
            strikes: BTreeMap::new(),
        };
        assert_eq!(
            settle(&mut file, 200),
            Some(Blamed {
                path: PathBuf::from("/notes/a.md"),
                strikes: 1
            })
        );
        assert_eq!(file.suspect, None);
        assert_eq!(file.panicked_at, None);

        file.suspect = suspect("/notes/a.md", 300);
        file.panicked_at = Some(320);
        assert_eq!(
            settle(&mut file, 400),
            Some(Blamed {
                path: PathBuf::from("/notes/a.md"),
                strikes: 2
            })
        );
    }

    #[test]
    fn a_panic_long_after_activation_is_not_blamed_on_the_file() {
        let mut file = RecoveryFile {
            suspect: suspect("/notes/a.md", 100),
            panicked_at: Some(100 + BLAME_WINDOW.as_secs() + 1),
            strikes: BTreeMap::from([(PathBuf::from("/notes/b.md"), 1)]),
        };
        assert_eq!(settle(&mut file, 1000), None);
        assert_eq!(file.suspect, None);
        assert_eq!(file.panicked_at, None);
        assert_eq!(
            file.strikes,
            BTreeMap::from([(PathBuf::from("/notes/b.md"), 1)]),
            "an unattributed panic keeps existing strikes"
        );
    }

    #[test]
    fn a_suspect_noted_after_the_panic_is_not_blamed() {
        let mut file = RecoveryFile {
            suspect: suspect("/notes/a.md", 150),
            panicked_at: Some(100),
            strikes: BTreeMap::new(),
        };
        assert_eq!(settle(&mut file, 200), None);
    }

    #[test]
    fn the_recovery_file_round_trips_and_tolerates_absence() {
        let dir = std::env::temp_dir().join(format!(
            "suzuri-recovery-test-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RECOVERY_FILE_NAME);

        assert_eq!(read_file(&path), RecoveryFile::default());

        note_suspect(&path, Path::new("/notes/a.md"), 100);
        let mut file = read_file(&path);
        assert_eq!(file.suspect, suspect("/notes/a.md", 100));

        file.panicked_at = Some(101);
        write_file(&path, &file).unwrap();
        assert_eq!(read_file(&path), file);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_file(&path), RecoveryFile::default());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
