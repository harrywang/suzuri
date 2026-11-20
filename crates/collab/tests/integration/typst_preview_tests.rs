//! End-to-end collaborative Typst preview.
//!
//! A real in-process collab server connects a host and a guest. The host has a
//! fake preview backend (the same seam the preview pane uses), so the test
//! covers the whole relay without a GPU-bound Tinymist:
//!
//! guest `RelayClient` -> `ExecuteLspCommand` -> collab server -> host
//! command handler -> stream queue -> long-poll -> guest -> vector IR
//!
//! It asserts frame delivery, IR merging, viewport suppression, outline
//! forwarding, pane joining/refcounting, per-user click resolution, and
//! per-user PDF export.

use std::{path::Path, sync::Arc, time::Duration};

use base64::Engine as _;
use call::ActiveCall;
use gpui::{Entity, TestAppContext};
use lsp::{LanguageServerBinary, LanguageServerName};
use project::lsp_store::ShowDocumentOrigin;
use project::{LanguageServerShowDocumentRequest, LspStore, Project, WorktreeId};
use serde_json::json;
use typst_live_preview::{
    PreviewSession,
    collab::{
        RelayClient, RelayItem,
        test_support::{FakeHost, HostCommand},
    },
    protocol::preview_update_from_bytes,
};
use util::path;

use crate::TestServer;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/typst_live_preview/tests/fixtures")
            .join(name),
    )
    .expect("the recorded preview fixture")
}

/// Returns the next non-idle relay item, failing if the host stays silent.
async fn next_item(relay: &mut RelayClient, cx: &mut gpui::AsyncApp) -> RelayItem {
    for _ in 0..50 {
        if let Some(item) = relay.next(cx).await.expect("relay poll") {
            return item;
        }
    }
    panic!("timed out waiting for a relay item");
}

async fn setup(
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
    root: &str,
) -> (
    TestServer,
    crate::TestClient,
    crate::TestClient,
    Entity<Project>,
    Entity<Project>,
    Entity<LspStore>,
    WorktreeId,
) {
    let _ = env_logger::builder().try_init();
    let mut server = TestServer::start(cx_a.executor()).await;
    let client_a = server.create_client(cx_a, "host").await;
    let client_b = server.create_client(cx_b, "guest").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b)])
        .await;

    // The host must install the preview module before its project exists, so
    // the new project gets a relay and a command handler.
    cx_a.update(typst_live_preview::init);

    client_a
        .fs()
        .insert_tree(Path::new(root), json!({ "test.typ": "= Hello" }))
        .await;
    let (host_project, worktree_id) = client_a.build_local_project(Path::new(root), cx_a).await;
    let project_id = cx_a
        .read(ActiveCall::global)
        .update(cx_a, |call, cx| {
            call.share_project(host_project.clone(), cx)
        })
        .await
        .expect("share the host project");
    let guest_project = client_b.join_remote_project(project_id, cx_b).await;
    cx_a.background_executor.run_until_parked();

    let host_lsp = host_project.read_with(cx_a, |project, _| project.lsp_store());
    (
        server,
        client_a,
        client_b,
        host_project,
        guest_project,
        host_lsp,
        worktree_id,
    )
}

/// The host must answer a start request by actually starting Tinymist, not by
/// hanging. This drives the real `start_preview_task` path through a fake
/// language server, so a missing or mismatched Tinymist command produces a
/// response instead of a guest-side timeout.
#[gpui::test]
async fn host_starts_the_preview_through_its_language_server(
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
) {
    let (_server, _client_a, _client_b, _host_project, guest_project, host_lsp, worktree_id) =
        setup(cx_a, cx_b, "/host-b").await;
    let guest_lsp = guest_project.read_with(cx_b, |project, _| project.lsp_store());

    let server_id = lsp::LanguageServerId::from_proto(99);
    let mut app_a = cx_a.to_async();
    let (server, fake_server) = lsp::FakeLanguageServer::new(
        server_id,
        LanguageServerBinary {
            path: "/fake/tinymist".into(),
            arguments: Vec::new(),
            env: None,
        },
        "tinymist".to_owned(),
        lsp::ServerCapabilities {
            execute_command_provider: Some(lsp::ExecuteCommandOptions {
                commands: vec![
                    "tinymist.focusMain".to_owned(),
                    "tinymist.doStartBrowsingPreview".to_owned(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
        &mut app_a,
    );
    fake_server.set_request_handler::<lsp::request::ExecuteCommand, _, _>(move |params, _| {
        let result = if params.command == "tinymist.doStartBrowsingPreview" {
            json!({ "dataPlanePort": 1 })
        } else {
            json!(null)
        };
        async move { Ok(Some(result)) }
    });
    // `execute_lsp_command` refuses commands the server does not advertise.
    server.update_capabilities(|capabilities| {
        capabilities.execute_command_provider = Some(lsp::ExecuteCommandOptions {
            commands: vec![
                "tinymist.focusMain".to_owned(),
                "tinymist.doStartBrowsingPreview".to_owned(),
            ],
            ..Default::default()
        });
    });
    host_lsp.update(cx_a, |store, _| {
        store.add_test_language_server(
            server_id,
            LanguageServerName("tinymist".into()),
            Some(worktree_id),
            Arc::new(server),
        );
    });
    let injected = host_lsp.update(cx_a, |store, _| store.language_server_for_id(server_id));
    assert!(injected.is_some(), "the fake Tinymist is registered");
    let commands = injected
        .unwrap()
        .capabilities()
        .execute_command_provider
        .map(|options| options.commands)
        .unwrap_or_default();
    assert!(
        commands.contains(&"tinymist.doStartBrowsingPreview".to_owned()),
        "the fake advertises the preview command: {commands:?}"
    );

    let mut app_b = cx_b.to_async();
    let mut relay =
        match RelayClient::start(guest_lsp, Path::new("/host-b/test.typ"), &mut app_b).await {
            Ok(relay) => relay,
            Err(error) => panic!("the host answered the start request: {error:#?}"),
        };
    assert!(!relay.stream_id().is_empty());

    // The fake reports port 1. The relay cannot connect and closes the stream
    // with a reason. The important part is that this arrives at all; a host
    // that never answers would hit the 20 s start timeout instead.
    match next_item(&mut relay, &mut app_b).await {
        RelayItem::Stopped(reason) => assert!(!reason.is_empty(), "a reason is reported"),
        other => panic!("expected a connect failure, got {other:?}"),
    }
}

#[gpui::test]
async fn guest_preview_relays_every_feature(cx_a: &mut TestAppContext, cx_b: &mut TestAppContext) {
    let mut server = TestServer::start(cx_a.executor()).await;
    let client_a = server.create_client(cx_a, "host").await;
    let client_b = server.create_client(cx_b, "guest").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b)])
        .await;

    // The host must install the preview module before its project exists, so
    // the new project gets a relay and a command handler.
    cx_a.update(typst_live_preview::init);

    client_a
        .fs()
        .insert_tree(path!("/host-a"), json!({ "test.typ": "= Hello" }))
        .await;
    let (host_project, _) = client_a.build_local_project(path!("/host-a"), cx_a).await;
    let project_id = cx_a
        .read(ActiveCall::global)
        .update(cx_a, |call, cx| {
            call.share_project(host_project.clone(), cx)
        })
        .await
        .expect("share the host project");
    let guest_project = client_b.join_remote_project(project_id, cx_b).await;
    cx_a.background_executor.run_until_parked();

    let host_lsp = host_project.read_with(cx_a, |project, _| project.lsp_store());
    let guest_lsp = guest_project.read_with(cx_b, |project, _| project.lsp_store());

    let fake = FakeHost::new("/host-a/test.typ");
    fake.install();

    // -- Start -----------------------------------------------------------------
    let mut app_b = cx_b.to_async();
    let abs_path = Path::new("/host-a/test.typ");
    let mut relay = RelayClient::start(guest_lsp.clone(), abs_path, &mut app_b)
        .await
        .expect("the guest can start a preview");
    let stream_id = relay.stream_id().to_owned();
    assert!(!stream_id.is_empty());
    let server_id = relay.server_id();
    assert_eq!(
        fake.senders().len(),
        1,
        "the host started exactly one stream"
    );

    // -- Frames reach the guest and merge into the IR --------------------------
    let new_frame = fixture("preview-new.bin");
    fake.senders()[0]
        .send(HostCommand::Frame(new_frame.clone()))
        .await
        .unwrap();
    let item = next_item(&mut relay, &mut app_b).await;
    match item {
        RelayItem::Frame(frame) => assert_eq!(frame, new_frame),
        other => panic!("expected the full document frame, got {other:?}"),
    }
    let update = preview_update_from_bytes(&new_frame).expect("a data-plane frame");
    assert!(update.reset_before_merge);
    let mut session = PreviewSession::new();
    session.merge(update);
    assert_eq!(session.page_metas().len(), 2, "the fixture has two pages");

    // -- Viewport frames stay on the host --------------------------------------
    fake.senders()[0]
        .send(HostCommand::Frame(b"viewport,1 10 20".to_vec()))
        .await
        .unwrap();
    let diff_frame = fixture("preview-diff.bin");
    fake.senders()[0]
        .send(HostCommand::Frame(diff_frame.clone()))
        .await
        .unwrap();
    match next_item(&mut relay, &mut app_b).await {
        RelayItem::Frame(frame) => {
            assert_eq!(
                frame, diff_frame,
                "the viewport frame must be dropped, not relayed"
            );
        }
        other => panic!("expected the diff frame, got {other:?}"),
    }

    // -- Outline ---------------------------------------------------------------
    let outline = json!({ "items": [{ "title": "Intro", "children": [] }] });
    fake.senders()[0]
        .send(HostCommand::Outline(outline.clone()))
        .await
        .unwrap();
    match next_item(&mut relay, &mut app_b).await {
        RelayItem::Outline(received) => assert_eq!(received, outline),
        other => panic!("expected the outline, got {other:?}"),
    }

    // -- A second pane joins the same host task --------------------------------
    let mut second = RelayClient::start(guest_lsp.clone(), abs_path, &mut app_b)
        .await
        .expect("the second pane starts");
    assert_eq!(second.stream_id(), stream_id, "panes share one stream");
    assert_eq!(
        fake.senders().len(),
        1,
        "the second pane must not start a second host task"
    );

    // -- Click-through: only the guest receives the location -------------------
    let resolve = cx_b.spawn({
        let guest_lsp = guest_lsp.clone();
        let stream_id = stream_id.clone();
        async move |mut cx| {
            RelayClient::resolve_at(&guest_lsp, server_id, &stream_id, 0, 12.0, 34.0, &mut cx).await
        }
    });

    let mut src_point = None;
    for _ in 0..100 {
        if let Ok(text) = fake.src_points().try_recv() {
            src_point = Some(text);
            break;
        }
        cx_a.background_executor
            .advance_clock(Duration::from_millis(10));
        cx_a.background_executor.run_until_parked();
        cx_b.background_executor
            .advance_clock(Duration::from_millis(10));
        cx_b.background_executor.run_until_parked();
    }
    let src_point = src_point.expect("the host asked Tinymist for a source point");
    assert!(src_point.starts_with("src-point "), "{src_point}");

    let uri = "file:///host-a/test.typ".parse::<lsp::Uri>().unwrap();
    let expected_selection = lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 5));
    cx_a.update(|cx| {
        host_lsp.update(cx, |_, cx| {
            let mut request = LanguageServerShowDocumentRequest::without_response(
                uri,
                true,
                Some(expected_selection),
            );
            request.origin = ShowDocumentOrigin::Preview;
            cx.emit(project::LspStoreEvent::LanguageServerShowDocument(request));
        });
    });
    cx_a.background_executor.run_until_parked();
    cx_b.background_executor.run_until_parked();

    let resolved = resolve.await.expect("resolve response");
    let (uri, selection) = resolved.expect("the host resolved the point");
    assert_eq!(uri.as_str(), "file:///host-a/test.typ");
    assert_eq!(selection, Some(expected_selection));

    // -- Per-user PDF export ---------------------------------------------------
    let pdf = b"%PDF-1.7 relay".to_vec();
    let pdf_base64 = base64::engine::general_purpose::STANDARD.encode(&pdf);
    host_lsp.update(cx_a, |store, _| {
        let pdf_base64 = pdf_base64.clone();
        store.register_command_handler(
            "tinymist.exportPdf",
            std::sync::Arc::new(move |_, _, arguments, _, _| {
                let pdf_base64 = pdf_base64.clone();
                Box::pin(async move {
                    // The guest must ask for bytes so the host's disk is untouched.
                    assert_eq!(arguments[2], json!({ "write": false }));
                    Ok(Some(json!({ "data": pdf_base64 })))
                })
            }),
        );
    });
    let export = guest_lsp
        .update(cx_b, |store, cx| {
            store.execute_lsp_command(
                server_id,
                "tinymist.exportPdf".to_owned(),
                vec![
                    json!("/host-a/test.typ"),
                    json!({}),
                    json!({ "write": false }),
                ],
                cx,
            )
        })
        .await
        .expect("export response")
        .expect("export result");
    let data = base64::engine::general_purpose::STANDARD
        .decode(export["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(data, pdf);

    // -- Closing every pane releases the host stream ---------------------------
    cx_b.update(|cx| {
        second.stop(cx);
        relay.stop(cx);
    });
    cx_a.background_executor.run_until_parked();
    cx_b.background_executor.run_until_parked();
    assert!(
        matches!(
            next_item(&mut second, &mut app_b).await,
            RelayItem::Stopped(_)
        ),
        "closing the last pane closes the relayed stream"
    );

    typst_live_preview::collab::test_support::clear_fake_preview();
}
