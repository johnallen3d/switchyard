//! The MCP endpoint: Switchyard's task operations for agents on any tailnet
//! host. Only the config host (the Mac) can spawn, and nothing can reach it
//! but this: the endpoint listens on loopback and the Mac's Tailscale
//! address, and answers peers from loopback or the tailnet only. There is
//! no token; the tailnet is the boundary.
//!
//! Transport: MCP's Streamable HTTP, at `POST /mcp`, with every reply a
//! single JSON body (no SSE streams, so `GET /mcp` is 405) and no sessions.
//! A request with a browser `Origin` is refused, so a web page can't reach
//! it through DNS rebinding.
//!
//! Tools (see `tools`) wrap core: `list_tasks` (the snapshot's open tasks
//! with live agent status), `list_projects` (`hosts::catalog`), `spawn_task`
//! (`launch_text`), `read_task` (`transcript`), `message_task`
//! (`actions::message`), `message_parent` (`notices::send`, to the agent that
//! spawned the caller's task) and `stop_task` (a plain archive, never
//! force-archive). They name a task `PROJECT/NAME`, as the pickers label it.
//!
//! `spawn_task` records who spawned the task when it knows how to reach them:
//! - A Rex caller, from its `parent_session` / `parent_block` arguments, else
//!   from the request's `X-Rex-Session` / `X-Rex-Block` headers (which a
//!   client config fills from `REX_SESSION` / `REX_BLOCK`): nested under the
//!   caller's tab in the picker and notified there when it settles.
//! - A non-Rex caller, from `parent_url` / `X-Parent-Url` (and optional
//!   `parent_label` / `X-Parent-Label`): left top-level in the picker
//!   (`parent_window` unset) and notified by JSON `POST` to `parent_url`.
//!
//! Settle notices are sent unless `notify_parent` is false (see `notices`).
//! `message_parent` finds the caller's task from its `session` / `block`
//! arguments or Rex headers and delivers transparently to either parent type.
//! Empty values and unexpanded placeholders (`${REX_BLOCK}`) count as
//! absent.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use switchyard_core::hosts::{Project, catalog};
use switchyard_core::launch::{Parent, Placement, launch_text};
use switchyard_core::naming::{
    HARNESSES, default_harness, harness_spec, models, name_char, split_harness,
};
use switchyard_core::rex::local_label;
use switchyard_core::tasks::{in_window, live_tasks, load_tasks};
use switchyard_core::{actions, transcript};

use crate::api;
use crate::notices;
use crate::socket::Asks;
use crate::state::Shared;

/// Protocol revisions this endpoint speaks, newest first: it uses only
/// what they share (tools, plain JSON replies).
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The most a request's headers or body may hold.
const MAX_HEADERS: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;
/// How long a kept-alive connection may sit idle.
const IDLE: Duration = Duration::from_secs(120);
/// How often to look for the Tailscale address until it's there.
const TAILSCALE_RETRY: Duration = Duration::from_secs(30);
/// Transcript entries `read_task` returns by default, and at most.
const READ_DEFAULT: usize = 20;
const READ_MAX: usize = 150;

const INSTRUCTIONS: &str = "Switchyard runs agent tasks: each is an agent (claude, pi or agy, the Antigravity CLI) in a Rex tab, \
working in a Git worktree of a catalog project on one of John's hosts. Tasks are named PROJECT/NAME. \
list_tasks shows the open ones with live status (working, idle, blocked on a prompt, scheduled to resume itself by a timer or background job, at wake_at when known, ...). \
list_projects shows the catalog projects you can spawn into; \
spawn_task starts a task with a first prompt; read_task shows what its agent has been saying; \
message_task types text into its tab; stop_task archives it (refused while it has uncommitted changes). \
A task spawned with your parent_block or parent_url tells you when its agent stops working (typed into your Rex tab, or POSTed as JSON to parent_url), so wait for that rather than polling; \
a spawned agent can write to its spawner with message_parent.";

#[derive(Clone)]
pub struct Endpoint {
    pub shared: Arc<Shared>,
    /// Background work channels (task refresh, zoxide folder refresh).
    pub asks: Asks,
    /// Whether `POST /mcp` is served (remote `/api/*` is always served).
    pub mcp_enabled: bool,
}

/// Listen on loopback now and on the Tailscale address once there is one.
pub fn start(endpoint: Endpoint, port: u16) {
    let label = if endpoint.mcp_enabled {
        "http/mcp"
    } else {
        "http"
    };
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => {
            crate::log(&format!("{label}: listening on http://127.0.0.1:{port}"));
            let endpoint = endpoint.clone();
            thread::spawn(move || accept(&l, &endpoint));
        }
        Err(e) => crate::log(&format!("{label}: 127.0.0.1:{port}: {e}")),
    }
    thread::spawn(move || {
        loop {
            if let Some(ip) = tailscale_ipv4() {
                match TcpListener::bind((ip, port)) {
                    Ok(l) => {
                        crate::log(&format!("{label}: listening on http://{ip}:{port}"));
                        return accept(&l, &endpoint);
                    }
                    Err(e) => crate::log(&format!("{label}: {ip}:{port}: {e}")),
                }
            }
            thread::sleep(TAILSCALE_RETRY);
        }
    });
}

/// This machine's Tailscale IPv4 address: the interface address in
/// 100.64.0.0/10 (Tailscale's CGNAT range).
fn tailscale_ipv4() -> Option<Ipv4Addr> {
    let mut found = None;
    // SAFETY: getifaddrs fills a list freed below with freeifaddrs; each
    // address is read as sockaddr_in only when its family says AF_INET.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&raw mut list) != 0 {
            return None;
        }
        let mut cur = list;
        while !cur.is_null() {
            let addr = (*cur).ifa_addr;
            if !addr.is_null() && i32::from((*addr).sa_family) == libc::AF_INET {
                // ifa_addr is typed as the 2-byte-aligned sockaddr, so read
                // the sockaddr_in it points to without assuming alignment.
                let sin = addr.cast::<libc::sockaddr_in>().read_unaligned();
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if in_tailnet(IpAddr::V4(ip)) {
                    found = Some(ip);
                    break;
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    found
}

/// Tailscale's address ranges: 100.64.0.0/10 and `fd7a:115c:a1e0::/48`.
pub(crate) fn in_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// Whether `ip` is loopback or on the tailnet (unwrapping IPv4-mapped IPv6).
pub(crate) fn allowed_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip @ IpAddr::V4(_) => ip,
    };
    ip.is_loopback() || in_tailnet(ip)
}

fn allowed_peer(peer: SocketAddr) -> bool {
    allowed_ip(peer.ip())
}

fn accept(listener: &TcpListener, endpoint: &Endpoint) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let endpoint = endpoint.clone();
                thread::spawn(move || {
                    let _ = connection(stream, &endpoint);
                });
            }
            Err(e) => crate::log(&format!("mcp: {e}")),
        }
    }
}

#[cfg(test)]
pub(crate) fn accept_once(listener: &TcpListener, endpoint: &Endpoint) -> bool {
    match listener.accept() {
        Ok((stream, _)) => {
            let endpoint = endpoint.clone();
            thread::spawn(move || {
                let _ = connection(stream, &endpoint);
            });
            true
        }
        Err(_) => false,
    }
}

/// One HTTP request, as far as this endpoint reads it.
#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub origin: Option<String>,
    caller: Caller,
    close: bool,
    pub body: Vec<u8>,
}

/// Who the caller says it is, from its request headers (Rex location and/or
/// non-Rex callback URL + label).
#[derive(Debug, Default, Clone, PartialEq)]
struct Caller {
    session: Option<String>,
    block: Option<String>,
    parent_url: Option<String>,
    parent_label: Option<String>,
}

/// A Rex location value, or None for an empty one or a placeholder the
/// client didn't expand (`${REX_BLOCK}` when the variable is unset).
fn location(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && !value.starts_with('$')).then_some(value)
}

/// Read the next request; None when the client has hung up between
/// requests. Err(status) for one this endpoint won't read.
fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Result<Request, u16>>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (method, target, version) = (parts.next(), parts.next(), parts.next());
    let (Some(method), Some(target), Some(version)) = (method, target, version) else {
        return Ok(Some(Err(400)));
    };
    let mut req = Request {
        method: method.to_string(),
        path: target.split('?').next().unwrap_or("").to_string(),
        close: version == "HTTP/1.0",
        ..Request::default()
    };
    let (mut length, mut size) = (0usize, line.len());
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        size += header.len();
        if size > MAX_HEADERS {
            return Ok(Some(Err(431)));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => match value.parse() {
                Ok(n) => length = n,
                Err(_) => return Ok(Some(Err(400))),
            },
            "transfer-encoding" => return Ok(Some(Err(411))),
            "origin" => req.origin = Some(value.to_string()),
            "x-rex-session" => req.caller.session = location(value).map(String::from),
            "x-rex-block" => req.caller.block = location(value).map(String::from),
            "x-parent-url" => req.caller.parent_url = location(value).map(String::from),
            "x-parent-label" => req.caller.parent_label = location(value).map(String::from),
            "connection" => match value.to_ascii_lowercase().as_str() {
                "close" => req.close = true,
                "keep-alive" => req.close = false,
                _ => {}
            },
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Ok(Some(Err(413)));
    }
    req.body = vec![0; length];
    reader.read_exact(&mut req.body)?;
    Ok(Some(Ok(req)))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

fn respond(out: &mut impl Write, status: u16, body: Option<&Value>, close: bool) -> io::Result<()> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\n",
        reason(status),
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if status == 405 {
        head.push_str("Allow: POST\r\n");
    }
    head.push_str(if close {
        "Connection: close\r\n\r\n"
    } else {
        "Connection: keep-alive\r\n\r\n"
    });
    out.write_all(head.as_bytes())?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

fn connection(stream: TcpStream, endpoint: &Endpoint) -> io::Result<()> {
    let peer = stream.peer_addr()?;
    if !allowed_peer(peer) {
        crate::log(&format!("mcp: refused {peer}: not on the tailnet"));
        return Ok(());
    }
    stream.set_read_timeout(Some(IDLE))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some(req) = read_request(&mut reader)? {
        let req = match req {
            Ok(req) => req,
            Err(status) => return respond(&mut out, status, None, true),
        };
        if req.path == "/api/subscribe" {
            if req.origin.is_some() {
                return respond(
                    &mut out,
                    403,
                    Some(&json!({ "error": "browser requests are refused" })),
                    true,
                );
            }
            if req.method != "GET" {
                return respond(&mut out, 405, None, true);
            }
            return api::subscribe(&mut out, &endpoint.shared);
        }
        let (status, body) = route(&req, endpoint);
        respond(&mut out, status, body.as_ref(), req.close)?;
        if req.close {
            break;
        }
    }
    Ok(())
}

fn route(req: &Request, endpoint: &Endpoint) -> (u16, Option<Value>) {
    if req.origin.is_some() {
        return (
            403,
            Some(rpc_error(
                &Value::Null,
                -32600,
                "browser requests are refused",
            )),
        );
    }
    match req.path.as_str() {
        "/api/snapshot" => return api::route_snapshot(req, endpoint),
        "/api/rpc" => return api::route_rpc(req, endpoint),
        "/mcp" if endpoint.mcp_enabled => {}
        _ => return (404, None),
    }
    if req.method != "POST" {
        return (405, None);
    }
    let Ok(message) = serde_json::from_slice::<Value>(&req.body) else {
        return (400, Some(rpc_error(&Value::Null, -32700, "parse error")));
    };
    // Batches came and went between revisions; answering them costs little.
    let replies: Vec<Value> = match &message {
        Value::Array(batch) => batch
            .iter()
            .filter_map(|m| handle(m, &req.caller, endpoint))
            .collect(),
        m => handle(m, &req.caller, endpoint).into_iter().collect(),
    };
    match (message.is_array(), replies.len()) {
        (_, 0) => (202, None),
        (false, _) => (200, replies.into_iter().next()),
        (true, _) => (200, Some(Value::Array(replies))),
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The reply to one JSON-RPC message; None for notifications and responses.
fn handle(message: &Value, caller: &Caller, endpoint: &Endpoint) -> Option<Value> {
    let Some(method) = message["method"].as_str() else {
        // A response to something this server never asks, or junk.
        return message
            .get("id")
            .is_none()
            .then(|| rpc_error(&Value::Null, -32600, "invalid request"));
    };
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "switchyard", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            let started = Instant::now();
            let outcome = call(name, args, caller, endpoint);
            let took = started.elapsed().as_millis();
            match &outcome {
                Ok(_) => crate::log(&format!("mcp: {name} ok ({took} ms)")),
                Err(e) => crate::log(&format!("mcp: {name} failed ({took} ms): {e}")),
            }
            match outcome {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "structuredContent": value,
                    "isError": false,
                }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            }
        }
        _ => return Some(rpc_error(&id, -32601, &format!("unknown method {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    let task =
        json!({ "type": "string", "description": "The task, as PROJECT/NAME (see list_tasks)." });
    json!([
        {
            "name": "list_tasks",
            "description": "List the open agent tasks with each agent's live status.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "list_projects",
            "description": "List the configured catalog projects that spawn_task accepts.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "spawn_task",
            "description": "Start an agent task: a new worktree branch of a catalog project, \
    with an agent working on PROMPT in its own Rex tab. Returns the task's PROJECT/NAME.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Catalog project: its name (the folder name) or path (see list_projects)." },
                    "name": { "type": "string", "description": "Task name, normalised as the picker does (lowercase, spaces become dashes); the branch is feature/NAME." },
                    "harness": { "type": "string", "enum": HARNESSES, "description": "Omit for the project's profile default (the config's [harnesses], else claude)." },
                    "model": { "type": "string", "description": model_help() },
                    "prompt": { "type": "string", "description": "The agent's first message." },
                    "host": { "type": "string", "description": "Rex host label, or auto (the default) for the project's least busy host." },
                    "work_item": { "type": "string", "description": "Optional issue or card the work is for (a ref or URL)." },
                    "parent_session": { "type": "string", "description": "Your own Rex session: the REX_SESSION environment variable, if set. With parent_block, the new task is shown under yours in the tab picker." },
                    "parent_block": { "type": "string", "description": "Your own Rex pane: the REX_BLOCK environment variable, if set. Cannot be combined with an explicit parent_url." },
                    "parent_url": { "type": "string", "description": "Optional HTTP callback URL (http:// to loopback or tailnet only) for non-Rex callers to receive settle notices and message_parent JSON POSTs. Cannot be combined with an explicit parent_block; overrides X-Parent-Url and ignores Rex headers." },
                    "parent_label": { "type": "string", "description": "Optional human-readable label for a callback parent (e.g. \"marvin\" or \"Marvin #42\"), exposed in list_tasks/read_task summaries. Overrides X-Parent-Label." },
                    "notify_parent": { "type": "boolean", "default": true, "description": "With parent_block or parent_url known, notify the parent each time the task's agent stops working (idle, done, or waiting on a prompt), with its last message, so you needn't poll it or set a timer." },
                },
                "required": ["project", "name", "prompt"],
            },
        },
        {
            "name": "read_task",
            "description": "What a task's agent has been saying: its live status and the last entries of its \
    transcript (user and assistant messages, tool calls, questions). An agent just spawned may have \
    no transcript yet (transcript_error says why).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": task,
                    "entries": { "type": "integer", "minimum": 1, "maximum": READ_MAX, "default": READ_DEFAULT },
                },
                "required": ["task"],
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "message_task",
            "description": "Send text to a task's agent as if typed in its tab: an idle agent starts a turn, \
    a working one takes it in at its next chance. Refused while the agent waits on a prompt.",
            "inputSchema": {
                "type": "object",
                "properties": { "task": task, "text": { "type": "string" } },
                "required": ["task", "text"],
            },
        },
        {
            "name": "message_parent",
            "description": "From a spawned task: send text to its spawner (typed into a Rex parent's tab as \
    message_task does, or POSTed as JSON to its parent_url callback) to report progress or ask it \
    something without ending your turn. Refused while a Rex parent waits on a prompt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "session": { "type": "string", "description": "Your own Rex session: the REX_SESSION environment variable." },
                    "block": { "type": "string", "description": "Your own Rex pane: the REX_BLOCK environment variable." },
                },
                "required": ["text"],
            },
        },
        {
            "name": "stop_task",
            "description": "Archive a task: close its tab and remove its worktree. Refused while the worktree \
    has uncommitted changes.",
            "inputSchema": { "type": "object", "properties": { "task": task }, "required": ["task"] },
            "annotations": { "destructiveHint": true },
        },
    ])
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn opt_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn label(task: &Value) -> String {
    format!(
        "{}/{}",
        task["project"].as_str().unwrap_or(""),
        task["name"].as_str().unwrap_or("")
    )
}

fn call(name: &str, args: &Value, caller: &Caller, endpoint: &Endpoint) -> Result<Value, String> {
    match name {
        "list_tasks" => {
            Ok(json!({ "tasks": open_tasks(endpoint)?.iter().map(summary).collect::<Vec<_>>() }))
        }
        "list_projects" => Ok(projects_list(&catalog()?, default_harness)),
        "spawn_task" => spawn(args, caller, endpoint),
        "read_task" => {
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let wanted = args["entries"].as_u64().map_or(READ_DEFAULT, |n| {
                usize::try_from(n).unwrap_or(usize::MAX).clamp(1, READ_MAX)
            });
            let (host, session, window) = tab(&task);
            let mut log = transcript::fetch_on(host, session, window);
            let mut out = summary(&task);
            // The task is there even when its log isn't (yet: an agent just
            // spawned writes none until it starts), so a poller still gets
            // its status.
            if let Some(entries) = log["entries"].as_array_mut() {
                let skip = entries.len().saturating_sub(wanted);
                out["transcript"] = entries.drain(skip..).collect::<Vec<_>>().into();
            } else {
                out["transcript"] = json!([]);
                out["transcript_error"] = log["error"].take();
            }
            Ok(out)
        }
        "message_task" => {
            let text = str_arg(args, "text")?;
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let (host, session, window) = tab(&task);
            actions::message(host, session, window, text).map(|msg| json!({ "message": msg }))
        }
        "message_parent" => message_parent(args, caller, endpoint),
        "stop_task" => {
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let (host, session, window) = tab(&task);
            let result = actions::act("archive", host, session, window);
            let _ = endpoint.asks.refresh.send("mcp");
            result.map(|msg| json!({ "message": msg }))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Send text to the agent that spawned the caller's task, found from the
/// caller's block.
fn message_parent(args: &Value, caller: &Caller, endpoint: &Endpoint) -> Result<Value, String> {
    let text = str_arg(args, "text")?;
    let pick = |key: &str, header: &Option<String>| {
        args[key]
            .as_str()
            .and_then(location)
            .or_else(|| header.as_deref().and_then(location))
            .map(String::from)
    };
    let session = pick("session", &caller.session);
    let block = pick("block", &caller.block).ok_or("block is required: pass your REX_BLOCK")?;
    let tab = caller_tab(endpoint, session.as_deref(), Some(&block))
        .ok_or_else(|| format!("no Rex server has block {block}"))?;
    let task = load_tasks()?
        .into_iter()
        .find(|t| t["ended_at"].is_null() && in_window(t, &tab.window))
        .ok_or("your tab isn't an open Switchyard task")?;
    notices::send(&task, text).map(|msg| json!({ "message": msg }))
}

fn tab(task: &Value) -> (&str, &str, &str) {
    let f = |k: &str| task[k].as_str().unwrap_or("");
    (f("host"), f("session_id"), f("window_id"))
}

/// The open tasks with live status: the snapshot's, or a direct look
/// before the server's first check.
fn open_tasks(endpoint: &Endpoint) -> Result<Vec<Value>, String> {
    let snap = endpoint.shared.lock().snapshot(Instant::now());
    let tasks = match snap["tasks"].as_array() {
        Some(tasks) => tasks.clone(),
        None => live_tasks()?,
    };
    Ok(tasks
        .into_iter()
        .filter(|t| t["ended_at"].is_null())
        .collect())
}

/// What a caller needs of a task record.
fn summary(task: &Value) -> Value {
    let mut out = json!({ "task": label(task) });
    for key in [
        "project",
        "name",
        "title",
        "host",
        "harness",
        "model",
        "branch",
        "worktree",
        "status",
        "status_msg",
        "status_at",
        "wake_at",
        "created_at",
        "parent_label",
        "links",
    ] {
        if let Some(v) = task.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }
    out
}

/// Response payload for `list_projects`.
fn projects_list(projects: &[Project], harness: impl Fn(&str) -> String) -> Value {
    json!({
        "projects": projects
            .iter()
            .map(|p| project_summary(p, &harness(&p.profile)))
            .collect::<Vec<_>>()
    })
}

/// What a caller needs of a catalog project.
fn project_summary(project: &Project, default_harness: &str) -> Value {
    let mut out = json!({
        "name": project.name,
        "path": project.path,
        "profile": project.profile,
        "default_harness": default_harness,
    });
    if !project.hosts.is_empty() {
        out["hosts"] = json!(project.hosts);
    }
    if !project.tracker.is_empty() {
        out["tracker"] = json!(project.tracker);
    }
    out
}

/// The open task `wanted` names (see `find_task`): from the snapshot, for
/// its live status, else from the registry, which has a task just spawned
/// before the server's next check does.
fn find_open(wanted: &str, endpoint: &Endpoint) -> Result<Value, String> {
    find_task(wanted, &open_tasks(endpoint)?).or_else(|e| {
        let registry: Vec<Value> = load_tasks()?
            .into_iter()
            .filter(|t| t["ended_at"].is_null())
            .collect();
        find_task(wanted, &registry).map_err(|_| e)
    })
}

/// The open task named `PROJECT/NAME`, or a bare NAME only one has.
fn find_task(wanted: &str, open: &[Value]) -> Result<Value, String> {
    let matches: Vec<&Value> = match wanted.split_once('/') {
        Some(_) => open.iter().filter(|t| label(t) == wanted).collect(),
        None => open
            .iter()
            .filter(|t| t["name"].as_str() == Some(wanted))
            .collect(),
    };
    match matches.as_slice() {
        [task] => Ok((*task).clone()),
        [] => Err(format!("no open task {wanted} (see list_tasks)")),
        _ => Err(format!(
            "{wanted} names {} open tasks; give PROJECT/NAME",
            matches.len()
        )),
    }
}

/// Where a Rex block is: its host's Rex label, session and window.
#[derive(Clone, Debug, PartialEq)]
struct Tab {
    host: String,
    session: String,
    window: String,
}

/// Where `block` is, from the first of `hosts` (Rex server names, `local`
/// for this one) that has it.
fn block_window(hosts: &[String], block: &str) -> Option<Tab> {
    hosts.iter().find_map(|host| {
        let mut cmd = switchyard_core::rex::rex_command();
        if host != "local" {
            cmd.args(["-S", host]);
        }
        let out = cmd
            .args(["--timeout", "3s", "block", "inspect", block, "--json"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let info: Value = serde_json::from_slice(&out.stdout).ok()?;
        let window = info["block"]["window_id"]
            .as_str()
            .filter(|w| !w.is_empty())?;
        Some(Tab {
            host: if host == "local" {
                local_label()
            } else {
                host.clone()
            },
            session: info["session_id"].as_str().unwrap_or("").to_string(),
            window: window.to_string(),
        })
    })
}

/// Where the caller's block is, from the given session and block (see
/// `parent_location`).
fn caller_tab(endpoint: &Endpoint, session: Option<&str>, block: Option<&str>) -> Option<Tab> {
    let block = block?;
    let hosts = endpoint
        .shared
        .lock()
        .hosts_for_session(session.unwrap_or(""));
    let found = block_window(&hosts, block);
    if found.is_none() {
        crate::log(&format!("mcp: no Rex server has block {block}"));
    }
    found
}

/// How `spawn_task` should record the caller as parent.
#[derive(Debug, PartialEq, Eq)]
enum SpawnParent<'a> {
    None,
    Rex {
        session: Option<&'a str>,
        block: &'a str,
    },
    Callback {
        url: &'a str,
        label: Option<&'a str>,
    },
}

/// Resolve the spawner's parent from tool arguments and request headers.
///
/// Explicit tool arguments take precedence over headers. Passing both an
/// explicit `parent_block` and an explicit `parent_url` is rejected. When a
/// callback `parent_url` is selected (by argument or `X-Parent-Url` header),
/// Rex headers are ignored so `parent_window` stays unset.
fn resolve_spawn_parent<'a>(
    args: &'a Value,
    caller: &'a Caller,
) -> Result<SpawnParent<'a>, String> {
    let explicit_block = args["parent_block"].as_str().and_then(location);
    let explicit_url = args["parent_url"].as_str().and_then(location);
    if explicit_block.is_some() && explicit_url.is_some() {
        return Err("cannot set both parent_block and parent_url".into());
    }
    let header_block = caller.block.as_deref().and_then(location);
    let header_url = caller.parent_url.as_deref().and_then(location);
    let label = args["parent_label"]
        .as_str()
        .and_then(location)
        .or_else(|| caller.parent_label.as_deref().and_then(location));
    let session = || {
        args["parent_session"]
            .as_str()
            .and_then(location)
            .or_else(|| caller.session.as_deref().and_then(location))
    };

    if let Some(url) = explicit_url {
        return Ok(SpawnParent::Callback { url, label });
    }
    if let Some(block) = explicit_block {
        return Ok(SpawnParent::Rex {
            session: session(),
            block,
        });
    }
    if let Some(url) = header_url {
        return Ok(SpawnParent::Callback { url, label });
    }
    if let Some(block) = header_block {
        return Ok(SpawnParent::Rex {
            session: session(),
            block,
        });
    }
    Ok(SpawnParent::None)
}

fn normalise(name: &str) -> String {
    name.chars().filter_map(name_char).collect()
}

/// `spawn_task`'s model argument, listing each harness's models.
fn model_help() -> String {
    let list = |h| models(h)[1..].join(", ");
    format!(
        "Optional model for the harness; omit for its default (with no harness, the profile default's; a model alone picks the harness that lists it). claude: {}. pi: {}. agy: {}.",
        list("claude"),
        list("pi"),
        list("agy")
    )
}

/// `spawn_task`'s harness spec: the harness and model asked for, with the
/// project's profile default (`default`) filling in what's left out. A
/// model asked for without a harness goes with the default's harness if
/// that lists it, else the first harness that does.
fn spawn_harness(harness: Option<&str>, model: Option<&str>, default: &str) -> String {
    match (harness, model) {
        (Some(harness), model) => harness_spec(harness, model),
        (None, Some(model)) => {
            let default = split_harness(default).0;
            let harness = std::iter::once(default)
                .chain(HARNESSES)
                .find(|h| models(h).contains(&model))
                .unwrap_or(default);
            harness_spec(harness, Some(model))
        }
        (None, None) => default.to_string(),
    }
}

fn spawn(args: &Value, caller: &Caller, endpoint: &Endpoint) -> Result<Value, String> {
    let project = str_arg(args, "project")?;
    // As the picker takes typed names: `MCP smoke test` is `mcp-smoke-test`.
    let name = normalise(str_arg(args, "name")?);
    let name = name.as_str();
    let prompt = str_arg(args, "prompt")?;
    let host = opt_arg(args, "host").unwrap_or("auto");
    // Validate parent arguments before touching the catalog or opening a tab.
    let spawn_parent = resolve_spawn_parent(args, caller)?;
    if let SpawnParent::Callback { url, .. } = spawn_parent {
        notices::validate_callback_url(url)?;
    }
    // Catalog projects only: a folder outside it would need its host's
    // checkout, which a remote caller can't see.
    let projects = catalog()?;
    let found = projects
        .iter()
        .find(|p| p.name == project || p.path == project)
        .ok_or_else(|| format!("{project} is not a catalog project (see list_projects)"))?;
    let harness = spawn_harness(
        opt_arg(args, "harness"),
        opt_arg(args, "model"),
        &default_harness(&found.profile),
    );
    let harness = harness.as_str();
    let notify = args["notify_parent"].as_bool().unwrap_or(true);
    let parent_tab;
    let parent = match spawn_parent {
        SpawnParent::None => None,
        SpawnParent::Rex { session, block } => {
            // The spawner's tab, so the picker can nest the task under it. Not
            // finding it only costs the nesting.
            parent_tab = caller_tab(endpoint, session, Some(block));
            parent_tab.as_ref().map(|tab| Parent::Rex {
                host: &tab.host,
                session: &tab.session,
                window: &tab.window,
                notify,
            })
        }
        SpawnParent::Callback { url, label } => Some(Parent::Callback { url, label, notify }),
    };
    let (host, session, window) = launch_text(
        host,
        &found.path,
        name,
        harness,
        prompt,
        opt_arg(args, "work_item"),
        Placement { parent },
    )?;
    let _ = endpoint.asks.refresh.send("mcp");
    // The registry has it now: name it as list_tasks will.
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks
                .iter()
                .rev()
                .find(|t| t["ended_at"].is_null() && in_window(t, &window))
                .map(label)
        })
        .unwrap_or_else(|| format!("{}/{name}", found.name));
    Ok(json!({ "task": label, "host": host, "session_id": session, "window_id": window }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn spawn_harness_fills_in_the_profile_default() {
        let d = "agy:gemini-4-neon";
        assert_eq!(super::spawn_harness(None, None, d), d);
        assert_eq!(super::spawn_harness(Some("claude"), None, d), "claude");
        assert_eq!(super::spawn_harness(Some("pi"), Some("x"), d), "pi:x");
        assert_eq!(super::spawn_harness(None, Some("opus"), d), "claude:opus");
        assert_eq!(
            super::spawn_harness(None, Some("gemini-4-neon-xhigh"), d),
            "agy:gemini-4-neon-xhigh"
        );
    }

    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    fn endpoint() -> Endpoint {
        let (refresh, _) = mpsc::channel();
        let (folders, _) = mpsc::channel();
        Endpoint {
            shared: Arc::new(Shared::new()),
            asks: Asks { refresh, folders },
            mcp_enabled: true,
        }
    }

    fn post(body: &str) -> Request {
        Request {
            method: "POST".into(),
            path: "/mcp".into(),
            body: body.as_bytes().to_vec(),
            ..Request::default()
        }
    }

    #[test]
    fn tailnet_and_loopback_peers_only() {
        let ok = |s: &str| allowed_peer(s.parse().unwrap());
        assert!(ok("127.0.0.1:1"));
        assert!(ok("100.95.69.110:1"));
        assert!(ok("[fd7a:115c:a1e0::1]:1"));
        assert!(ok("[::ffff:100.64.0.1]:1"));
        assert!(!ok("100.128.0.1:1"));
        assert!(!ok("192.168.1.10:1"));
        assert!(!ok("[2001:db8::1]:1"));
    }

    #[test]
    fn reads_a_request_and_its_body() {
        let raw = "POST /mcp?x=1 HTTP/1.1\r\nHost: mac\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        let req = read_request(&mut Cursor::new(raw))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            (req.method.as_str(), req.path.as_str(), req.close),
            ("POST", "/mcp", true)
        );
        assert_eq!(req.body, b"{}");
        let chunked = "POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            read_request(&mut Cursor::new(chunked))
                .unwrap()
                .unwrap()
                .unwrap_err(),
            411
        );
        assert!(read_request(&mut Cursor::new("")).unwrap().is_none());
    }

    #[test]
    fn reads_the_callers_rex_and_callback_headers() {
        let raw = "POST /mcp HTTP/1.1\r\nx-rex-session: session:a\r\nX-REX-BLOCK:  block:b \r\nX-Parent-Url: http://127.0.0.1:8080/cb \r\nX-Parent-Label: Marvin #42\r\nContent-Length: 0\r\n\r\n";
        let req = read_request(&mut Cursor::new(raw))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            req.caller,
            Caller {
                session: Some("session:a".into()),
                block: Some("block:b".into()),
                parent_url: Some("http://127.0.0.1:8080/cb".into()),
                parent_label: Some("Marvin #42".into()),
            }
        );
        // An unset variable leaves an empty value or the literal placeholder.
        let raw = "POST /mcp HTTP/1.1\r\nX-Rex-Session: ${REX_SESSION}\r\nX-Rex-Block: \r\nX-Parent-Url: ${PARENT_URL}\r\nX-Parent-Label:  \r\n\r\n";
        let req = read_request(&mut Cursor::new(raw))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(req.caller, Caller::default());
    }

    #[test]
    fn spawn_parent_precedence_and_conflict_rules() {
        let rex_headers = Caller {
            session: Some("session:h".into()),
            block: Some("block:h".into()),
            ..Caller::default()
        };
        let cb_headers = Caller {
            parent_url: Some("http://127.0.0.1:8080/hdr".into()),
            parent_label: Some("marvin".into()),
            ..Caller::default()
        };
        let none = json!({});

        // Rex headers fallback & tool arg override
        assert_eq!(
            resolve_spawn_parent(&none, &rex_headers).unwrap(),
            SpawnParent::Rex {
                session: Some("session:h"),
                block: "block:h",
            }
        );
        let both_rex = json!({ "parent_session": "session:a", "parent_block": "block:a" });
        assert_eq!(
            resolve_spawn_parent(&both_rex, &rex_headers).unwrap(),
            SpawnParent::Rex {
                session: Some("session:a"),
                block: "block:a",
            }
        );
        let blank_rex = json!({ "parent_session": " ", "parent_block": "${REX_BLOCK}" });
        assert_eq!(
            resolve_spawn_parent(&blank_rex, &rex_headers).unwrap(),
            SpawnParent::Rex {
                session: Some("session:h"),
                block: "block:h",
            }
        );

        // Callback header fallback & tool arg override
        assert_eq!(
            resolve_spawn_parent(&none, &cb_headers).unwrap(),
            SpawnParent::Callback {
                url: "http://127.0.0.1:8080/hdr",
                label: Some("marvin"),
            }
        );
        let arg_cb = json!({
            "parent_url": "http://127.0.0.1:9090/arg",
            "parent_label": "Marvin #42",
        });
        assert_eq!(
            resolve_spawn_parent(&arg_cb, &cb_headers).unwrap(),
            SpawnParent::Callback {
                url: "http://127.0.0.1:9090/arg",
                label: Some("Marvin #42"),
            }
        );

        // Explicit parent_url ignores Rex headers
        assert_eq!(
            resolve_spawn_parent(&arg_cb, &rex_headers).unwrap(),
            SpawnParent::Callback {
                url: "http://127.0.0.1:9090/arg",
                label: Some("Marvin #42"),
            }
        );
        // Header X-Parent-Url ignores Rex headers too
        let mixed_headers = Caller {
            session: Some("session:h".into()),
            block: Some("block:h".into()),
            parent_url: Some("http://127.0.0.1:8080/hdr".into()),
            parent_label: Some("marvin".into()),
        };
        assert_eq!(
            resolve_spawn_parent(&none, &mixed_headers).unwrap(),
            SpawnParent::Callback {
                url: "http://127.0.0.1:8080/hdr",
                label: Some("marvin"),
            }
        );

        // Passing both explicit parent_block and parent_url is rejected
        let conflict = json!({
            "parent_block": "block:a",
            "parent_url": "http://127.0.0.1:8080/cb",
        });
        assert!(resolve_spawn_parent(&conflict, &Caller::default()).is_err());
        assert_eq!(
            resolve_spawn_parent(&none, &Caller::default()).unwrap(),
            SpawnParent::None
        );
    }

    #[test]
    fn summary_exposes_parent_label_when_set_and_hides_parent_url() {
        let with_cb = json!({
            "project": "switchyard",
            "name": "cb",
            "parent_url": "http://127.0.0.1:8080/cb",
            "parent_label": "Marvin #42",
        });
        let s = summary(&with_cb);
        assert_eq!(s["parent_label"], "Marvin #42");
        assert!(s.get("parent_url").is_none());

        let without = json!({ "project": "switchyard", "name": "plain" });
        assert!(summary(&without).get("parent_label").is_none());
    }

    #[test]
    fn spawn_task_rejects_bad_parent_url_and_conflicting_parent_args() {
        let e = endpoint();
        let bad_url = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "spawn_task",
                "arguments": {
                    "project": "switchyard",
                    "name": "test",
                    "prompt": "hi",
                    "parent_url": "https://127.0.0.1:8080/cb",
                }
            }
        });
        let reply = route(&post(&bad_url.to_string()), &e).1.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("http://")
        );

        let conflict = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "spawn_task",
                "arguments": {
                    "project": "switchyard",
                    "name": "test",
                    "prompt": "hi",
                    "parent_block": "block:1",
                    "parent_url": "http://127.0.0.1:8080/cb",
                }
            }
        });
        let reply = route(&post(&conflict.to_string()), &e).1.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("both parent_block and parent_url")
        );
    }

    #[test]
    fn initializes_with_a_version_both_speak() {
        let e = endpoint();
        let init = |v: &str| {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": v, "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } });
            route(&post(&body.to_string()), &e).1.unwrap()["result"]["protocolVersion"].clone()
        };
        assert_eq!(init("2025-06-18"), "2025-06-18");
        assert_eq!(init("1999-01-01"), VERSIONS[0]);
    }

    #[test]
    fn notifications_get_202_and_unknown_methods_an_error() {
        let e = endpoint();
        let (status, body) = route(
            &post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            &e,
        );
        assert_eq!((status, body), (202, None));
        let (status, body) = route(&post(r#"{"jsonrpc":"2.0","id":"a","method":"nope"}"#), &e);
        assert_eq!(status, 200);
        assert_eq!(body.unwrap()["error"]["code"], -32601);
        assert_eq!(route(&post("not json"), &e).0, 400);
    }

    #[test]
    fn refuses_browsers_other_paths_and_methods() {
        let e = endpoint();
        let mut req = post("{}");
        req.origin = Some("http://evil.example".into());
        assert_eq!(route(&req, &e).0, 403);
        let get = Request {
            method: "GET".into(),
            path: "/mcp".into(),
            ..Request::default()
        };
        assert_eq!(route(&get, &e).0, 405);
        let other = Request {
            method: "POST".into(),
            path: "/".into(),
            ..Request::default()
        };
        assert_eq!(route(&other, &e).0, 404);
    }

    #[test]
    fn lists_seven_tools() {
        let e = endpoint();
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let reply = route(&post(&body.to_string()), &e).1.unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "list_tasks",
                "list_projects",
                "spawn_task",
                "read_task",
                "message_task",
                "message_parent",
                "stop_task"
            ]
        );
        let list_projects = tools.iter().find(|t| t["name"] == "list_projects").unwrap();
        assert_eq!(list_projects["annotations"]["readOnlyHint"], true);
        assert_eq!(
            list_projects["inputSchema"],
            json!({ "type": "object", "properties": {} })
        );
        let spawn = tools.iter().find(|t| t["name"] == "spawn_task").unwrap();
        assert!(
            spawn["inputSchema"]["properties"]["project"]["description"]
                .as_str()
                .unwrap()
                .contains("list_projects")
        );
        assert!(spawn["inputSchema"]["properties"]["parent_url"].is_object());
        assert!(spawn["inputSchema"]["properties"]["parent_label"].is_object());
        assert!(INSTRUCTIONS.contains("list_projects"));
        assert!(INSTRUCTIONS.contains("parent_url"));
    }

    #[test]
    fn project_summary_keeps_useful_fields_and_omits_empty_ones() {
        let full = Project {
            name: "switchyard".into(),
            path: "~/dev/src/playground/switchyard".into(),
            profile: "personal".into(),
            clone_url: "git@github.com:johnallen3d/switchyard.git".into(),
            hosts: vec!["omarchy".into(), "config".into()],
            tracker: "fizzy:6284043/board-1".into(),
        };
        let minimal = Project {
            name: "rex".into(),
            path: "~/dev/src/rex".into(),
            profile: "work".into(),
            clone_url: "git@github.com:example/rex.git".into(),
            hosts: Vec::new(),
            tracker: String::new(),
        };
        assert_eq!(
            projects_list(&[full, minimal], |profile| match profile {
                "personal" => "agy:gemini-4-neon".into(),
                _ => "claude".into(),
            }),
            json!({
                "projects": [
                    {
                        "name": "switchyard",
                        "path": "~/dev/src/playground/switchyard",
                        "profile": "personal",
                        "default_harness": "agy:gemini-4-neon",
                        "hosts": ["omarchy", "config"],
                        "tracker": "fizzy:6284043/board-1",
                    },
                    {
                        "name": "rex",
                        "path": "~/dev/src/rex",
                        "profile": "work",
                        "default_harness": "claude",
                    },
                ]
            })
        );
        assert_eq!(
            projects_list(&[], |_| "claude".into()),
            json!({ "projects": [] })
        );
    }

    #[test]
    fn finds_tasks_by_label_or_unique_name() {
        let t = |p: &str, n: &str| json!({ "project": p, "name": n, "ended_at": null });
        let open = vec![t("switchyard", "api"), t("rex", "api"), t("rex", "fix-it")];
        assert_eq!(label(&find_task("rex/api", &open).unwrap()), "rex/api");
        assert_eq!(label(&find_task("fix-it", &open).unwrap()), "rex/fix-it");
        assert!(
            find_task("api", &open)
                .unwrap_err()
                .contains("2 open tasks")
        );
        assert!(find_task("rex/nope", &open).is_err());
    }

    #[test]
    fn normalises_names_as_the_picker_does() {
        assert_eq!(normalise("MCP smoke_test!"), "mcp-smoke-test");
    }

    #[test]
    fn tool_errors_are_results_not_protocol_errors() {
        let e = endpoint();
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "message_task", "arguments": { "task": "x/y" } } });
        let reply = route(&post(&body.to_string()), &e).1.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "text is required");
    }
}
