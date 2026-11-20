//! Auto sync: commit and push a project's git repository a few seconds after
//! every change, and pull from its remote every few minutes, so a folder of
//! notes stays backed up on GitHub without anyone running git by hand.
//!
//! It is opted into per repository and per machine, with a key in that
//! repository's own `.git/config`, so a project in one repository can sync
//! while another stays manual. Every git operation goes through Zed's
//! `Repository`, which serializes them with the Git panel's own.

mod connect;
mod github;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use askpass::{AskPassDelegate, EncryptedPassword};
use collections::HashMap;
use fs::Fs;
use futures::channel::oneshot;
use git::repository::{Branch, CommitOptions, PushOptions, UpstreamTracking};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, PromptLevel,
    SharedString, Subscription, Task, TaskExt as _, WeakEntity, Window, actions,
};
use project::{
    Project,
    git_store::{GitStoreEvent, Repository, RepositoryEvent, RepositoryId},
    trusted_worktrees::TrustedWorktrees,
};
use settings::{RegisterSetting, Settings};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

actions!(
    auto_sync,
    [
        /// Commits and pushes this repository a few seconds after every change, and pulls from its remote every few minutes.
        TurnOn,
        /// Stops syncing this repository automatically.
        TurnOff,
        /// Syncs this repository now, and resumes syncing after an error stopped it.
        SyncNow,
        /// Signs in to GitHub again, for when the remote stopped accepting the saved sign-in.
        SignIn,
    ]
);

const DEFAULT_SYNC_DELAY_SECONDS: u64 = 5;
const DEFAULT_PULL_INTERVAL_MINUTES: u64 = 5;

/// Settings for auto sync. Which repositories sync is not among them: that
/// is opted into per repository, in its own git config.
#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct AutoSyncSettings {
    /// How long the working tree must stay unchanged before a sync, so a
    /// burst of autosaves becomes one commit.
    pub sync_delay: Duration,
    /// How often changes made elsewhere are pulled.
    pub pull_interval: Duration,
}

impl Settings for AutoSyncSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.auto_sync.clone().unwrap_or_default();
        // Both are non-zero by type: a zero would commit on every
        // keystroke's autosave, or pull in a tight loop.
        Self {
            sync_delay: Duration::from_secs(
                content
                    .sync_delay_seconds
                    .map_or(DEFAULT_SYNC_DELAY_SECONDS, |seconds| seconds.get().into()),
            ),
            pull_interval: Duration::from_secs(
                content
                    .pull_interval_minutes
                    .map_or(DEFAULT_PULL_INTERVAL_MINUTES, |minutes| {
                        minutes.get().into()
                    })
                    .saturating_mul(60),
            ),
        }
    }
}

fn sync_delay(cx: &App) -> Duration {
    AutoSyncSettings::get_global(cx).sync_delay
}

const RETRY_DELAY: Duration = Duration::from_secs(30);
const CONFLICT_RECHECK_DELAY: Duration = Duration::from_secs(10);
/// Failures in a row, other than being offline, before syncing stops and asks
/// for help instead of retrying forever.
const MAX_FAILURES: u32 = 3;

const ENABLED_KEY: &str = "suzuri.autosync";
/// Set when turning on auto sync also set `pull.rebase`, so turning it off
/// removes only what it added.
const MANAGED_PULL_KEY: &str = "suzuri.autosyncpullrebase";

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        let project = workspace.project().clone();
        // `git config` and the background askpass only work on the machine
        // that owns the repository.
        if !project.read(cx).is_local() {
            return;
        }
        let workspace_handle = cx.weak_entity();
        let auto_sync = cx.new(|cx| AutoSync::new(project, workspace_handle, cx));
        // Attached per render, so the menu offers Turn On or Turn Off and
        // Sync Now depending on the opened folder's repository.
        workspace.register_action_renderer(move |div, workspace, _window, cx| {
            let enabled = target_repository(workspace, cx)
                .is_some_and(|repository| auto_sync.read(cx).is_enabled(repository.read(cx).id));
            if enabled {
                div.on_action(cx.listener({
                    let auto_sync = auto_sync.clone();
                    move |workspace, _: &TurnOff, window, cx| {
                        turn_off(&auto_sync, workspace, window, cx)
                    }
                }))
                .on_action(cx.listener({
                    let auto_sync = auto_sync.clone();
                    move |workspace, _: &SyncNow, _window, cx| sync_now(&auto_sync, workspace, cx)
                }))
                .on_action(cx.listener({
                    let auto_sync = auto_sync.clone();
                    move |workspace, _: &SignIn, window, cx| {
                        sign_in_again(&auto_sync, workspace, window, cx)
                    }
                }))
            } else {
                div.on_action(cx.listener({
                    let auto_sync = auto_sync.clone();
                    move |workspace, _: &TurnOn, window, cx| {
                        turn_on(&auto_sync, workspace, window, cx)
                    }
                }))
            }
        });
    })
    .detach();
}

struct AutoSync {
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    repositories: HashMap<RepositoryId, RepositorySync>,
    /// The GitHub sign-in, once it has been looked up: the outer `None` means
    /// the credential store has not been read yet, which is put off until a
    /// GitHub remote is actually synced.
    github_token: Option<Option<String>>,
    _subscription: Subscription,
}

#[derive(Default)]
struct RepositorySync {
    enabled: bool,
    paused: Option<Pause>,
    failures: u32,
    pending: Option<Trigger>,
    scheduled: Option<Task<()>>,
    running: Option<Task<()>>,
    pull_timer: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Trigger {
    Change,
    Timer,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pause {
    /// Resumes by itself once the conflicted files are resolved.
    Conflict,
    /// Waits for Sync Now.
    Stopped,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Idle,
    Synced,
    /// Nothing was synced yet; the same trigger runs again shortly.
    Deferred,
}

enum Problem {
    Blocked(Blocker),
    Failed(anyhow::Error),
}

#[derive(Debug, PartialEq, Eq)]
enum Blocker {
    NotTrusted,
    NoBranch,
    Rebasing,
    Conflicts(Vec<PathBuf>),
    NoRemote,
    SeveralRemotes,
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Offline,
    Auth,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
struct Plan {
    commit: bool,
    remote: SharedString,
    remote_branch: SharedString,
    pull: bool,
    set_upstream: bool,
    ahead: u32,
}

impl AutoSync {
    fn new(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let git_store = project.read(cx).git_store().clone();
        let subscription = cx.subscribe(&git_store, |this, _, event, cx| match event {
            GitStoreEvent::RepositoryAdded | GitStoreEvent::RepositoryRemoved(_) => {
                this.reconcile(cx)
            }
            GitStoreEvent::RepositoryUpdated(id, RepositoryEvent::StatusesChanged, _) => {
                this.request(*id, Trigger::Change, sync_delay(cx), cx)
            }
            _ => {}
        });
        let mut this = Self {
            project,
            workspace,
            repositories: HashMap::default(),
            github_token: None,
            _subscription: subscription,
        };
        this.reconcile(cx);
        this
    }

    fn is_enabled(&self, id: RepositoryId) -> bool {
        self.repositories
            .get(&id)
            .is_some_and(|repository| repository.enabled)
    }

    fn repository(&self, id: RepositoryId, cx: &App) -> Option<Entity<Repository>> {
        self.project
            .read(cx)
            .git_store()
            .read(cx)
            .repositories()
            .get(&id)
            .cloned()
    }

    /// Tracks the repositories the project has, reading each new one's opt-in
    /// from its git config.
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let repositories = self
            .project
            .read(cx)
            .git_store()
            .read(cx)
            .repositories()
            .clone();
        self.repositories
            .retain(|id, _| repositories.contains_key(id));
        for (id, repository) in repositories {
            if self.repositories.contains_key(&id) {
                continue;
            }
            self.repositories.insert(id, RepositorySync::default());
            let path = repository.read(cx).work_directory_abs_path.clone();
            let read = self.project.read(cx).git_config(
                path,
                vec!["--local".into(), "--get".into(), ENABLED_KEY.into()],
                cx,
            );
            cx.spawn(async move |this, cx| {
                // `git config --get` fails when the key is unset, which is
                // the common case of a repository that never opted in.
                let enabled = read.await.is_ok_and(|value| value.trim() == "true");
                if enabled {
                    // Quiet on purpose: restoring a session should neither
                    // toast "Synced" nor complain before the first scan.
                    this.update(cx, |this, cx| {
                        this.set_enabled(id, Some(Trigger::Timer), cx)
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    /// `first` is the trigger of the sync that starts right away, or `None`
    /// to turn syncing off.
    fn set_enabled(&mut self, id: RepositoryId, first: Option<Trigger>, cx: &mut Context<Self>) {
        let Some(state) = self.repositories.get_mut(&id) else {
            return;
        };
        *state = RepositorySync::default();
        let Some(first) = first else {
            return;
        };
        state.enabled = true;
        state.pull_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                // Read each time round, so a changed setting applies from
                // the next pull without restarting anything.
                let interval = cx.update(|cx| AutoSyncSettings::get_global(cx).pull_interval);
                cx.background_executor().timer(interval).await;
                if this
                    .update(cx, |this, cx| {
                        this.request(id, Trigger::Timer, Duration::ZERO, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        let delay = if first == Trigger::Manual {
            Duration::ZERO
        } else {
            sync_delay(cx)
        };
        self.request(id, first, delay, cx);
    }

    fn request(
        &mut self,
        id: RepositoryId,
        trigger: Trigger,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.repositories.get_mut(&id) else {
            return;
        };
        if !state.enabled || (state.paused == Some(Pause::Stopped) && trigger != Trigger::Manual) {
            return;
        }
        state.pending = Some(
            state
                .pending
                .map_or(trigger, |pending| pending.max(trigger)),
        );
        // Replacing the task restarts the delay, which is what turns a run of
        // saves into a single commit.
        state.scheduled = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.start(id, cx)).ok();
        }));
    }

    fn start(&mut self, id: RepositoryId, cx: &mut Context<Self>) {
        let Some(repository) = self.repository(id, cx) else {
            return;
        };
        let fs = self.project.read(cx).fs().clone();
        let Some(state) = self.repositories.get_mut(&id) else {
            return;
        };
        // A cycle in flight picks the pending trigger up when it finishes.
        if state.running.is_some() {
            return;
        }
        let Some(trigger) = state.pending.take() else {
            return;
        };
        if trigger == Trigger::Manual {
            state.paused = None;
            state.failures = 0;
        }
        let cached_token = self.github_token.clone();
        let nested = {
            let work_dir = repository.read(cx).work_directory_abs_path.clone();
            connect::nested_repositories(&work_dir, self.project.read(cx), cx)
        };
        state.running = Some(cx.spawn(async move |this, cx| {
            // Asked of git rather than read from the snapshot, which has not
            // caught up yet when a remote was added a moment ago.
            let urls = repository.update(cx, |repository, _| repository.remote_urls());
            let on_github = job_result(urls.await).is_ok_and(|urls| {
                urls.values()
                    .any(|url| url.starts_with("https://github.com/"))
            });
            let token = match cached_token {
                Some(token) => token,
                None if on_github => {
                    let token = connect::stored_token(cx).await;
                    this.update(cx, |this, _| this.github_token = Some(token.clone()))
                        .ok();
                    token
                }
                None => None,
            };
            let token = token.filter(|_| on_github);
            let result = run_cycle(repository, fs, trigger, token, nested, cx).await;
            this.update(cx, |this, cx| this.finish(id, trigger, result, cx))
                .ok();
        }));
    }

    fn finish(
        &mut self,
        id: RepositoryId,
        trigger: Trigger,
        result: Result<Outcome, Problem>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.repositories.get_mut(&id) else {
            return;
        };
        state.running = None;
        if !state.enabled {
            return;
        }
        match result {
            Ok(Outcome::Deferred) => {
                state.pending = Some(
                    state
                        .pending
                        .map_or(trigger, |pending| pending.max(trigger)),
                );
            }
            Ok(outcome) => {
                let was_paused = state.paused.take().is_some();
                state.failures = 0;
                if was_paused {
                    self.dismiss(id, cx);
                }
                if trigger == Trigger::Manual {
                    let message = match outcome {
                        Outcome::Synced => "Synced with the remote repository.",
                        Outcome::Idle | Outcome::Deferred => {
                            "Already in sync with the remote repository."
                        }
                    };
                    self.notify(id, message.into(), None, true, cx);
                }
            }
            Err(Problem::Blocked(Blocker::NotTrusted)) => {
                // Restricted mode has its own banner, and trusting the folder
                // lets the next change sync; only someone who just asked for
                // a sync needs telling why nothing happened.
                if trigger == Trigger::Manual {
                    self.notify(
                        id,
                        "Auto sync is waiting: this folder is in Restricted Mode. Trust the folder, then run Sync Now.".into(),
                        Some(sync_now_button()),
                        false,
                        cx,
                    );
                }
            }
            Err(Problem::Blocked(Blocker::Conflicts(paths))) => {
                // Removing the markers from a file git still reports as
                // unmerged changes no git status, so no event says the
                // conflict is resolved; look again shortly instead.
                state.pending = Some(state.pending.unwrap_or(Trigger::Timer));
                if state.paused != Some(Pause::Conflict) {
                    state.paused = Some(Pause::Conflict);
                    self.notify_conflicts(id, paths, cx);
                }
            }
            Err(Problem::Blocked(Blocker::NoBranch)) if trigger != Trigger::Manual => {
                // Also what a repository looks like before its first scan.
            }
            Err(Problem::Blocked(blocker)) => {
                state.paused = Some(Pause::Stopped);
                let message = match blocker {
                    Blocker::NoBranch => {
                        "Auto sync stopped: no branch is checked out. Check out a branch, then run Sync Now."
                    }
                    Blocker::Rebasing => {
                        "Auto sync stopped: a rebase is in progress. Finish or abort it in the terminal, then run Sync Now."
                    }
                    Blocker::NoRemote => {
                        "Auto sync stopped: this repository has no remote to push to. Add one, then run Sync Now."
                    }
                    Blocker::SeveralRemotes => {
                        "Auto sync stopped: this repository has several remotes. Push once from the Git panel to choose one, then run Sync Now."
                    }
                    Blocker::NotTrusted | Blocker::Conflicts(_) => return,
                };
                self.notify(id, message.into(), Some(sync_now_button()), false, cx);
            }
            Err(Problem::Failed(error)) => {
                let message = format!("{error:#}");
                log::warn!("auto sync failed: {message}");
                match classify_failure(&message) {
                    // The pull timer and the next save retry; nothing to tell.
                    Failure::Offline => {}
                    Failure::Auth => {
                        state.paused = Some(Pause::Stopped);
                        self.notify(
                            id,
                            "Auto sync stopped: the remote did not accept your sign-in. For a GitHub repository, sign in again. For any other remote, push once from the Git panel, then run Sync Now.".into(),
                            Some((
                                "Sign In to GitHub".into(),
                                Arc::new(|window: &mut Window, cx: &mut App| {
                                    window.dispatch_action(Box::new(SignIn), cx)
                                }),
                            )),
                            false,
                            cx,
                        );
                    }
                    Failure::Other => {
                        state.failures += 1;
                        if state.failures >= MAX_FAILURES {
                            state.paused = Some(Pause::Stopped);
                            let detail = error_summary(&message);
                            self.notify(
                                id,
                                format!("Auto sync stopped: {detail}"),
                                Some(sync_now_button()),
                                false,
                                cx,
                            );
                        } else {
                            state.pending = Some(state.pending.unwrap_or(Trigger::Timer));
                        }
                    }
                }
            }
        }

        let Some(state) = self.repositories.get(&id) else {
            return;
        };
        if let Some(pending) = state.pending {
            let delay = if state.paused == Some(Pause::Conflict) {
                CONFLICT_RECHECK_DELAY
            } else if state.failures > 0 {
                RETRY_DELAY
            } else {
                sync_delay(cx)
            };
            self.request(id, pending, delay, cx);
        }
    }

    fn notify_conflicts(&self, id: RepositoryId, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let names = paths
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let message = format!(
            "Auto sync paused: {} changed both here and on the remote. Open it, keep the version you want (or combine them), and save. Syncing resumes by itself.",
            if names.is_empty() {
                "a file".to_string()
            } else {
                names.join(", ")
            }
        );
        let open = paths.into_iter().next().map(|path| {
            let workspace = self.workspace.clone();
            let action: (SharedString, ToastAction) = (
                "Open File".into(),
                Arc::new(move |window: &mut Window, cx: &mut App| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .open_abs_path(path.clone(), Default::default(), window, cx)
                                .detach_and_log_err(cx);
                        })
                        .ok();
                }),
            );
            action
        });
        self.notify(id, message, open, false, cx);
    }

    fn notify(
        &self,
        id: RepositoryId,
        message: String,
        action: Option<(SharedString, ToastAction)>,
        autohide: bool,
        cx: &mut Context<Self>,
    ) {
        let mut toast = Toast::new(notification_id(id), message);
        if let Some((label, on_click)) = action {
            toast = toast.on_click(label.to_string(), move |window, cx| on_click(window, cx));
        }
        if autohide {
            toast = toast.autohide();
        }
        self.workspace
            .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
            .ok();
    }

    fn dismiss(&self, id: RepositoryId, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss_toast(&notification_id(id), cx)
            })
            .ok();
    }
}

type ToastAction = Arc<dyn Fn(&mut Window, &mut App)>;

fn sync_now_button() -> (SharedString, ToastAction) {
    (
        "Sync Now".into(),
        Arc::new(|window: &mut Window, cx: &mut App| window.dispatch_action(Box::new(SyncNow), cx)),
    )
}

fn notification_id(id: RepositoryId) -> NotificationId {
    NotificationId::named(format!("auto-sync-{}", id.to_proto()).into())
}

async fn run_cycle(
    repository: Entity<Repository>,
    fs: Arc<dyn Fs>,
    trigger: Trigger,
    github_token: Option<String>,
    nested: Vec<String>,
    cx: &mut AsyncApp,
) -> Result<Outcome, Problem> {
    let (trusted, branch, changes, conflicted, repository_dir, work_dir, exclude_path) = repository
        .read_with(cx, |repository, _| {
            let mut changes = Vec::new();
            let mut conflicted = Vec::new();
            for entry in repository.status() {
                if entry.status.is_ignored() {
                    continue;
                }
                if entry.status.is_conflicted() {
                    conflicted.push(entry.repo_path.as_std_path().to_path_buf());
                }
                changes.push(entry.repo_path.as_std_path().to_path_buf());
            }
            (
                repository.is_trusted(),
                repository.branch.clone(),
                changes,
                conflicted,
                repository.repository_dir_abs_path.clone(),
                repository.work_directory_abs_path.clone(),
                connect::exclude_path(repository),
            )
        });
    if !trusted {
        return Err(Problem::Blocked(Blocker::NotTrusted));
    }
    // Repositories nested in this one would be committed as bare pointers.
    // Once they are newly excluded, the status read above still lists them,
    // so this cycle stops and the next one works from a fresh scan.
    if connect::ensure_excluded(&fs, &exclude_path, &nested)
        .await
        .map_err(Problem::Failed)?
    {
        return Ok(Outcome::Deferred);
    }

    let rebasing = fs.is_dir(&repository_dir.join("rebase-merge")).await
        || fs.is_dir(&repository_dir.join("rebase-apply")).await;
    let merging = fs.is_file(&repository_dir.join("MERGE_HEAD")).await;
    // During a merge every changed file is checked, not only the ones git
    // reports as conflicted: the status scan can lag behind the pull that
    // started the merge, and staging a file with conflict markers in it would
    // commit them.
    let candidates = if merging { &changes } else { &conflicted };
    let mut unresolved = Vec::new();
    for path in candidates {
        let abs_path = work_dir.join(path);
        let is_unresolved = match fs.load(&abs_path).await {
            Ok(text) => has_conflict_markers(&text),
            // A deleted or binary file that git reports as conflicted cannot
            // be checked for markers, so it stays a conflict.
            Err(_) => conflicted.contains(path),
        };
        if is_unresolved {
            unresolved.push(abs_path);
        }
    }

    let remotes = match &branch {
        Some(branch) if upstream_remote(branch).is_none() => {
            let branch_name = branch.name().to_string();
            let remotes = repository.update(cx, |repository, _| {
                repository.get_remotes(Some(branch_name), true)
            });
            job_result(remotes.await)
                .map_err(Problem::Failed)?
                .into_iter()
                .map(|remote| remote.name)
                .collect()
        }
        _ => Vec::new(),
    };

    let plan = plan_cycle(
        branch.as_ref(),
        rebasing,
        unresolved,
        !changes.is_empty(),
        &remotes,
        trigger,
    )
    .map_err(Problem::Blocked)?;
    let Some(plan) = plan else {
        return Ok(Outcome::Idle);
    };
    let Some(branch) = branch else {
        return Err(Problem::Blocked(Blocker::NoBranch));
    };

    let mut committed = false;
    if plan.commit {
        repository
            .update(cx, |repository, cx| repository.stage_all(cx))
            .await
            .map_err(Problem::Failed)?;
        let message = commit_message(&changes);
        let askpass = background_askpass(github_token.clone(), cx);
        let commit = repository.update(cx, |repository, cx| {
            repository.commit(message.into(), None, CommitOptions::default(), askpass, cx)
        });
        match job_result(commit.await) {
            Ok(()) => committed = true,
            // The working tree changed back before the commit ran.
            Err(error) if format!("{error:#}").contains("nothing to commit") => {}
            Err(error) => return Err(Problem::Failed(error)),
        }
    }

    let mut pulled = false;
    if plan.pull {
        let askpass = background_askpass(github_token.clone(), cx);
        let remote = plan.remote.clone();
        let pull = repository.update(cx, |repository, cx| {
            repository.pull(None, remote, false, askpass, cx)
        });
        let output = job_result(pull.await).map_err(Problem::Failed)?;
        pulled = !output.stdout.contains("Already up to date");
    }

    if committed || pulled || plan.set_upstream || plan.ahead > 0 {
        let askpass = background_askpass(github_token.clone(), cx);
        let options = plan.set_upstream.then_some(PushOptions::SetUpstream);
        let push = repository.update(cx, |repository, cx| {
            repository.push(
                branch.name().to_owned().into(),
                plan.remote_branch.clone(),
                plan.remote.clone(),
                options,
                askpass,
                cx,
            )
        });
        job_result(push.await).map_err(Problem::Failed)?;
        return Ok(Outcome::Synced);
    }
    Ok(if committed || pulled {
        Outcome::Synced
    } else {
        Outcome::Idle
    })
}

/// Decides what one sync does, or why it cannot run. `None` means there is
/// nothing to do.
fn plan_cycle(
    branch: Option<&Branch>,
    rebasing: bool,
    unresolved: Vec<PathBuf>,
    has_changes: bool,
    push_remotes: &[SharedString],
    trigger: Trigger,
) -> Result<Option<Plan>, Blocker> {
    if rebasing {
        return Err(Blocker::Rebasing);
    }
    if !unresolved.is_empty() {
        return Err(Blocker::Conflicts(unresolved));
    }
    let branch = branch.ok_or(Blocker::NoBranch)?;

    let (remote, remote_branch, pull, set_upstream, ahead) = match upstream_remote(branch) {
        Some(remote) => {
            let upstream = branch.upstream.as_ref();
            let remote_branch = upstream
                .and_then(|upstream| upstream.branch_name())
                .unwrap_or(branch.name());
            match upstream.map(|upstream| upstream.tracking) {
                Some(UpstreamTracking::Tracked(status)) => {
                    (remote, remote_branch, true, false, status.ahead)
                }
                // The remote branch was deleted, or never pushed: publish it.
                _ => (remote, remote_branch, false, true, 0),
            }
        }
        None => match push_remotes {
            [remote] => (remote.as_ref(), branch.name(), false, true, 0),
            [] => return Err(Blocker::NoRemote),
            _ => return Err(Blocker::SeveralRemotes),
        },
    };

    // A change that leaves nothing to commit or push is usually this sync's
    // own commit showing up in the status; it is not worth a network trip.
    if trigger == Trigger::Change && !has_changes && !set_upstream && ahead == 0 {
        return Ok(None);
    }

    Ok(Some(Plan {
        commit: has_changes,
        remote: SharedString::from(remote.to_string()),
        remote_branch: SharedString::from(remote_branch.to_string()),
        pull,
        set_upstream,
        ahead,
    }))
}

fn upstream_remote(branch: &Branch) -> Option<&str> {
    branch
        .upstream
        .as_ref()
        .and_then(|upstream| upstream.remote_name())
}

fn has_conflict_markers(text: &str) -> bool {
    let mut opened = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<<") {
            opened = true;
        } else if opened && line.starts_with(">>>>>>>") {
            return true;
        }
    }
    false
}

fn commit_message(changes: &[PathBuf]) -> String {
    match changes {
        [path] => format!("Update {}", path.display()),
        _ => format!("Update {} files", changes.len()),
    }
}

/// The line of git's output that says what went wrong. Git reports progress
/// on stderr too ("From ../remote", "To github.com:…"), so the first line is
/// often not it.
fn error_summary(message: &str) -> &str {
    let lines = || {
        message
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
    };
    lines()
        .find(|line| {
            line.starts_with("fatal:") || line.starts_with("error:") || line.starts_with("! [")
        })
        .or_else(|| lines().next_back())
        .unwrap_or("git failed")
}

fn classify_failure(message: &str) -> Failure {
    let message = message.to_lowercase();
    let mentions = |needles: &[&str]| needles.iter().any(|needle| message.contains(needle));
    if mentions(&[
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied (publickey",
        "returned error: 401",
        "returned error: 403",
        "terminal prompts disabled",
        // What a declined background credential prompt ends with.
        "operation cancelled by user",
    ]) {
        Failure::Auth
    } else if mentions(&[
        "could not resolve host",
        "network is unreachable",
        "connection timed out",
        "operation timed out",
        "connection refused",
        "failed to connect",
        "no route to host",
    ]) {
        Failure::Offline
    } else {
        Failure::Other
    }
}

/// A sync that runs while nobody is looking must never open a password
/// prompt. Credentials come from git's own helpers first; when git still asks
/// and the remote is on GitHub, the token from Suzuri's own sign-in answers.
/// Any other prompt is declined by dropping the response sender, which makes
/// askpass end the git process at once, so the repository's job queue is not
/// held up waiting for an answer.
fn background_askpass(github_token: Option<String>, cx: &mut AsyncApp) -> AskPassDelegate {
    AskPassDelegate::new(cx, move |prompt, response, _cx| {
        let answer = github_token
            .as_deref()
            .and_then(|token| github_credential(&prompt, token))
            .and_then(|answer| EncryptedPassword::try_from(answer).ok());
        match answer {
            Some(answer) => {
                log::info!("auto sync answered a GitHub credential prompt from its own sign-in");
                response.send(answer).ok();
            }
            None => log::warn!("auto sync declined a git credential prompt: {prompt}"),
        }
    })
}

/// What to answer when git asks for GitHub credentials over HTTPS: any
/// username works with a token as the password.
fn github_credential<'a>(prompt: &str, token: &'a str) -> Option<&'a str> {
    // Git quotes the URL it is authenticating to. The host has to match
    // exactly, or a remote named like `github.com.example.org` would be
    // handed the token.
    let url = prompt.split('\'').nth(1)?;
    let authority = url.strip_prefix("https://")?;
    let host = authority.rsplit('@').next()?;
    if host != "github.com" {
        return None;
    }
    if prompt.starts_with("Username for ") {
        Some("x-access-token")
    } else if prompt.starts_with("Password for ") {
        Some(token)
    } else {
        None
    }
}

fn job_result<T>(result: Result<Result<T>, oneshot::Canceled>) -> Result<T> {
    result.map_err(|_| anyhow!("the git operation was cancelled"))?
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What Turn On acts on. It is always the folder the user opened, never a
/// repository further up that happens to contain it, and never a repository
/// nested inside it that the active file happens to belong to.
enum Target {
    Repository(Entity<Repository>),
    Folder {
        root: Arc<Path>,
        /// A repository further up that the folder is part of.
        enclosing: Option<Arc<Path>>,
    },
    NoFolder,
}

fn sync_target(workspace: &Workspace, cx: &App) -> Target {
    let project = workspace.project().read(cx);
    let roots = project
        .visible_worktrees(cx)
        .filter(|worktree| !worktree.read(cx).is_single_file())
        .map(|worktree| worktree.read(cx).abs_path())
        .collect::<Vec<_>>();
    let repositories = project.git_store().read(cx).repositories();
    let is_root = |repository: &Entity<Repository>| {
        let work_dir = repository.read(cx).work_directory_abs_path.clone();
        roots.iter().any(|root| root.as_ref() == work_dir.as_ref())
    };
    // With several folders open, the active repository picks among them.
    if let Some(active) = project
        .active_repository(cx)
        .filter(|active| is_root(active))
    {
        return Target::Repository(active);
    }
    if let Some(repository) = repositories.values().find(|repository| is_root(repository)) {
        return Target::Repository(repository.clone());
    }
    let Some(root) = roots.into_iter().next() else {
        return Target::NoFolder;
    };
    let enclosing = repositories
        .values()
        .map(|repository| repository.read(cx).work_directory_abs_path.clone())
        .find(|work_dir| work_dir.as_ref() != root.as_ref() && root.starts_with(work_dir));
    Target::Folder { root, enclosing }
}

fn target_repository(workspace: &Workspace, cx: &App) -> Option<Entity<Repository>> {
    match sync_target(workspace, cx) {
        Target::Repository(repository) => Some(repository),
        Target::Folder { .. } | Target::NoFolder => None,
    }
}

fn turn_on(
    auto_sync: &Entity<AutoSync>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let auto_sync = auto_sync.clone();
    // Sync refuses to run git in a folder the user has not trusted, so
    // setting it up there would end with a repository that never syncs.
    if TrustedWorktrees::has_restricted_worktrees(&project.read(cx).worktree_store(), cx) {
        let prompt = window.prompt(
            PromptLevel::Info,
            "Trust this folder first",
            Some(
                "This folder is open in Restricted Mode. Trust it (the Restricted Mode button in the title bar), then turn on auto sync.",
            ),
            &["OK"],
            cx,
        );
        cx.background_spawn(prompt).detach();
        return;
    }
    let repository = match sync_target(workspace, cx) {
        Target::Repository(repository) => repository,
        Target::NoFolder => {
            let prompt = window.prompt(
                PromptLevel::Info,
                "Open a folder first",
                Some(
                    "Auto sync backs up a whole folder of notes. Open the folder, then turn it on.",
                ),
                &["OK"],
                cx,
            );
            cx.background_spawn(prompt).detach();
            return;
        }
        // A notes folder inside a home directory under git, a dotfiles
        // repository or a monorepo belongs to that larger repository, and
        // syncing it would upload everything else in there too. Making it a
        // repository of its own would instead drop it from the larger one's
        // history.
        Target::Folder {
            root,
            enclosing: Some(enclosing),
        } => {
            let detail = format!(
                "“{}” is part of the git repository at {}. Syncing it would upload that whole repository, not just this folder. Move your notes into a folder of their own, or open the repository's own folder if you mean to sync all of it.",
                folder_name(&root),
                enclosing.display()
            );
            let prompt = window.prompt(
                PromptLevel::Warning,
                "This folder is inside a larger git repository",
                Some(&detail),
                &["OK"],
                cx,
            );
            cx.background_spawn(prompt).detach();
            return;
        }
        Target::Folder {
            root,
            enclosing: None,
        } => {
            cx.spawn_in(window, async move |workspace, cx| {
                let result =
                    connect::connect(root, None, project, auto_sync, workspace.clone(), cx).await;
                report_connect_failure(result, cx).await
            })
            .detach_and_log_err(cx);
            return;
        }
    };
    cx.spawn_in(window, async move |workspace, cx| {
        let (id, work_dir, branch) = repository.read_with(cx, |repository, _| {
            (
                repository.id,
                repository.work_directory_abs_path.clone(),
                repository.branch.clone(),
            )
        });

        let remote = match branch.as_ref().and_then(upstream_remote) {
            Some(remote) => Some(SharedString::from(remote.to_string())),
            None => {
                let branch_name = branch.as_ref().map(|branch| branch.name().to_string());
                let remotes = repository.update(cx, |repository, _| {
                    repository.get_remotes(branch_name, true)
                });
                let remotes = job_result(remotes.await)?;
                match remotes.as_slice() {
                    [remote] => Some(remote.name.clone()),
                    [] => None,
                    _ => {
                        return show_info(
                            cx,
                            "This repository has several remotes",
                            "Push once from the Git panel and pick the one to sync with. Auto sync then follows that branch.",
                        )
                        .await;
                    }
                }
            }
        };
        // No remote yet: the same first-time path as a plain folder, minus
        // creating the repository.
        let Some(remote) = remote else {
            let result = connect::connect(
                work_dir,
                Some(repository),
                project,
                auto_sync,
                workspace.clone(),
                cx,
            )
            .await;
            return report_connect_failure(result, cx).await;
        };
        if branch.is_none() {
            return show_info(
                cx,
                "No branch is checked out",
                "Check out a branch, then turn on auto sync.",
            )
            .await;
        }
        let urls = repository.update(cx, |repository, _| repository.remote_urls());
        let url = job_result(urls.await)?
            .remove(remote.as_ref())
            .unwrap_or_else(|| remote.to_string());

        let folder = folder_name(&work_dir);
        let message = format!("Sync “{folder}” with {url}?");
        let answer = cx.update(|window, cx| {
            window.prompt(
                PromptLevel::Info,
                &message,
                Some(
                    "Suzuri will commit every change in this folder, including changes not committed yet, and push it a few seconds after you save. It also pulls changes from the remote every 5 minutes. Files listed in .gitignore are never uploaded. This applies to this folder on this computer only.",
                ),
                &["Turn On Auto Sync", "Cancel"],
                cx,
            )
        })?;
        if answer.await != Ok(0) {
            return Ok(());
        }

        enable(id, work_dir, &project, &auto_sync, cx).await?;
        workspace.update(cx, |workspace, cx| {
            workspace.show_toast(
                Toast::new(
                    notification_id(id),
                    format!("Auto sync is on for “{folder}”."),
                )
                .autohide(),
                cx,
            )
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

/// Records the opt-in in the repository's git config and starts syncing.
async fn enable(
    id: RepositoryId,
    work_dir: Arc<Path>,
    project: &Entity<Project>,
    auto_sync: &Entity<AutoSync>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let git_config = |args: &[&str], cx: &mut AsyncApp| {
        let args = args.iter().map(|arg| arg.to_string()).collect();
        project.read_with(cx, |project, cx| {
            project.git_config(work_dir.clone(), args, cx)
        })
    };
    git_config(&["--local", ENABLED_KEY, "true"], cx)
        .await
        .context("turning on auto sync")?;
    // A plain `git pull` refuses to reconcile divergent branches unless told
    // how, and a rebase that stops on a conflict has no UI to finish it, so
    // syncing merges. A choice already made for this repository is left
    // alone.
    if git_config(&["--local", "--get", "pull.rebase"], cx)
        .await
        .is_err()
    {
        git_config(&["--local", "pull.rebase", "false"], cx).await?;
        git_config(&["--local", MANAGED_PULL_KEY, "true"], cx).await?;
    }
    auto_sync.update(cx, |auto_sync, cx| {
        auto_sync.set_enabled(id, Some(Trigger::Manual), cx)
    });
    Ok(())
}

/// A first-time setup that fails halfway must say so; a log line alone would
/// leave the user looking at a menu that did nothing.
async fn report_connect_failure(
    result: Result<()>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<()> {
    let Err(error) = result else {
        return Ok(());
    };
    let detail = format!("{error:#}");
    log::warn!("connecting to GitHub failed: {detail}");
    show_info(cx, "Could not finish connecting to GitHub", &detail).await
}

async fn show_info(cx: &mut gpui::AsyncWindowContext, message: &str, detail: &str) -> Result<()> {
    let answer = cx.update(|window, cx| {
        window.prompt(PromptLevel::Info, message, Some(detail), &["OK"], cx)
    })?;
    answer.await.ok();
    Ok(())
}

fn turn_off(
    auto_sync: &Entity<AutoSync>,
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repository) = target_repository(workspace, cx) else {
        return;
    };
    let (id, work_dir) = {
        let repository = repository.read(cx);
        (repository.id, repository.work_directory_abs_path.clone())
    };
    auto_sync.update(cx, |auto_sync, cx| auto_sync.set_enabled(id, None, cx));
    // Through the workspace being updated here: `AutoSync` reaching for it
    // would update it twice and panic.
    workspace.dismiss_toast(&notification_id(id), cx);
    let project = workspace.project().clone();
    cx.spawn(async move |workspace, cx| {
        let git_config = |args: &[&str], cx: &mut AsyncApp| {
            let args = args.iter().map(|arg| arg.to_string()).collect();
            project.read_with(cx, |project, cx| {
                project.git_config(work_dir.clone(), args, cx)
            })
        };
        git_config(&["--local", "--unset", ENABLED_KEY], cx).await?;
        if git_config(&["--local", "--get", MANAGED_PULL_KEY], cx)
            .await
            .is_ok()
        {
            git_config(&["--local", "--unset", "pull.rebase"], cx)
                .await
                .log_err();
            git_config(&["--local", "--unset", MANAGED_PULL_KEY], cx).await?;
        }
        workspace.update(cx, |workspace, cx| {
            workspace.show_toast(
                Toast::new(notification_id(id), "Auto sync is off.").autohide(),
                cx,
            )
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn sign_in_again(
    auto_sync: &Entity<AutoSync>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repository) = target_repository(workspace, cx) else {
        return;
    };
    let id = repository.read(cx).id;
    let auto_sync = auto_sync.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let http = cx.update(|_, cx| cx.http_client())?;
        let result = async {
            let Some((token, _)) = connect::sign_in(&http, &workspace, cx).await? else {
                return anyhow::Ok(());
            };
            auto_sync.update(cx, |auto_sync, cx| {
                auto_sync.github_token = Some(Some(token));
                auto_sync.request(id, Trigger::Manual, Duration::ZERO, cx)
            });
            Ok(())
        }
        .await;
        report_connect_failure(result, cx).await
    })
    .detach_and_log_err(cx);
}

fn sync_now(auto_sync: &Entity<AutoSync>, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(repository) = target_repository(workspace, cx) else {
        return;
    };
    let id = repository.read(cx).id;
    auto_sync.update(cx, |auto_sync, cx| {
        auto_sync.request(id, Trigger::Manual, Duration::ZERO, cx)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::repository::{Upstream, UpstreamTrackingStatus};

    fn branch(upstream: Option<(&str, UpstreamTracking)>) -> Branch {
        Branch {
            is_head: true,
            ref_name: "refs/heads/main".into(),
            upstream: upstream.map(|(ref_name, tracking)| Upstream {
                ref_name: ref_name.to_string().into(),
                tracking,
            }),
            most_recent_commit: None,
        }
    }

    fn tracked(ahead: u32, behind: u32) -> UpstreamTracking {
        UpstreamTracking::Tracked(UpstreamTrackingStatus { ahead, behind })
    }

    #[test]
    fn tracked_branch_commits_pulls_and_pushes_to_its_upstream() {
        let branch = branch(Some(("refs/remotes/origin/main", tracked(0, 0))));
        let plan = plan_cycle(Some(&branch), false, Vec::new(), true, &[], Trigger::Change);
        assert_eq!(
            plan,
            Ok(Some(Plan {
                commit: true,
                remote: "origin".into(),
                remote_branch: "main".into(),
                pull: true,
                set_upstream: false,
                ahead: 0,
            }))
        );
    }

    #[test]
    fn own_commit_showing_up_in_status_does_nothing() {
        let branch = branch(Some(("refs/remotes/origin/main", tracked(0, 0))));
        let plan = plan_cycle(
            Some(&branch),
            false,
            Vec::new(),
            false,
            &[],
            Trigger::Change,
        );
        assert_eq!(plan, Ok(None));
    }

    #[test]
    fn clean_tree_still_pulls_on_the_timer_and_pushes_when_ahead() {
        let branch = branch(Some(("refs/remotes/origin/main", tracked(0, 2))));
        let plan = plan_cycle(Some(&branch), false, Vec::new(), false, &[], Trigger::Timer)
            .unwrap()
            .unwrap();
        assert!(plan.pull && !plan.commit);

        let branch = self::branch(Some(("refs/remotes/origin/main", tracked(1, 0))));
        let plan = plan_cycle(
            Some(&branch),
            false,
            Vec::new(),
            false,
            &[],
            Trigger::Change,
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.ahead, 1);
    }

    #[test]
    fn branch_without_upstream_publishes_to_the_only_remote() {
        let branch = branch(None);
        let plan = plan_cycle(
            Some(&branch),
            false,
            Vec::new(),
            false,
            &["origin".into()],
            Trigger::Change,
        )
        .unwrap()
        .unwrap();
        assert!(plan.set_upstream && !plan.pull);
        assert_eq!(plan.remote, "origin");
        assert_eq!(plan.remote_branch, "main");
    }

    #[test]
    fn gone_upstream_is_published_again_without_pulling() {
        let branch = branch(Some(("refs/remotes/origin/main", UpstreamTracking::Gone)));
        let plan = plan_cycle(
            Some(&branch),
            false,
            Vec::new(),
            false,
            &[],
            Trigger::Change,
        )
        .unwrap()
        .unwrap();
        assert!(plan.set_upstream && !plan.pull);
    }

    #[test]
    fn blockers_stop_the_cycle() {
        let branch = branch(None);
        assert_eq!(
            plan_cycle(Some(&branch), false, Vec::new(), true, &[], Trigger::Change),
            Err(Blocker::NoRemote)
        );
        assert_eq!(
            plan_cycle(
                Some(&branch),
                false,
                Vec::new(),
                true,
                &["origin".into(), "fork".into()],
                Trigger::Change
            ),
            Err(Blocker::SeveralRemotes)
        );
        assert_eq!(
            plan_cycle(None, false, Vec::new(), true, &[], Trigger::Change),
            Err(Blocker::NoBranch)
        );
        assert_eq!(
            plan_cycle(Some(&branch), true, Vec::new(), true, &[], Trigger::Change),
            Err(Blocker::Rebasing)
        );
        let conflicts = vec![PathBuf::from("/notes/a.md")];
        assert_eq!(
            plan_cycle(
                Some(&branch),
                false,
                conflicts.clone(),
                true,
                &["origin".into()],
                Trigger::Manual
            ),
            Err(Blocker::Conflicts(conflicts))
        );
    }

    #[test]
    fn conflict_markers_need_both_ends() {
        assert!(has_conflict_markers(
            "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> origin/main\n"
        ));
        assert!(!has_conflict_markers("a\n<<<<<<< not closed\n"));
        assert!(!has_conflict_markers(">>>>>>> before\n<<<<<<< after\n"));
        assert!(!has_conflict_markers("plain text"));
    }

    #[test]
    fn commit_message_names_a_single_file() {
        assert_eq!(
            commit_message(&[PathBuf::from("notes/today.md")]),
            "Update notes/today.md"
        );
        assert_eq!(
            commit_message(&[PathBuf::from("a.md"), PathBuf::from("b.md")]),
            "Update 2 files"
        );
    }

    /// Runs the real controller against real git: a bare remote stands in
    /// for GitHub, and a second clone stands in for another computer.
    #[gpui::test]
    async fn syncs_merges_and_recovers_from_a_conflict(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, UpdateGlobal as _, WeakEntity};
        use settings::SettingsStore;
        use std::path::Path;

        cx.executor().allow_parking();
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            project::trusted_worktrees::init(Default::default(), cx);
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"session":{"trust_all_worktrees":true}}"#, cx)
                    .expect("valid settings");
            });
        });

        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().canonicalize().expect("canonical temp dir");
        // The test plays the other computer with blocking git calls between
        // steps, so it waits on them like a user at a terminal would.
        #[allow(clippy::disallowed_methods)]
        let git = |dir: &Path, args: &[&str]| -> String {
            let output = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let remote_file = |path: &str| {
            git(
                &root,
                &["--git-dir", "remote.git", "show", &format!("main:{path}")],
            )
        };
        let remote_commits = || {
            git(
                &root,
                &["--git-dir", "remote.git", "rev-list", "--count", "main"],
            )
            .trim()
            .parse::<u32>()
            .expect("commit count")
        };

        git(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        git(&root, &["init", "-q", "-b", "main", "here"]);
        let here = root.join("here");
        for (key, value) in [
            ("user.name", "Here"),
            ("user.email", "here@example.com"),
            ("pull.rebase", "false"),
            (ENABLED_KEY, "true"),
        ] {
            git(&here, &["config", key, value]);
        }
        git(&here, &["remote", "add", "origin", "../remote.git"]);
        std::fs::write(here.join("note.md"), "one\ntwo\nthree\n").expect("write");
        git(&here, &["add", "-A"]);
        git(&here, &["commit", "-qm", "Initial"]);
        git(&here, &["push", "-q", "-u", "origin", "main"]);
        git(&root, &["clone", "-q", "remote.git", "there"]);
        let there = root.join("there");

        let project =
            project::Project::test(fs::RealFs::new(None, cx.executor()), [here.as_path()], cx)
                .await;
        let auto_sync = cx.new(|cx| AutoSync::new(project, WeakEntity::new_invalid(), cx));

        let wait = |cx: &mut gpui::TestAppContext,
                    what: &str,
                    done: &dyn Fn(&mut gpui::TestAppContext) -> bool| {
            for _ in 0..240 {
                cx.executor().advance_clock(Duration::from_secs(1));
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                // Git and the file watcher run in real time.
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("timed out waiting until {what}");
        };
        let paused = |cx: &mut gpui::TestAppContext| {
            auto_sync.read_with(cx, |auto_sync, _| {
                auto_sync
                    .repositories
                    .values()
                    .find(|repository| repository.enabled)
                    .and_then(|repository| repository.paused)
            })
        };

        wait(cx, "the opt-in is read from git config", &|cx| {
            auto_sync.read_with(cx, |auto_sync, _| {
                auto_sync
                    .repositories
                    .values()
                    .any(|repository| repository.enabled)
            })
        });

        std::fs::write(here.join("note.md"), "one\ntwo\nthree\nfour\n").expect("write");
        wait(cx, "the first edit reaches the remote", &|_| {
            remote_commits() == 2
        });
        assert_eq!(remote_file("note.md"), "one\ntwo\nthree\nfour\n");

        // Staging the same path again must not be skipped as already staged.
        std::fs::write(here.join("note.md"), "one\ntwo\nthree\nfour\nfive\n").expect("write");
        wait(
            cx,
            "a second edit to the same file reaches the remote",
            &|_| remote_commits() == 3,
        );

        std::fs::write(there.join("other.md"), "from there\n").expect("write");
        git(&there, &["pull", "-q"]);
        git(&there, &["add", "-A"]);
        git(&there, &["commit", "-qm", "Add other.md"]);
        git(&there, &["push", "-q"]);
        std::fs::write(here.join("mine.md"), "from here\n").expect("write");
        wait(cx, "diverged histories are merged and pushed", &|_| {
            here.join("other.md").exists() && remote_commits() == 6
        });

        git(&there, &["pull", "-q"]);
        std::fs::write(there.join("note.md"), "one\nTHERE\nthree\nfour\nfive\n").expect("write");
        git(&there, &["commit", "-qam", "Edit there"]);
        git(&there, &["push", "-q"]);
        std::fs::write(here.join("note.md"), "one\nHERE\nthree\nfour\nfive\n").expect("write");
        wait(cx, "the conflict pauses syncing", &|cx| {
            paused(cx) == Some(Pause::Conflict)
        });
        assert!(
            !remote_file("note.md").contains("<<<<<<<"),
            "conflict markers must never be pushed"
        );

        let resolved = "one\nHERE\nTHERE\nthree\nfour\nfive\n";
        std::fs::write(here.join("note.md"), resolved).expect("write");
        wait(cx, "resolving the conflict resumes syncing", &|cx| {
            paused(cx).is_none() && remote_file("note.md") == resolved
        });
    }

    /// The local half of connecting a folder for the first time, against
    /// real git: a plain folder with a repository nested in it becomes a
    /// repository, gets a remote, and its notes arrive there without the
    /// nested repository.
    #[gpui::test]
    async fn connects_a_plain_folder_and_leaves_nested_repositories_out(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{AppContext as _, UpdateGlobal as _, WeakEntity};
        use settings::SettingsStore;
        use std::{cell::RefCell, path::Path, rc::Rc};

        cx.executor().allow_parking();
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            project::trusted_worktrees::init(Default::default(), cx);
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"session":{"trust_all_worktrees":true}}"#, cx)
                    .expect("valid settings");
            });
        });

        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().canonicalize().expect("canonical temp dir");
        // The test sets the scene with blocking git calls between steps.
        #[allow(clippy::disallowed_methods)]
        let git = |dir: &Path, args: &[&str]| -> String {
            let output = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };

        git(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        let notes = root.join("My Notes");
        std::fs::create_dir_all(notes.join("drafts")).expect("create");
        std::fs::write(notes.join("Ideas.md"), "an idea\n").expect("write");
        std::fs::write(notes.join("drafts").join("Chapter.md"), "a draft\n").expect("write");
        let nested = notes.join("paper-code");
        std::fs::create_dir_all(&nested).expect("create");
        git(&nested, &["init", "-q", "-b", "main"]);
        std::fs::write(nested.join("train.py"), "print('hi')\n").expect("write");
        git(&nested, &["add", "-A"]);
        git(&nested, &["commit", "-qm", "Code"]);

        let project =
            project::Project::test(fs::RealFs::new(None, cx.executor()), [notes.as_path()], cx)
                .await;
        let auto_sync = cx.new(|cx| AutoSync::new(project.clone(), WeakEntity::new_invalid(), cx));

        let wait = |cx: &mut gpui::TestAppContext,
                    what: &str,
                    done: &dyn Fn(&mut gpui::TestAppContext) -> bool| {
            for _ in 0..240 {
                cx.executor().advance_clock(Duration::from_secs(1));
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("timed out waiting until {what}");
        };

        wait(cx, "the nested repository is discovered", &|cx| {
            project.read_with(cx, |project, cx| {
                connect::nested_repositories(&notes, project, cx) == ["paper-code"]
            })
        });

        let outcome = Rc::new(RefCell::new(None));
        cx.update(|cx| {
            let outcome = outcome.clone();
            let project = project.clone();
            let auto_sync = auto_sync.clone();
            let work_dir: Arc<Path> = notes.clone().into();
            let remote_url = root.join("remote.git").to_string_lossy().into_owned();
            cx.spawn(async move |cx| {
                let result = async {
                    let identity = connect::Identity {
                        name: "Ada".into(),
                        email: "7+ada@users.noreply.github.com".into(),
                    };
                    let repository =
                        connect::prepare_local(work_dir.clone(), None, &identity, &project, cx)
                            .await?;
                    connect::attach_remote(&repository, &remote_url, cx).await?;
                    let id = repository.read_with(cx, |repository, _| repository.id);
                    enable(id, work_dir, &project, &auto_sync, cx).await
                }
                .await;
                *outcome.borrow_mut() = Some(result);
            })
            .detach();
        });
        wait(cx, "the folder is connected", &|_| {
            outcome.borrow().is_some()
        });
        if let Some(Err(error)) = outcome.borrow().as_ref() {
            panic!("connecting failed: {error:#}");
        }

        // Fails while the remote has no branch yet, which the wait rides out.
        #[allow(clippy::disallowed_methods)]
        let remote_files = || {
            std::process::Command::new("git")
                .current_dir(&root)
                .args([
                    "--git-dir",
                    "remote.git",
                    "ls-tree",
                    "-r",
                    "--name-only",
                    "main",
                ])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default()
        };
        wait(cx, "the notes reach the remote", &|_| {
            let files = remote_files();
            files.contains("Ideas.md") && files.contains("drafts/Chapter.md")
        });
        let files = remote_files();
        assert!(
            !files.contains("paper-code"),
            "the nested repository must stay out of the backup, got:\n{files}"
        );
        let exclude = std::fs::read_to_string(notes.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.contains("/paper-code/"), "{exclude}");
        assert_eq!(
            git(&notes, &["config", "--local", ENABLED_KEY]).trim(),
            "true"
        );
    }

    /// Pins the shipped defaults, that zero is refused, and that `default.json`
    /// really has the section: `from_settings` falls back to the same
    /// constants when it is missing, so only the raw text can tell.
    #[gpui::test]
    fn settings_default_to_five_and_refuse_zero(cx: &mut gpui::App) {
        let defaults = settings::default_settings();
        assert!(
            defaults.contains("\n  \"auto_sync\": {"),
            "default.json has no top-level auto_sync section"
        );
        let store = settings::SettingsStore::new(cx, &defaults);
        cx.set_global(store);
        let settings = AutoSyncSettings::get_global(cx);
        assert_eq!(settings.sync_delay, Duration::from_secs(5));
        assert_eq!(settings.pull_interval, Duration::from_secs(5 * 60));

        assert!(
            serde_json::from_str::<settings::AutoSyncSettingsContent>(
                r#"{"sync_delay_seconds": 0}"#
            )
            .is_err(),
            "a zero delay must not parse"
        );
        let content = settings::SettingsContent {
            auto_sync: serde_json::from_str(
                r#"{"sync_delay_seconds": 1, "pull_interval_minutes": 30}"#,
            )
            .expect("valid settings"),
            ..Default::default()
        };
        let custom = AutoSyncSettings::from_settings(&content);
        assert_eq!(custom.sync_delay, Duration::from_secs(1));
        assert_eq!(custom.pull_interval, Duration::from_secs(30 * 60));
    }

    #[test]
    fn only_github_https_prompts_get_the_token() {
        assert_eq!(
            github_credential("Username for 'https://github.com': ", "gho_x"),
            Some("x-access-token")
        );
        assert_eq!(
            github_credential(
                "Password for 'https://x-access-token@github.com': ",
                "gho_x"
            ),
            Some("gho_x")
        );
        assert_eq!(
            github_credential("Password for 'https://ada@gitlab.com': ", "gho_x"),
            None
        );
        assert_eq!(
            github_credential("Username for 'https://github.com.evil.test': ", "gho_x"),
            None
        );
        assert_eq!(
            github_credential(
                "Enter passphrase for key '/Users/ada/.ssh/id_ed25519': ",
                "gho_x"
            ),
            None
        );
    }

    #[test]
    fn error_summary_skips_progress_lines() {
        assert_eq!(
            error_summary(
                "From ../remote\n * branch main -> FETCH_HEAD\nerror: Your local changes to the following files would be overwritten by merge:\n\tnote.md\n"
            ),
            "error: Your local changes to the following files would be overwritten by merge:"
        );
        assert_eq!(
            error_summary(
                "To github.com:x/y.git\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs\n"
            ),
            "! [rejected]        main -> main (fetch first)"
        );
        assert_eq!(error_summary("something odd\n\n"), "something odd");
        assert_eq!(error_summary(""), "git failed");
    }

    #[test]
    fn failures_are_classified_from_git_output() {
        assert_eq!(
            classify_failure("fatal: Authentication failed for 'https://github.com/x/y.git/'"),
            Failure::Auth
        );
        assert_eq!(
            classify_failure("git@github.com: Permission denied (publickey)."),
            Failure::Auth
        );
        assert_eq!(
            classify_failure(git::repository::REMOTE_CANCELLED_BY_USER),
            Failure::Auth
        );
        assert_eq!(
            classify_failure(
                "fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com"
            ),
            Failure::Offline
        );
        assert_eq!(
            classify_failure("! [rejected] main -> main (fetch first)"),
            Failure::Other
        );
    }
}
