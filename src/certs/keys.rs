//! Private and public keys: which kind and how strong, how they are stored,
//! whether a private key is encrypted and how — and, where the file holds
//! enough to tell without decrypting anything, its public key, which is what
//! a key is recognised by. Key material itself is never shown.

use super::der::{self, Tlv};

/// A public key, as keys and certificates are matched by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicKey {
    Rsa {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    /// A point on the curve named by its object identifier.
    Ec {
        curve: String,
        point: Vec<u8>,
    },
    /// A key that is its bytes (Ed25519, X448, …), by the algorithm's
    /// object identifier.
    Raw {
        alg: String,
        key: Vec<u8>,
    },
}

impl PublicKey {
    /// The key in a SubjectPublicKeyInfo.
    pub fn from_spki(spki: &[u8]) -> Option<PublicKey> {
        let m = der::sequence(spki)?;
        let (alg, params) = algorithm(m.first()?)?;
        let bits = m.get(1).filter(|t| t.tag == der::BIT_STRING)?.body.get(1..)?;
        match alg.as_str() {
            der::RSA => rsa_public(bits),
            der::EC => {
                let curve = params.filter(|p| p.tag == der::OID).map(|p| der::oid_text(p.body))?;
                Some(PublicKey::Ec { curve, point: bits.to_vec() })
            }
            _ => Some(PublicKey::Raw { alg, key: bits.to_vec() }),
        }
    }

    /// The SubjectPublicKeyInfo holding this key.
    pub fn spki(&self) -> Vec<u8> {
        let (alg, key) = match self {
            PublicKey::Rsa { n, e } => {
                let alg = [der::oid(der::RSA), der::tlv(der::NULL, &[])].concat();
                let key = der::tlv(der::SEQUENCE, &[der::integer(n), der::integer(e)].concat());
                (alg, key)
            }
            PublicKey::Ec { curve, point } => {
                ([der::oid(der::EC), der::oid(curve)].concat(), point.clone())
            }
            PublicKey::Raw { alg, key } => (der::oid(alg), key.clone()),
        };
        let bits = [&[0u8][..], &key].concat();
        der::tlv(
            der::SEQUENCE,
            &[der::tlv(der::SEQUENCE, &alg), der::tlv(der::BIT_STRING, &bits)].concat(),
        )
    }

    /// `RSA 2048 bits`, `EC P-256`, `Ed25519`.
    pub fn describe(&self) -> String {
        match self {
            PublicKey::Rsa { n, .. } => format!("RSA {} bits", der::bit_len(n)),
            PublicKey::Ec { curve, .. } => {
                format!("EC {}", der::oid_name(curve).map_or_else(|| curve.clone(), str::to_string))
            }
            PublicKey::Raw { alg, .. } => {
                der::oid_name(alg).map_or_else(|| alg.clone(), str::to_string)
            }
        }
    }

    /// Whether the key is too small to trust today.
    pub fn weak(&self) -> bool {
        matches!(self, PublicKey::Rsa { n, .. } if der::bit_len(n) < 2048)
    }
}

/// An RSAPublicKey (PKCS#1): the modulus and exponent.
fn rsa_public(data: &[u8]) -> Option<PublicKey> {
    let m = der::sequence(data)?;
    let (n, e) = (m.first()?, m.get(1)?);
    (n.tag == der::INTEGER && e.tag == der::INTEGER).then(|| PublicKey::Rsa {
        n: der::unsigned(n.body).to_vec(),
        e: der::unsigned(e.body).to_vec(),
    })
}

/// An AlgorithmIdentifier: the algorithm's object identifier and its
/// parameters.
fn algorithm<'a>(t: &Tlv<'a>) -> Option<(String, Option<Tlv<'a>>)> {
    if t.tag != der::SEQUENCE {
        return None;
    }
    let m = der::children(t.body)?;
    let alg = m.first().filter(|o| o.tag == der::OID)?;
    Some((der::oid_text(alg.body), m.get(1).copied()))
}

/// A key found in a file.
#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub private: bool,
    /// `RSA 2048 bits`, `EC P-256`.
    pub algorithm: String,
    /// `PKCS#1`, `PKCS#8`, `SEC1`, `SubjectPublicKeyInfo`.
    pub format: &'static str,
    /// How a private key is encrypted, when it is.
    pub encryption: Option<String>,
    pub public: Option<PublicKey>,
    /// The line of the file the key starts on.
    pub line: Option<usize>,
}

impl KeyInfo {
    fn new(private: bool, format: &'static str, public: PublicKey) -> KeyInfo {
        KeyInfo {
            private,
            algorithm: public.describe(),
            format,
            encryption: None,
            public: Some(public),
            line: None,
        }
    }
}

/// The key in a PEM block labelled `label`, with the block's `headers`.
pub fn parse(label: &str, headers: &[(String, String)], data: &[u8]) -> Option<KeyInfo> {
    match label {
        "RSA PRIVATE KEY" => {
            // The old OpenSSL encryption keeps the label and says so in headers.
            let dek = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("DEK-Info"));
            if let Some((_, cipher)) = dek {
                let cipher = cipher.split(',').next().unwrap_or(cipher).to_string();
                return Some(KeyInfo {
                    private: true,
                    algorithm: "RSA".into(),
                    format: "PKCS#1",
                    encryption: Some(format!("{cipher} (OpenSSL)")),
                    public: None,
                    line: None,
                });
            }
            pkcs1_private(data)
        }
        "EC PRIVATE KEY" => sec1_private(data, None),
        "PRIVATE KEY" => pkcs8_private(data),
        "ENCRYPTED PRIVATE KEY" => pkcs8_encrypted(data),
        "PUBLIC KEY" => {
            PublicKey::from_spki(data).map(|k| KeyInfo::new(false, "SubjectPublicKeyInfo", k))
        }
        "RSA PUBLIC KEY" => rsa_public(data).map(|k| KeyInfo::new(false, "PKCS#1", k)),
        _ => None,
    }
}

/// A key in a DER file, whatever its encoding.
pub fn parse_der(data: &[u8]) -> Option<KeyInfo> {
    pkcs8_private(data)
        .or_else(|| pkcs8_encrypted(data))
        .or_else(|| {
            PublicKey::from_spki(data).map(|k| KeyInfo::new(false, "SubjectPublicKeyInfo", k))
        })
        .or_else(|| pkcs1_private(data))
        .or_else(|| sec1_private(data, None))
}

/// An RSAPrivateKey (PKCS#1): version, modulus, public exponent, then the
/// private parts.
fn pkcs1_private(data: &[u8]) -> Option<KeyInfo> {
    let m = der::sequence(data)?;
    if m.len() < 9 || m.iter().take(9).any(|t| t.tag != der::INTEGER) {
        return None;
    }
    let public = PublicKey::Rsa {
        n: der::unsigned(m[1].body).to_vec(),
        e: der::unsigned(m[2].body).to_vec(),
    };
    Some(KeyInfo::new(true, "PKCS#1", public))
}

/// An ECPrivateKey (SEC1): the curve and the public point are optional, so
/// either may come from the PKCS#8 wrapping instead (`curve`).
fn sec1_private(data: &[u8], curve: Option<String>) -> Option<KeyInfo> {
    let m = der::sequence(data)?;
    let version = m.first().filter(|t| t.tag == der::INTEGER)?;
    if version.body != [1] || m.get(1)?.tag != der::OCTET_STRING {
        return None;
    }
    let tagged =
        |tag: u8| m.iter().find(|t| t.tag == tag).and_then(|t| der::read(t.body)).map(|(t, _)| t);
    let curve =
        tagged(0xa0).filter(|t| t.tag == der::OID).map(|t| der::oid_text(t.body)).or(curve)?;
    let point = tagged(0xa1).filter(|t| t.tag == der::BIT_STRING).and_then(|t| t.body.get(1..));
    let name = der::oid_name(&curve).map_or_else(|| curve.clone(), str::to_string);
    Some(KeyInfo {
        private: true,
        algorithm: format!("EC {name}"),
        format: "SEC1",
        encryption: None,
        public: point.map(|p| PublicKey::Ec { curve, point: p.to_vec() }),
        line: None,
    })
}

/// A PrivateKeyInfo / OneAsymmetricKey (PKCS#8): the algorithm, the key in
/// its own encoding, and — in version 2 — the public key.
fn pkcs8_private(data: &[u8]) -> Option<KeyInfo> {
    let m = der::sequence(data)?;
    if m.first()?.tag != der::INTEGER {
        return None;
    }
    let (alg, params) = algorithm(m.get(1)?)?;
    let inner = m.get(2).filter(|t| t.tag == der::OCTET_STRING)?.body;
    let mut key = match alg.as_str() {
        der::RSA => pkcs1_private(inner)?,
        der::EC => {
            let curve = params.filter(|p| p.tag == der::OID).map(|p| der::oid_text(p.body));
            sec1_private(inner, curve)?
        }
        _ => {
            // The key as bytes; the public half only when stored beside it.
            let public = m
                .iter()
                .find(|t| t.tag == 0x81)
                .and_then(|t| t.body.get(1..))
                .map(|k| PublicKey::Raw { alg: alg.clone(), key: k.to_vec() });
            KeyInfo {
                private: true,
                algorithm: der::oid_name(&alg).map_or_else(|| alg.clone(), str::to_string),
                format: "PKCS#8",
                encryption: None,
                public,
                line: None,
            }
        }
    };
    key.format = "PKCS#8";
    Some(key)
}

/// An EncryptedPrivateKeyInfo (PKCS#8): only how it is encrypted can be read.
fn pkcs8_encrypted(data: &[u8]) -> Option<KeyInfo> {
    let m = der::sequence(data)?;
    if m.len() != 2 || m[1].tag != der::OCTET_STRING {
        return None;
    }
    let (alg, params) = algorithm(&m[0])?;
    let name = |oid: &str| der::oid_name(oid).map_or_else(|| oid.to_string(), str::to_string);
    let mut how = name(&alg);
    if alg == der::PBES2
        && let Some(p) = params.and_then(|p| der::children(p.body))
        && let (Some((kdf, kdf_params)), Some((cipher, _))) =
            (p.first().and_then(algorithm), p.get(1).and_then(algorithm))
    {
        // PBKDF2's parameters end with the PRF, when it isn't HMAC-SHA1.
        let prf = kdf_params
            .and_then(|kp| der::children(kp.body))
            .and_then(|kp| kp.iter().find(|t| t.tag == der::SEQUENCE).and_then(algorithm))
            .map(|(prf, _)| format!(" with {}", name(&prf)))
            .unwrap_or_default();
        how = format!("{how} ({}{prf}, {})", name(&kdf), name(&cipher));
    }
    Some(KeyInfo {
        private: true,
        algorithm: "unknown until decrypted".into(),
        format: "PKCS#8",
        encryption: Some(how),
        public: None,
        line: None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{pem_blocks, testdata};
    use super::*;

    fn key(pem: &str) -> KeyInfo {
        let b = pem_blocks(pem).pop().unwrap();
        parse(&b.label, &b.headers, &b.der).unwrap()
    }

    #[test]
    fn keys_say_what_they_are_and_how_they_are_kept() {
        let rsa = key(testdata::RSA_KEY);
        assert_eq!(
            (rsa.private, rsa.algorithm.as_str(), rsa.format),
            (true, "RSA 2048 bits", "PKCS#1")
        );
        assert!(!rsa.public.as_ref().unwrap().weak());
        let ec = key(testdata::LEAF_KEY);
        assert_eq!((ec.algorithm.as_str(), ec.format), ("EC P-256", "SEC1"));
        assert!(ec.public.is_some(), "SEC1 keeps the public point");
        let enc = key(testdata::LEAF_KEY_ENCRYPTED);
        assert_eq!(enc.encryption.as_deref(), Some("PBES2 (PBKDF2 with HMAC-SHA256, AES-256-CBC)"));
        assert!(enc.public.is_none());
        let public = key(testdata::RSA_PUBLIC);
        assert!(!public.private);
        assert_eq!(public.public, rsa.public, "the public key is the private key's");
    }

    #[test]
    fn a_rebuilt_spki_hashes_to_the_pin_openssl_gives() {
        use base64::Engine;
        use sha2::Digest;
        let rsa = key(testdata::RSA_KEY).public.unwrap();
        let pin =
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(rsa.spki()));
        assert_eq!(pin, testdata::RSA_PIN);
        // And reading the SPKI back gives the same key.
        assert_eq!(PublicKey::from_spki(&rsa.spki()), Some(rsa));
        let ec = key(testdata::LEAF_KEY).public.unwrap();
        assert_eq!(PublicKey::from_spki(&ec.spki()), Some(ec));
    }

    #[test]
    fn legacy_encrypted_pem_is_read_from_its_headers() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00112233445566778899AABBCCDDEEFF\n\nAAAA\n-----END RSA PRIVATE KEY-----\n";
        let k = key(pem);
        assert_eq!(k.encryption.as_deref(), Some("AES-128-CBC (OpenSSL)"));
        assert!(k.public.is_none());
    }
}
