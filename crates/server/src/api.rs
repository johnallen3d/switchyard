//! Remote control-plane HTTP API (`/api/snapshot`, `/api/subscribe`,
//! `/api/rpc`), served on the same loopback + tailnet HTTP listener as `/mcp`.
//!
//! Client hosts with `[control_plane] url` configured (such as the Mac when
//! `switchyard-server` runs on Omarchy) use these endpoints in place of the
//! local Unix socket and direct `spawn-tasks.json` access.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use switchyard_core::links::Unlinked;
use switchyard_core::{actions, changes, folders, launch, tasks, transcript};

use crate::hostinfo::FOLDERS_STALE;
use crate::mcp::{Endpoint, Request};
use crate::state::Shared;

/// Periodic blank-line keepalive on `/api/subscribe` so gone tailnet clients
/// (e.g. a sleeping laptop) are noticed promptly.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// `GET /api/snapshot`: the current snapshot JSON.
pub fn route_snapshot(req: &Request, endpoint: &Endpoint) -> (u16, Option<Value>) {
    if req.method != "GET" {
        return (405, None);
    }
    (200, Some(endpoint.shared.lock().snapshot(Instant::now())))
}

/// `GET /api/subscribe`: stream the initial snapshot and each subsequent
/// change as newline-delimited JSON, with periodic blank keepalive lines.
pub fn subscribe(out: &mut impl Write, shared: &Arc<Shared>) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let first = {
        let mut state = shared.lock();
        state.subscribe(tx);
        state.snapshot(Instant::now())
    };
    out.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    writeln!(out, "{first}")?;
    out.flush()?;
    loop {
        match rx.recv_timeout(KEEPALIVE) {
            Ok(update) => {
                writeln!(out, "{update}")?;
                out.flush()?;
            }
            Err(RecvTimeoutError::Timeout) => {
                out.write_all(b"\n")?;
                out.flush()?;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// `POST /api/rpc`: one JSON request `{ "cmd": "...", ... }` returning one
/// JSON reply (or `{ "error": "..." }`).
pub fn route_rpc(req: &Request, endpoint: &Endpoint) -> (u16, Option<Value>) {
    if req.method != "POST" {
        return (405, None);
    }
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return (400, Some(json!({ "error": "invalid JSON body" })));
    };
    let cmd = body["cmd"].as_str().unwrap_or("");
    let reply = match dispatch(cmd, &body, endpoint) {
        Ok(v) => v,
        Err(e) => json!({ "error": e }),
    };
    (200, Some(reply))
}

fn str_field<'a>(body: &'a Value, key: &str) -> Result<&'a str, String> {
    body[key]
        .as_str()
        .ok_or_else(|| format!("missing string field {key}"))
}

fn opt_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body[key].as_str().filter(|s| !s.is_empty())
}

fn tab_fields(body: &Value) -> Result<(&str, &str, &str), String> {
    Ok((
        str_field(body, "host")?,
        str_field(body, "session")?,
        str_field(body, "window")?,
    ))
}

fn dispatch(cmd: &str, body: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    match cmd {
        "snapshot" => Ok(endpoint.shared.lock().snapshot(Instant::now())),
        "folders" => Ok(rpc_folders(body, endpoint)),
        "probes" => {
            let project = body["project"].as_str().unwrap_or("");
            Ok(endpoint.shared.lock().probes_reply(project, Instant::now()))
        }
        "refresh" => {
            let _ = endpoint.asks.refresh.send("refresh");
            Ok(json!({ "ok": true }))
        }
        "tasks" => {
            let list = if body["live"].as_bool().unwrap_or(false) {
                tasks::live_tasks()?
            } else {
                tasks::reconcile_tasks()?
            };
            Ok(json!({ "tasks": list }))
        }
        "transcript" => {
            let (host, session, window) = tab_fields(body)?;
            Ok(json!({ "result": transcript::fetch_on(host, session, window) }))
        }
        "changes" => {
            let (host, session, window) = tab_fields(body)?;
            Ok(json!({ "result": changes::fetch_on(host, session, window) }))
        }
        "task_dir" => {
            let (host, session, window) = tab_fields(body)?;
            Ok(json!({ "dir": changes::task_dir(host, session, window)? }))
        }
        "open" => {
            let (host, dir) = (str_field(body, "host")?, str_field(body, "dir")?);
            let (host, session) = folders::open_on(host, dir)?;
            Ok(json!({ "host": host, "session": session }))
        }
        "launch" => rpc_launch(body, endpoint),
        "act" | "archive_family" | "snooze" | "interrupt" | "switch" | "move" => {
            rpc_action(cmd, body, endpoint)
        }
        "message" => rpc_message(body),
        "unlink" => rpc_unlink(body, endpoint),
        _ => Err(format!("unknown rpc command: {cmd}")),
    }
}

fn rpc_folders(body: &Value, endpoint: &Endpoint) -> Value {
    let host = body["host"].as_str().unwrap_or("");
    let (reply, stale) = {
        let mut guard = endpoint.shared.lock();
        let (reply, stale) = guard.folders_reply(host, Instant::now(), FOLDERS_STALE);
        if stale && let Some(l) = guard.info.folders.get_mut(host) {
            l.wanted = true;
        }
        (reply, stale)
    };
    if stale {
        let _ = endpoint.asks.folders.send(());
    }
    reply
}

fn rpc_action(cmd: &str, body: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    let (result, should_refresh) = match cmd {
        "act" => {
            let action = str_field(body, "action")?;
            let (host, session, window) = tab_fields(body)?;
            (
                actions::act(action, host, session, window)?,
                action != "link",
            )
        }
        "archive_family" => {
            let action = str_field(body, "action")?;
            let (host, session, window) = tab_fields(body)?;
            (
                actions::archive_family(action, host, session, window)?,
                true,
            )
        }
        "snooze" => {
            let window = str_field(body, "window")?;
            let until = body["until"]
                .as_u64()
                .ok_or("missing integer field until")?;
            (actions::snooze(window, until)?, true)
        }
        "interrupt" => {
            let (host, session, window) = tab_fields(body)?;
            (actions::interrupt(host, session, window)?, false)
        }
        "switch" => {
            let (host, session, window) = tab_fields(body)?;
            let harness = str_field(body, "harness")?;
            (
                actions::switch_harness(host, session, window, harness)?,
                true,
            )
        }
        "move" => {
            let (host, session, window) = tab_fields(body)?;
            let target = str_field(body, "target")?;
            (actions::move_host(host, session, window, target)?, true)
        }
        _ => return Err(format!("unknown rpc command: {cmd}")),
    };
    if should_refresh {
        let _ = endpoint.asks.refresh.send("refresh");
    }
    Ok(json!({ "result": result }))
}

fn rpc_launch(body: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    let host = str_field(body, "host")?;
    let path = str_field(body, "path")?;
    let name = body["name"].as_str().unwrap_or("");
    let harness = body["harness"].as_str().unwrap_or("");
    let work_item = opt_str(body, "work_item");
    let (host, session, window) = if let Some(prompt_ref) = opt_str(body, "prompt_ref") {
        launch::launch(host, path, name, harness, Some(prompt_ref), work_item, None)?
    } else {
        let prompt = body["prompt"].as_str().unwrap_or("");
        let from_host = opt_str(body, "from_host");
        launch::launch_text_from(
            host,
            path,
            name,
            harness,
            prompt,
            from_host,
            work_item,
            launch::Placement::default(),
        )?
    };
    let _ = endpoint.asks.refresh.send("refresh");
    Ok(json!({ "host": host, "session": session, "window": window }))
}

fn rpc_message(body: &Value) -> Result<Value, String> {
    let host = str_field(body, "host")?;
    let session = str_field(body, "session")?;
    let window = str_field(body, "window")?;
    let result = if let Some(prompt_ref) = opt_str(body, "prompt_ref") {
        actions::message_ref(host, session, window, prompt_ref)?
    } else {
        let text = str_field(body, "text")?;
        if let Some(from) = opt_str(body, "from_host") {
            actions::message_from(from, host, session, window, text)?
        } else {
            actions::message(host, session, window, text)?
        }
    };
    Ok(json!({ "result": result }))
}

fn rpc_unlink(body: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    let host = str_field(body, "host")?;
    let worktree = str_field(body, "worktree")?;
    let refs: Vec<&str> = body["refs"]
        .as_array()
        .ok_or("missing array field refs")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let outcomes = tasks::unlink_task_refs_on(host, worktree, &refs)?;
    let _ = endpoint.asks.refresh.send("refresh");
    let names: Vec<&str> = outcomes
        .into_iter()
        .map(|o| match o {
            Unlinked::Removed => "removed",
            Unlinked::Derived => "derived",
            Unlinked::Absent => "absent",
        })
        .collect();
    Ok(json!({ "outcomes": names }))
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use switchyard_core::server;

    use crate::mcp::{Endpoint, accept_once};
    use crate::socket::Asks;
    use crate::state::{Shared, split_rows};

    fn test_server() -> (
        String,
        Arc<Shared>,
        mpsc::Receiver<&'static str>,
        mpsc::Receiver<()>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared::new());
        {
            let mut s = shared.lock();
            s.info.local = "omarchy".into();
            s.info.track("omarchy", &["omarchy".into()], &[]);
            s.info
                .folders
                .get_mut("omarchy")
                .unwrap()
                .set(Ok("/home/johna/dev\n".into()), Instant::now());
            s.set_hosts(&["local".into()]);
            s.set_rows(
                "local",
                split_rows("S\t-\tlocal\ts1\tmarvin\nT\tw1\tworking\tchat\t~\n"),
                Instant::now(),
            );
            s.set_tasks(Ok(vec![json!({"window_id": "w1"})]), true, Instant::now());
        }
        let (refresh_tx, refresh_rx) = mpsc::channel();
        let (folders_tx, folders_rx) = mpsc::channel();
        let endpoint = Endpoint {
            shared: shared.clone(),
            asks: Asks {
                refresh: refresh_tx,
                folders: folders_tx,
            },
            mcp_enabled: false,
        };
        thread::spawn(move || {
            for _ in 0..4 {
                if !accept_once(&listener, &endpoint) {
                    break;
                }
            }
        });
        (
            format!("http://127.0.0.1:{port}"),
            shared,
            refresh_rx,
            folders_rx,
        )
    }

    #[test]
    fn http_snapshot_and_rpc_round_trip() {
        let (url, _, refresh_rx, _) = test_server();
        let snap = server::snapshot_at(&url).unwrap();
        assert_eq!(snap["server_host"], "omarchy");
        assert_eq!(snap["tasks"][0]["window_id"], "w1");

        let folders = server::rpc_at(
            &url,
            &json!({ "cmd": "folders", "host": "omarchy" }),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(folders["folders"], "/home/johna/dev\n");

        let ok =
            server::rpc_at(&url, &json!({ "cmd": "refresh" }), Duration::from_secs(2)).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(
            refresh_rx.recv_timeout(Duration::from_secs(1)),
            Ok("refresh")
        );

        let err =
            server::rpc_at(&url, &json!({ "cmd": "nope" }), Duration::from_secs(2)).unwrap_err();
        assert!(err.contains("unknown rpc command"), "{err}");
    }

    #[test]
    fn api_routes_reject_wrong_methods_and_invalid_json() {
        let (refresh_tx, _) = mpsc::channel();
        let (folders_tx, _) = mpsc::channel();
        let ep = Endpoint {
            shared: Arc::new(Shared::new()),
            asks: Asks {
                refresh: refresh_tx,
                folders: folders_tx,
            },
            mcp_enabled: false,
        };
        let req = |method: &str, path: &str, body: &[u8]| {
            let mut r = crate::mcp::Request::default();
            r.method = method.into();
            r.path = path.into();
            r.body = body.to_vec();
            r
        };
        assert_eq!(
            super::route_snapshot(&req("POST", "/api/snapshot", b""), &ep).0,
            405
        );
        assert_eq!(super::route_rpc(&req("GET", "/api/rpc", b""), &ep).0, 405);
        assert_eq!(
            super::route_rpc(&req("POST", "/api/rpc", b"{not json"), &ep).0,
            400
        );
    }

    #[test]
    fn http_subscribe_streams_initial_snapshot_and_updates() {
        let (url, shared, _, _) = test_server();
        let mut sub = server::subscribe_at(&url).unwrap();
        let first = sub.next().unwrap().unwrap();
        assert_eq!(first["seq"], 0);
        assert_eq!(first["server_host"], "omarchy");

        shared.update(|s| {
            s.set_rows(
                "local",
                split_rows("S\t-\tlocal\ts1\tmarvin\nT\tw1\tidle\tchat\t~\n"),
                Instant::now(),
            )
        });
        let second = sub.next().unwrap().unwrap();
        assert_eq!(second["seq"], 1);
        assert!(
            second["hosts"][0]["rows"]
                .as_str()
                .unwrap()
                .contains("\tidle\t")
        );
    }
}
