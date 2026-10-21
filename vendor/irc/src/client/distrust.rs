//! Lets the caller refuse server certificates on top of the usual
//! verification (CayenChat addition, see vendor/README.md and
//! `Config::refuse_server_cert`).

use std::sync::Arc;

use tokio_rustls::rustls::{
    self,
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    CertificateError, DigitallySignedStruct, SignatureScheme,
};

/// The usual verifier, refusing first a chain in which `refuse` matches any
/// certificate the server sent.
#[derive(Debug)]
pub(crate) struct RefusingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    refuse: fn(&[u8]) -> bool,
}

impl RefusingVerifier {
    pub(crate) fn new(inner: Arc<WebPkiServerVerifier>, refuse: fn(&[u8]) -> bool) -> Self {
        Self { inner, refuse }
    }
}

impl ServerCertVerifier for RefusingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if std::iter::once(end_entity)
            .chain(intermediates)
            .any(|cert| (self.refuse)(cert))
        {
            return Err(rustls::Error::InvalidCertificate(CertificateError::Revoked));
        }
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
