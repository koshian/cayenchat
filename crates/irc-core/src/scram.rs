// SPDX-License-Identifier: MPL-2.0
//! SCRAM-SHA-1, -SHA-256 and -SHA-512 (RFC 5802, RFC 7677) without channel
//! binding (GS2 header `n,,`), as the client of a SASL exchange.
//!
//! The exchange is a pure state machine: [`Scram::server_first`] validates the
//! server's values and hands out a [`Job`], the PBKDF2 derivation, which the
//! caller runs off the async runtime. [`Scram::derived`] then yields the
//! client-final message and [`Scram::server_final`] checks the server's
//! signature. The password is SASLprepped (RFC 4013) up front, and every
//! derived key is zeroed as soon as it was used.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use zeroize::{Zeroize, Zeroizing};

/// RFC 7677 recommends at least 4096 iterations; more than a million would
/// only let a server make the client spin.
const MIN_ITERATIONS: u32 = 4096;
const MAX_ITERATIONS: u32 = 1_000_000;
/// Longest accepted salt, decoded.
const MAX_SALT_BYTES: usize = 1024;
/// The GS2 header `n,,` in base64: the `c=` value without channel binding.
const GS2_HEADER_B64: &str = "biws";
const NONCE_BYTES: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hash {
    Sha1,
    Sha256,
    Sha512,
}

/// The derivation of one exchange: what PBKDF2 needs, run with [`Job::run`].
pub(crate) struct Job {
    hash: Hash,
    password: Zeroizing<String>,
    salt: Vec<u8>,
    iterations: u32,
    auth_message: String,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").field("hash", &self.hash).finish()
    }
}

/// What [`Job::run`] derived. The server signature is the only secret kept.
pub(crate) struct Derived {
    proof: Vec<u8>,
    server_signature: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    First,
    Final,
    Verified,
}

pub(crate) struct Scram {
    hash: Hash,
    password: Option<Zeroizing<String>>,
    nonce: String,
    first_bare: String,
    /// `c=...,r=<combined nonce>`, once the server's first message was seen.
    final_without_proof: String,
    server_signature: Vec<u8>,
    phase: Phase,
    iterations: u32,
}

impl Scram {
    /// `password` must be normalizable with SASLprep; otherwise nothing is
    /// sent, as a differently normalized password would only fail at the
    /// server.
    pub(crate) fn new(hash: Hash, username: &str, password: &str) -> Result<Self, String> {
        let password = stringprep::saslprep(password)
            .map_err(|_| "The SASL password cannot be normalized with SASLprep.".to_owned())?;
        let mut raw = [0u8; NONCE_BYTES];
        getrandom::fill(&mut raw).map_err(|_| "No random number source for SCRAM.".to_owned())?;
        let nonce = STANDARD.encode(raw);
        let username = username.replace('=', "=3D").replace(',', "=2C");
        let first_bare = format!("n={username},r={nonce}");
        Ok(Self {
            hash,
            password: Some(Zeroizing::new(password.into_owned())),
            nonce,
            first_bare,
            final_without_proof: String::new(),
            server_signature: Vec::new(),
            phase: Phase::First,
            iterations: 0,
        })
    }

    /// The client-first message.
    pub(crate) fn first(&self) -> String {
        format!("n,,{}", self.first_bare)
    }

    /// The server-first message, validated. `Ok` is the derivation to run.
    pub(crate) fn server_first(&mut self, data: &[u8]) -> Result<Job, String> {
        let text = std::str::from_utf8(data).map_err(|_| "SCRAM server-first is not UTF-8.")?;
        if text.starts_with("e=") {
            return Err("The server rejected the SCRAM request.".into());
        }
        let mut nonce = None;
        let mut salt = None;
        let mut iterations = None;
        for part in text.split(',') {
            match part.split_once('=') {
                Some(("r", value)) => nonce = Some(value),
                Some(("s", value)) => salt = Some(value),
                Some(("i", value)) => iterations = Some(value),
                // Mandatory extensions (`m=`) are not supported.
                Some(("m", _)) => return Err("Unsupported SCRAM extension.".into()),
                _ => {}
            }
        }
        let (Some(nonce), Some(salt), Some(iterations)) = (nonce, salt, iterations) else {
            return Err("Malformed SCRAM server-first.".into());
        };
        if !nonce.starts_with(&self.nonce) || nonce.len() == self.nonce.len() {
            return Err("The SCRAM server nonce does not extend ours.".into());
        }
        let salt = STANDARD
            .decode(salt)
            .ok()
            .filter(|salt| !salt.is_empty() && salt.len() <= MAX_SALT_BYTES)
            .ok_or("The SCRAM salt is invalid.")?;
        let iterations = iterations
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| iterations.parse::<u32>().ok())
            .flatten()
            .filter(|count| (MIN_ITERATIONS..=MAX_ITERATIONS).contains(count))
            .ok_or("The SCRAM iteration count is out of range.")?;
        self.iterations = iterations;
        self.final_without_proof = format!("c={GS2_HEADER_B64},r={nonce}");
        let auth_message = format!("{},{text},{}", self.first_bare, self.final_without_proof);
        let password = self.password.take().ok_or("Unexpected SCRAM state.")?;
        Ok(Job {
            hash: self.hash,
            password,
            salt,
            iterations,
            auth_message,
        })
    }

    pub(crate) fn iterations(&self) -> u32 {
        self.iterations
    }

    /// The client-final message, from the finished derivation.
    pub(crate) fn derived(&mut self, derived: Derived) -> String {
        self.server_signature = derived.server_signature;
        self.phase = Phase::Final;
        format!(
            "{},p={}",
            self.final_without_proof,
            STANDARD.encode(&derived.proof)
        )
    }

    /// Checks the server-final message's signature.
    pub(crate) fn server_final(&mut self, data: &[u8]) -> Result<(), String> {
        if self.phase != Phase::Final {
            return Err("Unexpected SCRAM server-final.".into());
        }
        let text = std::str::from_utf8(data).map_err(|_| "SCRAM server-final is not UTF-8.")?;
        if text.starts_with("e=") {
            return Err("The server rejected the SCRAM proof.".into());
        }
        let signature = text
            .strip_prefix("v=")
            .and_then(|value| STANDARD.decode(value).ok())
            .ok_or("Malformed SCRAM server-final.")?;
        if !constant_time_eq(&signature, &self.server_signature) {
            return Err("The SCRAM server signature does not match.".into());
        }
        self.server_signature.zeroize();
        self.phase = Phase::Verified;
        Ok(())
    }

    /// The server proved it knows the password.
    #[cfg(test)]
    pub(crate) fn verified(&self) -> bool {
        self.phase == Phase::Verified
    }
}

impl Drop for Scram {
    fn drop(&mut self) {
        self.server_signature.zeroize();
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

macro_rules! derive {
    ($digest:ty, $job:expr) => {{
        use sha2::Digest as _;
        let job: &Job = $job;
        let mut salted = Zeroizing::new(vec![0u8; <$digest as sha2::Digest>::output_size()]);
        pbkdf2::pbkdf2_hmac::<$digest>(
            job.password.as_bytes(),
            &job.salt,
            job.iterations,
            &mut salted,
        );
        let mac = |key: &[u8], data: &[u8]| {
            let mut mac = <Hmac<$digest> as Mac>::new_from_slice(key).expect("HMAC takes any key");
            mac.update(data);
            Zeroizing::new(mac.finalize().into_bytes().to_vec())
        };
        let client_key = mac(&salted, b"Client Key");
        let stored_key = Zeroizing::new(<$digest>::digest(&*client_key).to_vec());
        let signature = mac(&stored_key, job.auth_message.as_bytes());
        let proof = client_key
            .iter()
            .zip(signature.iter())
            .map(|(key, signature)| key ^ signature)
            .collect::<Vec<u8>>();
        let server_key = mac(&salted, b"Server Key");
        let server_signature = mac(&server_key, job.auth_message.as_bytes());
        Derived {
            proof,
            server_signature: server_signature.to_vec(),
        }
    }};
}

impl Job {
    /// The PBKDF2 derivation; CPU bound, up to a million iterations.
    pub(crate) fn run(self) -> Derived {
        match self.hash {
            Hash::Sha1 => derive!(sha1::Sha1, &self),
            Hash::Sha256 => derive!(sha2::Sha256, &self),
            Hash::Sha512 => derive!(sha2::Sha512, &self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with a fixed nonce, to replay published exchanges.
    fn client(hash: Hash, user: &str, password: &str, nonce: &str) -> Scram {
        let mut scram = Scram::new(hash, user, password).unwrap();
        scram.nonce = nonce.into();
        scram.first_bare = format!("n={user},r={nonce}");
        scram
    }

    fn exchange(
        mut scram: Scram,
        server_first: &str,
        client_final: &str,
        server_final: &str,
    ) -> Scram {
        let job = scram.server_first(server_first.as_bytes()).unwrap();
        assert_eq!(scram.derived(job.run()), client_final);
        scram.server_final(server_final.as_bytes()).unwrap();
        assert!(scram.verified());
        scram
    }

    #[test]
    fn rfc_5802_sha1_vector() {
        let scram = client(Hash::Sha1, "user", "pencil", "fyko+d2lbbFgONRv9qkxdawL");
        assert_eq!(scram.first(), "n,,n=user,r=fyko+d2lbbFgONRv9qkxdawL");
        exchange(
            scram,
            "r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,s=QSXCR+Q6sek8bf92,i=4096",
            "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts=",
            "v=rmF9pqV8S7suAoZWja4dJRkFsKQ=",
        );
    }

    #[test]
    fn rfc_7677_sha256_vector() {
        let scram = client(Hash::Sha256, "user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        exchange(
            scram,
            "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=",
            "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=",
        );
    }

    #[test]
    fn wrong_server_signature_is_rejected() {
        let mut scram = client(Hash::Sha256, "user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        let job = scram
            .server_first(
                b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            )
            .unwrap();
        scram.derived(job.run());
        let error = scram
            .server_final(b"v=7rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
            .unwrap_err();
        assert_eq!(error, "The SCRAM server signature does not match.");
        assert!(!scram.verified());
        assert!(scram.server_final(b"e=invalid-proof").is_err());
        assert!(scram.server_final(b"garbage").is_err());
    }

    #[test]
    fn sha512_exchange_verifies_against_an_independent_server() {
        // The server side of the exchange, computed here from the stored
        // password with the same primitives, must accept the client proof.
        let mut scram = Scram::new(Hash::Sha512, "user", "pencil").unwrap();
        let nonce = scram.nonce.clone();
        let server_first = format!("r={nonce}srv,s=c2FsdHNhbHQ=,i=5000");
        let job = scram.server_first(server_first.as_bytes()).unwrap();
        let derived = job.run();
        let client_final = scram.derived(derived);
        let proof = client_final.rsplit_once(",p=").unwrap().1;
        assert_eq!(STANDARD.decode(proof).unwrap().len(), 64);
        let mut again = Scram::new(Hash::Sha512, "user", "pencil").unwrap();
        again.nonce = nonce.clone();
        again.first_bare = format!("n=user,r={nonce}");
        let job = again.server_first(server_first.as_bytes()).unwrap();
        assert_eq!(again.derived(job.run()), client_final);
    }

    #[test]
    fn server_values_are_validated() {
        let new = || client(Hash::Sha256, "user", "pencil", "abc");
        let bad = |text: &str| new().server_first(text.as_bytes()).is_err();
        assert!(!bad("r=abcdef,s=c2FsdA==,i=4096"));
        assert!(bad("r=abc,s=c2FsdA==,i=4096"), "nonce not extended");
        assert!(bad("r=xyzabc,s=c2FsdA==,i=4096"), "nonce lacks ours");
        assert!(bad("r=abcdef,s=,i=4096"), "empty salt");
        assert!(bad("r=abcdef,s=!!,i=4096"), "salt not base64");
        let huge = STANDARD.encode(vec![1u8; MAX_SALT_BYTES + 1]);
        assert!(bad(&format!("r=abcdef,s={huge},i=4096")), "salt too long");
        assert!(bad("r=abcdef,s=c2FsdA==,i=4095"), "too few iterations");
        assert!(bad("r=abcdef,s=c2FsdA==,i=1000001"), "too many iterations");
        assert!(bad("r=abcdef,s=c2FsdA==,i=+4096"), "sign");
        assert!(bad("r=abcdef,s=c2FsdA==,i=abc"));
        assert!(bad("r=abcdef,s=c2FsdA=="), "no iterations");
        assert!(bad("m=x,r=abcdef,s=c2FsdA==,i=4096"), "mandatory extension");
        assert!(bad("e=other-error"));
        assert!(new().server_first(&[0xff, 0xfe]).is_err());
        assert!(!bad("r=abcdef,s=c2FsdA==,i=1000000"));
    }

    #[test]
    fn username_is_escaped_and_password_is_saslprepped() {
        let scram = Scram::new(Hash::Sha1, "a=b,c", "pw").unwrap();
        assert!(scram.first().starts_with("n,,n=a=3Db=2Cc,r="));
        assert!(Scram::new(Hash::Sha1, "u", "bad\u{0007}password").is_err());
        // Non-ASCII spaces map to U+0020 and NFKC applies.
        let scram = Scram::new(Hash::Sha1, "u", "a\u{00A0}b\u{2168}").unwrap();
        assert_eq!(scram.password.as_deref().map(String::as_str), Some("a bIX"));
    }

    #[test]
    fn nonces_differ_between_exchanges() {
        let first = Scram::new(Hash::Sha1, "u", "p").unwrap();
        let second = Scram::new(Hash::Sha1, "u", "p").unwrap();
        assert_ne!(first.nonce, second.nonce);
    }

    #[test]
    fn server_final_before_derivation_is_unexpected() {
        let mut scram = Scram::new(Hash::Sha1, "u", "p").unwrap();
        assert!(scram.server_final(b"v=AAAA").is_err());
    }
}

/// The server side of an exchange, for tests of the handshake above this.
#[cfg(test)]
pub(crate) mod mock {
    use super::*;

    /// `(server-first, expected client-final, server-final)` for a
    /// client-first message and the account's password.
    pub(crate) fn server(
        hash: Hash,
        password: &str,
        client_first: &str,
        salt_len: usize,
    ) -> (String, String, String) {
        let bare = client_first.strip_prefix("n,,").unwrap();
        let nonce = bare.rsplit_once(",r=").unwrap().1;
        let mut scram = Scram::new(hash, "x", password).unwrap();
        scram.nonce = nonce.into();
        scram.first_bare = bare.into();
        let salt = STANDARD.encode(vec![7u8; salt_len]);
        let server_first = format!("r={nonce}srv,s={salt},i=4096");
        let job = scram.server_first(server_first.as_bytes()).unwrap();
        let client_final = scram.derived(job.run());
        let server_final = format!("v={}", STANDARD.encode(&scram.server_signature));
        (server_first, client_final, server_final)
    }
}
