//! The little HTTP/1.1 the LAN servers speak (Send over LAN, Receive over LAN):
//! reading a request head, and the few helpers around names in URLs and headers.
//! Deliberately minimal — one request per connection, `Connection: close` — since
//! the only clients are a phone's browser and the odd `curl`.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest request head accepted; a client that never ends its headers can't
/// grow the buffer past this.
const HEAD_MAX: usize = 64 * 1024;

/// A parsed request line and headers.
#[derive(Debug, Default)]
pub struct Head {
    pub method: String,
    /// The request target as sent: path and query, still percent-encoded.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The target's path, without the query.
    pub fn path(&self) -> &str {
        self.target.split_once('?').map_or(self.target.as_str(), |(p, _)| p)
    }

    /// A query parameter, percent-decoded. `None` when absent or not valid UTF-8.
    pub fn query(&self, name: &str) -> Option<String> {
        let (_, query) = self.target.split_once('?')?;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| percent_decode(v))
    }
}

/// Read a request head, waiting at most `idle` for each chunk. Returns the head
/// and whatever arrived after it — the start of the body, which a caller that
/// reads one must not lose. `None` when the client sent no complete head (closed,
/// went quiet, or sent too much).
pub async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
    idle: Duration,
) -> std::io::Result<Option<(Head, Vec<u8>)>> {
    let mut req = Vec::new();
    let mut buf = [0u8; 4096];
    let end = loop {
        if let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if req.len() > HEAD_MAX {
            return Ok(None);
        }
        let n = match tokio::time::timeout(idle, stream.read(&mut buf)).await {
            Ok(r) => r?,
            Err(_) => return Ok(None),
        };
        if n == 0 {
            return Ok(None);
        }
        req.extend_from_slice(&buf[..n]);
    };
    let text = String::from_utf8_lossy(&req[..end]);
    let mut lines = text.split("\r\n");
    let mut request = lines.next().unwrap_or("").split_whitespace();
    let head = Head {
        method: request.next().unwrap_or("").to_string(),
        target: request.next().unwrap_or("/").to_string(),
        headers: lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect(),
    };
    Ok(Some((head, req[end + 4..].to_vec())))
}

/// Write a small complete response with a `text/plain` (or given) body.
pub async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Sanitize a name for a `Content-Disposition` header value: keep only the base
/// name and drop quotes / backslashes / control characters that would break the
/// header or let a name escape into a path.
pub fn header_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect()
}

/// Percent-encode a file's base name for use in a URL path.
pub fn url_encode(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out = String::with_capacity(base.len());
    for b in base.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode `%XX` escapes (as `encodeURIComponent` writes them). A `+` stays a
/// plus: this is a URL, not a submitted form. `None` for a broken escape or a
/// result that isn't UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_head_is_parsed_and_the_body_after_it_kept() {
        let raw = b"PUT /t0k/upload?name=a%20b.txt&x=1 HTTP/1.1\r\nHost: h\r\ncontent-length: 11\r\n\r\nhello";
        let mut src: &[u8] = raw;
        let (head, rest) = read_head(&mut src, Duration::from_secs(1)).await.unwrap().unwrap();
        assert_eq!(head.method, "PUT");
        assert_eq!(head.path(), "/t0k/upload");
        assert_eq!(head.query("name").as_deref(), Some("a b.txt"));
        assert_eq!(head.query("missing"), None);
        assert_eq!(head.header("Content-Length"), Some("11"), "names match in any case");
        assert_eq!(rest, b"hello");
    }

    #[tokio::test]
    async fn a_head_that_never_ends_is_refused() {
        let mut src: &[u8] = b"GET / HTTP/1.1\r\nHost: h\r\n";
        assert!(read_head(&mut src, Duration::from_secs(1)).await.unwrap().is_none());
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("Caf%C3%A9%20menu+1.pdf").as_deref(), Some("Café menu+1.pdf"));
        assert_eq!(percent_decode("bad%2"), None);
        assert_eq!(percent_decode("%ff"), None, "not UTF-8");
    }

    #[test]
    fn url_encode_keeps_base_name_and_escapes_specials() {
        assert_eq!(url_encode("/tmp/dir/Report (final).pdf"), "Report%20%28final%29.pdf");
        assert_eq!(url_encode("plain-name_1.0.tar.gz"), "plain-name_1.0.tar.gz");
    }

    #[test]
    fn header_filename_strips_dangerous_chars() {
        // Path separators reduce to the base name; a stray quote is dropped.
        assert_eq!(header_filename("../etc/pass\"wd"), "passwd");
        assert_eq!(header_filename("a\\b\\c.txt"), "c.txt");
        assert_eq!(header_filename("photo.jpg"), "photo.jpg");
    }
}
