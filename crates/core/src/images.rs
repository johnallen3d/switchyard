//! Images (and other files) pasted into a prompt box. Rex uploads a pasted
//! or dropped image to the machine running the terminal, here the float's
//! host, and pastes its shell-escaped path. That path means nothing on
//! another host, and Rex removes uploads after a while, so before a prompt
//! reaches an agent `carry` copies each upload to the agent's host and puts
//! the copy's path in its place.

use std::io::Write;
use std::ops::Range;
use std::process::{Command, Stdio};

use crate::rex::{host_bash, local_label, now, sh_quote, ssh};

/// The directory Rex keeps uploads in, wherever its state lives (on macOS,
/// `~/Library/Application Support/rex/uploads`).
const UPLOADS: &str = "/rex/uploads/";

/// Where copies go on the agent's host, relative to its state directory;
/// a copy is removed after `KEEP_DAYS`.
pub const IMAGE_DIR: &str = "rex/spawn-images";
const KEEP_DAYS: u32 = 7;

/// A pasted upload path: where it sits in the text, and the path itself.
#[derive(Debug, PartialEq)]
struct Upload {
    span: Range<usize>,
    path: String,
}

/// The upload paths pasted into `text`: whole words, shell-escaped or
/// quoted as Rex pastes them, naming a file in Rex's uploads directory.
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
        if word.starts_with('/') && word.contains(UPLOADS) && !word.ends_with('/') {
            found.push(Upload { span: i..i + len, path: word });
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

/// `text` with each upload pasted into it (a file on `from`) copied to
/// `to` and replaced by the copy's path. Text without uploads comes back
/// as it is. Fails, rather than hand the agent a path it can't open, when
/// an upload is gone (Rex keeps them only for a while) or can't be copied.
pub fn carry(from: &str, to: &str, text: &str) -> Result<String, String> {
    let found = uploads(text);
    if found.is_empty() {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (n, upload) in found.iter().enumerate() {
        let bytes = read(from, &upload.path)?;
        let copy = write(to, &copy_name(&upload.path, n), &bytes)?;
        out.push_str(&text[last..upload.span.start]);
        out.push_str(&copy);
        last = upload.span.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// A copy's file name: unique to this call and position, keeping the
/// upload's own name (made safe for a path and a script) for the agent.
fn copy_name(path: &str, n: usize) -> String {
    let safe: String = name(path)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '-' })
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

/// The bytes of the file at `path` on `host`.
fn read(host: &str, path: &str) -> Result<Vec<u8>, String> {
    let gone = || format!("pasted file {} is gone; paste it again", name(path));
    if host == local_label() {
        return std::fs::read(path).map_err(|_| gone());
    }
    let out = host_bash(host, &format!("cat -- {}\n", sh_quote(path)))
        .ok_or_else(|| format!("can't reach {host} for pasted file {}", name(path)))?;
    if !out.status.success() {
        return Err(gone());
    }
    Ok(out.stdout)
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
    let out = child.wait_with_output().map_err(|e| failed(e.to_string()))?;
    let path = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() || !path.starts_with('/') {
        return Err(failed(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "/Users/j/Library/Application Support/rex/uploads/p1/b2/Screenshot 1.png";

    #[test]
    fn finds_escaped_and_quoted_upload_paths() {
        let escaped = MAC.replace(' ', "\\ ");
        let text = format!("look at {escaped} and '{MAC}' or \"{MAC}\"\nthanks");
        let found = uploads(&text);
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|u| u.path == MAC));
        assert_eq!(&text[found[0].span.clone()], escaped);
        assert_eq!(&text[found[1].span.clone()], format!("'{MAC}'"));
    }

    #[test]
    fn leaves_other_paths_and_words_alone() {
        assert!(uploads("see /etc/hosts and ~/rex/uploads/x.png, rex/uploads/y.png").is_empty());
        assert!(uploads("/home/j/.local/state/rex/uploads/").is_empty());
        assert!(uploads("").is_empty());
        let linux = uploads("é /home/j/.local/state/rex/uploads/a/b/c.png");
        assert_eq!(linux[0].path, "/home/j/.local/state/rex/uploads/a/b/c.png");
    }

    #[test]
    fn carry_keeps_text_without_uploads() {
        assert_eq!(carry("nowhere", "nowhere", "just text").unwrap(), "just text");
    }

    #[test]
    fn carry_fails_on_a_missing_upload() {
        let me = local_label();
        let err = carry(&me, &me, "x /tmp/no-such/rex/uploads/gone.png").unwrap_err();
        assert!(err.contains("gone.png is gone"), "{err}");
    }

    #[test]
    fn copy_names_are_safe() {
        let n = copy_name(MAC, 2);
        assert!(n.ends_with("-2-Screenshot-1.png"), "{n}");
        assert!(copy_name("/x/rex/uploads/..'$(rm)", 0).ends_with("-0-rm-"));
    }
}
