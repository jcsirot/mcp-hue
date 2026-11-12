//! TLS verification for Hue bridges.
//!
//! A bridge certificate is issued by a private Philips/Signify CA, carries the
//! bridge id as its common name and has no subject alternative name, so standard
//! hostname verification cannot succeed. Instead we check that the certificate
//! chains to one of the embedded Hue root CAs and, when the expected bridge id is
//! known, that the certificate was issued for that bridge.

use std::sync::Arc;

use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        verify_server_cert_signed_by_trust_anchor,
    },
    crypto::{
        CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    },
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject},
    server::ParsedCertificate,
};
use x509_parser::prelude::{FromDer, X509Certificate};

/// Root CA of second-generation bridges ("root-bridge").
const HUE_ROOT_BRIDGE_PEM: &[u8] = include_bytes!("../../certs/hue-root-bridge.pem");
/// Root CA used by newer bridges ("Hue Root CA 01").
const SIGNIFY_HUE_ROOT_CA_01_PEM: &[u8] = include_bytes!("../../certs/signify-hue-root-ca-01.pem");

/// Builds a TLS client configuration that only trusts genuine Hue bridges.
pub fn client_config(expected_bridge_id: Option<&str>) -> Result<ClientConfig, rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = HueBridgeVerifier::new(hue_roots()?, &provider, expected_bridge_id);
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

fn hue_roots() -> Result<RootCertStore, rustls::Error> {
    let mut roots = RootCertStore::empty();
    for pem in [HUE_ROOT_BRIDGE_PEM, SIGNIFY_HUE_ROOT_CA_01_PEM] {
        let cert = CertificateDer::from_pem_slice(pem).map_err(|err| {
            rustls::Error::General(format!("invalid embedded Hue root CA: {err}"))
        })?;
        roots.add(cert)?;
    }
    Ok(roots)
}

#[derive(Debug)]
struct HueBridgeVerifier {
    roots: RootCertStore,
    algorithms: WebPkiSupportedAlgorithms,
    expected_bridge_id: Option<String>,
}

impl HueBridgeVerifier {
    fn new(
        roots: RootCertStore,
        provider: &CryptoProvider,
        expected_bridge_id: Option<&str>,
    ) -> Self {
        Self {
            roots,
            algorithms: provider.signature_verification_algorithms,
            expected_bridge_id: expected_bridge_id.map(str::to_owned),
        }
    }
}

impl ServerCertVerifier for HueBridgeVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let parsed = ParsedCertificate::try_from(end_entity)?;
        verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &self.roots,
            intermediates,
            now,
            self.algorithms.all,
        )?;

        if let Some(expected) = &self.expected_bridge_id {
            let actual = common_name(end_entity)?;
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(CertificateError::NotValidForName.into());
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// The certificate's subject common name, which a Hue bridge sets to its bridge id.
fn common_name(cert: &CertificateDer<'_>) -> Result<String, rustls::Error> {
    let (_, parsed) =
        X509Certificate::from_der(cert.as_ref()).map_err(|_| CertificateError::BadEncoding)?;
    parsed
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| CertificateError::NotValidForName.into())
}

#[cfg(test)]
mod tests {
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };

    use super::*;

    const BRIDGE_ID: &str = "ecb5fafffe13af9c";

    struct TestCa {
        cert: CertificateDer<'static>,
        issuer: Issuer<'static, KeyPair>,
    }

    fn test_ca() -> TestCa {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "test-root");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let cert = params.self_signed(&key).unwrap().der().clone();
        TestCa {
            cert,
            issuer: Issuer::new(params, key),
        }
    }

    /// A leaf shaped like a bridge certificate: bridge id as CN, no SAN.
    fn bridge_cert(ca: &TestCa, bridge_id: &str) -> CertificateDer<'static> {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, bridge_id);
        params.signed_by(&key, &ca.issuer).unwrap().der().clone()
    }

    fn verify(
        trusted: &TestCa,
        cert: &CertificateDer<'_>,
        expected_bridge_id: Option<&str>,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut roots = RootCertStore::empty();
        roots.add(trusted.cert.clone()).unwrap();
        let provider = rustls::crypto::ring::default_provider();
        HueBridgeVerifier::new(roots, &provider, expected_bridge_id).verify_server_cert(
            cert,
            &[],
            &ServerName::try_from("192.168.1.7").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn embedded_roots_load() {
        assert_eq!(hue_roots().unwrap().len(), 2);
        client_config(Some(BRIDGE_ID)).unwrap();
    }

    #[test]
    fn accepts_certificate_from_trusted_root_without_san() {
        let ca = test_ca();
        let cert = bridge_cert(&ca, BRIDGE_ID);
        verify(&ca, &cert, None).unwrap();
    }

    #[test]
    fn accepts_matching_bridge_id_ignoring_case() {
        let ca = test_ca();
        let cert = bridge_cert(&ca, BRIDGE_ID);
        verify(&ca, &cert, Some("ECB5FAFFFE13AF9C")).unwrap();
    }

    #[test]
    fn rejects_certificate_from_unknown_root() {
        let trusted = test_ca();
        let cert = bridge_cert(&test_ca(), BRIDGE_ID);
        assert!(verify(&trusted, &cert, None).is_err());
    }

    #[test]
    fn rejects_other_bridge_id() {
        let ca = test_ca();
        let cert = bridge_cert(&ca, "001788fffe000000");
        let err = verify(&ca, &cert, Some(BRIDGE_ID)).unwrap_err();
        assert_eq!(
            err,
            rustls::Error::InvalidCertificate(CertificateError::NotValidForName)
        );
    }
}
