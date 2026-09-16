//! How the certificates in one file hang together: which issued which,
//! whether each signature checks out against its issuer's key, whether they
//! come in the order a server has to send them (each followed by its issuer),
//! and which certificate a key belongs to.
//!
//! Only the file is looked at: not the system's trusted certificates, not
//! host names, not revocation.

use super::keys::PublicKey;
use super::x509::{public_key, short_name};
use super::{Label, Row, Tone};
use x509_parser::certificate::X509Certificate;
use x509_parser::error::X509Error;
use x509_parser::extensions::ParsedExtension;

fn key_id<'a>(cert: &'a X509Certificate, authority: bool) -> Option<&'a [u8]> {
    cert.extensions().iter().find_map(|e| match e.parsed_extension() {
        ParsedExtension::AuthorityKeyIdentifier(a) if authority => {
            a.key_identifier.as_ref().map(|k| k.0)
        }
        ParsedExtension::SubjectKeyIdentifier(k) if !authority => Some(k.0),
        _ => None,
    })
}

/// Whether the certificate names itself as its issuer.
fn self_issued(cert: &X509Certificate) -> bool {
    cert.subject().as_raw() == cert.issuer().as_raw()
}

/// The certificate among `certs` that issued certificate `i`: the one whose
/// subject is its issuer (and whose key identifier matches, when both give
/// one) — itself when it is self-issued.
pub fn issuer_of(certs: &[X509Certificate], i: usize) -> Option<usize> {
    let c = &certs[i];
    if self_issued(c) {
        return Some(i);
    }
    let wanted = key_id(c, true);
    let mut candidates = certs.iter().enumerate().filter(|&(j, o)| {
        j != i
            && o.subject().as_raw() == c.issuer().as_raw()
            && match (wanted, key_id(o, false)) {
                (Some(a), Some(s)) => a == s,
                _ => true,
            }
    });
    let first = candidates.next().map(|(j, _)| j);
    // Of several (a re-issued CA), the one whose key made the signature.
    first.map(|f| {
        std::iter::once(f)
            .chain(candidates.map(|(j, _)| j))
            .find(|&j| c.verify_signature(Some(certs[j].public_key())).is_ok())
            .unwrap_or(f)
    })
}

/// The certificate holding the public key `key`.
pub fn certificate_for(key: &PublicKey, certs: &[X509Certificate]) -> Option<usize> {
    certs.iter().position(|c| public_key(c).as_ref() == Some(key))
}

/// The Chain tab: whether the order is right, then each certificate's issuer
/// and signature.
pub fn rows(certs: &[X509Certificate], lines: &[Option<usize>]) -> Vec<Row> {
    let mut rows = Vec::new();
    // Only certificates issued by another one in the file have an order to
    // keep; a bundle of roots has none.
    let issued = (0..certs.len()).any(|i| issuer_of(certs, i).is_some_and(|j| j != i));
    if issued {
        let misplaced = (0..certs.len()).find_map(|i| {
            let j = issuer_of(certs, i)?;
            (j != i && j != i + 1).then_some((i, j))
        });
        rows.push(match misplaced {
            None => {
                Row::new("Order", "each certificate is followed by its issuer").tone(Tone::Good)
            }
            Some((i, j)) => {
                Row::new("Order", format!("#{} is not followed by its issuer, #{}", i + 1, j + 1))
                    .tone(Tone::Warn)
            }
        });
    }
    rows.push(
        Row::new("Not checked", "trust by the system's certificates, host names, revocation")
            .tone(Tone::Dim),
    );
    for (i, cert) in certs.iter().enumerate() {
        let line = lines.get(i).copied().flatten();
        let (text, tone) = match issuer_of(certs, i) {
            Some(j) => {
                let by = if j == i {
                    "self-signed".to_string()
                } else {
                    format!("issued by #{} {}", j + 1, short_name(certs[j].subject()))
                };
                match cert.verify_signature(Some(certs[j].public_key())) {
                    Ok(()) => (format!("{by}, signature valid"), Tone::Good),
                    Err(X509Error::SignatureUnsupportedAlgorithm) => {
                        (format!("{by}, signature not checked: algorithm not supported"), Tone::Dim)
                    }
                    Err(_) => (format!("{by}, signature does not verify"), Tone::Bad),
                }
            }
            None => (format!("{}, not in this file", short_name(cert.issuer())), Tone::Dim),
        };
        rows.push(Row::heading(
            Label::Text(format!("#{}", i + 1)),
            short_name(cert.subject()),
            line,
        ));
        let mut issuer = Row::new("Issuer", text).tone(tone);
        issuer.line = line;
        rows.push(issuer);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::{pem_blocks, testdata, x509};
    use super::*;

    fn ders(pems: &[&str]) -> Vec<Vec<u8>> {
        pems.iter().map(|p| pem_blocks(p).pop().unwrap().der).collect()
    }

    #[test]
    fn a_chain_in_order_links_and_verifies() {
        let data = ders(&[testdata::LEAF, testdata::INTERMEDIATE, testdata::ROOT]);
        let certs: Vec<_> = data.iter().map(|d| x509::parse(d).unwrap()).collect();
        assert_eq!(issuer_of(&certs, 0), Some(1));
        assert_eq!(issuer_of(&certs, 1), Some(2));
        assert_eq!(issuer_of(&certs, 2), Some(2), "the root issued itself");
        let rows = rows(&certs, &[None, None, None]);
        assert_eq!(rows[0].tone, Tone::Good, "{:?}", rows[0]);
        let issuers: Vec<(&str, Tone)> = rows
            .iter()
            .filter(|r| r.label == Label::Key("Issuer"))
            .map(|r| (r.value.as_str(), r.tone))
            .collect();
        assert_eq!(
            issuers,
            vec![
                ("issued by #2 CN=Rat Test Intermediate, signature valid", Tone::Good),
                ("issued by #3 CN=Rat Test Root, signature valid", Tone::Good),
                ("self-signed, signature valid", Tone::Good),
            ]
        );
    }

    #[test]
    fn a_misordered_or_incomplete_chain_says_so() {
        let data = ders(&[testdata::INTERMEDIATE, testdata::LEAF]);
        let certs: Vec<_> = data.iter().map(|d| x509::parse(d).unwrap()).collect();
        let rows = rows(&certs, &[None, None]);
        assert_eq!(
            (rows[0].value.as_str(), rows[0].tone),
            ("#2 is not followed by its issuer, #1", Tone::Warn)
        );
        let int = rows.iter().find(|r| r.label == Label::Key("Issuer")).unwrap();
        assert_eq!(
            (int.value.as_str(), int.tone),
            ("CN=Rat Test Root, not in this file", Tone::Dim)
        );
    }

    #[test]
    fn a_bundle_of_roots_has_no_order_to_keep() {
        let data = ders(&[testdata::ROOT, testdata::RSA_CERT]);
        let certs: Vec<_> = data.iter().map(|d| x509::parse(d).unwrap()).collect();
        let rows = rows(&certs, &[None, None]);
        assert!(rows.iter().all(|r| r.label != Label::Key("Order")));
    }

    #[test]
    fn a_tampered_signature_does_not_verify() {
        let mut data = ders(&[testdata::LEAF, testdata::INTERMEDIATE]);
        // Flip a bit in the leaf's validity dates, inside what was signed.
        let at = data[0].windows(13).position(|w| w == b"250101000000Z").unwrap();
        data[0][at + 1] = b'6';
        let certs: Vec<_> = data.iter().map(|d| x509::parse(d).unwrap()).collect();
        let rows = rows(&certs, &[None, None]);
        let leaf = rows.iter().find(|r| r.label == Label::Key("Issuer")).unwrap();
        assert_eq!(leaf.tone, Tone::Bad, "{}", leaf.value);
    }
}
