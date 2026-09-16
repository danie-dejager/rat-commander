//! Remote VFS backends: SFTP and SCP (over SSH) and FTP/FTPS.
//!
//! Each connection is a distinct backend instance registered under a unique
//! scheme (e.g. `sftp-0`) so multiple sessions can coexist. Listing/transfer/
//! delete all flow through the [`Vfs`](crate::vfs::Vfs) trait, so the generic
//! ops engine handles cross-backend copy/move/delete for free.

pub mod auth;
pub mod ftp;
pub mod scp;
pub mod sftp;
pub mod sshconfig;

use crate::util::{Error, Result};
use crate::vfs::VfsKind;
use std::sync::Arc;

/// A directory entry parsed from a Unix `ls -l` / FTP `LIST` line.
pub(crate) struct ParsedListing {
    pub name: String,
    pub kind: VfsKind,
    pub size: u64,
    pub mode: Option<u32>,
    pub symlink_target: Option<String>,
}

/// Parse one Unix-style long listing line (handles both the classic
/// `Mon DD HH:MM` date and ISO `YYYY-MM-DD HH:MM`). Returns `None` for header
/// lines (`total N`), blanks, or `.`/`..`.
pub(crate) fn parse_unix_listing_line(line: &str) -> Option<ParsedListing> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with("total ") {
        return None;
    }
    let toks: Vec<&str> = line.split_whitespace().collect();
    if toks.len() < 8 {
        return None;
    }
    let perms = toks[0];
    if perms.len() < 10 {
        return None;
    }
    let type_char = perms.chars().next().unwrap();
    let kind = match type_char {
        'd' => VfsKind::Dir,
        'l' => VfsKind::Symlink,
        '-' => VfsKind::File,
        _ => VfsKind::Other,
    };
    let size = toks[4].parse::<u64>().unwrap_or(0);
    // Name starts after the date: ISO date (contains '-') uses 2 tokens, the
    // classic `Mon DD HH:MM`/`Mon DD YYYY` uses 3.
    let name_start = if toks[5].contains('-') { 7 } else { 8 };
    if toks.len() <= name_start {
        return None;
    }
    let rest = toks[name_start..].join(" ");

    let (name, symlink_target) = if kind == VfsKind::Symlink {
        match rest.split_once(" -> ") {
            Some((n, t)) => (n.to_string(), Some(t.to_string())),
            None => (rest, None),
        }
    } else {
        (rest, None)
    };
    if name == "." || name == ".." || name.is_empty() {
        return None;
    }
    Some(ParsedListing { name, kind, size, mode: Some(perms_to_mode(perms)), symlink_target })
}

/// Convert a `rwxr-xr-x` permission string (after the type char) to mode bits.
pub(crate) fn perms_to_mode(perms: &str) -> u32 {
    let bytes = perms.as_bytes();
    let mut mode = 0u32;
    // perms[1..10] = owner/group/other rwx.
    for (i, &b) in bytes.iter().skip(1).take(9).enumerate() {
        if b != b'-' {
            mode |= 1 << (8 - i);
        }
    }
    mode
}

/// Quote a path for safe use in a remote `sh -c` command (single-quoted).
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Which remote protocol a connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Sftp,
    Ftp,
    Scp,
}

impl Protocol {
    pub fn scheme_prefix(self) -> &'static str {
        match self {
            Protocol::Sftp => "sftp",
            Protocol::Ftp => "ftp",
            Protocol::Scp => "scp",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Sftp | Protocol::Scp => 22,
            Protocol::Ftp => 21,
        }
    }
}

/// Connection parameters collected from the connect dialog.
#[derive(Debug, Clone)]
pub struct RemoteCreds {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Initial remote directory (defaults to the server's choice if empty).
    pub path: String,
    /// FTP passive mode (PASV): the client opens the data connection. On by
    /// default and needed behind most NAT/firewalls. Ignored by SFTP/SCP, which
    /// tunnel data over the single SSH connection.
    pub passive: bool,
    /// SSH private key to authenticate with. Empty means "use the agent, then
    /// the usual `~/.ssh` defaults". Ignored by FTP.
    pub key_file: String,
    /// Passphrase for an encrypted `key_file`, collected by a prompt just before
    /// connecting. Held in memory for the attempt only and never persisted.
    pub key_passphrase: String,
}

/// A live remote connection: a VFS backend plus the directory to open.
pub struct Connection {
    pub backend: Arc<dyn crate::vfs::Vfs>,
    pub root: String,
    pub label: String,
}

/// Establish a remote connection of the requested protocol.
pub async fn connect(creds: &RemoteCreds) -> Result<Connection> {
    match creds.protocol {
        Protocol::Sftp => sftp::connect(creds).await,
        Protocol::Ftp => ftp::connect(creds).await,
        Protocol::Scp => scp::connect(creds).await,
    }
}

// ---------------------------------------------------------------------------
// Shared SSH client (used by SFTP and SCP)
// ---------------------------------------------------------------------------

/// russh client handler implementing trust-on-first-use against the user's
/// `~/.ssh/known_hosts`: a matching key is accepted, a *changed* key is
/// rejected (possible MITM), and an unknown host is accepted **and recorded**,
/// so a later key change for that host is detected rather than silently
/// trusted.
pub(crate) struct HostKeyHandler {
    host: String,
    port: u16,
}

impl russh::client::Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // russh 0.63 hands the handler either a bare host key or a host
        // certificate; `public_key()` yields the underlying key either way.
        let server_public_key = server_public_key.public_key();
        match russh::keys::check_known_hosts(&self.host, self.port, &server_public_key) {
            Ok(true) => Ok(true), // known host, key matches
            Ok(false) => {
                // Unknown host: trust on first use *and write it down*, so the
                // "first" in first-use means something and a swapped key later
                // trips the KeyChanged arm below.
                //
                // Not under `cfg(test)`: the in-process SSH tests dial
                // 127.0.0.1 on a throwaway port, and learning those would append
                // junk to the developer's real ~/.ssh/known_hosts on every run.
                #[cfg(not(test))]
                let _ = russh::keys::known_hosts::learn_known_hosts(
                    &self.host,
                    self.port,
                    &server_public_key,
                );
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { .. }) => Ok(false), // reject possible MITM
            Err(_) => Ok(true), // known_hosts unreadable — fall back to accepting
        }
    }
}

pub(crate) type SshHandle = russh::client::Handle<HostKeyHandler>;

/// An opened interactive shell channel on a remote SSH host — a PTY and shell are
/// already requested, so it is a live bidirectional byte stream. Wraps the russh
/// channel; the app drives it through [`crate::shell::RemoteShell`]. Only the
/// SSH-based backends (SFTP/SCP) can produce one.
pub struct RemoteShellChannel {
    pub channel: russh::Channel<russh::client::Msg>,
}

/// Open a session channel on `handle`, request a PTY of the given size and an
/// interactive shell, returning the ready channel.
pub(crate) async fn open_shell_channel(
    handle: &SshHandle,
    rows: u16,
    cols: u16,
) -> Result<RemoteShellChannel> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| Error::other(format!("shell channel open failed: {e}")))?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await
        .map_err(|e| Error::other(format!("request pty failed: {e}")))?;
    channel
        .request_shell(false)
        .await
        .map_err(|e| Error::other(format!("request shell failed: {e}")))?;
    Ok(RemoteShellChannel { channel })
}

/// Most jump hosts a connection goes through.
const MAX_HOPS: usize = 8;

/// One SSH connection on the way to a host: a jump host, or the host itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hop {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The key files to try, in order.
    pub keys: Vec<std::path::PathBuf>,
    /// Whether the agent's keys may be tried (not with `IdentitiesOnly`).
    pub agent: bool,
    /// The name the host's key is looked up by in `known_hosts`.
    pub key_host: String,
    /// The host itself: the only hop the password is offered to.
    pub last: bool,
}

/// The key files a host's settings name (those that exist), or the defaults.
fn config_keys(
    s: &sshconfig::HostSettings,
    host: &str,
    alias: &str,
    port: u16,
    user: &str,
) -> Vec<std::path::PathBuf> {
    if s.identity_files.is_empty() {
        return auth::default_key_paths();
    }
    s.identity_files
        .iter()
        .map(|f| sshconfig::expand_tilde(&sshconfig::expand_tokens(f, host, alias, port, user)))
        .filter(|p| p.is_file())
        .collect()
}

/// The hops a connection with `creds` goes through, as `~/.ssh/config`
/// (`cfg`) has it: its `ProxyJump` hosts in order, then the host. What the
/// connect form says wins over the config, as the command line does over it
/// for `ssh`: a user, a port other than the protocol's default, a key file.
pub(crate) fn plan(creds: &RemoteCreds, cfg: &sshconfig::SshConfig) -> Result<Vec<Hop>> {
    use sshconfig::{expand_tokens, jump_specs, local_user};
    let alias = creds.host.trim();
    let s = cfg.resolve(alias);
    if s.proxy_jump.is_none()
        && s.proxy_command.as_deref().is_some_and(|c| !c.eq_ignore_ascii_case("none"))
    {
        return Err(Error::other(format!(
            "{alias}: ProxyCommand in ~/.ssh/config isn't supported — use ProxyJump"
        )));
    }
    let port = if creds.port != creds.protocol.default_port() {
        creds.port
    } else {
        s.port.unwrap_or(creds.port)
    };
    let user = match creds.user.trim() {
        "" => s.user.clone().unwrap_or_else(local_user),
        typed => typed.to_string(),
    };
    let host = s
        .hostname
        .as_deref()
        .map_or_else(|| alias.to_string(), |h| expand_tokens(h, alias, alias, port, &user));
    let explicit = creds.key_file.trim();
    let keys = if explicit.is_empty() {
        config_keys(&s, &host, alias, port, &user)
    } else {
        vec![sshconfig::expand_tilde(explicit)]
    };
    let agent = !(s.identities_only && explicit.is_empty() && !s.identity_files.is_empty());
    let key_host = s.host_key_alias.clone().unwrap_or_else(|| host.clone());
    let mut hops = Vec::new();
    // A jump host's own ProxyJump is not followed: the list is the route.
    for spec in s.proxy_jump.as_deref().map(jump_specs).unwrap_or_default() {
        let js = cfg.resolve(&spec.host);
        let jport = spec.port.or(js.port).unwrap_or(22);
        let juser = spec.user.clone().or_else(|| js.user.clone()).unwrap_or_else(local_user);
        let jhost = js.hostname.as_deref().map_or_else(
            || spec.host.clone(),
            |h| expand_tokens(h, &spec.host, &spec.host, jport, &juser),
        );
        hops.push(Hop {
            keys: config_keys(&js, &jhost, &spec.host, jport, &juser),
            agent: !(js.identities_only && !js.identity_files.is_empty()),
            key_host: js.host_key_alias.clone().unwrap_or_else(|| jhost.clone()),
            host: jhost,
            port: jport,
            user: juser,
            last: false,
        });
    }
    hops.push(Hop { host, port, user, keys, agent, key_host, last: true });
    if hops.len() > MAX_HOPS + 1 {
        return Err(Error::other(format!("{alias}: more than {MAX_HOPS} jump hosts")));
    }
    for (i, h) in hops.iter().enumerate() {
        if hops[..i].iter().any(|o| o.host == h.host && o.port == h.port) {
            return Err(Error::other(format!(
                "{alias}: the route goes through {}:{} twice",
                h.host, h.port
            )));
        }
    }
    Ok(hops)
}

/// An authenticated SSH connection, and the connections to the jump hosts it
/// runs through — kept open for as long as it is.
pub(crate) struct SshSession {
    handle: SshHandle,
    _jumps: Vec<SshHandle>,
    /// The user logged in as.
    pub user: String,
}

impl std::ops::Deref for SshSession {
    type Target = SshHandle;

    fn deref(&self) -> &SshHandle {
        &self.handle
    }
}

/// Open an SSH connection and authenticate (agent, then keys, then password —
/// see [`auth::authenticate`]), through the jump hosts `~/.ssh/config` names.
pub(crate) async fn ssh_connect(creds: &RemoteCreds) -> Result<SshSession> {
    let hops = plan(creds, &sshconfig::SshConfig::load_user())?;
    connect_hops(&hops, creds).await
}

/// Connect along `hops`: each one reached through a tunnel on the one before,
/// its host key checked and its user authenticated in turn.
pub(crate) async fn connect_hops(hops: &[Hop], creds: &RemoteCreds) -> Result<SshSession> {
    let config = Arc::new(russh::client::Config::default());
    let mut open: Vec<SshHandle> = Vec::new();
    for hop in hops {
        let handler = HostKeyHandler { host: hop.key_host.clone(), port: hop.port };
        let via = |e: &dyn std::fmt::Display| match open.len() {
            0 => format!("SSH connect failed: {e}"),
            _ => format!("SSH connect to {} through a jump host failed: {e}", hop.host),
        };
        let mut handle = match open.last() {
            None => russh::client::connect(config.clone(), (hop.host.as_str(), hop.port), handler)
                .await
                .map_err(|e| Error::other(via(&e)))?,
            Some(prev) => {
                let channel = prev
                    .channel_open_direct_tcpip(hop.host.clone(), hop.port as u32, "127.0.0.1", 0)
                    .await
                    .map_err(|e| Error::other(via(&e)))?;
                russh::client::connect_stream(config.clone(), channel.into_stream(), handler)
                    .await
                    .map_err(|e| Error::other(via(&e)))?
            }
        };
        auth::authenticate(&mut handle, hop, creds).await.map_err(|e| {
            if hop.last { e } else { Error::other(format!("jump host {}: {e}", hop.host)) }
        })?;
        open.push(handle);
    }
    let handle = open.pop().ok_or_else(|| Error::other("no host to connect to"))?;
    let user = hops.last().map(|h| h.user.clone()).unwrap_or_default();
    Ok(SshSession { handle, _jumps: open, user })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(host: &str, port: u16, user: &str) -> RemoteCreds {
        RemoteCreds {
            protocol: Protocol::Sftp,
            host: host.into(),
            port,
            user: user.into(),
            password: String::new(),
            path: String::new(),
            passive: false,
            key_file: String::new(),
            key_passphrase: String::new(),
        }
    }

    const CONFIG: &str = "Host db\n  HostName db.internal\n  User dba\n  Port 2222\n  ProxyJump ops@bastion,gw:2200\n  HostKeyAlias db-key\nHost bastion\n  HostName bastion.example.com\n  Port 22022\nHost loop\n  ProxyJump loop\nHost legacy\n  ProxyCommand nc -X 5 -x proxy:1080 %h %p\n";

    #[test]
    fn the_route_follows_the_config_and_the_form_overrides_it() {
        let cfg = sshconfig::SshConfig::parse(CONFIG, std::path::Path::new("/nowhere"));
        let hops = plan(&creds("db", 22, ""), &cfg).unwrap();
        let route: Vec<(&str, u16, &str, bool)> =
            hops.iter().map(|h| (h.host.as_str(), h.port, h.user.as_str(), h.last)).collect();
        assert_eq!(
            route,
            vec![
                ("bastion.example.com", 22022, "ops", false),
                ("gw", 2200, sshconfig::local_user().as_str(), false),
                ("db.internal", 2222, "dba", true),
            ]
        );
        assert_eq!(hops[2].key_host, "db-key");
        // A user and a port typed into the form win.
        let hops = plan(&creds("db", 2022, "root"), &cfg).unwrap();
        let last = hops.last().unwrap();
        assert_eq!((last.port, last.user.as_str()), (2022, "root"));
        // A host the config doesn't know is connected to as typed.
        let hops = plan(&creds("plain.example", 22, "me"), &cfg).unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(
            (hops[0].host.as_str(), hops[0].key_host.as_str()),
            ("plain.example", "plain.example")
        );
    }

    #[test]
    fn loops_and_proxy_commands_are_refused() {
        let cfg = sshconfig::SshConfig::parse(CONFIG, std::path::Path::new("/nowhere"));
        let err = plan(&creds("loop", 22, "u"), &cfg).unwrap_err().to_string();
        assert!(err.contains("twice"), "{err}");
        let err = plan(&creds("legacy", 22, "u"), &cfg).unwrap_err().to_string();
        assert!(err.contains("ProxyCommand"), "{err}");
    }

    #[test]
    fn parses_classic_ls_line() {
        let p =
            parse_unix_listing_line("-rw-r--r-- 1 user group 1234 Jan  2 12:00 notes.txt").unwrap();
        assert_eq!(p.name, "notes.txt");
        assert_eq!(p.kind, VfsKind::File);
        assert_eq!(p.size, 1234);
        assert_eq!(p.mode, Some(0o644));
    }

    #[test]
    fn parses_iso_dir_and_symlink() {
        let dir = parse_unix_listing_line("drwxr-xr-x 2 u g 4096 2024-01-02 12:00 mydir").unwrap();
        assert_eq!(dir.name, "mydir");
        assert_eq!(dir.kind, VfsKind::Dir);
        assert_eq!(dir.mode, Some(0o755));

        let link =
            parse_unix_listing_line("lrwxrwxrwx 1 u g 7 2024-01-02 12:00 link -> target").unwrap();
        assert_eq!(link.name, "link");
        assert_eq!(link.kind, VfsKind::Symlink);
        assert_eq!(link.symlink_target.as_deref(), Some("target"));
    }

    #[test]
    fn skips_total_and_dot_entries() {
        assert!(parse_unix_listing_line("total 12").is_none());
        assert!(parse_unix_listing_line("drwxr-xr-x 2 u g 4096 2024-01-02 12:00 .").is_none());
        assert!(parse_unix_listing_line("drwxr-xr-x 2 u g 4096 2024-01-02 12:00 ..").is_none());
        assert!(parse_unix_listing_line("").is_none());
    }

    #[test]
    fn shell_quote_escapes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
