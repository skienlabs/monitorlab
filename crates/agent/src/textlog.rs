//! Reading and following text log files (UTF-8 or UTF-16LE such as the SQL Server ERRORLOG).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use monitorlab_common::{AgentMsg, LogLine, StreamData};

const MAX_LINE: usize = 32 * 1024;
const MAX_CHUNK: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enc {
    Utf8,
    Utf16Le,
}

pub fn detect_encoding(path: &Path) -> std::io::Result<Enc> {
    let mut f = File::open(path)?;
    let mut b = [0u8; 4];
    let n = f.read(&mut b)?;
    if n >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        return Ok(Enc::Utf16Le);
    }
    // Heuristic for BOM-less UTF-16LE: ASCII text with zero high bytes.
    if n == 4 && b[1] == 0 && b[3] == 0 && b[0] != 0 && b[2] != 0 {
        return Ok(Enc::Utf16Le);
    }
    Ok(Enc::Utf8)
}

pub fn decode(bytes: &[u8], enc: Enc) -> String {
    match enc {
        Enc::Utf8 => {
            let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            String::from_utf8_lossy(b).into_owned()
        }
        Enc::Utf16Le => {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let s = String::from_utf16_lossy(&units);
            s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s)
        }
    }
}

fn clean_line(l: &str) -> String {
    let l = l.strip_suffix('\r').unwrap_or(l);
    crate::util::truncate(l.to_string(), MAX_LINE)
}

/// Last `n` lines of a file, reading at most `max_bytes` from its end.
pub fn tail_lines(path: &Path, n: usize, max_bytes: u64) -> std::io::Result<Vec<String>> {
    let enc = detect_encoding(path)?;
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let unit: u64 = if enc == Enc::Utf16Le { 2 } else { 1 };
    let mut start = len.saturating_sub(max_bytes);
    if enc == Enc::Utf16Le && start % 2 == 1 {
        start += 1;
    }
    // Read one extra character before the window: the first (possibly partial) segment is
    // always discarded below, and if that character is a newline nothing real is lost.
    if start > 0 {
        start = start.saturating_sub(unit);
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    let text = decode(&buf, enc);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // probably a partial line
    }
    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(n);
    Ok(lines[skip..].iter().map(|l| clean_line(l)).collect())
}

/// Decoded text of the first `n` bytes of a file.
pub fn head_text(path: &Path, n: u64) -> std::io::Result<String> {
    let enc = detect_encoding(path)?;
    let f = File::open(path)?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf)?;
    if enc == Enc::Utf16Le && buf.len() % 2 == 1 {
        buf.pop();
    }
    Ok(decode(&buf, enc))
}

pub fn to_loglines(lines: Vec<String>) -> Vec<LogLine> {
    lines.into_iter().map(|text| LogLine { ts: None, s: 0, text, ..Default::default() }).collect()
}

struct FollowState {
    path: Option<PathBuf>,
    enc: Enc,
    offset: u64,
    partial: String,
}

/// Follow a file (like `tail -F`). `resolve` is re-evaluated every poll so that daily
/// rotated logs (IIS) or a new ERRORLOG after restart are picked up automatically.
pub async fn follow<F>(resolve: F, tail: u32, sid: u64, out: &mpsc::Sender<AgentMsg>) -> Result<()>
where
    F: Fn() -> Option<PathBuf> + Send + Sync + 'static,
{
    let resolve = std::sync::Arc::new(resolve);
    let first = {
        let r = resolve.clone();
        tokio::task::spawn_blocking(move || r()).await?
    };
    let Some(first) = first else {
        anyhow::bail!("log file not found");
    };

    // Initial tail.
    let (lines, enc, len) = {
        let p = first.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<(Vec<String>, Enc, u64)> {
            let enc = detect_encoding(&p)?;
            let len = std::fs::metadata(&p)?.len();
            let lines = if tail > 0 { tail_lines(&p, tail as usize, 4 * 1024 * 1024)? } else { vec![] };
            Ok((lines, enc, len))
        })
        .await??
    };
    if !lines.is_empty() {
        send_lines(out, sid, lines).await?;
    }

    let mut st = FollowState { path: Some(first), enc, offset: len, partial: String::new() };
    loop {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let r = resolve.clone();
        let prev_path = st.path.clone();
        let prev_off = st.offset;
        let prev_enc = st.enc;
        let res = tokio::task::spawn_blocking(move || poll_once(r.as_ref(), prev_path, prev_off, prev_enc)).await?;
        match res {
            Ok(Some(PollResult { path, enc, offset, text, switched })) => {
                if switched {
                    st.partial.clear();
                    send_info(out, sid, format!("Switched to {}", path.display())).await?;
                }
                st.path = Some(path);
                st.enc = enc;
                st.offset = offset;
                if !text.is_empty() {
                    st.partial.push_str(&text);
                    if let Some(idx) = st.partial.rfind('\n') {
                        let complete: String = st.partial.drain(..=idx).collect();
                        let lines: Vec<String> =
                            complete.trim_end_matches('\n').split('\n').map(clean_line).collect();
                        send_lines(out, sid, lines).await?;
                    } else if st.partial.len() > MAX_LINE {
                        let l = std::mem::take(&mut st.partial);
                        send_lines(out, sid, vec![clean_line(&l)]).await?;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("follow poll error: {e}");
            }
        }
    }
}

struct PollResult {
    path: PathBuf,
    enc: Enc,
    offset: u64,
    text: String,
    switched: bool,
}

fn poll_once<F: Fn() -> Option<PathBuf>>(
    resolve: &F,
    prev: Option<PathBuf>,
    mut offset: u64,
    mut enc: Enc,
) -> std::io::Result<Option<PollResult>> {
    let Some(path) = resolve() else { return Ok(None) };
    let mut switched = false;
    if prev.as_ref() != Some(&path) {
        switched = true;
        offset = 0;
        enc = detect_encoding(&path)?;
    }
    let len = std::fs::metadata(&path)?.len();
    if len < offset {
        // Truncated / recreated.
        offset = 0;
        enc = detect_encoding(&path)?;
    }
    if len == offset {
        return Ok(Some(PollResult { path, enc, offset, text: String::new(), switched }));
    }
    let mut to_read = (len - offset).min(MAX_CHUNK);
    if enc == Enc::Utf16Le && to_read % 2 == 1 {
        to_read -= 1;
    }
    let mut f = File::open(&path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; to_read as usize];
    f.read_exact(&mut buf)?;
    let text = decode(&buf, enc);
    Ok(Some(PollResult { path, enc, offset: offset + to_read, text, switched }))
}

async fn send_lines(out: &mpsc::Sender<AgentMsg>, sid: u64, lines: Vec<String>) -> Result<()> {
    for chunk in lines.chunks(500) {
        out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines: to_loglines(chunk.to_vec()) } })
            .await
            .map_err(|_| anyhow::anyhow!("connection closed"))?;
    }
    Ok(())
}

async fn send_info(out: &mpsc::Sender<AgentMsg>, sid: u64, msg: String) -> Result<()> {
    out.send(AgentMsg::Stream { sid, data: StreamData::Info { msg } })
        .await
        .map_err(|_| anyhow::anyhow!("connection closed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("monitorlab-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn utf16_tail() {
        let p = tmp("u16");
        let mut f = File::create(&p).unwrap();
        let text = "line one\r\nline two\r\nline three\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        f.write_all(&bytes).unwrap();
        drop(f);
        assert_eq!(detect_encoding(&p).unwrap(), Enc::Utf16Le);
        assert_eq!(tail_lines(&p, 2, 1 << 20).unwrap(), vec!["line two", "line three"]);
        // odd max_bytes must still align correctly
        let t = tail_lines(&p, 5, 25).unwrap();
        assert_eq!(t, vec!["line three"]);
        assert_eq!(t.last().unwrap(), "line three");
        assert!(head_text(&p, 11).unwrap().starts_with("line"));
    }

    #[test]
    fn utf8_tail() {
        let p = tmp("u8");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(tail_lines(&p, 2, 1 << 20).unwrap(), vec!["b", "c"]);
        assert_eq!(tail_lines(&p, 10, 1 << 20).unwrap(), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn follow_appends() {
        let p = tmp("follow");
        std::fs::write(&p, "old1\nold2\n").unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let pp = p.clone();
        let task = tokio::spawn(async move { follow(move || Some(pp.clone()), 1, 9, &tx).await });
        let first = rx.recv().await.unwrap();
        match first {
            AgentMsg::Stream { data: StreamData::Lines { lines }, .. } => assert_eq!(lines[0].text, "old2"),
            other => panic!("{other:?}"),
        }
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"new1\nnew").unwrap();
        f.flush().unwrap();
        let next = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
        match next {
            AgentMsg::Stream { data: StreamData::Lines { lines }, .. } => {
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].text, "new1");
            }
            other => panic!("{other:?}"),
        }
        task.abort();
    }
}
