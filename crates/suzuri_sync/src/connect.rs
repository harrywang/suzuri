//! Connecting a folder to GitHub for the first time, for someone who has
//! never used git: sign in, pick or create the private repository, make the
//! folder a repository, and hand over to auto sync. Nothing here asks the
//! user to open a terminal.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use fs::Fs;
use git::repository::CommitOptions;
use gpui::{App, AsyncApp, AsyncWindowContext, ClipboardItem, Entity, PromptLevel, WeakEntity};
use http_client::HttpClient;
use project::{Project, git_store::Repository};
use workspace::{Toast, Workspace, notifications::NotificationId};

use crate::{
    AutoSync, background_askpass, enable, folder_name,
    github::{self, GITHUB, Poll, Repo, User},
    job_result, show_info,
};

/// Where the signed-in user's token is kept in the system credential store.
const TOKEN_KEY: &str = "https://github.com/login/oauth/suzuri-sync";
const FIRST_COMMIT_MESSAGE: &str = "Start syncing with Suzuri";

/// The author git records when the user has never configured one.
pub(crate) struct Identity {
    pub name: String,
    pub email: String,
}

enum Destination {
    Create(String),
    Existing(Repo),
}

pub(crate) async fn connect(
    root: Arc<Path>,
    repository: Option<Entity<Repository>>,
    project: Entity<Project>,
    auto_sync: Entity<AutoSync>,
    workspace: WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let folder = folder_name(&root);

    if !git_is_installed().await {
        return show_info(
            cx,
            "Git is not installed on this computer",
            "Suzuri uses git to sync. On a Mac, accept the “command line developer tools” install that just appeared (or run `xcode-select --install`). On Windows, install Git from git-scm.com. Then turn on auto sync again.",
        )
        .await;
    }

    let nested = project.read_with(cx, |project, cx| nested_repositories(&root, project, cx));
    let message = format!("Back up “{folder}” to GitHub?");
    let mut detail = "Suzuri will sign you in to GitHub and keep this folder in a private repository there. After that, every change is saved to GitHub a few seconds after you make it, and changes made on your other computers arrive here by themselves.".to_string();
    if !nested.is_empty() {
        detail.push_str(&format!(
            "\n\nNot included, because each has its own git history: {}. Turn on auto sync inside one of them to back it up too.",
            nested.join(", ")
        ));
    }
    let answer = cx.update(|window, cx| {
        window.prompt(
            PromptLevel::Info,
            &message,
            Some(&detail),
            &["Continue", "Cancel"],
            cx,
        )
    })?;
    if answer.await != Ok(0) {
        return Ok(());
    }

    let http = cx.update(|_, cx| cx.http_client())?;
    let (token, user) = match signed_in_user(&http, cx).await {
        Some(signed_in) => signed_in,
        None => match sign_in(&http, &workspace, cx).await? {
            Some(signed_in) => signed_in,
            None => return Ok(()),
        },
    };

    let Some(destination) =
        choose_destination(&http, &token, &user, &folder, &workspace, cx).await?
    else {
        return Ok(());
    };

    // The local steps come before anything is created on GitHub, so a folder
    // that cannot be prepared does not leave an orphan repository behind.
    let identity = Identity {
        name: user.name.clone().unwrap_or_else(|| user.login.clone()),
        email: user.noreply_email(),
    };
    let repository = prepare_local(root.clone(), repository, &identity, &project, cx).await?;

    let repo = match destination {
        Destination::Existing(repo) => repo,
        Destination::Create(name) => {
            match github::create_private_repo(&http, GITHUB, &token, &name).await {
                Ok(repo) => repo,
                Err(error) if error.to_string().contains("already have") => {
                    let detail = format!(
                        "You already have a repository named “{name}” on GitHub. If it is empty, turn on auto sync again and choose it from your existing repositories. Otherwise rename this folder first."
                    );
                    return show_info(cx, "That name is taken", &detail).await;
                }
                Err(error) => return Err(error),
            }
        }
    };

    attach_remote(&repository, &repo.clone_url, cx).await?;
    auto_sync.update(cx, |auto_sync, _| {
        auto_sync.github_token = Some(Some(token));
    });
    let id = repository.read_with(cx, |repository, _| repository.id);
    enable(id, root, &project, &auto_sync, cx).await?;

    // The first sync reports its own result; this only says where it goes.
    workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(
                NotificationId::named("auto-sync-connected".into()),
                format!(
                    "Connected “{folder}” to github.com/{}. The first sync is running.",
                    repo.full_name
                ),
            )
            .autohide(),
            cx,
        )
    })?;
    Ok(())
}

/// Makes the folder a repository with an author identity and a first commit
/// on a branch, which is what syncing needs before a remote can be attached.
pub(crate) async fn prepare_local(
    root: Arc<Path>,
    repository: Option<Entity<Repository>>,
    identity: &Identity,
    project: &Entity<Project>,
    cx: &mut AsyncApp,
) -> Result<Entity<Repository>> {
    let repository = match repository {
        Some(repository) => repository,
        None => {
            project
                .read_with(cx, |project, cx| {
                    project.git_init(root.clone(), "main".to_string(), cx)
                })
                .await
                .context("making this folder a git repository")?;
            wait_for(cx, "the new repository to appear", |cx| {
                project.read_with(cx, |project, cx| {
                    project
                        .git_store()
                        .read(cx)
                        .repositories()
                        .values()
                        .find(|repository| {
                            repository.read(cx).work_directory_abs_path.as_ref() == root.as_ref()
                        })
                        .cloned()
                })
            })
            .await?
        }
    };

    let git_config = |args: &[&str], cx: &mut AsyncApp| {
        let args = args.iter().map(|arg| arg.to_string()).collect();
        project.read_with(cx, |project, cx| project.git_config(root.clone(), args, cx))
    };
    // Someone who has never used git has no author identity, and git refuses
    // to commit without one.
    if git_config(&["user.name"], cx).await.is_err() {
        git_config(&["--local", "user.name", &identity.name], cx).await?;
    }
    if git_config(&["user.email"], cx).await.is_err() {
        git_config(&["--local", "user.email", &identity.email], cx).await?;
    }

    let has_commits = repository.read_with(cx, |repository, _| repository.head_commit.is_some());
    if !has_commits {
        let askpass = background_askpass(None, cx);
        let commit = repository.update(cx, |repository, cx| {
            repository.commit(
                FIRST_COMMIT_MESSAGE.into(),
                None,
                // Empty on purpose. The files are committed by the first
                // sync, which first keeps nested repositories out and works
                // from a status scan known to be complete.
                CommitOptions {
                    allow_empty: true,
                    ..Default::default()
                },
                askpass,
                cx,
            )
        });
        job_result(commit.await).context("making the first commit")?;
        wait_for(cx, "the first commit to show up", |cx| {
            repository.read_with(cx, |repository, _| {
                (repository.branch.is_some() && repository.head_commit.is_some()).then_some(())
            })
        })
        .await?;
    }
    Ok(repository)
}

pub(crate) async fn attach_remote(
    repository: &Entity<Repository>,
    url: &str,
    cx: &mut AsyncApp,
) -> Result<()> {
    let create_remote = repository.update(cx, |repository, _| {
        repository.create_remote("origin".to_string(), url.to_string())
    });
    job_result(create_remote.await).context("connecting the folder to GitHub")
}

/// Repositories inside `root`, as paths relative to it. Git would record
/// each as a bare pointer, so a backup of `root` would hold an empty entry
/// where the user expects their files.
pub(crate) fn nested_repositories(root: &Path, project: &Project, cx: &App) -> Vec<String> {
    let mut nested = project
        .git_store()
        .read(cx)
        .repositories()
        .values()
        .filter_map(|other| {
            let work_dir = other.read(cx).work_directory_abs_path.clone();
            let relative = work_dir.strip_prefix(root).ok()?;
            (!relative.as_os_str().is_empty())
                .then(|| relative.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    nested.sort();
    nested
}

pub(crate) fn exclude_path(repository: &Repository) -> PathBuf {
    repository.common_dir_abs_path.join("info").join("exclude")
}

/// Lists nested repositories in the repository's exclude file, which is
/// local and never pushed. Paths git already tracks are unaffected, so
/// submodules keep working. Returns whether the file changed.
pub(crate) async fn ensure_excluded(
    fs: &Arc<dyn Fs>,
    exclude_path: &Path,
    nested: &[String],
) -> Result<bool> {
    if nested.is_empty() {
        return Ok(false);
    }
    let existing = fs.load(exclude_path).await.unwrap_or_default();
    let mut text = existing.clone();
    for path in nested {
        let line = format!("/{path}/");
        if !text.lines().any(|existing| existing.trim() == line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
        }
    }
    if text == existing {
        return Ok(false);
    }
    if let Some(info_dir) = exclude_path.parent() {
        fs.create_dir(info_dir).await?;
    }
    fs.write(exclude_path, text.as_bytes()).await?;
    Ok(true)
}

/// The stored token and its account, when there is one GitHub still accepts.
async fn signed_in_user(http: &Arc<dyn HttpClient>, cx: &AsyncApp) -> Option<(String, User)> {
    let token = stored_token(cx).await?;
    let user = github::user(http, GITHUB, &token).await.ok()?;
    Some((token, user))
}

pub(crate) async fn stored_token(cx: &AsyncApp) -> Option<String> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let (_, token) = provider
        .read_credentials(TOKEN_KEY, cx)
        .await
        .ok()
        .flatten()?;
    String::from_utf8(token).ok()
}

/// GitHub's device flow, ending with the token saved. `None` means the user
/// cancelled.
pub(crate) async fn sign_in(
    http: &Arc<dyn HttpClient>,
    workspace: &WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<Option<(String, User)>> {
    let code = github::request_device_code(http, GITHUB).await?;
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(code.user_code.clone()));
    })?;
    let detail = format!(
        "Your one-time code is {} and it is already copied. Click Open GitHub, paste the code there, and approve Suzuri. Then come back to this window.",
        code.user_code
    );
    let answer = cx.update(|window, cx| {
        window.prompt(
            PromptLevel::Info,
            "Sign in to GitHub",
            Some(&detail),
            &["Open GitHub", "Cancel"],
            cx,
        )
    })?;
    if answer.await != Ok(0) {
        return Ok(None);
    }
    cx.update(|_, cx| cx.open_url(&code.verification_uri))?;

    let waiting = NotificationId::named("auto-sync-sign-in".into());
    workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(
                waiting.clone(),
                format!(
                    "Waiting for you to approve Suzuri on github.com. Your code is {}.",
                    code.user_code
                ),
            ),
            cx,
        )
    })?;
    let token = poll_until_approved(http, &code, cx).await;
    workspace
        .update(cx, |workspace, cx| workspace.dismiss_toast(&waiting, cx))
        .ok();
    let token = token?;

    let user = github::user(http, GITHUB, &token).await?;
    let provider = cx.update(|_, cx| zed_credentials_provider::global(cx))?;
    provider
        .write_credentials(TOKEN_KEY, &user.login, token.as_bytes(), cx)
        .await
        .context("saving the GitHub sign-in")?;
    Ok(Some((token, user)))
}

async fn poll_until_approved(
    http: &Arc<dyn HttpClient>,
    code: &github::DeviceCode,
    cx: &mut AsyncWindowContext,
) -> Result<String> {
    let mut interval = code.interval.max(1);
    let mut waited = 0;
    while waited < code.expires_in {
        cx.background_executor()
            .timer(Duration::from_secs(interval))
            .await;
        waited += interval;
        match github::poll_for_token(http, GITHUB, &code.device_code).await? {
            Poll::Token(token) => return Ok(token),
            Poll::Pending => {}
            Poll::SlowDown => interval += 5,
        }
    }
    Err(anyhow!("the sign-in code expired before it was approved"))
}

/// `None` means the user cancelled or was told why it cannot continue.
async fn choose_destination(
    http: &Arc<dyn HttpClient>,
    token: &str,
    user: &User,
    folder: &str,
    workspace: &WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<Option<Destination>> {
    let name = github::repo_name_for(folder);
    let message = format!("Where on GitHub should “{folder}” be kept?");
    let detail = format!(
        "You are signed in as {}. A private repository is visible only to you.",
        user.login
    );
    let create = format!("Create Private Repository “{name}”");
    let answer = cx.update(|window, cx| {
        window.prompt(
            PromptLevel::Info,
            &message,
            Some(&detail),
            &[
                create.as_str(),
                "Use an Empty Repository I Already Have…",
                "Cancel",
            ],
            cx,
        )
    })?;
    match answer.await {
        Ok(0) => Ok(Some(Destination::Create(name))),
        Ok(1) => {
            // Syncing into a repository that already has content would mean
            // merging two unrelated histories, which is what cloning is for.
            let empty = github::list_repos(http, GITHUB, token)
                .await?
                .into_iter()
                .filter(|repo| repo.size == 0)
                .collect::<Vec<_>>();
            if empty.is_empty() {
                show_info(
                    cx,
                    "None of your repositories is empty",
                    "Auto sync can connect this folder to a new or an empty repository. To bring a repository that already has notes onto this computer, clone it (git: Clone) and turn on auto sync in the cloned folder.",
                )
                .await?;
                return Ok(None);
            }
            let options = empty
                .iter()
                .map(|repo| repo.full_name.clone().into())
                .collect();
            let workspace = workspace.clone();
            let choice = cx
                .update(|window, cx| {
                    git_ui::picker_prompt::prompt(
                        "Choose the empty repository to back up to",
                        options,
                        workspace,
                        window,
                        cx,
                    )
                })?
                .await;
            Ok(choice
                .and_then(|index| empty.get(index).cloned())
                .map(Destination::Existing))
        }
        _ => Ok(None),
    }
}

async fn git_is_installed() -> bool {
    util::command::new_command("git")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

/// Polls for state that a background scan fills in.
async fn wait_for<T>(
    cx: &mut AsyncApp,
    what: &str,
    mut check: impl FnMut(&mut AsyncApp) -> Option<T>,
) -> Result<T> {
    for _ in 0..150 {
        if let Some(value) = check(cx) {
            return Ok(value);
        }
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
    Err(anyhow!("timed out waiting for {what}"))
}
