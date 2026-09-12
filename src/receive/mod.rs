//! LAN "Receive files" support: the reverse of [`crate::send`]. A tiny
//! ephemeral HTTP server serves an upload page to whatever device scans the QR
//! code, and saves what it sends into the active panel's directory.
//!
//! The page uploads each file as the raw body of its own `PUT`, named in the
//! query string, which spares the server a multipart parser: the browser's
//! `XMLHttpRequest` always sends a `File` with a `Content-Length`, and reports
//! progress while it does.
//!
//! **Safety.** The server listens on the whole subnet, so every URL carries a
//! random token and anything else gets a 404 before its body is read. A name is
//! reduced to a plain file name, so nothing can land outside the directory. A
//! file is written under a hidden `.part` name and only renamed into place once
//! complete — never over an existing file, which gets a ` (1)` name instead —
//! and a transfer that fails, stalls or is cancelled leaves nothing behind.

use crate::app::event::AppEvent;
use crate::ops::CancelToken;
use crate::util::async_bridge::AppSender;
use crate::util::http::{Head, read_head, respond};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

#[cfg(test)]
mod tests;

/// The upload page, with `{{DIR}}` standing for the target directory's name.
const PAGE: &str = include_str!("page.html");
/// How long a connection may sit silent before it is dropped.
const IDLE: Duration = Duration::from_secs(30);
/// Connections served at once; more get a 503 rather than a queue.
const MAX_CONNECTIONS: usize = 8;
/// Longest file name kept, in bytes (the limit of most filesystems).
const NAME_MAX: usize = 255;
/// How often an upload in flight reports its progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A running receive server, and what it takes to stop it.
pub struct ReceiveServer {
    handle: JoinHandle<()>,
    cancel: CancelToken,
    /// The directory files are saved into.
    pub dir: PathBuf,
}

impl ReceiveServer {
    /// Stop accepting, and abort every upload still in flight — each removes its
    /// partial file on the way out.
    pub fn shutdown(self) {
        self.cancel.cancel();
        self.handle.abort();
    }
}

/// Start receiving into `dir`, behind `token`. Binds all interfaces on a free
/// port, known on return for the URL. Progress, completed files and failures
/// are reported on `tx`.
pub fn start(dir: PathBuf, token: String, tx: AppSender) -> std::io::Result<(u16, ReceiveServer)> {
    let std_listener = std::net::TcpListener::bind(("0.0.0.0", 0))?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let listener = TcpListener::from_std(std_listener)?;
    let cancel = CancelToken::new();
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let server = Arc::new(Server { dir: dir.clone(), token, tx });
    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            loop {
                let accepted = tokio::select! {
                    a = listener.accept() => a,
                    _ = cancel.cancelled() => break,
                };
                let Ok((mut stream, _peer)) = accepted else { break };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    let _ = respond(&mut stream, "503 Service Unavailable", "text/plain", b"Busy")
                        .await;
                    continue;
                };
                let server = server.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    tokio::select! {
                        _ = server.serve(stream) => {}
                        _ = cancel.cancelled() => {}
                    }
                });
            }
        }
    });
    Ok((port, ReceiveServer { handle, cancel, dir }))
}

/// The upload URL to advertise: `http://<ip>:<port>/<token>/`.
pub fn url_for(ip: std::net::IpAddr, port: u16, token: &str) -> String {
    let host = match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    };
    format!("http://{host}:{port}/{token}/")
}

struct Server {
    dir: PathBuf,
    token: String,
    tx: AppSender,
}

impl Server {
    async fn serve(&self, mut stream: TcpStream) {
        let Ok(Some((head, body_start))) = read_head(&mut stream, IDLE).await else { return };
        let root = format!("/{}", self.token);
        let upload = format!("/{}/upload", self.token);
        let method = head.method.to_ascii_uppercase();
        let _ = match (method.as_str(), head.path()) {
            ("GET" | "HEAD", p) if p == root => {
                // Relative upload URLs need the trailing slash.
                let location = format!(
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: {root}/\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(location.as_bytes()).await
            }
            ("GET" | "HEAD", p) if p.strip_suffix('/') == Some(root.as_str()) => {
                respond(&mut stream, "200 OK", "text/html; charset=utf-8", self.page().as_bytes())
                    .await
            }
            ("PUT" | "POST", p) if p == upload => self.upload(&mut stream, &head, body_start).await,
            _ => respond(&mut stream, "404 Not Found", "text/plain", b"Not found").await,
        };
    }

    fn page(&self) -> String {
        let name = self
            .dir
            .file_name()
            .map_or_else(|| self.dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        PAGE.replace("{{DIR}}", &html_escape(&name))
    }

    /// Receive one file: its name from the query, its bytes as the body.
    async fn upload(
        &self,
        stream: &mut TcpStream,
        head: &Head,
        body_start: Vec<u8>,
    ) -> std::io::Result<()> {
        let Some(name) = head.query("name").and_then(|n| sanitize_name(&n)) else {
            return respond(stream, "400 Bad Request", "text/plain", b"Not a usable file name")
                .await;
        };
        if head.header("Transfer-Encoding").is_some() {
            return respond(stream, "411 Length Required", "text/plain", b"Send a length").await;
        }
        let Some(total) = head.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) else {
            return respond(stream, "411 Length Required", "text/plain", b"Send a length").await;
        };
        if free_space(&self.dir).is_some_and(|free| free < total) {
            let msg = b"Not enough space on the receiving disk";
            return respond(stream, "507 Insufficient Storage", "text/plain", msg).await;
        }
        // `curl` waits for this before sending a large body.
        if head.header("Expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
            stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        }

        let part = match PartFile::create(&self.dir).await {
            Ok(part) => part,
            Err(e) => {
                let _ = self.tx.try_send(AppEvent::ReceiveFailed { name, error: e.to_string() });
                let msg = b"Cannot write into the receiving directory";
                return respond(stream, "500 Internal Server Error", "text/plain", msg).await;
            }
        };
        match self.receive_body(stream, &part, &name, total, body_start).await {
            Ok(()) => {}
            Err(e) => {
                let _ = self.tx.try_send(AppEvent::ReceiveFailed { name, error: e.to_string() });
                return Ok(()); // the part file is removed as it drops
            }
        }
        match part.keep(&self.dir, &name).await {
            Ok(saved) => {
                let _ = self
                    .tx
                    .send(AppEvent::FileReceived { name: saved.clone(), bytes: total })
                    .await;
                respond(stream, "201 Created", "text/plain; charset=utf-8", saved.as_bytes()).await
            }
            Err(e) => {
                let _ = self.tx.try_send(AppEvent::ReceiveFailed { name, error: e.to_string() });
                respond(
                    stream,
                    "500 Internal Server Error",
                    "text/plain",
                    b"Could not save the file",
                )
                .await
            }
        }
    }

    /// Copy exactly `total` body bytes into `part`, reporting progress.
    async fn receive_body(
        &self,
        stream: &mut TcpStream,
        part: &PartFile,
        name: &str,
        total: u64,
        body_start: Vec<u8>,
    ) -> std::io::Result<()> {
        let mut file = part.file().await?;
        let first = body_start.len().min(total as usize);
        file.write_all(&body_start[..first]).await?;
        let mut received = first as u64;
        let mut buf = vec![0u8; 256 * 1024];
        let mut last_report = Instant::now();
        let _ =
            self.tx.try_send(AppEvent::ReceiveProgress { name: name.to_string(), received, total });
        while received < total {
            let want = buf.len().min((total - received) as usize);
            let n = match tokio::time::timeout(IDLE, stream.read(&mut buf[..want])).await {
                Ok(r) => r?,
                Err(_) => return Err(std::io::Error::other("the sender went quiet")),
            };
            if n == 0 {
                return Err(std::io::Error::other("the transfer was cut off"));
            }
            file.write_all(&buf[..n]).await?;
            received += n as u64;
            if last_report.elapsed() >= PROGRESS_EVERY {
                last_report = Instant::now();
                let _ = self.tx.try_send(AppEvent::ReceiveProgress {
                    name: name.to_string(),
                    received,
                    total,
                });
            }
        }
        file.flush().await?;
        Ok(())
    }
}

/// A hidden file an upload is written to, removed on drop unless kept.
struct PartFile {
    path: PathBuf,
    kept: bool,
}

impl PartFile {
    async fn create(dir: &Path) -> std::io::Result<Self> {
        loop {
            let path = dir.join(format!(".rc-upload-{}.part", crate::util::rng::token(6)));
            match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await {
                Ok(_) => return Ok(PartFile { path, kept: false }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    async fn file(&self) -> std::io::Result<tokio::fs::File> {
        tokio::fs::OpenOptions::new().write(true).open(&self.path).await
    }

    /// Move the finished file into place under the first free variant of
    /// `name`, returning the name it got. The final name is claimed with an
    /// exclusive create before the rename, so a file that appeared in the
    /// meantime is never overwritten; unlike a hard link, that works on FAT and
    /// exFAT sticks too.
    async fn keep(mut self, dir: &Path, name: &str) -> std::io::Result<String> {
        for n in 0.. {
            let candidate = numbered_name(name, n);
            let target = dir.join(&candidate);
            match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&target).await {
                Ok(_) => {
                    tokio::fs::rename(&self.path, &target).await?;
                    self.kept = true;
                    return Ok(candidate);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        unreachable!("an unbounded range")
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.kept {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Reduce a name a client sent to a plain file name: its last path component,
/// without control characters, within the length limit. `None` for nothing
/// usable, or a name that would mean the directory itself or its parent.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() || clean == "." || clean == ".." {
        return None;
    }
    let mut end = clean.len().min(NAME_MAX);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    Some(clean[..end].to_string())
}

/// `name` itself for `n == 0`, else `name (n)` before its extension:
/// `photo.jpg` → `photo (2).jpg`. A leading dot is not an extension.
pub fn numbered_name(name: &str, n: usize) -> String {
    if n == 0 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Bytes free for an unprivileged user on the filesystem holding `dir`, where
/// that can be asked.
#[cfg(unix)]
fn free_space(dir: &Path) -> Option<u64> {
    let st = nix::sys::statvfs::statvfs(dir).ok()?;
    Some(st.blocks_available() as u64 * st.fragment_size() as u64)
}

#[cfg(not(unix))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}
