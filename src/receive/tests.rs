use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn names_are_reduced_to_a_plain_file_name() {
    assert_eq!(sanitize_name("photo.jpg").as_deref(), Some("photo.jpg"));
    assert_eq!(sanitize_name("../../etc/passwd").as_deref(), Some("passwd"));
    assert_eq!(sanitize_name("C:\\Users\\me\\notes.txt").as_deref(), Some("notes.txt"));
    assert_eq!(sanitize_name("bad\u{0}name\n.txt").as_deref(), Some("badname.txt"));
    assert_eq!(sanitize_name(".bashrc").as_deref(), Some(".bashrc"));
    for refused in ["", "  ", ".", "..", "dir/", "a/.."] {
        assert_eq!(sanitize_name(refused), None, "{refused:?}");
    }
    let long = format!("{}.txt", "é".repeat(200));
    let cut = sanitize_name(&long).unwrap();
    assert!(cut.len() <= NAME_MAX && long.starts_with(&cut), "cut on a character boundary");
}

#[test]
fn a_taken_name_gets_a_number_before_its_extension() {
    assert_eq!(numbered_name("photo.jpg", 0), "photo.jpg");
    assert_eq!(numbered_name("photo.jpg", 2), "photo (2).jpg");
    assert_eq!(numbered_name("README", 1), "README (1)");
    assert_eq!(numbered_name(".env", 1), ".env (1)");
}

fn scratch(tag: &str) -> PathBuf {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("rc_recv_{tag}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Send a raw request and read the whole response.
async fn request(port: u16, raw: &[u8]) -> String {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    sock.write_all(raw).await.unwrap();
    let mut resp = Vec::new();
    sock.read_to_end(&mut resp).await.unwrap();
    String::from_utf8_lossy(&resp).into_owned()
}

fn put(token: &str, name: &str, body: &[u8]) -> Vec<u8> {
    let mut raw = format!(
        "PUT /{token}/upload?name={name} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    raw.extend_from_slice(body);
    raw
}

/// Names of the files in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<AppEvent>) -> AppEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an event arrives")
            .expect("channel open");
        if !matches!(ev, AppEvent::ReceiveProgress { .. }) {
            return ev;
        }
    }
}

#[tokio::test]
async fn an_upload_is_saved_and_reported() {
    let dir = scratch("save");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let (port, server) = start(dir.clone(), "tok".into(), tx).unwrap();

    let body = b"picture bytes \xff\x00".repeat(1000);
    let resp = request(port, &put("tok", "Holiday%20photo.jpg", &body)).await;
    assert!(resp.starts_with("HTTP/1.1 201"), "{resp}");
    assert!(resp.ends_with("Holiday photo.jpg"), "the page is told the saved name");
    assert_eq!(std::fs::read(dir.join("Holiday photo.jpg")).unwrap(), body);
    assert!(matches!(
        next_event(&mut rx).await,
        AppEvent::FileReceived { name, bytes } if name == "Holiday photo.jpg" && bytes == body.len() as u64
    ));

    // The same name again doesn't overwrite: the second copy is numbered.
    let resp = request(port, &put("tok", "Holiday%20photo.jpg", b"second")).await;
    assert!(resp.ends_with("Holiday photo (1).jpg"), "{resp}");
    assert_eq!(std::fs::read(dir.join("Holiday photo.jpg")).unwrap(), body, "untouched");
    assert_eq!(listing(&dir), ["Holiday photo (1).jpg", "Holiday photo.jpg"], "no .part left");

    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn without_the_token_nothing_is_served_or_written() {
    let dir = scratch("token");
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let (port, server) = start(dir.clone(), "secret".into(), tx).unwrap();

    assert!(request(port, &put("guess", "x.txt", b"data")).await.starts_with("HTTP/1.1 404"));
    assert!(request(port, b"GET / HTTP/1.1\r\n\r\n").await.starts_with("HTTP/1.1 404"));
    assert!(request(port, b"GET /secret/../ HTTP/1.1\r\n\r\n").await.starts_with("HTTP/1.1 404"));
    let resp = request(port, &put("secret", "..", b"data")).await;
    assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
    assert!(listing(&dir).is_empty());

    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_page_is_served_with_the_directory_named() {
    let dir = scratch("page");
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let (port, server) = start(dir.clone(), "tok".into(), tx).unwrap();

    let resp = request(port, b"GET /tok/ HTTP/1.1\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("Choose files"));
    assert!(resp.contains(&*dir.file_name().unwrap().to_string_lossy()));
    let resp = request(port, b"GET /tok HTTP/1.1\r\n\r\n").await;
    assert!(resp.starts_with("HTTP/1.1 301") && resp.contains("Location: /tok/"), "{resp}");

    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_cut_off_upload_leaves_nothing_behind() {
    let dir = scratch("cut");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let (port, server) = start(dir.clone(), "tok".into(), tx).unwrap();

    let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    sock.write_all(
        b"PUT /tok/upload?name=big.iso HTTP/1.1\r\nContent-Length: 100000\r\n\r\npartial",
    )
    .await
    .unwrap();
    drop(sock);
    assert!(
        matches!(next_event(&mut rx).await, AppEvent::ReceiveFailed { name, .. } if name == "big.iso")
    );
    assert!(listing(&dir).is_empty(), "{:?}", listing(&dir));

    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn closing_the_dialog_aborts_an_upload_in_flight() {
    let dir = scratch("cancel");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let (port, server) = start(dir.clone(), "tok".into(), tx).unwrap();

    let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    sock.write_all(
        b"PUT /tok/upload?name=slow.bin HTTP/1.1\r\nContent-Length: 100000\r\n\r\nstart",
    )
    .await
    .unwrap();
    // Wait until the upload is under way, then shut the server down.
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap(),
        AppEvent::ReceiveProgress { .. }
    ));
    assert_eq!(listing(&dir).len(), 1, "the part file exists while it runs");
    server.shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !listing(&dir).is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(listing(&dir).is_empty(), "the partial file is removed");
    drop(sock);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_url_ends_in_the_token_directory() {
    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 7));
    assert_eq!(url_for(ip, 8080, "abc"), "http://192.168.1.7:8080/abc/");
}
