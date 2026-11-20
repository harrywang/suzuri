//! A minimal LSP client for the live-preview example harnesses.
//!
//! These examples drive a real `tinymist lsp` process over piped stdio, so they
//! need to frame JSON-RPC messages. `std::process::Command` is used on purpose;
//! these are developer tools, not application code.

#![allow(clippy::disallowed_methods)]
#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

pub struct LspServer {
    pub child: Child,
    stdin: ChildStdin,
    messages: mpsc::Receiver<Value>,
    received: Vec<Value>,
}

impl LspServer {
    pub fn spawn(tinymist: &str, root: &str) -> Result<Self> {
        let mut child = Command::new(tinymist)
            .arg("lsp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning {tinymist} lsp"))?;
        let stdin = child.stdin.take().context("tinymist stdin")?;
        let stdout = child.stdout.take().context("tinymist stdout")?;

        let (messages_tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = read_message(&mut reader) {
                if messages_tx.send(message).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            messages,
            received: Vec::new(),
        })
    }

    pub fn send(&mut self, message: &Value) -> Result<()> {
        let body = serde_json::to_vec(message)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        self.stdin.write_all(&body)?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .expect("sending an LSP notification");
    }

    pub fn request(&mut self, id: i64, method: &str, params: Value) -> Result<Value> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for the response to {method} ({id})");
            }
            let message = self
                .messages
                .recv_timeout(remaining)
                .with_context(|| format!("waiting for the response to {method}"))?;
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                return Ok(message);
            }
            self.received.push(message);
        }
    }

    /// Waits for a request the server sends to the client, such as
    /// `window/showDocument`.
    pub fn wait_for_request(&mut self, method: &str, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for the {method} request");
            }
            let message = self
                .messages
                .recv_timeout(remaining)
                .with_context(|| format!("waiting for the {method} request"))?;
            if message.get("method").and_then(Value::as_str) == Some(method) {
                return Ok(message);
            }
            self.received.push(message);
        }
    }

    /// Answers a server-to-client request.
    pub fn respond(&mut self, id: Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .expect("sending an LSP response");
    }

    pub fn shutdown(&mut self) {
        self.notify("exit", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }

    let Some(length) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// The LSP position just past the final character.
pub fn end_position(source: &str) -> (u32, u32) {
    let mut line = 0;
    let mut character = 0;
    for ch in source.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    (line, character)
}

/// Spawns Tinymist, opens `source` as `uri`, focuses it, and starts a headless
/// preview. Returns the server and the preview's data-plane port.
pub fn start_preview(
    tinymist: &str,
    root: &str,
    uri: &str,
    source: &str,
    entry: &str,
    task_id: &str,
) -> Result<(LspServer, u16)> {
    let mut server = LspServer::spawn(tinymist, root)?;
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": format!("file://{root}"),
            "capabilities": {},
            "workspaceFolders": [{ "uri": format!("file://{root}"), "name": "suzuri" }],
        }),
    )?;
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "typst",
                "version": 1,
                "text": source,
            }
        }),
    );
    server.request(
        2,
        "workspace/executeCommand",
        json!({
            "command": "tinymist.focusMain",
            "arguments": [entry],
        }),
    )?;
    let start = server.request(
        3,
        "workspace/executeCommand",
        json!({
            "command": "tinymist.doStartBrowsingPreview",
            "arguments": [[
                entry,
                "--data-plane-host=127.0.0.1:0",
                "--control-plane-host=127.0.0.1:0",
                "--no-open",
                format!("--task-id={task_id}"),
            ]],
        }),
    )?;
    let port = start
        .pointer("/result/dataPlanePort")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .with_context(|| format!("no dataPlanePort in {start}"))?;
    Ok((server, port))
}
