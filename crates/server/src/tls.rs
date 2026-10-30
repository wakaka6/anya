//! The gateway's TLS identity: one self-signed certificate, made on first
//! use and kept, that paired clients pin by its fingerprint.

use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::sync::Arc;

use ring::digest::{SHA256, digest};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use crate::private;
use crate::server::ServerError;

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// A certificate and its key, loaded from a directory. The certificate is
/// valid for `localhost`, `127.0.0.1` and this machine's [`lan_address`] at
/// the time it was made; clients pin its [`fingerprint`](TlsIdentity::fingerprint)
/// instead of checking a name, so a later change of address does not break them.
pub struct TlsIdentity {
    config: Arc<rustls::ServerConfig>,
    fingerprint: String,
}

impl std::fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsIdentity")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl TlsIdentity {
    /// Loads `cert.pem` and `key.pem` from `dir`, making them (owner-only,
    /// in a 0700 directory) when either is missing. Files that exist but
    /// are unusable are an error and are left alone: replacing them would
    /// silently unpair every client.
    pub fn load_or_create(dir: &Path) -> Result<TlsIdentity, ServerError> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if !(cert_path.exists() && key_path.exists()) {
            create(dir, &cert_path, &key_path)?;
        }
        let cert = CertificateDer::from_pem_file(&cert_path).map_err(|e| ServerError::Tls {
            path: cert_path.clone(),
            reason: e.to_string(),
        })?;
        let key = PrivateKeyDer::from_pem_file(&key_path).map_err(|e| ServerError::Tls {
            path: key_path.clone(),
            reason: e.to_string(),
        })?;
        let fingerprint = fingerprint_of(cert.as_ref());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_no_client_auth().with_single_cert(vec![cert], key))
            .map_err(|e| ServerError::Tls {
                path: dir.to_path_buf(),
                reason: e.to_string(),
            })?;
        Ok(TlsIdentity {
            config: Arc::new(config),
            fingerprint,
        })
    }

    /// SHA-256 of the certificate (DER) as lowercase hex pairs joined by
    /// `:`, the way the pairing QR code carries it.
    pub fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::clone(&self.config))
    }
}

fn create(dir: &Path, cert_path: &Path, key_path: &Path) -> Result<(), ServerError> {
    let tls = |path: &Path, reason: String| ServerError::Tls {
        path: path.to_path_buf(),
        reason,
    };
    let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
    names.extend(lan_address().map(|ip| ip.to_string()));
    let made = rcgen::generate_simple_self_signed(names)
        .map_err(|e| tls(dir, format!("making the certificate: {e}")))?;
    private::create_dir(dir).map_err(|e| tls(dir, e.to_string()))?;
    // The key first: a crash between the two leaves no certificate, which
    // the next start replaces.
    private::write(key_path, made.signing_key.serialize_pem().as_bytes())
        .map_err(|e| tls(key_path, e.to_string()))?;
    private::write(cert_path, made.cert.pem().as_bytes())
        .map_err(|e| tls(cert_path, e.to_string()))?;
    Ok(())
}

fn fingerprint_of(der: &[u8]) -> String {
    digest(&SHA256, der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// This machine's primary non-loopback IPv4 address, for URLs and the QR
/// code: the address the system would send from toward the internet. No
/// packet is sent. `None` when the machine has no such address.
pub fn lan_address() -> Option<IpAddr> {
    // UDP "connect" only picks a route; 192.0.2.1 is reserved for documentation.
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}
