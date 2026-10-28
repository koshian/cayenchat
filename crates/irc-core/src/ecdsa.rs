// SPDX-License-Identifier: MPL-2.0
//! The P-256 keys of SASL `ECDSA-NIST256P-CHALLENGE`: generation, import of
//! unencrypted PEM, the compressed public key to register with a network,
//! and the signature over the server's challenge.
//!
//! A private key is kept (in the credential store) as the base64 of its 32
//! byte scalar; [`SecretText`] is that text. Nothing here puts a private key,
//! or a part of it, into an error message.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use zeroize::{Zeroize, Zeroizing};

/// The length of the challenge the server sends.
pub const CHALLENGE_BYTES: usize = 32;
/// The most PEM text read; a P-256 key is a few hundred bytes.
const MAX_PEM_BYTES: usize = 16 * 1024;

/// The stored form of a private key: base64 of the 32 byte scalar.
pub type SecretText = Zeroizing<String>;

/// Why a key could not be created or read. The text is safe to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// The random source failed.
    Random,
    /// Too large, or not PEM text at all.
    NotPem,
    /// A passphrase-protected key (PKCS#8 or traditional).
    Encrypted,
    /// An OpenSSH-format private key.
    OpenSsh,
    /// An RSA private key.
    Rsa,
    /// A public key or certificate, not a private key.
    PublicOnly,
    /// A PEM block of a kind this does not read.
    UnsupportedBlock,
    /// A private key that does not parse, or is of another curve.
    NotP256,
    /// The stored text is not a P-256 key.
    Stored,
}

impl KeyError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Random => "The operating system could not provide random numbers.",
            Self::NotPem => "The file is not PEM text (or it is too large).",
            Self::Encrypted => {
                "The key is encrypted (protected by a passphrase). Remove the passphrase first, for example with `openssl pkcs8 -nocrypt`."
            }
            Self::OpenSsh => {
                "The key is in OpenSSH format. Use an unencrypted PEM file (PKCS#8 or SEC1)."
            }
            Self::Rsa => "The key is an RSA key; a P-256 (prime256v1) key is needed.",
            Self::PublicOnly => "The file holds a public key or certificate, not a private key.",
            Self::UnsupportedBlock => {
                "The file has no unencrypted PKCS#8 (PRIVATE KEY) or SEC1 (EC PRIVATE KEY) block."
            }
            Self::NotP256 => "The private key could not be read as a P-256 (prime256v1) key.",
            Self::Stored => "The stored key is not a valid P-256 key.",
        }
    }
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for KeyError {}

fn secret_text(key: &SigningKey) -> SecretText {
    let mut bytes = key.to_bytes();
    let text = STANDARD.encode(bytes);
    bytes.zeroize();
    Zeroizing::new(text)
}

/// A new key from the operating system's random numbers.
pub fn generate() -> Result<SecretText, KeyError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    // A random value is outside the valid range with a negligible
    // probability; trying again is enough.
    for _ in 0..8 {
        getrandom::fill(bytes.as_mut_slice()).map_err(|_| KeyError::Random)?;
        if let Ok(key) = SigningKey::from_slice(bytes.as_slice()) {
            return Ok(secret_text(&key));
        }
    }
    Err(KeyError::Random)
}

/// Reads an unencrypted PKCS#8 (`PRIVATE KEY`) or SEC1 (`EC PRIVATE KEY`)
/// PEM key. Other contents are refused with the reason.
pub fn import_pem(pem: &str) -> Result<SecretText, KeyError> {
    if pem.len() > MAX_PEM_BYTES {
        return Err(KeyError::NotPem);
    }
    let labels = block_labels(pem);
    if labels.is_empty() {
        return Err(KeyError::NotPem);
    }
    let has = |wanted: &str| labels.iter().any(|label| label == wanted);
    if has("ENCRYPTED PRIVATE KEY") || (has("EC PRIVATE KEY") && pem.contains("ENCRYPTED")) {
        return Err(KeyError::Encrypted);
    }
    let key = if has("PRIVATE KEY") {
        let block = extract_block(pem, "PRIVATE KEY").ok_or(KeyError::NotPem)?;
        SigningKey::from_pkcs8_pem(&block).map_err(|_| KeyError::NotP256)?
    } else if has("EC PRIVATE KEY") {
        let block = extract_block(pem, "EC PRIVATE KEY").ok_or(KeyError::NotPem)?;
        let secret = p256::SecretKey::from_sec1_pem(&block).map_err(|_| KeyError::NotP256)?;
        SigningKey::from(secret)
    } else if has("OPENSSH PRIVATE KEY") {
        return Err(KeyError::OpenSsh);
    } else if has("RSA PRIVATE KEY") {
        return Err(KeyError::Rsa);
    } else if has("PUBLIC KEY") || has("CERTIFICATE") || has("EC PUBLIC KEY") {
        return Err(KeyError::PublicOnly);
    } else {
        return Err(KeyError::UnsupportedBlock);
    };
    Ok(secret_text(&key))
}

/// The labels of the `-----BEGIN <label>-----` lines.
fn block_labels(pem: &str) -> Vec<String> {
    pem.lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("-----BEGIN ")
                .and_then(|rest| rest.strip_suffix("-----"))
                .map(str::to_owned)
        })
        .collect()
}

/// The block labeled `label` alone, so a leading `EC PARAMETERS` block (as
/// `openssl ecparam -genkey` writes) does not hide the key.
fn extract_block(pem: &str, label: &str) -> Option<Zeroizing<String>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem.find(&begin)?;
    let stop = pem[start..].find(&end)? + start + end.len();
    Some(Zeroizing::new(pem[start..stop].to_owned()))
}

fn signing_key(secret: &str) -> Result<SigningKey, KeyError> {
    let bytes = Zeroizing::new(STANDARD.decode(secret).map_err(|_| KeyError::Stored)?);
    if bytes.len() != 32 {
        return Err(KeyError::Stored);
    }
    SigningKey::from_slice(&bytes).map_err(|_| KeyError::Stored)
}

/// Whether `secret` is a stored key this can sign with.
pub fn is_valid(secret: &str) -> bool {
    signing_key(secret).is_ok()
}

/// The compressed (33 byte) public key in base64: the form to register with
/// the network's services. It reveals nothing of the private key.
pub fn public_key_base64(secret: &str) -> Result<String, KeyError> {
    let key = signing_key(secret)?;
    let point = key.verifying_key().to_encoded_point(true);
    Ok(STANDARD.encode(point.as_bytes()))
}

/// The DER ECDSA signature of `challenge`, which is signed as the digest
/// itself (no further hashing, "prehash"), as `ecdsatool` and the services
/// that verify it do.
pub fn sign(secret: &str, challenge: &[u8]) -> Result<Vec<u8>, KeyError> {
    let key = signing_key(secret)?;
    let signature: Signature = key.sign_prehash(challenge).map_err(|_| KeyError::Stored)?;
    Ok(signature.to_der().as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    use p256::pkcs8::EncodePrivateKey;

    #[test]
    fn a_generated_key_signs_and_its_public_key_is_compressed() {
        let secret = generate().unwrap();
        assert_ne!(*secret, *generate().unwrap(), "random keys differ");
        let public = STANDARD
            .decode(public_key_base64(&secret).unwrap())
            .unwrap();
        assert_eq!(public.len(), 33);
        assert!(public[0] == 2 || public[0] == 3, "compressed SEC1 tag");

        let challenge = [7u8; CHALLENGE_BYTES];
        let der = sign(&secret, &challenge).unwrap();
        let signature = Signature::from_der(&der).unwrap();
        let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&public).unwrap();
        // The challenge itself is the digest, not hashed again.
        verifying.verify_prehash(&challenge, &signature).unwrap();
        use p256::ecdsa::signature::Verifier;
        assert!(verifying.verify(&challenge, &signature).is_err());
    }

    fn pkcs8_pem(secret: &str) -> String {
        signing_key(secret)
            .unwrap()
            .to_pkcs8_pem(Default::default())
            .unwrap()
            .to_string()
    }

    fn sec1_pem(secret: &str) -> String {
        let key = p256::SecretKey::from_slice(&STANDARD.decode(secret).unwrap()).unwrap();
        key.to_sec1_pem(Default::default()).unwrap().to_string()
    }

    #[test]
    fn pkcs8_and_sec1_pem_are_read_to_the_same_key() {
        let secret = generate().unwrap();
        let from_pkcs8 = import_pem(&pkcs8_pem(&secret)).unwrap();
        let from_sec1 = import_pem(&sec1_pem(&secret)).unwrap();
        assert_eq!(*from_pkcs8, *secret);
        assert_eq!(*from_sec1, *secret);
        // `openssl ecparam -genkey` writes the parameters first.
        let with_parameters = format!(
            "-----BEGIN EC PARAMETERS-----\nBggqhkjOPQMBBw==\n-----END EC PARAMETERS-----\n{}",
            sec1_pem(&secret)
        );
        assert_eq!(*import_pem(&with_parameters).unwrap(), *secret);
    }

    #[test]
    fn unreadable_files_say_what_could_not_be_read() {
        let cases = [
            ("hello", KeyError::NotPem),
            (
                "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
                KeyError::Encrypted,
            ),
            (
                "-----BEGIN EC PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nAAAA\n-----END EC PRIVATE KEY-----\n",
                KeyError::Encrypted,
            ),
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
                KeyError::OpenSsh,
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n",
                KeyError::Rsa,
            ),
            (
                "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n",
                KeyError::PublicOnly,
            ),
            (
                "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
                KeyError::NotP256,
            ),
            (
                "-----BEGIN DSA PARAMETERS-----\nAAAA\n-----END DSA PARAMETERS-----\n",
                KeyError::UnsupportedBlock,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(import_pem(text).unwrap_err(), expected, "{text}");
        }
        assert_eq!(
            import_pem(&"a".repeat(MAX_PEM_BYTES + 1)).unwrap_err(),
            KeyError::NotPem
        );
    }

    #[test]
    fn a_pem_of_another_curve_is_refused_without_echoing_it() {
        let error =
            import_pem("-----BEGIN PRIVATE KEY-----\nSECRETTEXT\n-----END PRIVATE KEY-----\n")
                .unwrap_err();
        assert!(!error.message().contains("SECRETTEXT"));
    }

    #[test]
    fn a_stored_key_that_is_not_valid_is_refused() {
        assert!(!is_valid("not base64!"));
        assert!(!is_valid(&STANDARD.encode([0u8; 32])));
        assert!(!is_valid(&STANDARD.encode([1u8; 31])));
        assert!(is_valid(&generate().unwrap()));
        assert_eq!(sign("x", &[0; 32]).unwrap_err(), KeyError::Stored);
    }
}
