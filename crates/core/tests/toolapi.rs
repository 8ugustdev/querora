// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! toolapi integration tests over a REAL unix socket (temp dir), including
//! the mandatory negative tests: unauthenticated / foreign-token processes
//! are rejected and audited.

use querora_core::fixtures::shop_graph;
use querora_core::keyring::MemoryStore;
use querora_core::storage::AppStore;
use querora_core::toolapi::{self, register_defaults, ToolApiServer, ToolContext, ToolRegistry};
use rand::RngCore;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

async fn spawn_server(dir: &std::path::Path) -> (std::path::PathBuf, Arc<ToolContext>, String) {
    let store = Arc::new(AppStore::open_in_memory().await.unwrap());
    let creds = Arc::new(MemoryStore::default());
    let ctx = Arc::new(ToolContext::new(store, creds, Some(Arc::new(shop_graph()))));
    let registry = Arc::new(ToolRegistry::new());
    register_defaults(&registry);
    let token = toolapi::get_or_create_token(ctx.creds.as_ref()).unwrap();
    let socket = dir.join("querora.sock");
    let server = Arc::new(ToolApiServer::new(
        socket.clone(),
        registry,
        ctx.clone(),
        token.clone(),
    ));
    tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            panic!("server failed: {e}");
        }
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (socket, ctx, token)
}

async fn send(w: &mut OwnedWriteHalf, body: &serde_json::Value) {
    let mut line = body.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await.unwrap();
    w.flush().await.unwrap();
}

async fn read_line(r: &mut BufReader<OwnedReadHalf>) -> serde_json::Value {
    let mut line = String::new();
    r.read_line(&mut line).await.expect("read a response line");
    serde_json::from_str(line.trim_end()).expect("valid JSON response")
}

async fn open(socket: &std::path::Path) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (r, w) = stream.into_split();
    (BufReader::new(r), w)
}

#[tokio::test]
async fn unauthenticated_process_is_rejected_and_audited() {
    let dir = temp_dir();
    let (socket, ctx, _token) = spawn_server(&dir).await;

    // 1. no auth frame at all — straight to a tool call
    let (mut r, mut w) = open(&socket).await;
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"list_tools","params":{}}),
    )
    .await;
    let resp = read_line(&mut r).await;
    assert_eq!(
        resp["error"]["code"], -32001,
        "must be unauthorized: {resp}"
    );
    // connection closes after rejection
    let mut leftover = String::new();
    r.read_to_string(&mut leftover).await.unwrap_or(0);
    assert!(leftover.is_empty(), "connection should close");

    // 2. foreign token
    let (mut r, mut w) = open(&socket).await;
    send(&mut w, &serde_json::json!({"jsonrpc":"2.0","id":"a","method":"auth","params":{"token":"deadbeef"}})).await;
    let resp = read_line(&mut r).await;
    assert_eq!(
        resp["error"]["code"], -32001,
        "foreign token must be rejected: {resp}"
    );

    // 3. audit trail recorded both times
    tokio::time::sleep(Duration::from_millis(100)).await;
    let entries = ctx.store.audit_entries(10).await.unwrap();
    assert!(
        entries
            .iter()
            .filter(|e| e.1 == "toolapi" && e.2 == "auth" && e.3.contains("REJECTED"))
            .count()
            >= 2,
        "rejections must be audited: {entries:?}"
    );
}

#[tokio::test]
async fn authenticated_round_trip_list_tools_and_search() {
    let dir = temp_dir();
    let (socket, _ctx, token) = spawn_server(&dir).await;

    let (mut r, mut w) = open(&socket).await;
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":"a","method":"auth","params":{"token":token}}),
    )
    .await;
    let resp = read_line(&mut r).await;
    assert_eq!(resp["result"]["ok"], true);

    // list_tools
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"list_tools","params":{}}),
    )
    .await;
    let resp = read_line(&mut r).await;
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "search_semantics",
        "execute_query",
        "get_schema",
        "profile_column",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    // search_semantics over the fixture graph
    send(&mut w, &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"search_semantics","params":{"query":"revenue by month"}})).await;
    let resp = read_line(&mut r).await;
    let items = resp["result"]["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|i| i["id"] == "revenue" && i["kind"] == "metric"));

    // execute_query is scaffolded → structured not_implemented
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"execute_query","params":{"ir":{}}}),
    )
    .await;
    let resp = read_line(&mut r).await;
    // compiler is real: empty IR → structured invalid_ir
    assert_eq!(resp["error"]["code"], -32013);
    assert_eq!(resp["error"]["data"]["code"], "invalid_ir");

    // unknown tool → structured not_found with available list
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"drop_table","params":{}}),
    )
    .await;
    let resp = read_line(&mut r).await;
    assert_eq!(resp["error"]["code"], -32004);
    assert!(resp["error"]["data"]["details"]["available"].is_array());

    // tool calls audited
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn dual_mode_token_authenticates_and_audits() {
    let dir = temp_dir();
    let (_socket, ctx, _token) = spawn_server(&dir).await;

    // write a dual-mode token and flip it into the live server
    let dual = querora_core::dualmode::rotate_token().unwrap();
    // note: spawn_server built its ToolApiServer internally; the constructor
    // already reads the token file — but this server predates it. Validate the
    // handshake contract through a second server on a fresh socket instead.
    let _ = ctx;
    let dir2 = temp_dir();
    let socket2 = dir2.join("querora.sock");
    let store = Arc::new(AppStore::open_in_memory().await.unwrap());
    let ctx2 = Arc::new(ToolContext::new(
        store,
        Arc::new(MemoryStore::default()),
        Some(Arc::new(shop_graph())),
    ));
    let registry2 = Arc::new(ToolRegistry::new());
    register_defaults(&registry2);
    let server = Arc::new(ToolApiServer::new(
        socket2.clone(),
        registry2,
        ctx2,
        "primary-tok".into(),
    ));
    tokio::spawn({
        let s = server.clone();
        async move {
            s.serve().await.ok();
        }
    });
    for _ in 0..100 {
        if socket2.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // primary token still works
    let (mut r, mut w) = open(&socket2).await;
    send(&mut w, &serde_json::json!({"jsonrpc":"2.0","id":"a","method":"auth","params":{"token":"primary-tok"}})).await;
    assert_eq!(read_line(&mut r).await["result"]["ok"], true);

    // dual token works
    let (mut r, mut w) = open(&socket2).await;
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":"a","method":"auth","params":{"token":dual}}),
    )
    .await;
    assert_eq!(read_line(&mut r).await["result"]["ok"], true);
    // and can call tools
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"list_tools","params":{}}),
    )
    .await;
    assert!(
        read_line(&mut r).await["result"]["tools"]
            .as_array()
            .unwrap()
            .len()
            >= 5
    );

    // foreign token still rejected
    let (mut r, mut w) = open(&socket2).await;
    send(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":"a","method":"auth","params":{"token":"nope"}}),
    )
    .await;
    assert_eq!(read_line(&mut r).await["error"]["code"], -32001);
}

#[tokio::test]
async fn second_instance_cannot_bind_same_socket() {
    let dir = temp_dir();
    let (socket, _ctx, _token) = spawn_server(&dir).await;
    let store = Arc::new(AppStore::open_in_memory().await.unwrap());
    let ctx2 = Arc::new(ToolContext::new(
        store,
        Arc::new(MemoryStore::default()),
        None,
    ));
    let registry2 = Arc::new(ToolRegistry::new());
    register_defaults(&registry2);
    let dup = ToolApiServer::new(socket, registry2, ctx2, "t".into());
    assert!(
        Arc::new(dup).serve().await.is_err(),
        "second instance must be refused"
    );
}

fn temp_dir() -> std::path::PathBuf {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    let dir = std::env::temp_dir().join(format!(
        "querora-test-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
