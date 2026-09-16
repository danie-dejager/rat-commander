//! Certificates and keys laid out for reading: X.509 certificates and
//! certificate requests, private and public keys — PEM (one or many blocks,
//! text around them allowed) or DER.
//!
//! What comes out is a [`Report`]: tabs of labelled rows, each row toned
//! (expired, expiring, verified, weak …) and pointing back at the line of the
//! file it came from. The viewer shows it (F3 on such a file) and F8 switches
//! to the raw text.

pub mod chain;
pub mod der;
pub mod keys;
pub mod ssh;
#[cfg(test)]
pub mod testdata;
pub mod x509;

use base64::Engine;

/// Files larger than this aren't inspected.
pub const MAX_BYTES: u64 = 4 << 20;

/// How a row's value is to be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Good,
    Warn,
    Bad,
    Dim,
}

/// A row's label: a field's name (translated when shown), or text of the
/// file's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Key(&'static str),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// 0 for a heading naming an object, 1 for its fields.
    pub depth: u8,
    pub label: Label,
    pub value: String,
    pub tone: Tone,
    /// The line of the file (from 0) the row came from.
    pub line: Option<usize>,
}

impl Row {
    pub fn new(label: &'static str, value: impl Into<String>) -> Row {
        Row {
            depth: 1,
            label: Label::Key(label),
            value: value.into(),
            tone: Tone::Normal,
            line: None,
        }
    }

    pub fn heading(label: Label, value: impl Into<String>, line: Option<usize>) -> Row {
        Row { depth: 0, label, value: value.into(), tone: Tone::Normal, line }
    }

    pub fn tone(mut self, tone: Tone) -> Row {
        self.tone = tone;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Summary,
    Certificates,
    Requests,
    Keys,
    Entries,
    Chain,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Summary => "Summary",
            Tab::Certificates => "Certificates",
            Tab::Requests => "Requests",
            Tab::Keys => "Keys",
            Tab::Entries => "Entries",
            Tab::Chain => "Chain",
        }
    }
}

/// What a file holds, tab by tab (only the tabs with something in them).
#[derive(Debug, Clone)]
pub struct Report {
    /// `3 certificates, 1 private key`.
    pub summary: String,
    pub tabs: Vec<(Tab, Vec<Row>)>,
}

/// A PEM block: its label, headers, decoded contents and first line.
#[derive(Debug, Clone)]
pub struct Block {
    pub label: String,
    pub headers: Vec<(String, String)>,
    pub der: Vec<u8>,
    pub line: usize,
}

/// Every well-formed PEM block in `text`.
pub fn pem_blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((at, line)) = lines.next() {
        let Some(label) =
            line.trim().strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----"))
        else {
            continue;
        };
        let end = format!("-----END {label}-----");
        let (mut headers, mut body, mut closed) = (Vec::new(), String::new(), false);
        for (_, l) in lines.by_ref() {
            let l = l.trim();
            if l == end {
                closed = true;
                break;
            }
            // Base64 has no colons: a line with one before the data is a header.
            match l.split_once(':') {
                Some((k, v)) if body.is_empty() => headers.push((k.trim().into(), v.trim().into())),
                _ => body.push_str(l),
            }
        }
        if !closed {
            break;
        }
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body.as_bytes()) {
            out.push(Block { label: label.to_string(), headers, der, line: at });
        }
    }
    out
}

/// Names of files that hold certificates or keys, whatever their content
/// looks like.
fn named_like_one(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    const EXT: [&str; 8] = [".pem", ".crt", ".cer", ".der", ".csr", ".p10", ".key", ".pub"];
    EXT.iter().any(|e| lower.ends_with(e)) || lower.contains("ca-bundle")
}

/// Whether a file named `name` starting with `head` is worth inspecting: by
/// its name, or by beginning (blank lines aside) with a PEM block or an SSH
/// key.
pub fn sniff(name: &str, head: &[u8]) -> bool {
    let start = head.iter().position(|b| !b.is_ascii_whitespace()).map_or(&[][..], |i| &head[i..]);
    named_like_one(name)
        || ssh::named_like_one(name)
        || start.starts_with(b"-----BEGIN ")
        || ssh::sniff(&String::from_utf8_lossy(start))
}

#[derive(Default)]
struct Found {
    certs: Vec<(Vec<u8>, Option<usize>)>,
    requests: Vec<(Vec<u8>, Option<usize>)>,
    keys: Vec<keys::KeyInfo>,
    ssh: ssh::Found,
}

impl Found {
    fn is_empty(&self) -> bool {
        self.certs.is_empty()
            && self.requests.is_empty()
            && self.keys.is_empty()
            && self.ssh.is_empty()
    }

    fn add_block(&mut self, b: Block) {
        match b.label.as_str() {
            "CERTIFICATE" | "TRUSTED CERTIFICATE" | "X509 CERTIFICATE" => {
                if x509::parse(&b.der).is_some() {
                    self.certs.push((b.der, Some(b.line)));
                }
            }
            "CERTIFICATE REQUEST" | "NEW CERTIFICATE REQUEST" => {
                if x509::parse_request(&b.der).is_some() {
                    self.requests.push((b.der, Some(b.line)));
                }
            }
            "OPENSSH PRIVATE KEY" => ssh::private_key(&b.der, b.line, &mut self.ssh),
            label => {
                if let Some(mut k) = keys::parse(label, &b.headers, &b.der) {
                    k.line = Some(b.line);
                    self.keys.push(k);
                }
            }
        }
    }

    fn add_der(&mut self, der: &[u8]) {
        if x509::parse(der).is_some() {
            self.certs.push((der.to_vec(), None));
        } else if x509::parse_request(der).is_some() {
            self.requests.push((der.to_vec(), None));
        } else if let Some(k) = keys::parse_der(der) {
            self.keys.push(k);
        }
    }
}

/// What the file `name` holding `data` has in it, as of `now` (Unix time),
/// or `None` when it holds no certificate or key.
pub fn inspect(name: &str, data: &[u8], now: i64) -> Option<Report> {
    let mut found = Found::default();
    if let Ok(text) = std::str::from_utf8(data) {
        if text.contains("-----BEGIN ") {
            pem_blocks(text).into_iter().for_each(|b| found.add_block(b));
        }
        let kind = ssh::lines_kind(name);
        if kind != ssh::Lines::Keys || text.lines().any(ssh::sniff) {
            ssh::read_lines(kind, text, now, &mut found.ssh);
        }
    }
    if found.is_empty() && named_like_one(name) {
        found.add_der(data);
    }
    (!found.is_empty()).then(|| report(&found, now))
}

fn plural(n: usize, one: &str, many: &str) -> Option<String> {
    match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    }
}

fn report(found: &Found, now: i64) -> Report {
    let certs: Vec<_> = found.certs.iter().filter_map(|(d, _)| x509::parse(d)).collect();
    let cert_lines: Vec<Option<usize>> = found.certs.iter().map(|(_, l)| *l).collect();
    let requests: Vec<_> =
        found.requests.iter().filter_map(|(d, l)| Some((x509::parse_request(d)?, *l))).collect();

    let mut summary = Vec::new();
    let mut cert_rows = Vec::new();
    for (i, cert) in certs.iter().enumerate() {
        let (state, tone) = x509::validity(cert, now);
        let until = x509::date(cert.validity().not_after.timestamp());
        let mut row = Row::new(
            "Certificate",
            format!("#{} {}  until {until} ({state})", i + 1, x509::short_name(cert.subject())),
        )
        .tone(tone);
        row.line = cert_lines[i];
        summary.push(row);
        cert_rows.extend(x509::rows(i + 1, cert, cert_lines[i], now));
    }
    let mut request_rows = Vec::new();
    for (i, (req, line)) in requests.iter().enumerate() {
        let mut row = Row::new(
            "Certificate request",
            x509::short_name(&req.certification_request_info.subject),
        );
        row.line = *line;
        summary.push(row);
        request_rows.extend(x509::request_rows(i + 1, req, *line));
    }
    let mut key_rows = Vec::new();
    for (i, k) in found.keys.iter().enumerate() {
        let kind = if k.private { "Private key" } else { "Public key" };
        let matched = k.public.as_ref().map(|p| chain::certificate_for(p, &certs));
        let (matches, tone) = match matched {
            Some(Some(c)) => (
                format!("certificate #{} {}", c + 1, x509::short_name(certs[c].subject())),
                Tone::Good,
            ),
            Some(None) if certs.is_empty() => (String::new(), Tone::Normal),
            Some(None) => ("no certificate in this file".to_string(), Tone::Dim),
            None => ("can't tell: the public key isn't stored in the file".to_string(), Tone::Dim),
        };
        let mut what = k.algorithm.clone();
        if k.encryption.is_some() {
            what.push_str(", encrypted");
        }
        if let Some(Some(c)) = matched {
            what.push_str(&format!(" — certificate #{}", c + 1));
        }
        let mut row = Row::new(kind, what).tone(if matched.flatten().is_some() {
            Tone::Good
        } else {
            Tone::Normal
        });
        row.line = k.line;
        summary.push(row);
        let mut rows = vec![Row::heading(Label::Text(format!("#{}", i + 1)), kind, k.line)];
        rows.push(Row::new("Algorithm", k.algorithm.clone()).tone(
            if k.public.as_ref().is_some_and(|p| p.weak()) { Tone::Warn } else { Tone::Normal },
        ));
        rows.push(Row::new("Format", k.format));
        if let Some(how) = &k.encryption {
            rows.push(Row::new("Encryption", how.clone()));
        }
        if let Some(p) = &k.public {
            rows.push(Row::new("SPKI SHA-256", x509::spki_pin(&p.spki())));
        }
        if !matches.is_empty() {
            rows.push(Row::new("Matches", matches).tone(tone));
        }
        rows.iter_mut().skip(1).for_each(|r| r.line = k.line);
        key_rows.extend(rows);
    }
    // SSH keys and certificates go on after the others, numbered on from them.
    let numbered = |rows: &[Row], n: usize| -> Vec<Row> {
        let mut rows = rows.to_vec();
        if let Some(head) = rows.first_mut() {
            head.label = Label::Text(format!("#{n}"));
        }
        rows
    };
    for (i, (sum, rows, _)) in found.ssh.keys.iter().enumerate() {
        summary.push(sum.clone());
        key_rows.extend(numbered(rows, found.keys.len() + i + 1));
    }
    for (i, (sum, rows)) in found.ssh.certs.iter().enumerate() {
        let n = certs.len() + i + 1;
        let mut sum = sum.clone();
        sum.value = format!("#{n} {}", sum.value);
        summary.push(sum);
        cert_rows.extend(numbered(rows, n));
    }
    let entry_count = found.ssh.entries.len() - found.ssh.unreadable;
    if let Some(kind) = found.ssh.entries_kind {
        let label = if kind == ssh::Lines::KnownHosts { "Known hosts" } else { "Authorized keys" };
        let mut row = Row::new(label, format!("{entry_count} keys"));
        if found.ssh.unreadable > 0 {
            row.value.push_str(&format!(", {} lines can't be read", found.ssh.unreadable));
            row.tone = Tone::Bad;
        }
        summary.push(row);
    }
    let entry_rows: Vec<Row> = found.ssh.entries.concat();

    let private = found.keys.iter().filter(|k| k.private).count()
        + found.ssh.keys.iter().filter(|k| k.2).count();
    let public = found.keys.len() + found.ssh.keys.len() - private;
    let entries = match found.ssh.entries_kind {
        Some(ssh::Lines::KnownHosts) => plural(entry_count, "known host", "known hosts"),
        Some(_) => plural(entry_count, "authorized key", "authorized keys"),
        None => None,
    };
    let text: Vec<String> = [
        plural(certs.len(), "certificate", "certificates"),
        plural(found.ssh.certs.len(), "SSH certificate", "SSH certificates"),
        plural(requests.len(), "request", "requests"),
        plural(private, "private key", "private keys"),
        plural(public, "public key", "public keys"),
        entries,
    ]
    .into_iter()
    .flatten()
    .collect();
    let chain_rows = if certs.is_empty() { Vec::new() } else { chain::rows(&certs, &cert_lines) };
    let tabs = [
        (Tab::Summary, summary),
        (Tab::Certificates, cert_rows),
        (Tab::Requests, request_rows),
        (Tab::Keys, key_rows),
        (Tab::Entries, entry_rows),
        (Tab::Chain, chain_rows),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .collect();
    Report { summary: text.join(", "), tabs }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARCH_2025: i64 = 1_740_787_200;

    fn tab(r: &Report, t: Tab) -> &[Row] {
        r.tabs.iter().find(|(x, _)| *x == t).map(|(_, rows)| rows.as_slice()).unwrap_or(&[])
    }

    #[test]
    fn only_certificate_and_key_files_are_taken() {
        assert!(sniff("server.crt", b"anything"));
        assert!(sniff("chain", b"\n\n-----BEGIN CERTIFICATE-----\n"));
        assert!(!sniff("README.md", b"# Setup\n\n-----BEGIN CERTIFICATE-----\n"));
        assert!(
            inspect("README.md", testdata::LEAF.as_bytes(), MARCH_2025).is_some(),
            "a PEM file by content"
        );
        assert!(inspect("notes.pem", b"nothing to see", MARCH_2025).is_none());
        assert!(inspect("x.der", &[0x30, 0x03, 0x02, 0x01, 0x00], MARCH_2025).is_none());
    }

    #[test]
    fn a_bundle_with_its_key_is_summed_up_tab_by_tab() {
        let bundle = format!(
            "# the site\n{}{}{}{}",
            testdata::LEAF,
            testdata::INTERMEDIATE,
            testdata::ROOT,
            testdata::LEAF_KEY
        );
        let r = inspect("site.pem", bundle.as_bytes(), MARCH_2025).unwrap();
        assert_eq!(r.summary, "3 certificates, 1 private key");
        let tabs: Vec<Tab> = r.tabs.iter().map(|(t, _)| *t).collect();
        assert_eq!(tabs, vec![Tab::Summary, Tab::Certificates, Tab::Keys, Tab::Chain]);
        let summary = tab(&r, Tab::Summary);
        assert_eq!(summary[0].line, Some(1), "the leaf starts on line 2");
        assert!(summary[0].value.contains("expires in 31 days"), "{}", summary[0].value);
        let keys = tab(&r, Tab::Keys);
        let matches = keys.iter().find(|r| r.label == Label::Key("Matches")).unwrap();
        assert_eq!(
            (matches.value.as_str(), matches.tone),
            ("certificate #1 CN=www.example.test", Tone::Good)
        );
        // Nothing of the key itself is shown.
        let key_der = pem_blocks(testdata::LEAF_KEY).pop().unwrap().der;
        let secret = &key_der[7..39];
        let hexed = x509::hex(secret);
        assert!(r.tabs.iter().flat_map(|(_, rows)| rows).all(|row| !row.value.contains(&hexed)));
    }

    #[test]
    fn ssh_files_are_taken_by_name_or_content_and_numbered_with_the_rest() {
        assert!(sniff("id_ed25519", b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(sniff("known_hosts", b"|1|abc"));
        assert!(sniff("deploy", testdata::SSH_ED25519_PUB.as_bytes()));
        assert!(!sniff("install.sh", b"#!/bin/sh\nssh-keygen -t ed25519\n"));

        let keys = format!("{}{}", testdata::SSH_ED25519_KEY, testdata::SSH_RSA_PUB);
        let r = inspect("keys.txt", keys.as_bytes(), MARCH_2025).unwrap();
        assert_eq!(r.summary, "1 private key, 1 public key");
        let heads: Vec<&Label> =
            tab(&r, Tab::Keys).iter().filter(|x| x.depth == 0).map(|x| &x.label).collect();
        assert_eq!(heads, vec![&Label::Text("#1".into()), &Label::Text("#2".into())]);

        let r = inspect("ca.pub", testdata::SSH_CA_PUB.as_bytes(), MARCH_2025).unwrap();
        let fp = tab(&r, Tab::Keys).iter().find(|x| x.label == Label::Key("Fingerprint")).unwrap();
        assert_eq!(fp.value, testdata::SSH_CA_FP);

        let r =
            inspect("authorized_keys", testdata::AUTHORIZED_KEYS.as_bytes(), MARCH_2025).unwrap();
        assert_eq!(r.summary, "2 authorized keys");
        let sum = &tab(&r, Tab::Summary)[0];
        assert_eq!((sum.value.as_str(), sum.tone), ("2 keys, 1 lines can't be read", Tone::Bad));
        assert_eq!(tab(&r, Tab::Entries).iter().filter(|x| x.depth == 0).count(), 3);

        let cert = format!("{}{}", testdata::LEAF, testdata::SSH_CERT);
        let r = inspect("mixed.pem", cert.as_bytes(), MARCH_2025).unwrap();
        assert_eq!(r.summary, "1 certificate, 1 SSH certificate");
        assert!(tab(&r, Tab::Summary)[1].value.starts_with("#2 alice-2025"));
    }

    #[test]
    fn der_files_are_read_by_their_name() {
        let der = pem_blocks(testdata::RSA_CERT).pop().unwrap().der;
        let r = inspect("rsa.cer", &der, MARCH_2025).unwrap();
        assert_eq!(r.summary, "1 certificate");
        assert!(inspect("rsa.bin", &der, MARCH_2025).is_none(), "not by content alone");
        let key = pem_blocks(testdata::LEAF_KEY_ENCRYPTED).pop().unwrap().der;
        let r = inspect("leaf.key", &key, MARCH_2025).unwrap();
        let keys = tab(&r, Tab::Keys);
        assert!(keys.iter().any(|k| k.label == Label::Key("Encryption")));
    }
}
