//! Telling a spawning caller about its child tasks, so it needn't poll them.
//!
//! A caller that spawns a task through the MCP endpoint is recorded as its
//! parent — either another Rex agent (`parent_host`, `parent_session`,
//! `parent_window`) or a non-Rex service reachable at an HTTP callback URL
//! (`parent_url`, optional `parent_label`; see core's `launch`). When the
//! child's agent stops working (its tab goes from `working` to `idle`, `done`
//! or `blocked`) the server notifies the parent: typing a short notice into a
//! Rex parent's tab (as `message_task` would), or `POST`ing a JSON payload to
//! `parent_url` with retries on connect/network/5xx errors. A child that has
//! set itself a timer or left a background job running will resume by itself,
//! so it gets no notice until it settles for good. `notify_parent: false` on
//! the task turns notices off.
//!
//! A child can also write to its parent itself, mid-work, with the MCP
//! tool `message_parent` (see `send`), which transparently delivers either
//! to a Rex parent tab or to `parent_url`.
//!
//! Transitions are taken from the live status in the rows, seen at each
//! check (new rows or tasks). The first status seen for a task is only
//! recorded, so restarting the server sends nothing. Sending reads the
//! child's log and types into the parent's tab over SSH/Rex (or `POST`s over
//! HTTP), so it runs on a thread of its own and never under the lock. A Rex
//! parent waiting on a prompt of its own refuses the notice; it is logged
//! and dropped.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use switchyard_core::actions;
use switchyard_core::tasks::TabStatus;
use switchyard_core::transcript;

use crate::mcp::allowed_ip;

/// The most of the child's last message a summary notice quotes.
const QUOTE: usize = 600;
/// The most of the child's last message included in a JSON callback payload.
const CALLBACK_QUOTE: usize = 2000;
/// TCP connect timeout for HTTP callback delivery.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// HTTP read timeout for callback delivery.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// HTTP write timeout for callback delivery.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum HTTP response headers size accepted from a callback receiver.
const MAX_HTTP_HEADERS: usize = 64 * 1024;
/// Maximum response error body bytes read for diagnostics.
const MAX_ERROR_BODY: usize = 512;
/// Backoff delays between retries for background settle-notice callbacks.
const SETTLE_RETRY_DELAYS: &[Duration] = &[Duration::from_secs(2), Duration::from_secs(5)];
/// Quick retry delay when synchronous `message_parent` hits connection refused.
const SYNC_REFUSED_RETRY_DELAY: Duration = Duration::from_millis(250);

/// A notice to send: the child task and the status it settled in.
#[derive(Debug)]
pub struct Job {
    task: Value,
    state: String,
    msg: String,
}

/// How to reach a task's parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentTarget<'a> {
    Rex {
        host: &'a str,
        session: &'a str,
        window: &'a str,
    },
    Callback {
        url: &'a str,
    },
}

/// The parent recorded on `task`, when it has either a callback URL or a Rex
/// tab.
fn parent_target(task: &Value) -> Option<ParentTarget<'_>> {
    let f = |k: &str| task[k].as_str().filter(|s| !s.is_empty());
    if let Some(url) = f("parent_url") {
        return Some(ParentTarget::Callback { url });
    }
    Some(ParentTarget::Rex {
        host: f("parent_host")?,
        session: f("parent_session")?,
        window: f("parent_window")?,
    })
}

fn label(task: &Value) -> String {
    format!(
        "{}/{}",
        task["project"].as_str().unwrap_or(""),
        task["name"].as_str().unwrap_or("")
    )
}

/// A validated HTTP callback target with resolved, SSRF-checked socket
/// addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallbackTarget {
    host_header: String,
    request_target: String,
    addrs: Vec<SocketAddr>,
}

/// Parse an `http://` callback URL into `(host, port, host_header, target)`,
/// rejecting non-HTTP schemes, control/whitespace chars, credentials,
/// fragments, and malformed hosts/ports.
fn parse_callback_url(url: &str) -> Result<(String, u16, String, String), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("parent_url is empty".into());
    }
    if !url.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("parent_url must contain only printable ASCII without whitespace".into());
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "parent_url must use http://".to_string())?;
    if rest.contains('#') {
        return Err("parent_url must not contain a fragment".into());
    }
    let (authority, request_target) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err("parent_url has no host".into());
    }
    if authority.contains('@') {
        return Err("parent_url must not include credentials".into());
    }
    if authority.contains('\\') {
        return Err("invalid host in parent_url".into());
    }
    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let (addr, tail) = after_bracket
            .split_once(']')
            .ok_or_else(|| "invalid IPv6 host in parent_url".to_string())?;
        let _: Ipv6Addr = addr
            .parse()
            .map_err(|_| "invalid IPv6 host in parent_url".to_string())?;
        let port = match tail {
            "" => 80,
            s => s
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|&p| p != 0)
                .ok_or_else(|| "invalid port in parent_url".to_string())?,
        };
        (addr.to_string(), port)
    } else {
        if authority.contains(['[', ']']) {
            return Err("invalid host in parent_url".into());
        }
        if let Some((h, p)) = authority.rsplit_once(':') {
            if h.contains(':') {
                return Err("IPv6 host in parent_url must be bracketed".into());
            }
            let port = p
                .parse::<u16>()
                .ok()
                .filter(|&port| port != 0)
                .ok_or_else(|| "invalid port in parent_url".to_string())?;
            (h.to_string(), port)
        } else {
            (authority.to_string(), 80)
        }
    };
    if host.is_empty() {
        return Err("parent_url has no host".into());
    }
    Ok((host, port, authority.to_string(), request_target))
}

impl CallbackTarget {
    fn resolve(url: &str) -> Result<Self, String> {
        Self::resolve_with(url, |host, port| {
            (host, port)
                .to_socket_addrs()
                .map(Iterator::collect)
                .map_err(|e| format!("can't resolve parent_url host {host}: {e}"))
        })
    }

    fn resolve_with<F>(url: &str, lookup: F) -> Result<Self, String>
    where
        F: FnOnce(&str, u16) -> Result<Vec<SocketAddr>, String>,
    {
        let (host, port, host_header, request_target) = parse_callback_url(url)?;
        let addrs = if let Ok(v6) = host.parse::<Ipv6Addr>() {
            let ip = IpAddr::V6(v6);
            if !allowed_ip(ip) {
                return Err(format!(
                    "parent_url host {host} is not loopback or on the tailnet"
                ));
            }
            vec![SocketAddr::new(ip, port)]
        } else if let Ok(v4) = host.parse::<Ipv4Addr>() {
            let ip = IpAddr::V4(v4);
            if !allowed_ip(ip) {
                return Err(format!(
                    "parent_url host {host} is not loopback or on the tailnet"
                ));
            }
            vec![SocketAddr::new(ip, port)]
        } else {
            let resolved = lookup(&host, port)?;
            if resolved.is_empty() {
                return Err(format!("parent_url host {host} resolved to no addresses"));
            }
            if let Some(bad) = resolved.iter().find(|a| !allowed_ip(a.ip())) {
                return Err(format!(
                    "parent_url host {host} resolves to disallowed address {}",
                    bad.ip()
                ));
            }
            resolved
        };
        Ok(Self {
            host_header,
            request_target,
            addrs,
        })
    }
}

/// Validate a `parent_url` for `spawn_task`: strict `http://` targeting
/// loopback or tailnet IPs (or DNS hostnames resolving exclusively to them).
///
/// # Errors
///
/// Fails when `url` is malformed, uses a non-`http://` scheme, or targets any
/// address outside loopback and the tailnet.
pub fn validate_callback_url(url: &str) -> Result<(), String> {
    CallbackTarget::resolve(url).map(|_| ())
}

/// The open tasks with a parent to tell whose agent has just stopped
/// working, recording each one's status in `seen`. Entries for tabs no
/// open task has are dropped.
pub fn due(
    seen: &mut HashMap<String, String>,
    tasks: &[Value],
    live: &HashMap<String, TabStatus>,
) -> Vec<Job> {
    let open: Vec<&Value> = tasks
        .iter()
        .filter(|t| t["ended_at"].is_null() && parent_target(t).is_some())
        .filter(|t| t["notify_parent"].as_bool() != Some(false))
        .collect();
    seen.retain(|window, _| open.iter().any(|t| t["window_id"].as_str() == Some(window)));
    let mut jobs = Vec::new();
    for task in open {
        let Some(window) = task["window_id"].as_str() else {
            continue;
        };
        let Some(status) = live.get(window) else {
            continue;
        };
        let before = seen.insert(window.to_string(), status.state.clone());
        let settled = matches!(status.state.as_str(), "idle" | "done" | "blocked");
        if before.as_deref() == Some("working") && settled {
            jobs.push(Job {
                task: task.clone(),
                state: status.state.clone(),
                msg: status.msg.clone(),
            });
        }
    }
    jobs
}

/// Send each notice on a thread of its own.
pub fn start(jobs: Vec<Job>) {
    for job in jobs {
        thread::spawn(move || {
            let who = label(&job.task);
            match notice(&job) {
                Some(settled) => match deliver_settled(&job, &settled, SETTLE_RETRY_DELAYS) {
                    Ok(_) => crate::log(&format!(
                        "notice: told the parent of {who} it is {}",
                        job.state
                    )),
                    Err(e) => crate::log(&format!("notice: {who}: {e}")),
                },
                None => crate::log(&format!("notice: {who} will resume by itself; not told")),
            }
        });
    }
}

/// Prepared content for a settled/blocked notice.
#[derive(Debug, PartialEq, Eq)]
struct SettledNotice {
    summary: String,
    last_message: Option<String>,
}

/// The notice for `job`, or `None` when the child will resume by itself.
fn notice(job: &Job) -> Option<SettledNotice> {
    if job.state == "blocked" {
        return Some(blocked_notice(&label(&job.task), &job.msg));
    }
    let f = |k: &str| job.task[k].as_str().unwrap_or("");
    let log = transcript::fetch_on(f("host"), f("session_id"), f("window_id"));
    notice_from_log(&label(&job.task), &job.state, &log)
}

fn blocked_notice(who: &str, msg: &str) -> SettledNotice {
    let what = if msg.is_empty() {
        String::new()
    } else {
        format!(" ({msg})")
    };
    SettledNotice {
        summary: format!(
            "[switchyard] Your child task {who} is waiting on a prompt{what}. \
             Only John can answer it, in its tab."
        ),
        last_message: None,
    }
}

fn notice_from_log(who: &str, state: &str, log: &Value) -> Option<SettledNotice> {
    if log["wake"].is_object() {
        return None;
    }
    Some(SettledNotice {
        summary: compose(who, state, log),
        last_message: last_assistant_message(log).map(|m| truncate_chars(m, CALLBACK_QUOTE)),
    })
}

fn last_assistant_message(log: &Value) -> Option<&str> {
    log["entries"]
        .as_array()?
        .iter()
        .rev()
        .find(|e| e["kind"] == "assistant")
        .and_then(|e| e["text"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

/// A settled child's notice from its log (see core's `transcript`).
fn compose(who: &str, state: &str, log: &Value) -> String {
    let mut text = format!("[switchyard] Your child task {who} is {state}.");
    if let Some(last) = last_assistant_message(log) {
        let quote = truncate_chars(last, QUOTE);
        let _ = write!(text, " Its last message: \"{quote}\"");
    }
    text.push_str(" Use read_task for more, or message_task to reply.");
    text
}

fn settled_payload(job: &Job, settled: &SettledNotice) -> Value {
    let f = |k: &str| job.task[k].as_str().unwrap_or("");
    json!({
        "event": "settled",
        "task": label(&job.task),
        "project": f("project"),
        "name": f("name"),
        "host": f("host"),
        "state": job.state,
        "status_msg": (!job.msg.is_empty()).then_some(job.msg.as_str()),
        "last_message": settled.last_message,
        "summary": settled.summary,
    })
}

fn message_payload(task: &Value, message: &str, summary: &str) -> Value {
    let f = |k: &str| task[k].as_str().unwrap_or("");
    json!({
        "event": "message",
        "task": label(task),
        "project": f("project"),
        "name": f("name"),
        "host": f("host"),
        "message": message,
        "summary": summary,
    })
}

fn deliver_settled(
    job: &Job,
    settled: &SettledNotice,
    retry_delays: &[Duration],
) -> Result<String, String> {
    match parent_target(&job.task) {
        Some(ParentTarget::Rex {
            host,
            session,
            window,
        }) => actions::message(host, session, window, &settled.summary),
        Some(ParentTarget::Callback { url }) => {
            let payload = settled_payload(job, settled);
            post_with_retries(url, &payload, retry_delays)
                .map(|status| format!("delivered to {url} (HTTP {status})"))
        }
        None => Err(format!("{} has no parent on record", label(&job.task))),
    }
}

/// Send `message` from a child `task` to the agent or callback URL that
/// spawned it.
///
/// # Errors
///
/// Fails when the task has no parent on record, as core's `actions::message`
/// does for a Rex parent (the parent's tab is gone, or it waits on a prompt),
/// or when delivering the JSON callback `POST` fails.
pub fn send(task: &Value, message: &str) -> Result<String, String> {
    send_with_retry_delay(task, message, SYNC_REFUSED_RETRY_DELAY)
}

fn send_with_retry_delay(
    task: &Value,
    message: &str,
    refused_retry_delay: Duration,
) -> Result<String, String> {
    let summary = format!(
        "[switchyard] From your child task {}: {message}",
        label(task)
    );
    match parent_target(task) {
        Some(ParentTarget::Rex {
            host,
            session,
            window,
        }) => actions::message(host, session, window, &summary),
        Some(ParentTarget::Callback { url }) => {
            let payload = message_payload(task, message, &summary);
            post_sync(url, &payload, refused_retry_delay)?;
            let dest = task["parent_label"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map_or_else(
                    || "parent callback".to_string(),
                    |l| format!("parent callback ({l})"),
                );
            Ok(format!("delivered to {dest}"))
        }
        None => Err(format!(
            "{} has no parent agent or callback on record \
             (it wasn't spawned through spawn_task with parent_block or parent_url)",
            label(task)
        )),
    }
}

/// Outcome of a single HTTP callback attempt.
enum PostError {
    /// Invalid or disallowed URL: never retry.
    InvalidUrl(String),
    /// Non-retryable HTTP response (4xx, 3xx): never retry.
    NonRetryableStatus(String),
    /// 5xx HTTP response: retryable for settle notices.
    ServerStatus(String),
    /// Connection refused: retryable for settle notices and 1 quick sync retry.
    ConnectRefused(String),
    /// Other network/IO/timeout error: retryable for settle notices.
    Network(String),
}

impl PostError {
    fn into_message(self) -> String {
        match self {
            Self::InvalidUrl(e)
            | Self::NonRetryableStatus(e)
            | Self::ServerStatus(e)
            | Self::ConnectRefused(e)
            | Self::Network(e) => e,
        }
    }

    const fn retryable_for_settle(&self) -> bool {
        matches!(
            self,
            Self::ServerStatus(_) | Self::ConnectRefused(_) | Self::Network(_)
        )
    }
}

fn post_with_retries(url: &str, body: &Value, retry_delays: &[Duration]) -> Result<u16, String> {
    let mut delays = retry_delays.iter();
    loop {
        match post_once(url, body) {
            Ok(status) => return Ok(status),
            Err(err) if err.retryable_for_settle() => {
                if let Some(&delay) = delays.next() {
                    thread::sleep(delay);
                } else {
                    return Err(err.into_message());
                }
            }
            Err(err) => return Err(err.into_message()),
        }
    }
}

fn post_sync(url: &str, body: &Value, refused_retry_delay: Duration) -> Result<u16, String> {
    match post_once(url, body) {
        Ok(status) => Ok(status),
        Err(PostError::ConnectRefused(_)) => {
            thread::sleep(refused_retry_delay);
            post_once(url, body).map_err(PostError::into_message)
        }
        Err(err) => Err(err.into_message()),
    }
}

fn post_once(url: &str, body: &Value) -> Result<u16, PostError> {
    let target = CallbackTarget::resolve(url).map_err(PostError::InvalidUrl)?;
    let mut stream = connect_target(&target, url)?;
    let payload = body.to_string();
    let head = format!(
        "POST {} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        target.request_target,
        target.host_header,
        payload.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(payload.as_bytes()))
        .and_then(|()| stream.flush())
        .map_err(|e| PostError::Network(format!("parent callback {url} write failed: {e}")))?;

    let mut reader = BufReader::new(stream);
    let (status, length) = read_http_head(&mut reader, url)?;
    if (200..300).contains(&status) {
        // Politely drain tiny known-length bodies so the peer sees a clean close.
        if let Some(len) = length.filter(|&n| n > 0 && n <= 4096) {
            let mut sink = vec![0u8; len];
            let _ = reader.read_exact(&mut sink);
        }
        return Ok(status);
    }
    let detail = read_error_detail(&mut reader, length);
    let msg = if detail.is_empty() {
        if (300..400).contains(&status) {
            format!("parent callback {url} returned HTTP {status} (redirects are not followed)")
        } else {
            format!("parent callback {url} returned HTTP {status}")
        }
    } else {
        format!("parent callback {url} returned HTTP {status}: {detail}")
    };
    if (500..600).contains(&status) {
        Err(PostError::ServerStatus(msg))
    } else {
        Err(PostError::NonRetryableStatus(msg))
    }
}

fn connect_target(target: &CallbackTarget, url: &str) -> Result<TcpStream, PostError> {
    let mut last_err = None;
    let mut all_refused = true;
    for addr in &target.addrs {
        match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                return Ok(stream);
            }
            Err(e) => {
                if e.kind() != ErrorKind::ConnectionRefused {
                    all_refused = false;
                }
                last_err = Some(e);
            }
        }
    }
    let detail = last_err.map_or_else(|| "connection failed".into(), |e| e.to_string());
    let msg = format!("can't reach parent callback {url}: {detail}");
    if all_refused {
        Err(PostError::ConnectRefused(msg))
    } else {
        Err(PostError::Network(msg))
    }
}

fn read_http_head(reader: &mut impl BufRead, url: &str) -> Result<(u16, Option<usize>), PostError> {
    let mut status_line = String::new();
    if reader
        .read_line(&mut status_line)
        .map_err(|e| PostError::Network(format!("parent callback {url} read failed: {e}")))?
        == 0
    {
        return Err(PostError::Network(format!(
            "parent callback {url} closed the connection"
        )));
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| {
            PostError::Network(format!(
                "invalid HTTP status from parent callback {url}: {}",
                status_line.trim()
            ))
        })?;
    let mut length = None;
    let mut total = status_line.len();
    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .map_err(|e| PostError::Network(format!("parent callback {url} read failed: {e}")))?
            == 0
        {
            return Err(PostError::Network(format!(
                "parent callback {url} closed headers early"
            )));
        }
        total += line.len();
        if total > MAX_HTTP_HEADERS {
            return Err(PostError::Network(format!(
                "parent callback {url} response headers too large"
            )));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, val)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            length = Some(n);
        }
    }
    Ok((status, length))
}

fn read_error_detail(reader: &mut impl BufRead, length: Option<usize>) -> String {
    let Some(len) = length.filter(|&n| n > 0) else {
        return String::new();
    };
    let mut buf = vec![0u8; len.min(MAX_ERROR_BODY)];
    if reader.read_exact(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn live(state: &str) -> HashMap<String, TabStatus> {
        HashMap::from([(
            "w".to_string(),
            TabStatus {
                state: state.into(),
                at: 1,
                msg: "m".into(),
            },
        )])
    }

    fn child() -> Value {
        json!({"project":"p","name":"n","window_id":"w",
            "parent_host":"h","parent_session":"s","parent_window":"pw"})
    }

    fn callback_child(url: &str) -> Value {
        json!({"project":"p","name":"n","host":"omarchy","window_id":"w",
            "parent_url": url, "parent_label": "marvin"})
    }

    fn read_http_post(stream: &mut TcpStream) -> (String, HashMap<String, String>, String) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut req_line = String::new();
        reader.read_line(&mut req_line).unwrap();
        let mut headers = HashMap::new();
        let mut content_len = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim().to_string();
                if key == "content-length" {
                    content_len = val.parse().unwrap();
                }
                headers.insert(key, val);
            }
        }
        let mut body = vec![0u8; content_len];
        reader.read_exact(&mut body).unwrap();
        (
            req_line.trim_end().to_string(),
            headers,
            String::from_utf8(body).unwrap(),
        )
    }

    struct CapturedPost {
        req_line: String,
        headers: HashMap<String, String>,
        body: Value,
    }

    struct MockServer {
        url: String,
        hits: Arc<AtomicUsize>,
        received: Arc<Mutex<Vec<CapturedPost>>>,
        handle: thread::JoinHandle<()>,
    }

    fn spawn_mock_server(statuses: Vec<(u16, &'static str)>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let hits_bg = hits.clone();
        let recv_bg = received.clone();
        let handle = thread::spawn(move || {
            for (status, body) in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                hits_bg.fetch_add(1, Ordering::SeqCst);
                let (req_line, headers, raw_body) = read_http_post(&mut stream);
                let json_body: Value = serde_json::from_str(&raw_body).unwrap();
                recv_bg.lock().unwrap().push(CapturedPost {
                    req_line,
                    headers,
                    body: json_body,
                });
                let resp = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        MockServer {
            url: format!("http://{addr}/hooks/switchyard?src=test"),
            hits,
            received,
            handle,
        }
    }

    #[test]
    fn a_child_that_stops_working_is_noticed_once() {
        let tasks = vec![child()];
        let mut seen = HashMap::new();
        // The first status seen is only recorded, even a settled one.
        assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        assert!(due(&mut seen, &tasks, &live("working")).is_empty());
        let jobs = due(&mut seen, &tasks, &live("blocked"));
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            (jobs[0].state.as_str(), jobs[0].msg.as_str()),
            ("blocked", "m")
        );
        assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        assert!(due(&mut seen, &tasks, &live("working")).is_empty());
        assert_eq!(due(&mut seen, &tasks, &live("done")).len(), 1);
    }

    #[test]
    fn callback_children_trigger_due_and_respect_notify_parent_false() {
        let mut seen = HashMap::new();
        let cb = callback_child("http://127.0.0.1:9999/cb");
        assert!(due(&mut seen, std::slice::from_ref(&cb), &live("working")).is_empty());
        let jobs = due(&mut seen, std::slice::from_ref(&cb), &live("done"));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, "done");

        let mut off = cb;
        off["notify_parent"] = false.into();
        due(&mut seen, std::slice::from_ref(&off), &live("working"));
        assert!(due(&mut seen, &[off], &live("idle")).is_empty());
        assert!(seen.is_empty());
    }

    #[test]
    fn tasks_without_a_parent_or_with_notices_off_are_left_alone() {
        let mut seen = HashMap::new();
        let orphan = vec![json!({"window_id":"w"})];
        let mut off = child();
        off["notify_parent"] = false.into();
        for tasks in [orphan, vec![off]] {
            due(&mut seen, &tasks, &live("working"));
            assert!(due(&mut seen, &tasks, &live("idle")).is_empty());
        }
        assert!(seen.is_empty());
        // A closed task's entry goes.
        due(&mut seen, &[child()], &live("working"));
        let mut ended = child();
        ended["ended_at"] = 1.into();
        due(&mut seen, &[ended], &live("idle"));
        assert!(seen.is_empty());
    }

    #[test]
    fn a_notice_quotes_the_last_assistant_message() {
        let log = json!({"entries":[
            {"kind":"assistant","text":"first"},
            {"kind":"assistant","text":" done: PR opened "},
            {"kind":"tool","text":"Bash"},
        ]});
        assert_eq!(
            compose("p/n", "idle", &log),
            "[switchyard] Your child task p/n is idle. Its last message: \"done: PR opened\" \
             Use read_task for more, or message_task to reply."
        );
        let truncated = json!({"entries":[{"kind":"assistant","text":"x".repeat(QUOTE + 5)}]});
        assert!(compose("p/n", "done", &truncated).contains(&format!("{}…\"", "x".repeat(QUOTE))));
        assert!(!compose("p/n", "idle", &json!({"error":"no log"})).contains("last message"));
    }

    #[test]
    fn notice_from_log_suppresses_wake_and_caps_callback_last_message() {
        let waking = json!({"wake": {"at": 123}, "entries": [{"kind":"assistant","text":"later"}]});
        assert!(notice_from_log("p/n", "idle", &waking).is_none());

        let text = "a".repeat(CALLBACK_QUOTE + 25);
        let transcript = json!({"entries": [{"kind":"assistant","text": text}]});
        let settled = notice_from_log("p/n", "done", &transcript).unwrap();
        assert_eq!(
            settled.last_message.as_deref(),
            Some(format!("{}…", "a".repeat(CALLBACK_QUOTE)).as_str())
        );
        assert!(
            settled
                .summary
                .contains(&format!("{}…\"", "a".repeat(QUOTE)))
        );

        let job = Job {
            task: callback_child("http://127.0.0.1:9/cb"),
            state: "blocked".into(),
            msg: "approve bash".into(),
        };
        let blocked = blocked_notice("p/n", "approve bash");
        let payload = settled_payload(&job, &blocked);
        assert_eq!(payload["event"], "settled");
        assert_eq!(payload["task"], "p/n");
        assert_eq!(payload["project"], "p");
        assert_eq!(payload["name"], "n");
        assert_eq!(payload["host"], "omarchy");
        assert_eq!(payload["state"], "blocked");
        assert_eq!(payload["status_msg"], "approve bash");
        assert!(payload["last_message"].is_null());
        assert!(
            payload["summary"]
                .as_str()
                .unwrap()
                .contains("waiting on a prompt (approve bash)")
        );
    }

    #[test]
    fn sending_needs_a_parent_on_record() {
        let err = send(&json!({"project":"p","name":"n"}), "hi").unwrap_err();
        assert!(err.starts_with("p/n has no parent agent"));
    }

    #[test]
    fn validates_callback_urls_for_loopback_and_tailnet_only() {
        // Allowed literals
        for ok in [
            "http://127.0.0.1:8080/cb",
            "http://127.0.0.2/cb?a=1&b=2",
            "http://[::1]:8080/hooks",
            "http://[::1]",
            "http://100.64.0.1:7392/notify",
            "http://100.95.69.110/cb",
            "http://100.127.255.254:80/cb",
            "http://[fd7a:115c:a1e0::1]:8080/cb",
            "http://[::ffff:100.64.0.1]:8080/cb",
            "http://[::ffff:127.0.0.1]:8080/cb",
        ] {
            assert!(
                validate_callback_url(ok).is_ok(),
                "expected {ok} to be allowed"
            );
        }

        // Hostname resolution checks via deterministic resolver
        let ok_tailnet =
            CallbackTarget::resolve_with("http://omarchy.ts.net:8080/cb?x=1", |_, p| {
                Ok(vec![
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 95, 69, 110)), p),
                    SocketAddr::new(IpAddr::V6("fd7a:115c:a1e0::2".parse().unwrap()), p),
                ])
            })
            .unwrap();
        assert_eq!(ok_tailnet.host_header, "omarchy.ts.net:8080");
        assert_eq!(ok_tailnet.request_target, "/cb?x=1");

        let ok_loopback = CallbackTarget::resolve_with("http://localhost?k=v", |_, p| {
            Ok(vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), p),
            ])
        })
        .unwrap();
        assert_eq!(ok_loopback.host_header, "localhost");
        assert_eq!(ok_loopback.request_target, "/?k=v");

        // Mixed tailnet + public DNS resolution must be rejected
        assert!(
            CallbackTarget::resolve_with("http://dual.example:8080/cb", |_, p| {
                Ok(vec![
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 95, 69, 110)), p),
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), p),
                ])
            })
            .is_err()
        );
        // Public-only or empty DNS resolution must be rejected
        assert!(
            CallbackTarget::resolve_with("http://public.example/cb", |_, p| {
                Ok(vec![SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                    p,
                )])
            })
            .is_err()
        );
        assert!(
            CallbackTarget::resolve_with("http://empty.example/cb", |_, _| Ok(vec![])).is_err()
        );

        // Rejected schemes, malformed URLs, SSRF targets, injection attempts
        for bad in [
            "",
            "https://127.0.0.1:8080/cb",
            "file:///etc/passwd",
            "ftp://127.0.0.1/cb",
            "http://",
            "http://:8080/cb",
            "http://127.0.0.1:0/cb",
            "http://127.0.0.1:99999/cb",
            "http://::1:8080/cb",
            "http://[localhost]:8080/cb",
            "http://127.0.0.1@8.8.8.8/cb",
            "http://127.0.0.1/cb#frag",
            "http://127.0.0.1/cb HTTP/1.1",
            "http://127.0.0.1/cb\r\nX-Evil: 1",
            "http://8.8.8.8/cb",
            "http://192.168.1.10/cb",
            "http://10.0.0.1/cb",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0:8080/cb",
            "http://[::]:8080/cb",
            "http://[2001:db8::1]/cb",
            "http://100.63.255.255/cb",
            "http://100.128.0.1/cb",
        ] {
            assert!(
                validate_callback_url(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn delivers_settle_notice_and_message_parent_over_loopback_http() {
        let server = spawn_mock_server(vec![(200, "ok"), (204, "")]);

        // 1. Settle notice
        let job = Job {
            task: callback_child(&server.url),
            state: "done".into(),
            msg: String::new(),
        };
        let transcript = json!({"entries": [{"kind":"assistant","text":"finished card 1518"}]});
        let settled = notice_from_log("p/n", "done", &transcript).unwrap();
        deliver_settled(&job, &settled, &[Duration::from_millis(5)]).unwrap();

        // 2. Synchronous message_parent
        let msg_out = send_with_retry_delay(
            &callback_child(&server.url),
            "ready for review",
            Duration::from_millis(5),
        )
        .unwrap();
        assert_eq!(msg_out, "delivered to parent callback (marvin)");

        server.handle.join().unwrap();
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        let reqs = server.received.lock().unwrap();
        assert_eq!(reqs[0].req_line, "POST /hooks/switchyard?src=test HTTP/1.1");
        assert_eq!(
            reqs[0].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(reqs[0].body["event"], "settled");
        assert_eq!(reqs[0].body["task"], "p/n");
        assert_eq!(reqs[0].body["project"], "p");
        assert_eq!(reqs[0].body["name"], "n");
        assert_eq!(reqs[0].body["host"], "omarchy");
        assert_eq!(reqs[0].body["state"], "done");
        assert!(reqs[0].body["status_msg"].is_null());
        assert_eq!(reqs[0].body["last_message"], "finished card 1518");
        assert!(
            reqs[0].body["summary"]
                .as_str()
                .unwrap()
                .contains("[switchyard]")
        );

        assert_eq!(reqs[1].body["event"], "message");
        assert_eq!(reqs[1].body["task"], "p/n");
        assert_eq!(reqs[1].body["project"], "p");
        assert_eq!(reqs[1].body["name"], "n");
        assert_eq!(reqs[1].body["host"], "omarchy");
        assert_eq!(reqs[1].body["message"], "ready for review");
        assert_eq!(
            reqs[1].body["summary"],
            "[switchyard] From your child task p/n: ready for review"
        );
    }

    #[test]
    fn settle_notice_does_not_retry_4xx_or_3xx_and_retries_5xx() {
        let settled = SettledNotice {
            summary: "[switchyard] hi".into(),
            last_message: None,
        };

        // 4xx -> single attempt, no retry
        let s4 = spawn_mock_server(vec![(400, "bad json")]);
        let job_4xx = Job {
            task: callback_child(&s4.url),
            state: "idle".into(),
            msg: String::new(),
        };
        let err = deliver_settled(
            &job_4xx,
            &settled,
            &[Duration::from_millis(5), Duration::from_millis(5)],
        )
        .unwrap_err();
        s4.handle.join().unwrap();
        assert_eq!(s4.hits.load(Ordering::SeqCst), 1);
        assert!(
            err.contains("HTTP 400") && err.contains("bad json"),
            "{err}"
        );

        // 3xx -> not followed, no retry
        let s3 = spawn_mock_server(vec![(302, "")]);
        let job_3xx = Job {
            task: callback_child(&s3.url),
            state: "idle".into(),
            msg: String::new(),
        };
        let err = deliver_settled(&job_3xx, &settled, &[Duration::from_millis(5)]).unwrap_err();
        s3.handle.join().unwrap();
        assert_eq!(s3.hits.load(Ordering::SeqCst), 1);
        assert!(err.contains("HTTP 302"), "{err}");

        // 5xx -> retries and succeeds on 3rd attempt
        let s5_ok = spawn_mock_server(vec![(503, "busy"), (500, "oops"), (200, "ok")]);
        let job_5xx = Job {
            task: callback_child(&s5_ok.url),
            state: "idle".into(),
            msg: String::new(),
        };
        deliver_settled(
            &job_5xx,
            &settled,
            &[Duration::from_millis(5), Duration::from_millis(5)],
        )
        .unwrap();
        s5_ok.handle.join().unwrap();
        assert_eq!(s5_ok.hits.load(Ordering::SeqCst), 3);

        // 5xx exhaustion -> stops after initial + 2 retries and returns error
        let s5_err = spawn_mock_server(vec![(502, "bad gw"), (502, "bad gw"), (502, "bad gw")]);
        let job_exhaust = Job {
            task: callback_child(&s5_err.url),
            state: "idle".into(),
            msg: String::new(),
        };
        let err = deliver_settled(
            &job_exhaust,
            &settled,
            &[Duration::from_millis(5), Duration::from_millis(5)],
        )
        .unwrap_err();
        s5_err.handle.join().unwrap();
        assert_eq!(s5_err.hits.load(Ordering::SeqCst), 3);
        assert!(err.contains("HTTP 502"), "{err}");

        // message_parent on 5xx -> single attempt, returns clear error
        let s_sync = spawn_mock_server(vec![(502, "down")]);
        let err = send_with_retry_delay(
            &callback_child(&s_sync.url),
            "ping",
            Duration::from_millis(5),
        )
        .unwrap_err();
        s_sync.handle.join().unwrap();
        assert_eq!(s_sync.hits.load(Ordering::SeqCst), 1);
        assert!(err.contains("HTTP 502") && err.contains("down"), "{err}");

        // message_parent on closed port -> clear connect error
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_addr = closed.local_addr().unwrap();
        drop(closed);
        let err = send_with_retry_delay(
            &callback_child(&format!("http://{closed_addr}/cb")),
            "ping",
            Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(err.contains("can't reach parent callback"), "{err}");
    }
}
