use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::str::FromStr;

use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, PKCS_ED25519};
use ring::digest::{SHA256, digest};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use thiserror::Error;
use unikvm_core::device::DeviceId;
use zeroize::Zeroizing;

/// This hostname identifies the TLS application. The separately matched leaf pin identifies the peer.
pub(crate) const TLS_NAME: &str = "unikvm.invalid";
pub const MAX_CERTIFICATE_SIZE: usize = 4096;
pub const MAX_IDENTITY_SIZE: usize = 8192;
const MAGIC: &[u8] = b"UNIKVM-ID\x01";

pub struct Identity {
    certificate: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

pub struct TrustedPeer {
    pub(crate) certificate: CertificateDer<'static>,
    pub(crate) fingerprint: Fingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("could not generate TLS identity")]
    Generate(#[from] rcgen::Error),
    #[error("invalid identity file")]
    InvalidIdentity,
    #[error("invalid certificate size")]
    CertificateSize,
    #[error("fingerprint must contain exactly 64 hexadecimal characters")]
    InvalidFingerprint,
    #[error("peer certificate differs from the approved SHA-256 fingerprint")]
    FingerprintMismatch,
    #[error("could not read peer certificate")]
    ReadCertificate(#[from] io::Error),
}

impl Identity {
    pub fn generate() -> Result<Self, IdentityError> {
        let keys = KeyPair::generate_for(&PKCS_ED25519)?;
        let mut params = CertificateParams::new(vec![TLS_NAME.to_owned()])?;
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        let certificate = params.self_signed(&keys)?.der().clone();
        let key = PrivatePkcs8KeyDer::from(keys.serialize_der());
        Ok(Self { certificate, key })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdentityError> {
        let (length, content) = bytes
            .strip_prefix(MAGIC)
            .filter(|_| bytes.len() <= MAX_IDENTITY_SIZE)
            .and_then(|content| content.split_first_chunk::<2>())
            .ok_or(IdentityError::InvalidIdentity)?;
        let (certificate, key) = content
            .split_at_checked(usize::from(u16::from_be_bytes(*length)))
            .filter(|(cert, key)| {
                !key.is_empty() && (1..=MAX_CERTIFICATE_SIZE).contains(&cert.len())
            })
            .ok_or(IdentityError::InvalidIdentity)?;
        Ok(Self {
            certificate: CertificateDer::from(certificate.to_vec()),
            key: PrivatePkcs8KeyDer::from(key.to_vec()),
        })
    }

    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let key = self.key.secret_pkcs8_der();
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            MAGIC.len() + 2 + self.certificate.len() + key.len(),
        ));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(self.certificate.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&self.certificate);
        bytes.extend_from_slice(key);
        bytes
    }

    pub fn certificate(&self) -> &[u8] {
        self.certificate.as_ref()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::for_certificate(&self.certificate)
    }

    pub(crate) fn tls_credentials(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (vec![self.certificate.clone()], self.key.clone_key().into())
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        use zeroize::Zeroize;

        self.key.zeroize();
    }
}

impl TrustedPeer {
    pub fn read(file: File, approved: Fingerprint) -> Result<Self, IdentityError> {
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer certificate must be a regular file",
            )
            .into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_CERTIFICATE_SIZE as u64 + 1)
            .read_to_end(&mut bytes)?;
        Self::approve(bytes, approved)
    }

    pub fn approve(certificate: Vec<u8>, approved: Fingerprint) -> Result<Self, IdentityError> {
        if !(1..=MAX_CERTIFICATE_SIZE).contains(&certificate.len()) {
            return Err(IdentityError::CertificateSize);
        }
        if Fingerprint::for_certificate(&certificate) != approved {
            return Err(IdentityError::FingerprintMismatch);
        }
        Ok(Self {
            certificate: CertificateDer::from(certificate),
            fingerprint: approved,
        })
    }
}

impl Fingerprint {
    pub(crate) fn for_certificate(certificate: &[u8]) -> Self {
        let hash = digest(&SHA256, certificate);
        let mut bytes = [0; 32];
        bytes.copy_from_slice(hash.as_ref());
        Self(bytes)
    }

    /// A stable ID derived from the randomly generated certificate. Trust uses the full fingerprint.
    pub fn device_id(self) -> DeviceId {
        let mut id = [0; 16];
        id.copy_from_slice(&self.0[..16]);
        DeviceId(id)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Fingerprint {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes().as_chunks::<2>();
        if bytes.0.len() != 32 || !bytes.1.is_empty() {
            return Err(IdentityError::InvalidFingerprint);
        }
        let mut result = [0; 32];
        for (output, pair) in result.iter_mut().zip(bytes.0) {
            let [high, low] = pair.map(|hex| char::from(hex).to_digit(16));
            *output = ((high.ok_or(IdentityError::InvalidFingerprint)? << 4)
                | low.ok_or(IdentityError::InvalidFingerprint)?) as u8;
        }
        Ok(Self(result))
    }
}

#[cfg(test)]
mod tests {
    use super::{Fingerprint, Identity, IdentityError, TrustedPeer};

    #[test]
    fn reload_preserves_identity_and_requires_explicit_fingerprint() {
        let initial = Identity::generate().unwrap();
        let expected = initial.fingerprint();
        let reloaded = Identity::from_bytes(&initial.to_bytes()).unwrap();
        assert_eq!(reloaded.fingerprint().device_id(), expected.device_id());
        assert_eq!(reloaded.fingerprint(), expected);
        assert_eq!(
            expected
                .to_string()
                .to_uppercase()
                .parse::<Fingerprint>()
                .unwrap(),
            expected
        );
        assert!("xx".repeat(32).parse::<Fingerprint>().is_err());
        assert!(Identity::from_bytes(&initial.to_bytes()[..12]).is_err());
        assert!(TrustedPeer::approve(reloaded.certificate().to_vec(), expected).is_ok());
        let different = Identity::generate().unwrap();
        assert!(matches!(
            TrustedPeer::approve(different.certificate().to_vec(), expected),
            Err(IdentityError::FingerprintMismatch)
        ));
    }
}
