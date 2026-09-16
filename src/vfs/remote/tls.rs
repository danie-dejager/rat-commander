//! Trust for FTPS servers. A certificate one of the system's trusted
//! authorities vouches for (for the host connected to, and in date) is
//! accepted. Any other — self-signed, from a private CA, expired — only when
//! its SHA-256 was pinned for that server after the user chose to trust it,
//! the way `known_hosts` works for SSH; a pinned server whose certificate
//! changes is refused, and asking again says so.

use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use suppaftp::tokio_rustls::rustls::client::WebPkiServerVerifier;
use suppaftp::tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use suppaftp::tokio_rustls::rustls::crypto::{self, CryptoProvider};
use suppaftp::tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use suppaftp::tokio_rustls::rustls::{
    self, CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};

/// Where a verifier leaves the certificate a handshake stopped at.
pub type Stopped = Arc<Mutex<Option<CertFailure>>>;

/// A certificate the connection stopped at, for asking whether to trust it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFailure {
    /// `host:port`, what a pin is kept under.
    pub host_port: String,
    /// The certificate's SHA-256, as hex.
    pub sha256: String,
    pub subject: String,
    pub issuer: String,
    pub not_after: String,
    /// Why the system's authorities didn't vouch for it.
    pub reason: String,
    /// A different certificate is pinned for this server.
    pub pinned_other: bool,
}

/// Where pins are kept: `host:port sha256:HEX`, one to a line.
pub fn pins_file() -> Option<PathBuf> {
    crate::config::paths::ftps_known_hosts_file()
}

/// The pinned SHA-256 for `host_port` in the file at `path`.
pub fn pinned(path: &Path, host_port: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let (who, sha) = line.trim().split_once(char::is_whitespace)?;
        (who == host_port).then(|| sha.trim().trim_start_matches("sha256:").to_ascii_uppercase())
    })
}

/// Pin `sha256` for `host_port`, replacing a pin it had.
pub fn add_pin(path: &Path, host_port: &str, sha256: &str) -> std::io::Result<()> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(host_port))
        .map(str::to_string)
        .collect();
    lines.push(format!("{host_port} sha256:{sha256}"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

fn sha256_hex(der: &[u8]) -> String {
    sha2::Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

/// What went wrong, in words.
fn reason(e: &rustls::Error) -> String {
    match e {
        rustls::Error::InvalidCertificate(c) => match c {
            CertificateError::UnknownIssuer => {
                "it is self-signed, or issued by an authority this system doesn't trust".into()
            }
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                "it has expired".into()
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                "it isn't valid yet".into()
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                "it was issued for a different host name".into()
            }
            other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}

#[derive(Debug)]
struct TofuVerifier {
    /// Checks against the system's authorities; `None` when it has none.
    system: Option<Arc<WebPkiServerVerifier>>,
    provider: Arc<CryptoProvider>,
    host_port: String,
    pin: Option<String>,
    failure: Stopped,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let system = match &self.system {
            Some(v) => {
                v.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            }
            None => Err(rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer)),
        };
        let Err(e) = system else { return system };
        let sha256 = sha256_hex(end_entity);
        if self.pin.as_deref() == Some(sha256.as_str()) {
            return Ok(ServerCertVerified::assertion());
        }
        let cert = crate::certs::x509::parse(end_entity);
        let failure = CertFailure {
            host_port: self.host_port.clone(),
            subject: cert.as_ref().map(|c| c.subject().to_string()).unwrap_or_default(),
            issuer: cert.as_ref().map(|c| c.issuer().to_string()).unwrap_or_default(),
            not_after: cert
                .as_ref()
                .map(|c| crate::certs::x509::date(c.validity().not_after.timestamp()))
                .unwrap_or_default(),
            reason: reason(&e),
            pinned_other: self.pin.is_some(),
            sha256,
        };
        *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(failure);
        Err(e)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A TLS client configuration for `host_port` trusting `roots` and the
/// certificate pinned for it (`pin`), and where the certificate a handshake
/// stops at is left.
pub fn client_config(
    host_port: &str,
    roots: RootCertStore,
    pin: Option<String>,
) -> crate::util::Result<(Arc<ClientConfig>, Stopped)> {
    let (verifier, failure) = tofu_verifier(host_port, roots, pin);
    let provider = verifier.provider.clone();
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| crate::util::Error::other(format!("TLS: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok((Arc::new(config), failure))
}

/// The verifier [`client_config`] uses, and where it leaves the certificate
/// it stops at.
fn tofu_verifier(
    host_port: &str,
    roots: RootCertStore,
    pin: Option<String>,
) -> (Arc<TofuVerifier>, Stopped) {
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let system = (!roots.is_empty())
        .then(|| {
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .ok()
        })
        .flatten();
    let failure = Arc::new(Mutex::new(None));
    let verifier = Arc::new(TofuVerifier {
        system,
        provider,
        host_port: host_port.to_string(),
        pin,
        failure: failure.clone(),
    });
    (verifier, failure)
}

/// The certificates the system trusts.
pub fn system_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certs::testdata;

    fn der(pem: &str) -> CertificateDer<'static> {
        CertificateDer::from(crate::certs::pem_blocks(pem).pop().unwrap().der)
    }

    fn verifier(pin: Option<String>) -> (Arc<TofuVerifier>, Stopped) {
        tofu_verifier("files.example.test:21", RootCertStore::empty(), pin)
    }

    fn check(v: &TofuVerifier, cert: &CertificateDer<'static>) -> bool {
        let name = ServerName::try_from("files.example.test").unwrap();
        v.verify_server_cert(cert, &[], &name, &[], UnixTime::now()).is_ok()
    }

    #[test]
    fn an_unknown_certificate_is_refused_and_described_until_it_is_pinned() {
        let cert = der(testdata::RSA_CERT);
        let (v, failure) = verifier(None);
        assert!(!check(&v, &cert));
        let f = failure.lock().unwrap().clone().expect("the refusal is described");
        assert_eq!(f.host_port, "files.example.test:21");
        assert_eq!(f.subject, "CN=rsa.example.test");
        assert_eq!(f.sha256.len(), 64);
        assert!(!f.pinned_other);

        let (v, failure) = verifier(Some(f.sha256.clone()));
        assert!(check(&v, &cert), "the pinned certificate is trusted");
        assert!(failure.lock().unwrap().is_none());

        let (v, failure) = verifier(Some("00".repeat(32)));
        assert!(!check(&v, &cert));
        assert!(failure.lock().unwrap().as_ref().unwrap().pinned_other, "a changed certificate");
    }

    #[test]
    fn a_system_authority_vouches_without_a_pin() {
        let mut roots = RootCertStore::empty();
        roots.add(der(testdata::ROOT)).unwrap();
        let (v, _) = tofu_verifier("www.example.test:21", roots, None);
        let name = ServerName::try_from("www.example.test").unwrap();
        let leaf = der(testdata::LEAF);
        let int = der(testdata::INTERMEDIATE);
        // In date (2025-03-01) and for the right name.
        let when = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_740_787_200));
        assert!(v.verify_server_cert(&leaf, std::slice::from_ref(&int), &name, &[], when).is_ok());
        let other = ServerName::try_from("elsewhere.test").unwrap();
        assert!(v.verify_server_cert(&leaf, &[int], &other, &[], when).is_err());
    }

    #[test]
    fn pins_are_kept_one_per_server() {
        let path = std::env::temp_dir().join(format!("rc_ftps_pins_{}", std::process::id()));
        std::fs::remove_file(&path).ok();
        add_pin(&path, "a.test:21", "AA").unwrap();
        add_pin(&path, "b.test:990", "BB").unwrap();
        add_pin(&path, "a.test:21", "CC").unwrap();
        assert_eq!(pinned(&path, "a.test:21").as_deref(), Some("CC"));
        assert_eq!(pinned(&path, "b.test:990").as_deref(), Some("BB"));
        assert_eq!(pinned(&path, "c.test:21"), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        std::fs::remove_file(&path).ok();
    }
}
