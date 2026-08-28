//! Records how a document was actually written.
//!
//! Claude Code already writes a complete record of what an *agent* did, which
//! `script/suzuri-agent-log.py` archives into the project. This crate records the
//! other half — what happened inside the editor — because two things are
//! invisible from outside it:
//!
//!   * A human edit leaves no event anywhere. Only the final text survives, so
//!     after the fact there is no way to know an edit happened, in what order, or
//!     whether it was typed or pasted.
//!   * An agent that writes a file by any means other than the Write/Edit tools
//!     — a `cat > file <<EOF` heredoc, `sed -i`, a formatter — fires no tool hook
//!     and leaves no checkpoint. The editor still sees the file change.
//!
//! Records are JSONL in the same envelope Claude Code uses (`type`, `uuid`,
//! `parentUuid`, `timestamp`, `sessionId`, `cwd`, `gitBranch`), so the two
//! streams parse with one reader and merge on timestamp into a single timeline
//! of human and agent activity.
//!
//! What is deliberately *not* recorded is the text itself. A record carries
//! offsets, lengths, and timing; the content already lives in the file. Pasted
//! text is reduced to a length and a hash, which is enough to later match it
//! against an archived agent transcript without either side storing prose.
//!
//! Recording is opt-in per project and off by default: it begins only when
//! `<project>/.suzuri/editor-log/enabled` exists.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result};
use chrono::{SecondsFormat, Utc};
use editor::{Editor, EditorEvent};
use gpui::{App, Context, Entity, Global, Window};
use language::{Buffer, BufferEvent};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Marker file that turns recording on for a project.
const ENABLED_MARKER: &str = "enabled";
const LEDGER_DIR: &[&str] = &[".suzuri", "editor-log"];
/// How far up the tree to look for the marker before giving up.
const MAX_ROOT_SEARCH_DEPTH: usize = 64;

pub fn init(cx: &mut App) {
    cx.set_global(GlobalLedger::default());
    cx.observe_new(register_editor).detach();
}

// ---------------------------------------------------------------------------
// records

/// One line of the ledger.
///
/// Field names match Claude Code's transcript records so both streams can be
/// read by the same parser. `payload` carries the per-type detail.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub r#type: &'static str,
    pub uuid: String,
    pub parent_uuid: Option<String>,
    /// UTC, RFC 3339, millisecond precision - the format Claude Code writes.
    pub timestamp: String,
    pub session_id: String,
    pub cwd: String,
    pub version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub payload: serde_json::Value,
}

/// Where an edit came from.
///
/// `Typed` and `Pasted` are both local user input; they are separated because
/// that distinction is the whole point of recording, and it is the one thing a
/// file-level diff can never recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSource {
    Typed,
    Pasted,
    /// An in-editor agent (Zed's own agent panel).
    Agent,
    /// The file changed on disk and the editor did not do it - an external
    /// agent, a formatter, another process.
    External,
    /// A collaborator over the network.
    Remote,
    Undo,
    Unknown,
}

pub const VERSION: &str = concat!("authoring_ledger/", env!("CARGO_PKG_VERSION"));

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// opt-in and paths

fn ledger_dir(root: &Path) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in LEDGER_DIR {
        path.push(part);
    }
    path
}

/// Walk up from `start` looking for a project whose ledger is switched on.
///
/// Returns the project root, not the ledger directory, because the root is what
/// identifies the project in the record and what the agent-log archive keys on.
pub fn enabled_root_for(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    for _ in 0..MAX_ROOT_SEARCH_DEPTH {
        let dir = current?;
        if ledger_dir(dir).join(ENABLED_MARKER).exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

// ---------------------------------------------------------------------------
// writer

/// Append-only JSONL writer for one project's session.
pub struct SessionWriter {
    session_id: String,
    root: PathBuf,
    file: File,
    last_uuid: Option<String>,
}

impl SessionWriter {
    pub fn open(root: &Path) -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        let sessions = ledger_dir(root).join("sessions");
        std::fs::create_dir_all(&sessions)
            .with_context(|| format!("creating {}", sessions.display()))?;
        let path = sessions.join(format!("{session_id}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            session_id,
            root: root.to_path_buf(),
            file,
            last_uuid: None,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Append one record. Records are chained by `parentUuid` in write order,
    /// which is what lets a reader reconstruct the sequence even though
    /// timestamps from interleaved sources are not globally monotonic.
    pub fn write(
        &mut self,
        kind: &'static str,
        file: Option<String>,
        payload: serde_json::Value,
    ) -> Result<()> {
        let uuid = Uuid::new_v4().to_string();
        let record = Record {
            r#type: kind,
            uuid: uuid.clone(),
            parent_uuid: self.last_uuid.clone(),
            timestamp: now_iso(),
            session_id: self.session_id.clone(),
            cwd: self.root.to_string_lossy().into_owned(),
            version: VERSION,
            file,
            payload,
        };
        let line = serde_json::to_string(&record)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.last_uuid = Some(uuid);
        Ok(())
    }
}

/// One writer per project root, shared by every editor open on that project.
#[derive(Default)]
pub struct GlobalLedger {
    writers: Arc<Mutex<HashMap<PathBuf, Option<SessionWriter>>>>,
}

impl Global for GlobalLedger {}

impl GlobalLedger {
    /// Run `f` against the writer for `root`, opening one on first use.
    ///
    /// A project whose writer failed to open is remembered as `None` so a broken
    /// path does not retry on every keystroke.
    fn with_writer(&self, root: &Path, f: impl FnOnce(&mut SessionWriter) -> Result<()>) {
        let mut writers = self.writers.lock();
        let entry = writers
            .entry(root.to_path_buf())
            .or_insert_with(|| match SessionWriter::open(root) {
                Ok(mut writer) => {
                    let payload = serde_json::json!({ "root": root.to_string_lossy() });
                    if let Err(error) = writer.write("session_start", None, payload) {
                        log::error!("authoring_ledger: session_start failed: {error:#}");
                    }
                    Some(writer)
                }
                Err(error) => {
                    log::error!("authoring_ledger: disabled for this project: {error:#}");
                    None
                }
            });
        if let Some(writer) = entry.as_mut()
            && let Err(error) = f(writer)
        {
            log::error!("authoring_ledger: write failed: {error:#}");
        }
    }
}

fn record(
    cx: &mut App,
    root: &Path,
    kind: &'static str,
    file: Option<String>,
    payload: serde_json::Value,
) {
    if !cx.has_global::<GlobalLedger>() {
        return;
    }
    let ledger = cx.global::<GlobalLedger>().writers.clone();
    let global = GlobalLedger { writers: ledger };
    global.with_writer(root, |writer| writer.write(kind, file, payload));
}

// ---------------------------------------------------------------------------
// editor integration

/// Per-editor state: which project this buffer belongs to, and enough of the
/// buffer's edit stream to describe a change without storing its text.
pub struct LedgerAddon {
    path: Option<String>,
    subscription: text::Subscription<usize>,
    /// Set when the last local edit's text matched the clipboard, which is how a
    /// paste is told from typing without patching the editor's paste handler.
    last_source: EditSource,
    _subscriptions: Vec<gpui::Subscription>,
}

impl editor::Addon for LedgerAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn register_editor(editor: &mut Editor, _window: Option<&mut Window>, cx: &mut Context<Editor>) {
    if !editor.mode().is_full() {
        return;
    }
    let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
        // Multi-buffer editors (search results, diffs) are not authoring
        // surfaces; recording them would attribute edits to the wrong file.
        return;
    };

    let Some(abs_path) = buffer
        .read(cx)
        .file()
        .and_then(|file| file.as_local().map(|local| local.abs_path(cx)))
    else {
        return;
    };
    let Some(root) = enabled_root_for(&abs_path) else {
        return;
    };
    let path = abs_path
        .strip_prefix(&root)
        .unwrap_or(&abs_path)
        .to_string_lossy()
        .into_owned();

    let subscription = buffer.update(cx, |buffer, _| buffer.subscribe());

    let mut subscriptions = Vec::new();
    subscriptions.push(cx.subscribe(&buffer, {
        let root = root.clone();
        move |editor, buffer, event: &BufferEvent, cx| {
            on_buffer_event(editor, &buffer, event, &root, cx)
        }
    }));
    subscriptions.push(cx.subscribe_self({
        let root = root.clone();
        let path = path.clone();
        move |_editor, event: &EditorEvent, cx| {
            let kind = match event {
                EditorEvent::Focused => "focus",
                EditorEvent::Blurred => "blur",
                EditorEvent::Saved => "save",
                _ => return,
            };
            record(cx, &root, kind, Some(path.clone()), serde_json::json!({}));
        }
    }));

    editor.register_addon(LedgerAddon {
        path: Some(path),
        subscription,
        last_source: EditSource::Unknown,
        _subscriptions: subscriptions,
    });
}

fn on_buffer_event(
    editor: &mut Editor,
    buffer: &Entity<Buffer>,
    event: &BufferEvent,
    root: &Path,
    cx: &mut Context<Editor>,
) {
    match event {
        BufferEvent::Edited { source } => {
            let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
            let Some(addon) = editor.addon_mut::<LedgerAddon>() else {
                return;
            };
            let edits = addon.subscription.consume().into_inner();
            if edits.is_empty() {
                return;
            }
            let path = addon.path.clone();
            let snapshot = buffer.read(cx).text_snapshot();

            let mut payload_edits = Vec::new();
            let mut source_kind = match source {
                language::BufferEditSource::Agent => EditSource::Agent,
                language::BufferEditSource::Remote => EditSource::Remote,
                language::BufferEditSource::User => EditSource::Typed,
            };
            for edit in &edits {
                let inserted_len = edit.new.end.saturating_sub(edit.new.start);
                let deleted_len = edit.old.end.saturating_sub(edit.old.start);
                let inserted: String = snapshot
                    .text_for_range(edit.new.start..edit.new.end)
                    .collect();
                // A local insert whose text is exactly the clipboard is a paste.
                // Typing the clipboard's contents by hand would be
                // misclassified, which is a price worth paying to avoid
                // patching the editor's paste path.
                if source_kind == EditSource::Typed
                    && inserted_len > 1
                    && clipboard.as_deref() == Some(inserted.as_str())
                {
                    source_kind = EditSource::Pasted;
                }
                payload_edits.push(serde_json::json!({
                    "offset": edit.new.start,
                    "inserted": inserted_len,
                    "deleted": deleted_len,
                    // Content is never stored; the hash is enough to later match
                    // a pasted span against an archived agent transcript.
                    "sha256": if source_kind == EditSource::Pasted {
                        serde_json::Value::String(hash_text(&inserted))
                    } else {
                        serde_json::Value::Null
                    },
                }));
            }
            addon.last_source = source_kind;
            record(
                cx,
                root,
                "edit",
                path,
                serde_json::json!({ "source": source_kind, "edits": payload_edits }),
            );
        }
        BufferEvent::Reloaded => {
            let path = editor
                .addon::<LedgerAddon>()
                .and_then(|addon| addon.path.clone());
            // The file changed underneath us: an agent or another process wrote
            // it. This is the only signal that catches a write made by something
            // other than the Write/Edit tools.
            record(
                cx,
                root,
                "external_write",
                path,
                serde_json::json!({ "source": EditSource::External }),
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn enable(root: &Path) {
        let dir = ledger_dir(root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(ENABLED_MARKER), "").unwrap();
    }

    #[test]
    fn recording_is_off_until_the_marker_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("paper.md");
        fs::write(&file, "hello").unwrap();
        assert_eq!(enabled_root_for(&file), None);

        enable(tmp.path());
        assert_eq!(
            enabled_root_for(&file).unwrap().canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn marker_is_found_from_a_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path());
        let nested = tmp.path().join("chapters").join("one");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("draft.md");
        fs::write(&file, "x").unwrap();
        assert_eq!(
            enabled_root_for(&file).unwrap().canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn records_use_the_claude_code_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path());
        let mut writer = SessionWriter::open(tmp.path()).unwrap();
        writer
            .write(
                "edit",
                Some("paper.md".into()),
                serde_json::json!({"source":"typed"}),
            )
            .unwrap();
        writer
            .write("save", Some("paper.md".into()), serde_json::json!({}))
            .unwrap();

        let path = ledger_dir(tmp.path())
            .join("sessions")
            .join(format!("{}.jsonl", writer.session_id()));
        let contents = fs::read_to_string(path).unwrap();
        let lines: Vec<serde_json::Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);

        for line in &lines {
            for field in ["type", "uuid", "timestamp", "sessionId", "cwd", "version"] {
                assert!(line.get(field).is_some(), "missing {field}");
            }
        }
        // parentUuid chains the records in write order.
        assert!(lines[0]["parentUuid"].is_null());
        assert_eq!(lines[1]["parentUuid"], lines[0]["uuid"]);
        // Timestamps must be UTC with millisecond precision, like Claude Code's.
        let stamp = lines[0]["timestamp"].as_str().unwrap();
        assert!(stamp.ends_with('Z'), "not UTC: {stamp}");
        assert_eq!(stamp.len(), 24, "not millisecond precision: {stamp}");
    }

    #[test]
    fn text_is_never_written_to_the_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path());
        let mut writer = SessionWriter::open(tmp.path()).unwrap();
        let secret = "an unpublished sentence";
        writer
            .write(
                "edit",
                Some("paper.md".into()),
                serde_json::json!({
                    "source": "pasted",
                    "edits": [{"offset": 0, "inserted": secret.len(), "sha256": hash_text(secret)}],
                }),
            )
            .unwrap();
        let path = ledger_dir(tmp.path())
            .join("sessions")
            .join(format!("{}.jsonl", writer.session_id()));
        let contents = fs::read_to_string(path).unwrap();
        assert!(!contents.contains(secret));
        assert!(contents.contains(&hash_text(secret)));
    }
}
