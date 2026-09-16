//! X.509 certificates and certificate requests, field by field.

use super::keys::PublicKey;
use super::{Label, Row, Tone};
use sha2::Digest;
use x509_parser::certificate::X509Certificate;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
use x509_parser::prelude::FromDer;
use x509_parser::x509::X509Name;

/// A certificate expiring within this many days is flagged.
pub const EXPIRY_WARNING_DAYS: i64 = 30;

pub fn parse(der: &[u8]) -> Option<X509Certificate<'_>> {
    x509_parser::parse_x509_certificate(der).ok().map(|(_, c)| c)
}

pub fn parse_request(der: &[u8]) -> Option<X509CertificationRequest<'_>> {
    X509CertificationRequest::from_der(der).ok().map(|(_, r)| r)
}

/// A name as it is best known: its common name, else the whole of it.
pub fn short_name(name: &X509Name) -> String {
    name.iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map_or_else(|| name.to_string(), |cn| format!("CN={cn}"))
}

/// A Unix time as a UTC date and time.
pub fn date(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = crate::util::bytes::civil_parts(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// Bytes as colon-separated hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Whether the certificate is valid at `now`, said with the days to go or
/// gone.
pub fn validity(cert: &X509Certificate, now: i64) -> (String, Tone) {
    let v = cert.validity();
    let (from, until) = (v.not_before.timestamp(), v.not_after.timestamp());
    if now < from {
        let days = (from - now).div_euclid(86_400);
        return (format!("not valid for another {days} days"), Tone::Bad);
    }
    let days = (until - now).div_euclid(86_400);
    if now > until {
        let ago = (now - until).div_euclid(86_400);
        (format!("expired {ago} days ago"), Tone::Bad)
    } else if days < EXPIRY_WARNING_DAYS {
        (format!("expires in {days} days"), Tone::Warn)
    } else {
        (format!("expires in {days} days"), Tone::Good)
    }
}

/// The public key a certificate holds.
pub fn public_key(cert: &X509Certificate) -> Option<PublicKey> {
    PublicKey::from_spki(cert.public_key().raw)
}

/// A signature algorithm's name, flagged when it is no longer safe.
fn signature(oid: &x509_parser::der_parser::oid::Oid) -> (String, Tone) {
    let registry = x509_parser::objects::oid_registry();
    let name = x509_parser::objects::oid2sn(oid, registry)
        .map_or_else(|_| oid.to_id_string(), str::to_string);
    let lower = name.to_ascii_lowercase();
    let weak = lower.contains("md5") || lower.contains("sha1") || lower.contains("md2");
    (name, if weak { Tone::Warn } else { Tone::Normal })
}

fn general_name(g: &GeneralName) -> String {
    match g {
        GeneralName::DNSName(s) => format!("DNS:{s}"),
        GeneralName::RFC822Name(s) => format!("email:{s}"),
        GeneralName::URI(s) => format!("URI:{s}"),
        GeneralName::IPAddress(b) => match b.len() {
            4 => format!(
                "IP:{}",
                std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*b).unwrap_or_default())
            ),
            16 => format!(
                "IP:{}",
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*b).unwrap_or_default())
            ),
            _ => format!("IP:{}", hex(b)),
        },
        GeneralName::DirectoryName(n) => format!("DirName:{n}"),
        GeneralName::RegisteredID(o) => format!("RID:{}", o.to_id_string()),
        other => format!("{other:?}"),
    }
}

fn names(list: &[GeneralName]) -> String {
    list.iter().map(general_name).collect::<Vec<_>>().join(", ")
}

/// The extensions a certificate or request carries, as rows.
fn extension_rows<'a>(exts: impl Iterator<Item = (&'a ParsedExtension<'a>, bool)>) -> Vec<Row> {
    let mut rows = Vec::new();
    let crit = |c: bool| if c { " (critical)" } else { "" };
    for (ext, critical) in exts {
        match ext {
            ParsedExtension::SubjectAlternativeName(san) => {
                rows.push(Row::new("Subject alternative names", names(&san.general_names)));
            }
            ParsedExtension::BasicConstraints(bc) => {
                let text = match (bc.ca, bc.path_len_constraint) {
                    (true, Some(n)) => format!("CA, path length {n}"),
                    (true, None) => "CA".to_string(),
                    (false, _) => "not a CA".to_string(),
                };
                rows.push(Row::new("Basic constraints", format!("{text}{}", crit(critical))));
            }
            ParsedExtension::KeyUsage(ku) => {
                let flags = [
                    (ku.digital_signature(), "Digital Signature"),
                    (ku.non_repudiation(), "Non Repudiation"),
                    (ku.key_encipherment(), "Key Encipherment"),
                    (ku.data_encipherment(), "Data Encipherment"),
                    (ku.key_agreement(), "Key Agreement"),
                    (ku.key_cert_sign(), "Certificate Sign"),
                    (ku.crl_sign(), "CRL Sign"),
                    (ku.encipher_only(), "Encipher Only"),
                    (ku.decipher_only(), "Decipher Only"),
                ];
                let on: Vec<&str> = flags.iter().filter(|f| f.0).map(|f| f.1).collect();
                rows.push(Row::new("Key usage", format!("{}{}", on.join(", "), crit(critical))));
            }
            ParsedExtension::ExtendedKeyUsage(eku) => {
                let mut on: Vec<String> = [
                    (eku.any, "any"),
                    (eku.server_auth, "TLS server"),
                    (eku.client_auth, "TLS client"),
                    (eku.code_signing, "code signing"),
                    (eku.email_protection, "email"),
                    (eku.time_stamping, "time stamping"),
                    (eku.ocsp_signing, "OCSP signing"),
                ]
                .iter()
                .filter(|f| f.0)
                .map(|f| f.1.to_string())
                .collect();
                on.extend(eku.other.iter().map(|o| o.to_id_string()));
                rows.push(Row::new("Extended key usage", on.join(", ")));
            }
            ParsedExtension::SubjectKeyIdentifier(id) => {
                rows.push(Row::new("Subject key ID", hex(id.0)));
            }
            ParsedExtension::AuthorityKeyIdentifier(aki) => {
                if let Some(id) = &aki.key_identifier {
                    rows.push(Row::new("Authority key ID", hex(id.0)));
                }
            }
            ParsedExtension::CRLDistributionPoints(points) => {
                let uris: Vec<String> = points
                    .iter()
                    .filter_map(|p| match &p.distribution_point {
                        Some(DistributionPointName::FullName(n)) => Some(names(n)),
                        _ => None,
                    })
                    .collect();
                rows.push(Row::new("CRL distribution points", uris.join(", ")));
            }
            ParsedExtension::AuthorityInfoAccess(aia) => {
                let text: Vec<String> = aia
                    .iter()
                    .map(|d| {
                        let method = match d.access_method.to_id_string().as_str() {
                            "1.3.6.1.5.5.7.48.1" => "OCSP".to_string(),
                            "1.3.6.1.5.5.7.48.2" => "CA issuers".to_string(),
                            other => other.to_string(),
                        };
                        format!("{method} {}", general_name(&d.access_location))
                    })
                    .collect();
                rows.push(Row::new("Authority information access", text.join(", ")));
            }
            _ => {}
        }
    }
    rows
}

/// A certificate's rows: a heading naming it, then its fields.
pub fn rows(n: usize, cert: &X509Certificate, line: Option<usize>, now: i64) -> Vec<Row> {
    let (state, tone) = validity(cert, now);
    let mut rows = vec![
        Row::heading(
            Label::Text(format!("#{n}")),
            format!("{}  ({state})", short_name(cert.subject())),
            line,
        )
        .tone(tone),
    ];
    let v = cert.validity();
    rows.push(Row::new("Subject", cert.subject().to_string()));
    rows.push(Row::new("Issuer", cert.issuer().to_string()));
    rows.push(Row::new("Serial number", hex(cert.raw_serial())));
    rows.push(Row::new("Valid from", date(v.not_before.timestamp())));
    rows.push(
        Row::new("Valid until", format!("{}  ({state})", date(v.not_after.timestamp()))).tone(tone),
    );
    match public_key(cert) {
        Some(k) => rows.push(Row::new("Public key", k.describe()).tone(if k.weak() {
            Tone::Warn
        } else {
            Tone::Normal
        })),
        None => {
            rows.push(Row::new("Public key", cert.public_key().algorithm.algorithm.to_id_string()))
        }
    }
    let (sig, sig_tone) = signature(&cert.signature_algorithm.algorithm);
    rows.push(Row::new("Signature algorithm", sig).tone(sig_tone));
    rows.extend(extension_rows(
        cert.extensions().iter().map(|e| (e.parsed_extension(), e.critical)),
    ));
    rows.push(Row::new("SHA-256 fingerprint", hex(&sha2::Sha256::digest(cert.as_raw()))));
    rows.push(Row::new("SHA-1 fingerprint", hex(&sha1::Sha1::digest(cert.as_raw()))));
    rows.push(Row::new("SPKI SHA-256", spki_pin(cert.public_key().raw)));
    for r in rows.iter_mut().skip(1) {
        r.line = line;
    }
    rows
}

/// A key pin: the SubjectPublicKeyInfo's SHA-256, in base64.
pub fn spki_pin(spki: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(spki))
}

/// A certificate request's rows: a heading, then its fields.
pub fn request_rows(n: usize, req: &X509CertificationRequest, line: Option<usize>) -> Vec<Row> {
    let info = &req.certification_request_info;
    let mut rows =
        vec![Row::heading(Label::Text(format!("#{n}")), short_name(&info.subject), line)];
    rows.push(Row::new("Subject", info.subject.to_string()));
    match PublicKey::from_spki(info.subject_pki.raw) {
        Some(k) => rows.push(Row::new("Public key", k.describe()).tone(if k.weak() {
            Tone::Warn
        } else {
            Tone::Normal
        })),
        None => {
            rows.push(Row::new("Public key", info.subject_pki.algorithm.algorithm.to_id_string()))
        }
    }
    let (sig, sig_tone) = signature(&req.signature_algorithm.algorithm);
    rows.push(Row::new("Signature algorithm", sig).tone(sig_tone));
    let signed = match req.verify_signature() {
        Ok(()) => ("valid: made with this key".to_string(), Tone::Good),
        Err(x509_parser::error::X509Error::SignatureUnsupportedAlgorithm) => {
            ("not checked: algorithm not supported".to_string(), Tone::Dim)
        }
        Err(_) => ("does not verify".to_string(), Tone::Bad),
    };
    rows.push(Row::new("Signature", signed.0).tone(signed.1));
    if let Some(exts) = req.requested_extensions() {
        rows.extend(extension_rows(exts.map(|e| (e, false))));
    }
    rows.push(Row::new("SPKI SHA-256", spki_pin(info.subject_pki.raw)));
    for r in rows.iter_mut().skip(1) {
        r.line = line;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::{pem_blocks, testdata};
    use super::*;

    fn der(pem: &str) -> Vec<u8> {
        pem_blocks(pem).pop().unwrap().der
    }

    fn value<'a>(rows: &'a [Row], label: &str) -> &'a Row {
        rows.iter()
            .find(|r| matches!(r.label, Label::Key(k) if k == label))
            .unwrap_or_else(|| panic!("no {label}"))
    }

    /// 2025-03-01, a month before the leaf expires.
    const MARCH_2025: i64 = 1_740_787_200;

    #[test]
    fn a_certificate_lays_out_its_fields() {
        let data = der(testdata::LEAF);
        let cert = parse(&data).unwrap();
        let rows = rows(3, &cert, Some(40), MARCH_2025);
        assert_eq!(rows[0].label, Label::Text("#3".into()));
        assert!(rows[0].value.starts_with("CN=www.example.test"), "{}", rows[0].value);
        assert_eq!(value(&rows, "Issuer").value, "C=AT, O=Rat Test, CN=Rat Test Intermediate");
        assert_eq!(value(&rows, "Serial number").value, "20:02");
        assert_eq!(value(&rows, "Valid from").value, "2025-01-01 00:00:00 UTC");
        let until = value(&rows, "Valid until");
        assert_eq!(
            (until.value.as_str(), until.tone),
            ("2025-04-01 00:00:00 UTC  (expires in 31 days)", Tone::Good)
        );
        assert_eq!(value(&rows, "Public key").value, "EC P-256");
        assert_eq!(value(&rows, "Signature algorithm").value, "ecdsa-with-SHA256");
        assert_eq!(
            value(&rows, "Subject alternative names").value,
            "DNS:www.example.test, DNS:example.test, IP:192.0.2.7"
        );
        assert_eq!(value(&rows, "Key usage").value, "Digital Signature (critical)");
        assert_eq!(value(&rows, "Extended key usage").value, "TLS server, TLS client");
        assert_eq!(value(&rows, "Basic constraints").value, "not a CA");
        assert_eq!(
            value(&rows, "CRL distribution points").value,
            "URI:http://crl.example.test/int.crl"
        );
        assert_eq!(
            value(&rows, "Authority information access").value,
            "OCSP URI:http://ocsp.example.test"
        );
        assert_eq!(value(&rows, "SHA-256 fingerprint").value, testdata::LEAF_SHA256);
        assert!(rows.iter().all(|r| r.line == Some(40)), "every row goes back to the certificate");
        let int = der(testdata::INTERMEDIATE);
        let int_rows = super::rows(2, &parse(&int).unwrap(), None, MARCH_2025);
        assert_eq!(value(&int_rows, "Basic constraints").value, "CA, path length 0 (critical)");
    }

    #[test]
    fn expiry_is_told_by_the_days_left() {
        let data = der(testdata::LEAF);
        let cert = parse(&data).unwrap();
        let day = 86_400;
        let until = cert.validity().not_after.timestamp();
        assert_eq!(validity(&cert, until - 10 * day), ("expires in 10 days".into(), Tone::Warn));
        assert_eq!(validity(&cert, until + 3 * day), ("expired 3 days ago".into(), Tone::Bad));
        let from = cert.validity().not_before.timestamp();
        assert_eq!(validity(&cert, from - 2 * day).1, Tone::Bad);
    }

    #[test]
    fn a_request_shows_what_it_asks_for_and_that_its_key_signed_it() {
        let data = der(testdata::LEAF_CSR);
        let req = parse_request(&data).unwrap();
        let rows = request_rows(1, &req, None);
        assert_eq!(value(&rows, "Subject").value, "CN=www.example.test");
        assert_eq!(value(&rows, "Signature").tone, Tone::Good);
        assert_eq!(
            value(&rows, "Subject alternative names").value,
            "DNS:www.example.test, DNS:example.test, IP:192.0.2.7"
        );
    }
}
