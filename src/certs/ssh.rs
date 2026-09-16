//! SSH keys and the files that list them: OpenSSH private keys (what kind,
//! whether and how they are encrypted, and the public key's fingerprint, which
//! is readable without the passphrase), public keys, OpenSSH certificates (for
//! whom and what, how long, signed by which CA, and whether that signature
//! holds), and every line of an `authorized_keys` or `known_hosts` file.

use super::x509::{EXPIRY_WARNING_DAYS, date};
use super::{Label, Row, Tone};
use russh::keys::ssh_key::authorized_keys::Entry as AuthorizedKey;
use russh::keys::ssh_key::known_hosts::{Entry as KnownHost, HostPatterns, Marker};
use russh::keys::ssh_key::public::KeyData;
use russh::keys::ssh_key::{
    Algorithm, Certificate, EcdsaCurve, HashAlg, Kdf, PrivateKey, PublicKey,
};

/// What the lines of a text file are, by the file's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    AuthorizedKeys,
    KnownHosts,
    /// Public keys and certificates, one to a line, among other text.
    Keys,
}

pub fn lines_kind(name: &str) -> Lines {
    let lower = name.to_ascii_lowercase();
    if lower.contains("authorized_keys") {
        Lines::AuthorizedKeys
    } else if lower.contains("known_hosts") {
        Lines::KnownHosts
    } else {
        Lines::Keys
    }
}

/// Whether a file's name says it holds SSH keys.
pub fn named_like_one(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("id_")
        || lower.ends_with("-cert.pub")
        || lower.contains("authorized_keys")
        || lower.contains("known_hosts")
}

/// Whether a line begins with an SSH key type.
fn key_type_first(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    ["ssh-", "ecdsa-sha2-", "sk-ssh-", "sk-ecdsa-"].iter().any(|p| first.starts_with(p))
}

/// Whether text begins like an SSH public key or a hashed known host.
pub fn sniff(text: &str) -> bool {
    let t = text.trim_start();
    key_type_first(t) || t.starts_with("|1|")
}

/// What an SSH file holds, sorted for the tabs.
#[derive(Debug, Default)]
pub struct Found {
    /// Private and public keys: a summary row and the key's rows.
    pub keys: Vec<(Row, Vec<Row>, bool)>,
    pub certs: Vec<(Row, Vec<Row>)>,
    /// The rows of each `authorized_keys` or `known_hosts` line.
    pub entries: Vec<Vec<Row>>,
    pub entries_kind: Option<Lines>,
    /// Lines of those files that couldn't be read.
    pub unreadable: usize,
}

impl Found {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.certs.is_empty() && self.entries.is_empty()
    }
}

/// A key's type and size, and whether it is too weak to trust.
fn describe(data: &KeyData) -> (String, Tone) {
    match data.algorithm() {
        Algorithm::Rsa { .. } => {
            let bits = data.rsa().map_or(0, |k| k.key_size());
            (format!("RSA {bits} bits"), if bits < 2048 { Tone::Warn } else { Tone::Normal })
        }
        Algorithm::Ecdsa { curve } => {
            let c = match curve {
                EcdsaCurve::NistP256 => "P-256",
                EcdsaCurve::NistP384 => "P-384",
                EcdsaCurve::NistP521 => "P-521",
            };
            (format!("ECDSA {c}"), Tone::Normal)
        }
        Algorithm::Ed25519 => ("Ed25519".into(), Tone::Normal),
        Algorithm::Dsa => ("DSA (obsolete)".into(), Tone::Warn),
        Algorithm::SkEcdsaSha2NistP256 => ("ECDSA P-256 on a security key".into(), Tone::Normal),
        Algorithm::SkEd25519 => ("Ed25519 on a security key".into(), Tone::Normal),
        other => (other.as_str().to_string(), Tone::Normal),
    }
}

fn fingerprint(data: &KeyData) -> String {
    data.fingerprint(HashAlg::Sha256).to_string()
}

/// Rows common to every public key: its type, fingerprint and comment.
fn key_rows(data: &KeyData, comment: &str) -> Vec<Row> {
    let (what, tone) = describe(data);
    let mut rows =
        vec![Row::new("Algorithm", what).tone(tone), Row::new("Fingerprint", fingerprint(data))];
    if !comment.is_empty() {
        rows.push(Row::new("Comment", comment));
    }
    rows
}

fn at_line(mut rows: Vec<Row>, line: usize) -> Vec<Row> {
    rows.iter_mut().for_each(|r| r.line = Some(line));
    rows
}

/// An OpenSSH private key (the contents of its PEM block).
pub fn private_key(data: &[u8], line: usize, found: &mut Found) {
    let Ok(key) = PrivateKey::from_bytes(data) else { return };
    let public = key.public_key();
    // Numbered with the file's other keys, when the report is put together.
    let mut rows = vec![Row::heading(Label::Text(String::new()), "Private key", Some(line))];
    let mut body = key_rows(public.key_data(), public.comment().as_str_lossy());
    body.insert(1, Row::new("Format", "OpenSSH"));
    if key.is_encrypted() {
        let kdf = match key.kdf() {
            Kdf::Bcrypt { rounds, .. } => format!(", bcrypt-pbkdf with {rounds} rounds"),
            _ => String::new(),
        };
        body.insert(2, Row::new("Encryption", format!("{}{kdf}", key.cipher())));
    }
    rows.extend(body);
    let (what, _) = describe(public.key_data());
    let summary = format!("{what}{}", if key.is_encrypted() { ", encrypted" } else { "" });
    let mut sum = Row::new("Private key", summary);
    sum.line = Some(line);
    found.keys.push((sum, at_line(rows, line), true));
}

/// Whether a certificate is valid at `now`, said with the days to go or gone.
fn validity(cert: &Certificate, now: i64) -> (String, Tone) {
    let (after, before) = (cert.valid_after() as i128, cert.valid_before() as i128);
    let now = now as i128;
    if now < after {
        return (format!("not valid for another {} days", (after - now) / 86_400), Tone::Bad);
    }
    if cert.valid_before() == u64::MAX {
        return ("valid forever".into(), Tone::Good);
    }
    let days = (before - now).div_euclid(86_400);
    if now > before {
        (format!("expired {} days ago", (now - before) / 86_400), Tone::Bad)
    } else if days < EXPIRY_WARNING_DAYS as i128 {
        (format!("expires in {days} days"), Tone::Warn)
    } else {
        (format!("expires in {days} days"), Tone::Good)
    }
}

fn certificate(cert: &Certificate, line: usize, now: i64, found: &mut Found) {
    let (state, tone) = validity(cert, now);
    let summary = format!("{}  ({state})", cert.key_id());
    let mut rows =
        vec![Row::heading(Label::Text(String::new()), summary.clone(), Some(line)).tone(tone)];
    let kind = match cert.cert_type() {
        russh::keys::ssh_key::certificate::CertType::User => "SSH user certificate",
        russh::keys::ssh_key::certificate::CertType::Host => "SSH host certificate",
    };
    rows.push(Row::new("Type", kind));
    rows.push(Row::new("Key ID", cert.key_id()));
    rows.push(Row::new("Serial number", cert.serial().to_string()));
    rows.push(match cert.valid_principals() {
        [] => Row::new("Principals", "any — none are listed").tone(Tone::Warn),
        p => Row::new("Principals", p.join(", ")),
    });
    let from = if cert.valid_after() == 0 {
        "always".to_string()
    } else {
        date(cert.valid_after() as i64)
    };
    rows.push(Row::new("Valid from", from));
    let until = if cert.valid_before() == u64::MAX {
        "forever".to_string()
    } else {
        format!("{}  ({state})", date(cert.valid_before().min(i64::MAX as u64) as i64))
    };
    rows.push(Row::new("Valid until", until).tone(tone));
    let (what, key_tone) = describe(cert.public_key());
    rows.push(
        Row::new("Public key", format!("{what}  {}", fingerprint(cert.public_key())))
            .tone(key_tone),
    );
    let (ca, _) = describe(cert.signature_key());
    rows.push(Row::new("Signed by", format!("{ca}  {}", fingerprint(cert.signature_key()))));
    rows.push(match cert.verify_signature() {
        Ok(()) => Row::new("Signature", "valid: made with the CA's key").tone(Tone::Good),
        Err(_) => Row::new("Signature", "does not verify").tone(Tone::Bad),
    });
    let options = |m: &russh::keys::ssh_key::certificate::OptionsMap| -> String {
        let list: Vec<String> = m
            .iter()
            .map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") })
            .collect();
        if list.is_empty() { "none".into() } else { list.join(", ") }
    };
    rows.push(Row::new("Critical options", options(cert.critical_options())));
    rows.push(Row::new("Extensions", options(cert.extensions())));
    if !cert.comment().is_empty() {
        rows.push(Row::new("Comment", cert.comment()));
    }
    let mut sum = Row::new("SSH certificate", summary).tone(tone);
    sum.line = Some(line);
    found.certs.push((sum, at_line(rows, line)));
}

/// The SSH keys, certificates or entries in the lines of `text`.
pub fn read_lines(kind: Lines, text: &str, now: i64, found: &mut Found) {
    for (at, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match kind {
            Lines::AuthorizedKeys | Lines::KnownHosts => {
                found.entries_kind = Some(kind);
                let n = found.entries.len() + 1;
                let parsed = match kind {
                    Lines::AuthorizedKeys => line.parse::<AuthorizedKey>().map(|e| {
                        let pk = e.public_key();
                        let mut rows = key_rows(pk.key_data(), pk.comment().as_str_lossy());
                        let opts = e.config_opts();
                        if !opts.is_empty() {
                            rows.push(Row::new("Options", opts.as_str()));
                        }
                        (pk.clone(), rows)
                    }),
                    _ => line.parse::<KnownHost>().map(|e| {
                        let pk = e.public_key();
                        let mut rows = Vec::new();
                        rows.push(match e.host_patterns() {
                            HostPatterns::Patterns(p) => Row::new("Hosts", p.join(", ")),
                            HostPatterns::HashedName { .. } => {
                                Row::new("Hosts", "hashed: the name isn't stored").tone(Tone::Dim)
                            }
                        });
                        match e.marker() {
                            Some(Marker::CertAuthority) => rows
                                .push(Row::new("Marker", "@cert-authority: a CA for these hosts")),
                            Some(Marker::Revoked) => {
                                rows.push(Row::new("Marker", "@revoked").tone(Tone::Warn))
                            }
                            None => {}
                        }
                        rows.extend(key_rows(pk.key_data(), pk.comment().as_str_lossy()));
                        (pk.clone(), rows)
                    }),
                };
                let mut rows = match parsed {
                    Ok((pk, body)) => {
                        let (what, _) = describe(pk.key_data());
                        let mut rows = vec![Row::heading(
                            Label::Text(format!("#{n}")),
                            format!("{what}  {}", fingerprint(pk.key_data())),
                            Some(at),
                        )];
                        rows.extend(body);
                        rows
                    }
                    Err(e) => {
                        found.unreadable += 1;
                        vec![
                            Row::heading(
                                Label::Text(format!("#{n}")),
                                format!("can't be read: {e}"),
                                Some(at),
                            )
                            .tone(Tone::Bad),
                        ]
                    }
                };
                rows.iter_mut().for_each(|r| r.line = Some(at));
                found.entries.push(rows);
            }
            Lines::Keys if key_type_first(line) => {
                let first = line.split_whitespace().next().unwrap_or("");
                if first.ends_with("-cert-v01@openssh.com") {
                    if let Ok(cert) = Certificate::from_openssh(line) {
                        certificate(&cert, at, now, found);
                    }
                } else if let Ok(pk) = PublicKey::from_openssh(line) {
                    let mut rows =
                        vec![Row::heading(Label::Text(String::new()), "Public key", Some(at))];
                    rows.extend(key_rows(pk.key_data(), pk.comment().as_str_lossy()));
                    let (what, _) = describe(pk.key_data());
                    let mut sum =
                        Row::new("Public key", format!("{what}  {}", fingerprint(pk.key_data())));
                    sum.line = Some(at);
                    found.keys.push((sum, at_line(rows, at), false));
                }
            }
            Lines::Keys => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{pem_blocks, testdata};
    use super::*;

    /// 2025-03-01, a month before the certificate expires.
    const MARCH_2025: i64 = 1_740_787_200;

    fn value<'a>(rows: &'a [Row], label: &str) -> &'a Row {
        rows.iter()
            .find(|r| matches!(r.label, Label::Key(k) if k == label))
            .unwrap_or_else(|| panic!("no {label} in {rows:?}"))
    }

    #[test]
    fn private_keys_show_their_public_side_even_when_encrypted() {
        let mut found = Found::default();
        for pem in [testdata::SSH_ED25519_KEY, testdata::SSH_RSA_KEY] {
            let b = pem_blocks(pem).pop().unwrap();
            private_key(&b.der, b.line, &mut found);
        }
        let (sum, rows, private) = &found.keys[0];
        assert!(private);
        assert_eq!(sum.value, "Ed25519");
        assert_eq!(value(rows, "Fingerprint").value, testdata::SSH_ED25519_FP);
        assert_eq!(value(rows, "Comment").value, "alice@laptop");
        let (sum, rows, _) = &found.keys[1];
        assert_eq!(sum.value, "RSA 2048 bits, encrypted");
        assert_eq!(value(rows, "Fingerprint").value, testdata::SSH_RSA_FP);
        assert!(
            value(rows, "Encryption").value.starts_with("aes256-ctr, bcrypt-pbkdf"),
            "{rows:?}"
        );
    }

    #[test]
    fn a_certificate_says_who_for_how_long_and_by_whom() {
        let mut found = Found::default();
        read_lines(Lines::Keys, testdata::SSH_CERT, MARCH_2025, &mut found);
        let (sum, rows) = &found.certs[0];
        assert_eq!(
            (sum.value.as_str(), sum.tone),
            ("alice-2025  (expires in 31 days)", Tone::Good)
        );
        assert_eq!(value(rows, "Type").value, "SSH user certificate");
        assert_eq!(value(rows, "Principals").value, "alice, deploy");
        assert_eq!(value(rows, "Serial number").value, "42");
        assert_eq!(value(rows, "Signed by").value, format!("Ed25519  {}", testdata::SSH_CA_FP));
        assert_eq!(value(rows, "Signature").tone, Tone::Good);
        assert!(value(rows, "Extensions").value.contains("permit-pty"));
        assert!(!value(rows, "Extensions").value.contains("permit-port-forwarding"));
        let mut later = Found::default();
        read_lines(Lines::Keys, testdata::SSH_CERT, MARCH_2025 + 60 * 86_400, &mut later);
        assert_eq!(later.certs[0].0.tone, Tone::Bad, "expired by May");
    }

    #[test]
    fn authorized_keys_lists_each_key_with_its_options_and_marks_bad_lines() {
        let mut found = Found::default();
        read_lines(Lines::AuthorizedKeys, testdata::AUTHORIZED_KEYS, MARCH_2025, &mut found);
        assert_eq!(found.entries.len(), 3, "the comment and blank line are skipped");
        assert_eq!(found.unreadable, 1);
        let first = &found.entries[0];
        assert_eq!(value(first, "Options").value, "command=\"/usr/bin/backup\",no-pty");
        assert_eq!(first[0].line, Some(0));
        assert_eq!(value(&found.entries[1], "Fingerprint").value, testdata::SSH_RSA_FP);
        assert_eq!(found.entries[1][0].line, Some(3));
        assert_eq!(found.entries[2][0].tone, Tone::Bad);
    }

    #[test]
    fn known_hosts_shows_hashed_names_and_markers() {
        let mut found = Found::default();
        read_lines(Lines::KnownHosts, testdata::KNOWN_HOSTS, MARCH_2025, &mut found);
        assert_eq!((found.entries.len(), found.unreadable), (3, 0));
        assert_eq!(value(&found.entries[0], "Hosts").tone, Tone::Dim);
        let ca = &found.entries[2];
        assert_eq!(value(ca, "Hosts").value, "*.example.test");
        assert!(value(ca, "Marker").value.starts_with("@cert-authority"));
        assert_eq!(value(ca, "Fingerprint").value, testdata::SSH_CA_FP);
    }
}
