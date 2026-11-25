// SPDX-License-Identifier: GPL-3.0-or-later
//! The device's identity: a self-signed certificate. Its SHA-256 (uppercase hex) is the LocalSend fingerprint, which
//! the other devices use to recognise this one, so the certificate is generated once and kept.
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    /// The private key, PKCS#8 DER.
    pub key_der: Vec<u8>,
    pub cert_der: Vec<u8>,
    /// SHA-256 of the DER certificate, uppercase hex.
    pub fingerprint: String,
}

pub fn fingerprint(cert_der: &[u8]) -> String {
    Sha256::digest(cert_der).iter().map(|b| format!("{b:02X}")).collect()
}

/// The DER bytes of the first PEM block whose label is `label` ("CERTIFICATE", "PRIVATE KEY").
fn pem_to_der(pem: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let body: String = pem.lines().skip_while(|l| l.trim() != begin).skip(1).take_while(|l| !l.starts_with("-----END")).collect();
    base64_decode(&body)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

impl Identity {
    pub fn generate() -> Result<Self, String> {
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, "LocalSend User");
        params.distinguished_name = name;
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2054, 1, 1);
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let cert_der = cert.der().to_vec();
        Ok(Self { fingerprint: fingerprint(&cert_der), cert_pem: cert.pem(), key_pem: key.serialize_pem(), key_der: key.serialize_der(), cert_der })
    }

    pub fn from_pem(cert_pem: String, key_pem: String) -> Option<Self> {
        let cert_der = pem_to_der(&cert_pem, "CERTIFICATE")?;
        let key_der = pem_to_der(&key_pem, "PRIVATE KEY")?;
        (!cert_der.is_empty() && !key_der.is_empty()).then(|| Self { fingerprint: fingerprint(&cert_der), cert_pem, key_pem, key_der, cert_der })
    }

    /// Loads `cert.pem` and `key.pem` from `dir`, or generates and saves them (the key readable by the owner only).
    pub fn load_or_create(dir: &Path) -> std::io::Result<Self> {
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        if let (Ok(c), Ok(k)) = (std::fs::read_to_string(&cert), std::fs::read_to_string(&key)) {
            if let Some(id) = Self::from_pem(c, k) {
                return Ok(id);
            }
        }
        let id = Self::generate().map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&cert, &id.cert_pem)?;
        std::fs::write(&key, &id.key_pem)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_the_uppercase_sha256_of_the_der_certificate() {
        let id = Identity::generate().unwrap();
        assert_eq!(id.fingerprint.len(), 64);
        assert!(id.fingerprint.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)));
        assert_eq!(id.fingerprint, fingerprint(&id.cert_der));
        assert!(id.cert_pem.starts_with("-----BEGIN CERTIFICATE-----") && id.key_pem.contains("PRIVATE KEY"));
        // a known value: SHA-256 of the empty input
        assert_eq!(fingerprint(&[]), "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855");
    }

    #[test]
    fn the_pem_gives_back_the_same_der_and_fingerprint() {
        let id = Identity::generate().unwrap();
        let back = Identity::from_pem(id.cert_pem.clone(), id.key_pem.clone()).unwrap();
        assert_eq!(back.cert_der, id.cert_der);
        assert_eq!(back.key_der, id.key_der, "the key survives the PEM round trip");
        assert_eq!(back.fingerprint, id.fingerprint);
        assert!(Identity::from_pem("not a certificate".into(), String::new()).is_none());
    }

    #[test]
    fn the_identity_is_created_once_and_loaded_after_with_a_private_key_file() {
        let dir = std::env::temp_dir().join(format!("zerocart-send-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let again = Identity::load_or_create(&dir).unwrap();
        assert_eq!(first.fingerprint, again.fingerprint, "the fingerprint must not change between starts");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
