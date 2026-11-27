//! Images (and other files) pasted into a prompt box. Rex uploads a pasted
//! or dropped image to the machine running the terminal, here the float's
//! host, and pastes its shell-escaped path. Clipboard managers and file
//! drops (such as macOS screenshots copied as file URLs) can also paste a
//! local image path directly without going through Rex's uploads directory.
//! Those paths mean nothing on another host, and Rex removes uploads after a
//! while, so before a prompt reaches an agent `carry` / `carry_from` copies
//! each pasted image to the agent's host and puts the copy's path in its
//! place.

use std::io::Write;
use std::ops::Range;
use std::process::{Command, Stdio};

use crate::rex::{diagnose, host_bash, local_label, now, sh_quote, ssh};

/// The directory Rex keeps uploads in, wherever its state lives (on macOS,
/// `~/Library/Application Support/rex/uploads`).
const UPLOADS: &str = "/rex/uploads/";

/// Where copies go on the agent's host, relative to its state directory;
/// a copy is removed after `KEEP_DAYS`.
pub const IMAGE_DIR: &str = "rex/spawn-images";
const SPAWN_IMAGES: &str = "/rex/spawn-images/";
const KEEP_DAYS: u32 = 7;

/// File extensions treated as pasted local images when not in `UPLOADS`.
const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff", "avif",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A Rex-managed upload under `/rex/uploads/`: always copied (even when
    /// `from == to`, because Rex expires uploads) and fails loudly if gone.
    RexUpload,
    /// Any other absolute image file path (e.g. a screenshot path pasted from
    /// macOS): best-effort copied when found on a source host different from
    /// the target host, left untouched otherwise.
    LocalImage,
}

/// A pasted image or upload path: where it sits in the text, the unescaped
/// path itself, and how strictly it should be handled.
#[derive(Debug, PartialEq, Eq)]
struct Upload {
    span: Range<usize>,
    path: String,
    kind: Kind,
}

/// Result of attempting to read a pasted file from a source host.
enum ReadOutcome {
    Found(Vec<u8>),
    Missing,
    Unreachable,
}

fn is_image_path(path: &str) -> bool {
    let file = name(path);
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty() && IMAGE_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// The upload and image paths pasted into `text`: whole words, shell-escaped
/// or quoted as terminals paste them, naming either a file in Rex's uploads
/// directory or an absolute image file path.
fn uploads(text: &str) -> Vec<Upload> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let (word, len) = shell_word(rest);
        if word.starts_with('/') && !word.ends_with('/') {
            let kind = if word.contains(UPLOADS) {
                Some(Kind::RexUpload)
            } else if !word.contains(SPAWN_IMAGES) && is_image_path(&word) {
                Some(Kind::LocalImage)
            } else {
                None
            };
            if let Some(kind) = kind {
                found.push(Upload {
                    span: i..i + len,
                    path: word,
                    kind,
                });
            }
        }
        i += len.max(1);
    }
    found
}

/// The first shell word of `s` with its quoting and escapes removed, and
/// how many bytes of `s` it takes, up to the first unquoted whitespace.
fn shell_word(s: &str) -> (String, usize) {
    let mut word = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => return (word, i),
            '\\' => {
                chars.next();
                if let Some((_, next)) = chars.next() {
                    word.push(next);
                }
                continue;
            }
            '\'' => {
                chars.next();
                for (_, q) in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
                continue;
            }
            '"' => {
                chars.next();
                while let Some((_, q)) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some((_, '"' | '\\' | '$' | '`'))) => {
                            if let Some((_, e)) = chars.next() {
                                word.push(e);
                            }
                        }
                        _ => word.push(q),
                    }
                }
                continue;
            }
            _ => word.push(c),
        }
        chars.next();
    }
    (word, s.len())
}

/// `text` with each upload or local image pasted into it (a file on `from`)
/// copied to `to` and replaced by the copy's path. Text without pasted files
/// comes back as it is. Fails, rather than hand the agent a path it can't
/// open, when a Rex upload is gone (Rex keeps them only for a while) or can't
/// be copied.
///
/// # Errors
///
/// Fails when a Rex upload can't be read on `from`, or when a pasted file
/// can't be written on `to`.
pub fn carry(from: &str, to: &str, text: &str) -> Result<String, String> {
    carry_from(&[from], to, text)
}

/// Like `carry`, checking candidate source hosts in `sources` in order (e.g.
/// the float's host first, then the client host that invoked the launch).
///
/// Rex uploads (`/rex/uploads/`) are always copied even when `source == to`
/// (because Rex expires them) and fail if missing on every source host.
/// Other absolute image paths are copied best-effort when present as a regular
/// file on a source host different from `to`, and left untouched otherwise so
/// paths that live on `to` or are mentioned in prose are not broken.
///
/// # Errors
///
/// Fails when a Rex upload can't be read on any source host, or when a copy
/// can't be written on `to`.
pub fn carry_from(sources: &[&str], to: &str, text: &str) -> Result<String, String> {
    carry_with(sources, to, text, read_file, write)
}

fn carry_with<R, W>(
    sources: &[&str],
    to: &str,
    text: &str,
    mut read_fn: R,
    mut write_fn: W,
) -> Result<String, String>
where
    R: FnMut(&str, &str) -> ReadOutcome,
    W: FnMut(&str, &str, &[u8]) -> Result<String, String>,
{
    let found = uploads(text);
    if found.is_empty() {
        return Ok(text.to_string());
    }
    let mut hosts: Vec<&str> = Vec::new();
    for &host in sources {
        if !host.is_empty() && !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (n, upload) in found.iter().enumerate() {
        let bytes = match upload.kind {
            Kind::RexUpload => Some(read_required(&hosts, &upload.path, &mut read_fn)?),
            Kind::LocalImage => read_optional(&hosts, to, &upload.path, &mut read_fn),
        };
        if let Some(bytes) = bytes {
            let copy = write_fn(to, &copy_name(&upload.path, n), &bytes)?;
            out.push_str(&text[last..upload.span.start]);
            out.push_str(&copy);
            last = upload.span.end;
        }
    }
    out.push_str(&text[last..]);
    Ok(out)
}

fn read_required<R>(hosts: &[&str], path: &str, read_fn: &mut R) -> Result<Vec<u8>, String>
where
    R: FnMut(&str, &str) -> ReadOutcome,
{
    let mut first_err = None;
    for &host in hosts {
        match read_fn(host, path) {
            ReadOutcome::Found(bytes) => return Ok(bytes),
            ReadOutcome::Missing => {
                first_err.get_or_insert_with(|| {
                    format!("pasted file {} is gone; paste it again", name(path))
                });
            }
            ReadOutcome::Unreachable => {
                first_err.get_or_insert_with(|| {
                    format!("can't reach {host} for pasted file {}", name(path))
                });
            }
        }
    }
    Err(first_err.unwrap_or_else(|| format!("pasted file {} is gone; paste it again", name(path))))
}

fn read_optional<R>(hosts: &[&str], to: &str, path: &str, read_fn: &mut R) -> Option<Vec<u8>>
where
    R: FnMut(&str, &str) -> ReadOutcome,
{
    for &host in hosts {
        if host == to {
            continue;
        }
        match read_fn(host, path) {
            ReadOutcome::Found(bytes) => return Some(bytes),
            ReadOutcome::Missing => {}
            ReadOutcome::Unreachable => {
                diagnose(&format!(
                    "can't reach {host} for pasted image {}",
                    name(path)
                ));
            }
        }
    }
    None
}

/// A copy's file name: unique to this call and position, keeping the
/// upload's own name (made safe for a path and a script) for the agent.
fn copy_name(path: &str, n: usize) -> String {
    let safe: String = name(path)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_start_matches(['.', '-']);
    // Keep the end, where the extension is.
    let safe = &safe[safe.len().saturating_sub(60)..];
    let safe = if safe.is_empty() { "image" } else { safe };
    format!("{}-{}-{n}-{safe}", now(), std::process::id())
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Bash script to verify `path` is a regular file and stream its bytes.
///
/// POSIX/bash `test -f` takes a single operand without `--` (passing `--`
/// makes `test` fail with `binary operator expected`).
fn remote_read_script(path: &str) -> String {
    let quoted = sh_quote(path);
    format!("test -f {quoted} && cat -- {quoted}\n")
}

/// The bytes of the regular file at `path` on `host`.
fn read_file(host: &str, path: &str) -> ReadOutcome {
    if host == local_label() {
        if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
            return ReadOutcome::Missing;
        }
        return std::fs::read(path).map_or(ReadOutcome::Missing, ReadOutcome::Found);
    }
    let Some(out) = host_bash(host, &remote_read_script(path)) else {
        return ReadOutcome::Unreachable;
    };
    if !out.status.success() {
        return ReadOutcome::Missing;
    }
    ReadOutcome::Found(out.stdout)
}

/// Write `bytes` to the file `file` in the image directory on `host`,
/// pruning old copies, and return its absolute path there.
fn write(host: &str, file: &str, bytes: &[u8]) -> Result<String, String> {
    let script = format!(
        "d=\"${{XDG_STATE_HOME:-$HOME/.local/state}}/{IMAGE_DIR}\" && mkdir -p \"$d\" && \
         {{ find \"$d\" -type f -mtime +{KEEP_DAYS} -delete 2>/dev/null; true; }} && \
         f=\"$d/{file}\" && cat > \"$f.tmp\" && mv \"$f.tmp\" \"$f\" && printf %s \"$f\""
    );
    let mut cmd = if host == local_label() {
        let mut c = Command::new("bash");
        c.arg("-c").arg(&script);
        c
    } else {
        // The hosts' login shell is Fish, which reads a single-quoted word
        // as sh does.
        let mut c = ssh(host);
        c.arg(format!("bash -c {}", sh_quote(&script)));
        c
    };
    let failed = |why: String| format!("couldn't copy pasted file {file} to {host}: {why}");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes).map_err(|e| failed(e.to_string()))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| failed(e.to_string()))?;
    let path = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() || !path.starts_with('/') {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "/Users/j/Library/Application Support/rex/uploads/p1/b2/Screenshot 1.png";
    const VORSSAINT: &str = "/Users/john.allen/Library/Caches/com.vorssaint.utils/Copied Screenshots/Screenshot 2026-11-26 at 19.50.35.png";

    #[test]
    fn finds_escaped_and_quoted_upload_paths() {
        let escaped = MAC.replace(' ', "\\ ");
        let text = format!("look at {escaped} and '{MAC}' or \"{MAC}\"\nthanks");
        let found = uploads(&text);
        assert_eq!(found.len(), 3);
        assert!(
            found
                .iter()
                .all(|u| u.path == MAC && u.kind == Kind::RexUpload)
        );
        assert_eq!(&text[found[0].span.clone()], escaped);
        assert_eq!(&text[found[1].span.clone()], format!("'{MAC}'"));
    }

    #[test]
    fn finds_local_image_paths_with_common_extensions() {
        let escaped = VORSSAINT.replace(' ', "\\ ");
        let text = format!(
            "what is `/q` referring to here?\n\n{escaped}\n'/Users/j/Shot 2.JPEG' \"/tmp/img.WEBP\""
        );
        let found = uploads(&text);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].path, VORSSAINT);
        assert_eq!(found[0].kind, Kind::LocalImage);
        assert_eq!(&text[found[0].span.clone()], escaped);
        assert_eq!(found[1].path, "/Users/j/Shot 2.JPEG");
        assert_eq!(found[1].kind, Kind::LocalImage);
        assert_eq!(found[2].path, "/tmp/img.WEBP");
        assert_eq!(found[2].kind, Kind::LocalImage);
    }

    #[test]
    fn leaves_other_paths_and_words_alone() {
        assert_eq!(
            uploads(
                "see /etc/hosts, /Users/j/notes.txt, /tmp/.png, ~/rex/uploads/x.png, rex/uploads/y.png, shot.png, /home/j/.local/state/rex/spawn-images/1-2-0-a.png"
            ),
            [] as [Upload; 0]
        );
        assert_eq!(
            uploads("/home/j/.local/state/rex/uploads/"),
            [] as [Upload; 0]
        );
        assert_eq!(uploads(""), [] as [Upload; 0]);
        let linux = uploads("é /home/j/.local/state/rex/uploads/a/b/c.png");
        assert_eq!(linux[0].path, "/home/j/.local/state/rex/uploads/a/b/c.png");
        assert_eq!(linux[0].kind, Kind::RexUpload);
    }

    #[test]
    fn carry_keeps_text_without_uploads() {
        assert_eq!(
            carry("nowhere", "nowhere", "just text").unwrap(),
            "just text"
        );
    }

    #[test]
    fn carry_fails_on_a_missing_upload() {
        let me = local_label();
        let err = carry(&me, &me, "x /tmp/no-such/rex/uploads/gone.png").unwrap_err();
        assert!(err.contains("gone.png is gone"), "{err}");
    }

    #[test]
    fn carry_copies_local_image_across_hosts_and_rewrites_escaped_span() {
        let escaped = VORSSAINT.replace(' ', "\\ ");
        let text = format!("what is `/q` referring to here?\n\n{escaped}");
        let mut reads = Vec::new();
        let out = carry_with(
            &["m4-mbp"],
            "omarchy",
            &text,
            |host, path| {
                reads.push((host.to_string(), path.to_string()));
                if host == "m4-mbp" && path == VORSSAINT {
                    ReadOutcome::Found(b"png-bytes".to_vec())
                } else {
                    ReadOutcome::Missing
                }
            },
            |host, file, bytes| {
                assert_eq!(host, "omarchy");
                assert_eq!(bytes, b"png-bytes");
                assert!(file.ends_with("-0-Screenshot-2026-11-26-at-19.50.35.png"));
                Ok(format!("/home/johna/.local/state/{IMAGE_DIR}/{file}"))
            },
        )
        .unwrap();
        assert_eq!(reads, vec![("m4-mbp".to_string(), VORSSAINT.to_string())]);
        assert!(out.starts_with(
            "what is `/q` referring to here?\n\n/home/johna/.local/state/rex/spawn-images/"
        ));
        assert!(out.ends_with("-0-Screenshot-2026-11-26-at-19.50.35.png"));
        assert!(!out.contains("\\ "));
    }

    #[test]
    fn carry_falls_back_to_client_host_when_float_ran_on_target_host() {
        let escaped = VORSSAINT.replace(' ', "\\ ");
        let text = format!("check {escaped} and /home/johna/repo/logo.png");
        let mut reads = Vec::new();
        let out = carry_with(
            &["omarchy", "m4-mbp"],
            "omarchy",
            &text,
            |host, path| {
                reads.push((host.to_string(), path.to_string()));
                if host == "m4-mbp" && path == VORSSAINT {
                    ReadOutcome::Found(b"img".to_vec())
                } else {
                    ReadOutcome::Missing
                }
            },
            |host, file, _bytes| Ok(format!("/copied/{host}/{file}")),
        )
        .unwrap();
        // Skips "omarchy" (same as target) and checks only "m4-mbp".
        assert_eq!(
            reads,
            vec![
                ("m4-mbp".to_string(), VORSSAINT.to_string()),
                (
                    "m4-mbp".to_string(),
                    "/home/johna/repo/logo.png".to_string()
                ),
            ]
        );
        // Vorssaint image was rewritten; target-host logo.png was left untouched.
        assert!(out.contains("/copied/omarchy/"));
        assert!(out.ends_with(" and /home/johna/repo/logo.png"));
    }

    #[test]
    fn carry_leaves_local_images_alone_on_same_host_but_still_copies_rex_uploads() {
        let text = format!(
            "local {} upload {}",
            VORSSAINT.replace(' ', "\\ "),
            MAC.replace(' ', "\\ ")
        );
        let mut reads = Vec::new();
        let out = carry_with(
            &["m4-mbp"],
            "m4-mbp",
            &text,
            |host, path| {
                reads.push((host.to_string(), path.to_string()));
                ReadOutcome::Found(b"x".to_vec())
            },
            |_host, file, _bytes| Ok(format!("/state/{file}")),
        )
        .unwrap();
        assert_eq!(reads, vec![("m4-mbp".to_string(), MAC.to_string())]);
        assert!(out.starts_with(&format!(
            "local {} upload /state/",
            VORSSAINT.replace(' ', "\\ ")
        )));
    }

    #[test]
    fn carry_ignores_unreachable_source_for_local_image() {
        let text = "see /Users/j/shot.png please";
        let out = carry_with(
            &["m4-mbp"],
            "omarchy",
            text,
            |_host, _path| ReadOutcome::Unreachable,
            |_host, _file, _bytes| unreachable!("should not write when unreadable"),
        )
        .unwrap();
        assert_eq!(out, text);
    }

    #[test]
    fn remote_read_script_reads_regular_files_and_rejects_dirs_or_missing() {
        let root = std::env::temp_dir().join(format!(
            "switchyard-images-test-{}-{}",
            std::process::id(),
            now()
        ));
        let spaced_dir = root.join("Copied Screenshots");
        std::fs::create_dir_all(&spaced_dir).unwrap();
        let file = spaced_dir.join("Screenshot 2026-11-27 at 08.30.30.png");
        let payload = b"\x89PNG\r\n\x1a\nfake-image-bytes";
        std::fs::write(&file, payload).unwrap();
        let quoted_file = root.join("it's a -shot.png");
        std::fs::write(&quoted_file, b"quoted").unwrap();
        let dir_path = root.join("dir.png");
        std::fs::create_dir_all(&dir_path).unwrap();
        let missing = root.join("missing.png");

        let run_bash = |path: &std::path::Path| {
            let script = remote_read_script(path.to_str().unwrap());
            let mut child = Command::new("bash")
                .arg("-s")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        };

        let out = run_bash(&file);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, payload);

        let out = run_bash(&quoted_file);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"quoted");

        let out = run_bash(&dir_path);
        assert!(!out.status.success());
        assert_eq!(out.stdout, b"");

        let out = run_bash(&missing);
        assert!(!out.status.success());
        assert_eq!(out.stdout, b"");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_names_are_safe() {
        let n = copy_name(MAC, 2);
        assert!(n.ends_with("-2-Screenshot-1.png"), "{n}");
        assert!(copy_name("/x/rex/uploads/..'$(rm)", 0).ends_with("-0-rm-"));
    }
}
