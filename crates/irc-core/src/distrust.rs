// SPDX-License-Identifier: MPL-2.0
//! Server certificates refused on top of the usual TLS verification.
//!
//! rustls checks neither revocation nor Certificate Transparency, and the
//! trusted roots are webpki-roots and the operating system's list. A CA
//! certificate that browsers block or its issuer revoked can therefore still
//! complete a chain, for example through a cross-signature under a root the
//! OS still lists. Keys known to be compromised are refused wherever they
//! appear in what the server sends: a chain through such a key has to carry
//! the certificate holding it, since none of them is a root. The connection
//! hands [`has_distrusted_key`] to the IRC library
//! (`irc::client::data::Config::refuse_server_cert`).

use sha2::{Digest, Sha256};

/// SHA-256 of the DER `SubjectPublicKeyInfo` of keys that must not be
/// trusted: the eight eMudhra intermediate keys of October 2026, whose
/// private keys could be computed from the public ones (Mozilla bug
/// 2083449). Sectigo also cross-signed these keys under AAA Certificate
/// Services, which operating systems still list; those cross-certificates
/// are revoked, which rustls does not check.
const DISTRUSTED_SPKI_SHA256: [&str; 8] = [
    "ebed6475c2c2d80fcb199f3fe8dd6e6b3ce60b77eb15d4e42c3dec0c6f5b5105",
    "49dd353dc2b919a3770645117d0e60c16115e7d1bc259eefe734b00ea434fe5f",
    "e0254c7fcdc265e2f0f8a428dabb75ab4ae039b4872d62f5ee57f0877d5a8a41",
    "5c0660a2b164910d8ff589c18e14552e9e0ec237dcefe2cb736d41a4811f81a0",
    "43bffa19d6ef651451f6841e019931e96f6f2eaeff28ab4b6f1ce4576de2717b",
    "63ecefee431d49523bc1136c4e72d0206a477086caf606cd5cf025686c109cf8",
    "ec4ffa1ad0ac88081314b7aaffef6a91f97cd57291bb5ba686c5415f5eb4f6b6",
    "92c063d67e8f14db71d288e11266ffdaf69cbf115f7bab14f3bc2f68d25e0e30",
];

/// Whether the DER certificate `cert` holds a distrusted key.
pub(crate) fn has_distrusted_key(cert: &[u8]) -> bool {
    has_listed_key(cert, &DISTRUSTED_SPKI_SHA256)
}

/// Whether `cert` holds a key in `list`. A certificate that cannot be read
/// is left to the usual verification, which refuses it if the chain needs it.
fn has_listed_key(cert: &[u8], list: &[&str]) -> bool {
    subject_public_key_info(cert).is_some_and(|spki| list.contains(&spki_sha256(spki).as_str()))
}

fn spki_sha256(spki: &[u8]) -> String {
    Sha256::digest(spki)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One DER element at the start of `input`: its tag, its contents and what
/// follows it, with the whole element. Only definite lengths of up to four
/// bytes are read.
struct Element<'a> {
    tag: u8,
    whole: &'a [u8],
    contents: &'a [u8],
    rest: &'a [u8],
}

fn element(input: &[u8]) -> Option<Element<'_>> {
    let (&tag, after_tag) = input.split_first()?;
    let (&first, after_first) = after_tag.split_first()?;
    let (length, body) = if first < 0x80 {
        (usize::from(first), after_first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || after_first.len() < count {
            return None;
        }
        let (bytes, body) = after_first.split_at(count);
        let length = bytes
            .iter()
            .fold(0usize, |length, &byte| (length << 8) | usize::from(byte));
        (length, body)
    };
    if body.len() < length {
        return None;
    }
    let header = input.len() - body.len();
    Some(Element {
        tag,
        whole: &input[..header + length],
        contents: &body[..length],
        rest: &body[length..],
    })
}

/// The DER `SubjectPublicKeyInfo` of an X.509 certificate, header included
/// (what is hashed for a key pin).
fn subject_public_key_info(cert: &[u8]) -> Option<&[u8]> {
    const SEQUENCE: u8 = 0x30;
    const VERSION: u8 = 0xa0;
    let certificate = element(cert).filter(|e| e.tag == SEQUENCE)?;
    let tbs = element(certificate.contents).filter(|e| e.tag == SEQUENCE)?;
    // [0] version (optional), serialNumber, signature, issuer, validity,
    // subject, then subjectPublicKeyInfo.
    let mut fields = tbs.contents;
    if element(fields)?.tag == VERSION {
        fields = element(fields)?.rest;
    }
    for _ in 0..5 {
        fields = element(fields)?.rest;
    }
    let spki = element(fields).filter(|e| e.tag == SEQUENCE)?;
    Some(spki.whole)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
    };

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    use super::*;

    // A root, an intermediate and a server certificate for 127.0.0.1 made
    // with OpenSSL for these tests (the key is a test key). The hashes are
    // `openssl x509 -noout -pubkey | openssl pkey -pubin -outform DER |
    // sha256sum` of each.
    const ROOT: &str = include_str!("distrust_fixtures/root.pem");
    const INTERMEDIATE: &str = include_str!("distrust_fixtures/intermediate.pem");
    const SERVER: &str = include_str!("distrust_fixtures/server.pem");
    const SERVER_KEY: &str = include_str!("distrust_fixtures/server.key");
    const ROOT_SPKI: &str = "64081c626e79caa9207abb5f06d95cddf403e9c76df689f10c2498a3c2a7e235";
    const INTERMEDIATE_SPKI: &str =
        "76dd0096954bd9e925fed7977952a6033c9da24193a789c6ae3f41d27fd6b10c";
    const SERVER_SPKI: &str = "b7a5a774ee0387ac352fff25364175a8e93e7fca97ac86ae1b94a0b2cdf5e9be";

    fn der(pem: &str) -> Vec<u8> {
        CertificateDer::from_pem_slice(pem.as_bytes())
            .unwrap()
            .to_vec()
    }

    #[test]
    fn keys_are_hashed_as_openssl_does() {
        for (pem, hash) in [
            (ROOT, ROOT_SPKI),
            (INTERMEDIATE, INTERMEDIATE_SPKI),
            (SERVER, SERVER_SPKI),
        ] {
            let der = der(pem);
            assert_eq!(spki_sha256(subject_public_key_info(&der).unwrap()), hash);
            assert!(has_listed_key(&der, &[hash]));
            assert!(!has_distrusted_key(&der));
        }
    }

    #[test]
    fn malformed_certificates_are_not_read() {
        let der = der(SERVER);
        for len in 0..der.len() {
            assert_eq!(subject_public_key_info(&der[..len]), None, "{len}");
        }
        for input in [
            &[][..],
            &[0x30],
            &[0x30, 0x80],
            &[0x30, 0x85, 1, 0, 0, 0, 0],
            &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
            &[0x31, 0x00],
            &[0x30, 0x02, 0x30, 0x00],
        ] {
            assert_eq!(subject_public_key_info(input), None, "{input:?}");
            assert!(!has_distrusted_key(input));
        }
    }

    #[test]
    fn the_list_has_eight_distinct_sha256_values() {
        let mut seen = std::collections::HashSet::new();
        for hash in DISTRUSTED_SPKI_SHA256 {
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            assert!(seen.insert(hash));
        }
    }

    fn refuse_test_intermediate(cert: &[u8]) -> bool {
        has_listed_key(cert, &[INTERMEDIATE_SPKI])
    }

    /// Connects the IRC library over TLS to a server sending the server
    /// certificate and the intermediate, trusting the root; returns whether
    /// the handshake succeeded.
    fn handshake(label: &str, refuse: Option<fn(&[u8]) -> bool>) -> bool {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(format!("distrust-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root_path = dir.join("root.pem");
        std::fs::write(&root_path, ROOT).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let chain = vec![
                CertificateDer::from(der(SERVER)),
                CertificateDer::from(der(INTERMEDIATE)),
            ];
            let key = PrivateKeyDer::from_pem_slice(SERVER_KEY.as_bytes()).unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap();
            let (socket, _) = listener.accept().unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, socket);
            // Completes the handshake (or sees it refused), then waits for
            // the client to close.
            let _ = tls.write_all(b":srv NOTICE * :hello\r\n");
            let mut buffer = [0; 256];
            let _ = tls.read(&mut buffer);
        });

        let config = irc::client::data::Config {
            server: Some("127.0.0.1".into()),
            port: Some(port),
            nickname: Some("alice".into()),
            use_tls: Some(true),
            cert_path: Some(root_path.to_string_lossy().into_owned()),
            refuse_server_cert: refuse,
            ..irc::client::data::Config::default()
        };
        let connected = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(irc::client::Client::from_config(config))
            .is_ok();
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        connected
    }

    #[test]
    fn a_refused_intermediate_ends_the_handshake() {
        assert!(handshake("plain", None), "the chain is valid");
        assert!(
            handshake("listed", Some(has_distrusted_key)),
            "nothing listed is sent"
        );
        assert!(!handshake("refused", Some(refuse_test_intermediate)));
    }
}
