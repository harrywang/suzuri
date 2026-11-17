//! Auto sync: commit and push a project's git repository a few seconds after
//! every change, and pull from its remote every few minutes, so a folder of
//! notes stays backed up on GitHub without anyone running git by hand.
//!
//! It is opted into per repository and per machine, with a key in that
//! repository's own `.git/config`, so a project in one repository can sync
//! while another stays manual. Every git operation goes through Zed's
//! `Repository`, which serializes them with the Git panel's own.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow};
use askpass::AskPassDelegate;
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
};
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
    ]
);

/// How long the working tree must stay unchanged before a sync, so a burst of
/// autosaves becomes one commit.
const CHANGE_DELAY: Duration = Duration::from_secs(5);
const PULL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RETRY_DELAY: Duration = Duration::from_secs(30);
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
        // Sync Now depending on the active repository.
        workspace.register_action_renderer(move |div, workspace, _window, cx| {
            let enabled = workspace
                .project()
                .read(cx)
                .active_repository(cx)
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
                this.request(*id, Trigger::Change, CHANGE_DELAY, cx)
            }
            _ => {}
        });
        let mut this = Self {
            project,
            workspace,
            repositories: HashMap::default(),
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
                    this.update(cx, |this, cx| this.set_enabled(id, true, cx))
                        .ok();
                }
            })
            .detach();
        }
    }

    fn set_enabled(&mut self, id: RepositoryId, enabled: bool, cx: &mut Context<Self>) {
        let Some(state) = self.repositories.get_mut(&id) else {
            return;
        };
        *state = RepositorySync::default();
        if !enabled {
            self.dismiss(id, cx);
            return;
        }
        state.enabled = true;
        state.pull_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PULL_INTERVAL).await;
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
        self.request(id, Trigger::Manual, Duration::ZERO, cx);
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
        state.running = Some(cx.spawn(async move |this, cx| {
            let result = run_cycle(repository, fs, trigger, cx).await;
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
            Ok(outcome) => {
                let was_paused = state.paused.take().is_some();
                state.failures = 0;
                if was_paused {
                    self.dismiss(id, cx);
                }
                if trigger == Trigger::Manual {
                    let message = match outcome {
                        Outcome::Synced => "Synced with the remote repository.",
                        Outcome::Idle => "Already in sync with the remote repository.",
                    };
                    self.notify(id, message.into(), None, true, cx);
                }
            }
            Err(Problem::Blocked(Blocker::NotTrusted)) => {
                // Restricted mode has its own banner, and trusting the folder
                // lets the next change sync.
            }
            Err(Problem::Blocked(Blocker::Conflicts(paths))) => {
                if state.paused != Some(Pause::Conflict) {
                    state.paused = Some(Pause::Conflict);
                    self.notify_conflicts(id, paths, cx);
                }
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
                            "Auto sync stopped: the remote did not accept your git credentials. Push once from the Git panel to sign in, then run Sync Now.".into(),
                            Some(sync_now_button()),
                            false,
                            cx,
                        );
                    }
                    Failure::Other => {
                        state.failures += 1;
                        if state.failures >= MAX_FAILURES {
                            state.paused = Some(Pause::Stopped);
                            let detail = message.lines().next().unwrap_or_default();
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
            let delay = if state.failures > 0 {
                RETRY_DELAY
            } else {
                CHANGE_DELAY
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
    cx: &mut AsyncApp,
) -> Result<Outcome, Problem> {
    let (trusted, branch, changes, conflicted, repository_dir, work_dir) =
        repository.read_with(cx, |repository, _| {
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
            )
        });
    if !trusted {
        return Err(Problem::Blocked(Blocker::NotTrusted));
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
        let askpass = background_askpass(cx);
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
        let askpass = background_askpass(cx);
        let remote = plan.remote.clone();
        let pull = repository.update(cx, |repository, cx| {
            repository.pull(None, remote, false, askpass, cx)
        });
        let output = job_result(pull.await).map_err(Problem::Failed)?;
        pulled = !output.stdout.contains("Already up to date");
    }

    if committed || pulled || plan.set_upstream || plan.ahead > 0 {
        let askpass = background_askpass(cx);
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

/// Credentials come from git's own helpers; a sync that runs while nobody is
/// looking must never open a password prompt. Dropping the response sender
/// makes askpass end the git process at once, so the repository's job queue
/// is not held up waiting for an answer.
fn background_askpass(cx: &mut AsyncApp) -> AskPassDelegate {
    AskPassDelegate::new(cx, |prompt, _response, _cx| {
        log::warn!("auto sync declined a git credential prompt: {prompt}");
    })
}

fn job_result<T>(result: Result<Result<T>, oneshot::Canceled>) -> Result<T> {
    result.map_err(|_| anyhow!("the git operation was cancelled"))?
}

fn active_repository(workspace: &Workspace, cx: &App) -> Option<Entity<Repository>> {
    workspace.project().read(cx).active_repository(cx)
}

fn turn_on(
    auto_sync: &Entity<AutoSync>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(repository) = active_repository(workspace, cx) else {
        let prompt = window.prompt(
            PromptLevel::Info,
            "This folder is not a git repository yet",
            Some(
                "Open the Git panel and choose Initialize Repository, then connect it to an empty repository on GitHub. Turn on auto sync after that.",
            ),
            &["OK"],
            cx,
        );
        cx.background_spawn(prompt).detach();
        return;
    };
    let project = workspace.project().clone();
    let auto_sync = auto_sync.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let (id, work_dir, branch) = repository.read_with(cx, |repository, _| {
            (
                repository.id,
                repository.work_directory_abs_path.clone(),
                repository.branch.clone(),
            )
        });
        let Some(branch) = branch else {
            return show_info(
                cx,
                "No branch is checked out",
                "Check out a branch, then turn on auto sync.",
            )
            .await;
        };

        let remote = match upstream_remote(&branch) {
            Some(remote) => Some(SharedString::from(remote.to_string())),
            None => {
                let branch_name = branch.name().to_string();
                let remotes = repository.update(cx, |repository, _| {
                    repository.get_remotes(Some(branch_name), true)
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
        let Some(remote) = remote else {
            return show_info(
                cx,
                "This repository is not connected to GitHub yet",
                "Create an empty private repository on github.com, then add it as a remote: open the branch picker and paste its URL, or run `git remote add origin <url>` in the terminal. Turn on auto sync after that.",
            )
            .await;
        };
        let urls = repository.update(cx, |repository, _| repository.remote_urls());
        let url = job_result(urls.await)?
            .remove(remote.as_ref())
            .unwrap_or_else(|| remote.to_string());

        let folder = work_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| work_dir.display().to_string());
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

        let git_config = |args: &[&str], cx: &mut gpui::AsyncWindowContext| {
            let args = args.iter().map(|arg| arg.to_string()).collect();
            project.read_with(cx, |project, cx| {
                project.git_config(work_dir.clone(), args, cx)
            })
        };
        git_config(&["--local", ENABLED_KEY, "true"], cx)
            .await
            .context("turning on auto sync")?;
        // A plain `git pull` refuses to reconcile divergent branches unless
        // told how, and a rebase that stops on a conflict has no UI to finish
        // it, so syncing merges. A choice already made for this repository is
        // left alone.
        if git_config(&["--local", "--get", "pull.rebase"], cx)
            .await
            .is_err()
        {
            git_config(&["--local", "pull.rebase", "false"], cx).await?;
            git_config(&["--local", MANAGED_PULL_KEY, "true"], cx).await?;
        }

        auto_sync.update(cx, |auto_sync, cx| auto_sync.set_enabled(id, true, cx));
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
    let Some(repository) = active_repository(workspace, cx) else {
        return;
    };
    let (id, work_dir) = {
        let repository = repository.read(cx);
        (repository.id, repository.work_directory_abs_path.clone())
    };
    auto_sync.update(cx, |auto_sync, cx| auto_sync.set_enabled(id, false, cx));
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

fn sync_now(auto_sync: &Entity<AutoSync>, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(repository) = active_repository(workspace, cx) else {
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
