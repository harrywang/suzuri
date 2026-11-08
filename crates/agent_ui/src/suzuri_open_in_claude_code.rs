use acp_thread::AcpThread;
use agent_servers::CLAUDE_AGENT_ID;
use gpui::{App, WeakEntity, Window};
use task::{RevealTarget, SpawnInTerminal, TaskId};
use workspace::Workspace;

/// The Claude agent stores each ACP session as an ordinary Claude Code transcript under
/// the same ID, so the CLI's `--resume` picks the conversation up where the panel left it.
pub fn claude_resume_task(thread: &AcpThread) -> Option<SpawnInTerminal> {
    if thread.connection().agent_id().0.as_ref() != CLAUDE_AGENT_ID {
        return None;
    }
    let session_id = thread.session_id().0.to_string();
    // The ID is spliced into a shell script, so anything but a UUID-like token is refused.
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return None;
    }
    // Claude Code keys transcripts by the directory it was started in, so resuming from
    // anywhere else finds no session.
    let cwd = thread
        .work_dirs()
        .and_then(|work_dirs| work_dirs.ordered_paths().next().cloned());
    let resume_command = format!("claude --resume {session_id}");
    Some(SpawnInTerminal {
        id: TaskId(format!("suzuri-claude-resume-{session_id}")),
        full_label: resume_command.clone(),
        label: "Claude Code".to_string(),
        command: Some(resume_script(&resume_command)),
        command_label: resume_command,
        cwd,
        use_new_terminal: true,
        reveal_target: RevealTarget::Center,
        ..SpawnInTerminal::default()
    })
}

/// The panel's Claude agent runs on Zed's bundled adapter, so a working panel says nothing
/// about whether the `claude` CLI is installed. Without this check a missing CLI shows only
/// the shell's bare "command not found".
fn resume_script(resume_command: &str) -> String {
    if cfg!(windows) {
        format!(
            "if (Get-Command claude -ErrorAction SilentlyContinue) {{ {resume_command} }} else {{ \
             Write-Host 'Claude Code is not installed, or the claude command is not on your PATH.'; \
             Write-Host 'Install it with: irm https://claude.ai/install.ps1 | iex'; \
             Write-Host 'Then choose Open Thread in Claude Code again, or run: {resume_command}' }}"
        )
    } else {
        // `type`, `&&`, `||` and `printf` only, so the script also runs under fish.
        format!(
            "type claude >/dev/null 2>&1 && {resume_command}; \
             type claude >/dev/null 2>&1 || printf '%s\\n' \
             'Claude Code is not installed, or the claude command is not on your PATH.' \
             'Install it with: curl -fsSL https://claude.ai/install.sh | bash' \
             'Then choose Open Thread in Claude Code again, or run: {resume_command}'"
        )
    }
}

pub fn open_in_claude_code(
    task: SpawnInTerminal,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(workspace) = workspace.upgrade() {
        workspace.update(cx, |workspace, cx| {
            workspace.spawn_in_terminal(task, window, cx).detach();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp_thread::{AgentConnection as _, StubAgentConnection};
    use fs::FakeFs;
    use gpui::TestAppContext;
    use project::{AgentId, Project};
    use settings::SettingsStore;
    use std::{path::Path, rc::Rc};
    use util::{path, path_list::PathList};

    async fn thread_for_agent(agent_id: &str, cx: &mut TestAppContext) -> gpui::Entity<AcpThread> {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let connection =
            Rc::new(StubAgentConnection::new().with_agent_id(AgentId(agent_id.to_string().into())));
        cx.update(|cx| {
            connection.new_session(project, PathList::new(&[Path::new(path!("/notes"))]), cx)
        })
        .await
        .expect("session should be created")
    }

    #[gpui::test]
    async fn resumes_a_claude_thread_by_its_session_id(cx: &mut TestAppContext) {
        let thread = thread_for_agent(CLAUDE_AGENT_ID, cx).await;
        let (task, session_id) = thread.read_with(cx, |thread, _| {
            (
                claude_resume_task(thread).expect("a Claude thread can be resumed"),
                thread.session_id().0.to_string(),
            )
        });
        let resume_command = format!("claude --resume {session_id}");
        assert_eq!(task.command_label, resume_command);
        let script = task.command.expect("the task runs a script");
        assert!(script.contains(&resume_command));
        assert!(script.contains("not on your PATH"));
        assert_eq!(task.cwd.as_deref(), Some(Path::new(path!("/notes"))));
    }

    #[gpui::test]
    async fn offers_nothing_for_other_agents(cx: &mut TestAppContext) {
        let thread = thread_for_agent("codex-acp", cx).await;
        thread.read_with(cx, |thread, _| {
            assert!(claude_resume_task(thread).is_none())
        });
    }
}
