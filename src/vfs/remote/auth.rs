//! SSH user authentication for the SFTP/SCP backends.
//!
//! OpenSSH's own order is followed: the **agent** first, then **key files**,
//! then the **password**. Each step is silent on failure and falls through to
//! the next, so a host that takes any one of them connects, and a host that
//! takes none reports what was actually attempted — a chain that just says
//! "authentication failed" is impossible to debug.
//!
//! Key files are read on a blocking worker: decrypting one runs bcrypt-pbkdf,
//! which takes long enough (~100ms) to stutter the render loop that awaits this.

use super::sshconfig::home_dir;
use super::{Hop, RemoteCreds, SshHandle};
use crate::util::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The private keys tried when the connect dialog names none, most-preferred
/// first (the same set and order OpenSSH uses). Only files that exist are
/// returned, so a missing `~/.ssh` simply yields nothing.
pub(crate) fn default_key_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let ssh = home.join(".ssh");
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| ssh.join(name))
        .filter(|p| p.is_file())
        .collect()
}

/// Whether this key file is encrypted and so needs a passphrase before it can
/// be used. Cheap: the format check fails before any KDF work is done.
pub(crate) fn needs_passphrase(path: &Path) -> bool {
    matches!(russh::keys::load_secret_key(path, None), Err(russh::keys::Error::KeyIsEncrypted))
}

/// The name of the first key any hop would try that needs a passphrase, if
/// any. Used as a pre-flight probe: the connect itself blocks the render loop,
/// so the passphrase has to be collected *before* it starts rather than half
/// way through.
pub(crate) fn first_encrypted_key(creds: &RemoteCreds) -> Option<String> {
    if !matches!(creds.protocol, super::Protocol::Sftp | super::Protocol::Scp) {
        return None;
    }
    let hops = super::plan(creds, &super::sshconfig::SshConfig::load_user()).ok()?;
    hops.iter().flat_map(|h| &h.keys).find(|p| needs_passphrase(p)).map(|p| display_path(p))
}

/// Authenticate `handle` as `hop.user` with the hop's keys — and, on the last
/// hop, the password from `creds`. Returns once some method succeeds. An
/// explicit key file is used even if it doesn't exist, so a typo surfaces as
/// an error instead of silently falling back.
pub(crate) async fn authenticate(
    handle: &mut SshHandle,
    hop: &Hop,
    creds: &RemoteCreds,
) -> Result<()> {
    // RSA keys need the hash the server actually supports; for every other key
    // type this is ignored. Failing to negotiate one is not fatal.
    let rsa_hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    let mut tried: Vec<String> = Vec::new();
    // The server's own list of what is still worth trying, once it tells us.
    let mut remaining: Option<Vec<russh::MethodKind>> = None;

    // 1. The agent, if one is running and holds anything.
    if hop.agent
        && let Some(mut agent) = agent_client().await
        && let Ok(identities) = agent.request_identities().await
    {
        for identity in identities {
            // Certificates need a different call; plain keys cover the
            // overwhelmingly common case, so skip them for now.
            let russh::keys::agent::AgentIdentity::PublicKey { key, comment } = identity else {
                continue;
            };
            tried.push(format!("agent:{}", short_comment(&comment)));
            match handle.authenticate_publickey_with(&hop.user, key, rsa_hash, &mut agent).await {
                Ok(result) if result.success() => return Ok(()),
                Ok(result) => remember_remaining(&result, &mut remaining),
                // A broken agent shouldn't stop the key/password fallbacks.
                Err(_) => break,
            }
        }
    }

    // 2. Key files.
    for path in &hop.keys {
        let passphrase = (!creds.key_passphrase.is_empty()).then(|| creds.key_passphrase.clone());
        let for_blocking = path.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            russh::keys::load_secret_key(&for_blocking, passphrase.as_deref())
        })
        .await
        .map_err(|e| Error::other(format!("join error: {e}")))?;

        let key = match loaded {
            Ok(key) => key,
            // An unreadable, encrypted-without-passphrase or malformed key is
            // just a method that didn't work; keep going.
            Err(e) => {
                tried.push(format!("{}: {e}", display_path(path)));
                continue;
            }
        };
        tried.push(display_path(path));
        let with_hash = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
        match handle.authenticate_publickey(&hop.user, with_hash).await {
            Ok(result) if result.success() => return Ok(()),
            Ok(result) => remember_remaining(&result, &mut remaining),
            Err(e) => return Err(Error::other(format!("SSH auth error: {e}"))),
        }
    }

    // 3. The password. Skipped only when the server has explicitly told us it
    //    won't take one — otherwise every connection to a key-only host would
    //    end on a pointless rejected attempt (and burn one of its MaxAuthTries).
    let password_offered =
        hop.last && remaining.as_ref().is_none_or(|m| m.contains(&russh::MethodKind::Password));
    if password_offered {
        tried.push("password".to_string());
        let result = handle
            .authenticate_password(&hop.user, &creds.password)
            .await
            .map_err(|e| Error::other(format!("SSH auth error: {e}")))?;
        if result.success() {
            return Ok(());
        }
    }

    Err(Error::other(format!(
        "SSH authentication failed (tried: {})",
        if tried.is_empty() { "nothing".to_string() } else { tried.join(", ") }
    )))
}

fn remember_remaining(
    result: &russh::client::AuthResult,
    out: &mut Option<Vec<russh::MethodKind>>,
) {
    if let russh::client::AuthResult::Failure { remaining_methods, .. } = result {
        *out = Some(remaining_methods.to_vec());
    }
}

/// Connect to the running SSH agent, if there is one.
#[cfg(unix)]
async fn agent_client() -> Option<russh::keys::agent::client::AgentClient<tokio::net::UnixStream>> {
    russh::keys::agent::client::AgentClient::connect_env().await.ok()
}

/// Windows: the OpenSSH agent listens on a named pipe; Pageant is the PuTTY
/// equivalent that some tooling still uses.
#[cfg(windows)]
async fn agent_client()
-> Option<russh::keys::agent::client::AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>>
{
    russh::keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
        .await
        .ok()
}

#[cfg(not(any(unix, windows)))]
async fn agent_client() -> Option<std::convert::Infallible> {
    None
}

/// A key path shortened for the "tried:" list — the file name is the part that
/// identifies it, and full paths make the message unreadable.
fn display_path(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Agent comments are free-form and can be long; keep the message readable.
fn short_comment(comment: &str) -> String {
    let trimmed = comment.trim();
    if trimmed.is_empty() {
        return "key".to_string();
    }
    match trimmed.char_indices().nth(40) {
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::sshconfig::{SshConfig, expand_tilde};
    use super::*;
    use crate::vfs::remote::Protocol;

    /// The keys the connection itself would try, with no `~/.ssh/config`.
    fn candidate_keys(creds: &RemoteCreds) -> Vec<PathBuf> {
        super::super::plan(creds, &SshConfig::default()).unwrap().pop().unwrap().keys
    }

    /// A passphrase-protected ed25519 key (passphrase: `hunter2`), so the
    /// encrypted-key probe can be tested without generating one at runtime.
    const ENCRYPTED_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABBvZdpuwd\n\
oW6O7yHny95eWBAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIAxdsrKBIxgbUieE\n\
JuHZ39i949Q0NfzId9KzA8yTujEMAAAAoDLjGNAlTOZvSkS3hqfJDaROjb93dZGFgxoVf+\n\
rc/1NejDZNbS5zWUVrC8c7H983zeufaTaqA2KeB4G8SjXjn5hEY3teLPBmWRoW94VP8o6n\n\
3GbG3PYuWAGTJAM0m8p3btECkkX7LEf1ZAh9fDrJ64pyEktZlXgBdDtq02r/7Ep5isbrRH\n\
ia+cKDrAU24ZyR/fWvSdF3fP01UBsLIh0jRbg=\n\
-----END OPENSSH PRIVATE KEY-----\n";

    /// The same key material as the test SSH host key in `crate::shell`, reused
    /// here as a *client* key. Unencrypted.
    const PLAIN_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
QyNTUxOQAAACBhtSAp308g5/FxsHPUCHBLm2jW2k9S/rE+TqPjPHBVlAAAAJB9CQOFfQkD\n\
hQAAAAtzc2gtZWQyNTUxOQAAACBhtSAp308g5/FxsHPUCHBLm2jW2k9S/rE+TqPjPHBVlA\n\
AAAEBuA4oTbyADSU6M0oRqvoIzRfsXXZ2ESA5/JFHtNMzhKGG1ICnfTyDn8XGwc9QIcEub\n\
aNbaT1L+sT5Oo+M8cFWUAAAAB3JjLXRlc3QBAgMEBQY=\n\
-----END OPENSSH PRIVATE KEY-----\n";

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rc_auth_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn creds_with_key(key_file: &str) -> RemoteCreds {
        RemoteCreds {
            protocol: Protocol::Sftp,
            host: "example.invalid".into(),
            port: 22,
            user: "u".into(),
            password: String::new(),
            path: String::new(),
            passive: false,
            key_file: key_file.to_string(),
            key_passphrase: String::new(),
        }
    }

    #[test]
    fn an_explicit_key_file_wins_over_the_defaults() {
        let creds = creds_with_key("/nowhere/id_special");
        assert_eq!(candidate_keys(&creds), vec![PathBuf::from("/nowhere/id_special")]);
    }

    #[test]
    fn a_blank_key_file_falls_back_to_the_defaults() {
        // Whatever the host has (possibly nothing), it must not be the literal
        // empty path — that would make `load_secret_key` fail confusingly.
        let creds = creds_with_key("   ");
        assert!(!candidate_keys(&creds).iter().any(|p| p.as_os_str().is_empty()));
        assert_eq!(candidate_keys(&creds), default_key_paths());
    }

    #[test]
    fn default_key_paths_only_lists_files_that_exist() {
        for p in default_key_paths() {
            assert!(p.is_file(), "{} was listed but does not exist", p.display());
        }
    }

    #[test]
    fn a_tilde_key_path_expands_to_the_home_directory() {
        let expanded = expand_tilde("~/.ssh/id_work");
        assert!(!expanded.to_string_lossy().starts_with('~'), "{expanded:?} still has a tilde");
        assert!(expanded.ends_with(".ssh/id_work"));
        // A non-tilde path is left exactly as typed.
        assert_eq!(expand_tilde("/abs/id_x"), PathBuf::from("/abs/id_x"));
    }

    #[test]
    fn needs_passphrase_distinguishes_encrypted_from_plain_keys() {
        let dir = tmp_dir("probe");
        let enc = dir.join("id_enc");
        let plain = dir.join("id_plain");
        std::fs::write(&enc, ENCRYPTED_KEY).unwrap();
        std::fs::write(&plain, PLAIN_KEY).unwrap();

        assert!(needs_passphrase(&enc), "the encrypted key needs a passphrase");
        assert!(!needs_passphrase(&plain), "the plain key does not");
        // A missing file is not "encrypted" — it is just unusable.
        assert!(!needs_passphrase(&dir.join("nope")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_encrypted_key_loads_once_its_passphrase_is_supplied() {
        let dir = tmp_dir("decrypt");
        let enc = dir.join("id_enc");
        std::fs::write(&enc, ENCRYPTED_KEY).unwrap();

        assert!(russh::keys::load_secret_key(&enc, Some("hunter2")).is_ok());
        assert!(russh::keys::load_secret_key(&enc, Some("wrong")).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_encrypted_key_reports_the_key_and_ignores_ftp() {
        let dir = tmp_dir("first");
        let enc = dir.join("id_enc");
        std::fs::write(&enc, ENCRYPTED_KEY).unwrap();

        let creds = creds_with_key(&enc.to_string_lossy());
        assert_eq!(first_encrypted_key(&creds).as_deref(), Some("id_enc"));

        // FTP has no keys at all, so it must never raise a passphrase prompt.
        let mut ftp = creds.clone();
        ftp.protocol = Protocol::Ftp;
        assert_eq!(first_encrypted_key(&ftp), None);

        // A plain key needs no prompt either.
        let plain = dir.join("id_plain");
        std::fs::write(&plain, PLAIN_KEY).unwrap();
        assert_eq!(first_encrypted_key(&creds_with_key(&plain.to_string_lossy())), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Real authentication against an in-process SSH server ---------------

    /// A server that takes a public key (any key, for the test's purposes) and
    /// refuses passwords, so a successful connect proves key auth ran.
    #[derive(Clone)]
    struct KeyOnlyServer;

    /// The mirror image: refuses keys, takes the password. Proves the chain
    /// falls through to the password when no key is accepted.
    #[derive(Clone)]
    struct PasswordOnlyServer;

    macro_rules! impl_test_server {
        ($name:ident, $pubkey:expr, $password:expr) => {
            impl russh::server::Server for $name {
                type Handler = $name;
                fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> $name {
                    self.clone()
                }
            }
            impl russh::server::Handler for $name {
                type Error = russh::Error;
                async fn auth_publickey(
                    &mut self,
                    _user: &str,
                    _key: &russh::keys::ssh_key::PublicKey,
                ) -> std::result::Result<russh::server::Auth, Self::Error> {
                    Ok($pubkey)
                }
                async fn auth_password(
                    &mut self,
                    _user: &str,
                    _password: &str,
                ) -> std::result::Result<russh::server::Auth, Self::Error> {
                    Ok($password)
                }
            }
        };
    }

    fn reject() -> russh::server::Auth {
        russh::server::Auth::Reject { proceed_with_methods: None, partial_success: false }
    }

    impl_test_server!(KeyOnlyServer, russh::server::Auth::Accept, reject());
    impl_test_server!(PasswordOnlyServer, reject(), russh::server::Auth::Accept);

    /// A jump host: takes any key, and forwards a direct-tcpip channel to the
    /// address asked for, counting the ones it forwarded.
    #[derive(Clone)]
    struct ForwardingServer(Arc<std::sync::atomic::AtomicUsize>);

    impl russh::server::Server for ForwardingServer {
        type Handler = ForwardingServer;
        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> ForwardingServer {
            self.clone()
        }
    }

    impl russh::server::Handler for ForwardingServer {
        type Error = russh::Error;
        async fn auth_publickey(
            &mut self,
            _user: &str,
            _key: &russh::keys::ssh_key::PublicKey,
        ) -> std::result::Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }
        async fn channel_open_direct_tcpip(
            &mut self,
            channel: russh::Channel<russh::server::Msg>,
            host: &str,
            port: u32,
            _originator_address: &str,
            _originator_port: u32,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> std::result::Result<(), Self::Error> {
            let Ok(mut target) = tokio::net::TcpStream::connect((host, port as u16)).await else {
                return Ok(());
            };
            reply.accept().await;
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
            });
            Ok(())
        }
    }

    /// Start `server` on a throwaway localhost port and return that port.
    async fn spawn_server<S>(server: S) -> u16
    where
        S: russh::server::Server + Send + 'static,
    {
        let host_key = russh::keys::PrivateKey::from_openssh(PLAIN_KEY).expect("host key");
        let config = std::sync::Arc::new(russh::server::Config {
            keys: vec![host_key],
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut server = server;
            let _ = server.run_on_socket(config, &listener).await;
        });
        port
    }

    fn local_creds(port: u16, key_file: &str, password: &str) -> RemoteCreds {
        RemoteCreds {
            protocol: Protocol::Sftp,
            host: "127.0.0.1".into(),
            port,
            user: "u".into(),
            password: password.to_string(),
            path: String::new(),
            passive: false,
            key_file: key_file.to_string(),
            key_passphrase: String::new(),
        }
    }

    /// The headline case this feature exists for: a server that does not accept
    /// passwords at all (`PasswordAuthentication no`) is reachable with a key.
    #[tokio::test]
    async fn a_key_file_authenticates_against_a_password_refusing_server() {
        let dir = tmp_dir("keyauth");
        let key = dir.join("id_test");
        std::fs::write(&key, PLAIN_KEY).unwrap();
        let port = spawn_server(KeyOnlyServer).await;

        // Empty password on purpose — only the key can possibly work here.
        let creds = local_creds(port, &key.to_string_lossy(), "");
        crate::vfs::remote::ssh_connect(&creds).await.expect("key auth should succeed");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An encrypted key works once the passphrase collected by the prompt is
    /// carried on the credentials.
    #[tokio::test]
    async fn an_encrypted_key_authenticates_once_the_passphrase_is_set() {
        let dir = tmp_dir("keyauth_enc");
        let key = dir.join("id_enc");
        std::fs::write(&key, ENCRYPTED_KEY).unwrap();
        let port = spawn_server(KeyOnlyServer).await;

        let mut creds = local_creds(port, &key.to_string_lossy(), "");
        // Without the passphrase the key cannot even be loaded, so auth fails.
        assert!(
            crate::vfs::remote::ssh_connect(&creds).await.is_err(),
            "an encrypted key with no passphrase must not authenticate"
        );

        creds.key_passphrase = "hunter2".to_string();
        crate::vfs::remote::ssh_connect(&creds).await.expect("passphrase should unlock the key");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Existing behaviour must not regress: with no usable key, the chain still
    /// falls through to the password.
    #[tokio::test]
    async fn authentication_falls_back_to_the_password_when_no_key_matches() {
        let dir = tmp_dir("pwfallback");
        let key = dir.join("id_test");
        std::fs::write(&key, PLAIN_KEY).unwrap();
        let port = spawn_server(PasswordOnlyServer).await;

        let creds = local_creds(port, &key.to_string_lossy(), "p");
        crate::vfs::remote::ssh_connect(&creds).await.expect("should fall back to the password");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `ProxyJump` route: the connection to the host runs through a tunnel on
    /// the jump host, which stays open while the session is used.
    #[tokio::test]
    async fn a_connection_runs_through_its_jump_host() {
        let dir = tmp_dir("jump");
        let key = dir.join("id_test");
        std::fs::write(&key, PLAIN_KEY).unwrap();
        let forwarded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let jump_port = spawn_server(ForwardingServer(forwarded.clone())).await;
        let target_port = spawn_server(KeyOnlyServer).await;

        let config = format!(
            "Host target\n  HostName 127.0.0.1\n  Port {target_port}\n  ProxyJump u@127.0.0.1:{jump_port}\n  IdentityFile {}\nHost 127.0.0.1\n  IdentityFile {}\n",
            key.display(),
            key.display()
        );
        let cfg = SshConfig::parse(&config, &dir);
        let mut creds = local_creds(22, "", "");
        creds.host = "target".into();
        let hops = super::super::plan(&creds, &cfg).unwrap();
        assert_eq!(hops.iter().map(|h| h.port).collect::<Vec<_>>(), vec![jump_port, target_port]);

        let session = super::super::connect_hops(&hops, &creds).await.expect("through the jump");
        assert_eq!(forwarded.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The tunnel is still up: the host answers a request for another
        // channel (this test server turns sessions down, but it answers).
        match session.channel_open_session().await {
            Ok(_) | Err(russh::Error::ChannelOpenFailure(_)) => {}
            Err(e) => panic!("the session went away with its jump host: {e}"),
        }
        assert!(!session.is_closed());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A total failure names what was attempted, so the message is debuggable
    /// rather than a bare "authentication failed".
    #[tokio::test]
    async fn a_failed_chain_reports_what_it_tried() {
        let dir = tmp_dir("nothing");
        let key = dir.join("id_test");
        std::fs::write(&key, PLAIN_KEY).unwrap();
        // Refuses both methods.
        #[derive(Clone)]
        struct RefuseAll;
        impl_test_server!(RefuseAll, reject(), reject());
        let port = spawn_server(RefuseAll).await;

        let creds = local_creds(port, &key.to_string_lossy(), "p");
        let err = match crate::vfs::remote::ssh_connect(&creds).await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a server refusing every method must not authenticate"),
        };
        assert!(err.contains("id_test"), "names the key it tried: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
