//! TLS for the VeNCrypt security types, as TurboVNC `rfbssl_openssl.c`:
//! anonymous Diffie-Hellman for the `tls*` types and an X.509 certificate
//! for the `x509*` types.

use std::io;
use std::path::Path;
use std::pin::Pin;

use openssl::dh::Dh;
use openssl::ssl::{Ssl, SslContext, SslFiletype, SslMethod, SslOptions, SslVersion};
use tokio_openssl::SslStream;
use tracing::info;

use crate::Config;
use crate::security::Tls;
use crate::session::Stream;

fn ssl_error(what: &str, e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("{what}: {e}"))
}

/// The TLS contexts for the enabled security types.
pub(crate) struct Contexts {
    anon: Option<SslContext>,
    x509: Option<SslContext>,
}

impl Contexts {
    pub fn new(config: &Config) -> io::Result<Self> {
        let types = &config.security_types;
        let ciphers = config.tls_ciphers.as_deref();
        let anon = if types.iter().any(|t| t.tls() == Some(Tls::Anon)) {
            Some(anon_context(ciphers).map_err(|e| ssl_error("anonymous TLS", e))?)
        } else {
            None
        };
        let x509 = if types.iter().any(|t| t.tls() == Some(Tls::X509)) {
            let cert = config.x509_cert.as_deref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the X509 security types need an X.509 certificate (-x509cert)",
                )
            })?;
            let key = config.x509_key.as_deref().unwrap_or(cert);
            Some(x509_context(cert, key, ciphers)?)
        } else {
            None
        };
        Ok(Self { anon, x509 })
    }

    /// Runs the server side of the TLS handshake on `stream`.
    pub async fn accept(&self, kind: Tls, stream: Box<dyn Stream>) -> io::Result<Box<dyn Stream>> {
        let ctx = match kind {
            Tls::Anon => &self.anon,
            Tls::X509 => &self.x509,
        };
        let ctx = ctx.as_ref().expect("built for every enabled TLS type");
        let ssl = Ssl::new(ctx).map_err(|e| ssl_error("SSL_new()", e))?;
        let mut stream = SslStream::new(ssl, stream).map_err(|e| ssl_error("TLS", e))?;
        Pin::new(&mut stream)
            .accept()
            .await
            .map_err(|e| ssl_error("TLS handshake", e))?;
        if let Some(cipher) = stream.ssl().current_cipher() {
            info!(
                "negotiated {} cipher suite {}",
                stream.ssl().version_str(),
                cipher.name()
            );
        }
        Ok(Box::new(stream))
    }
}

fn builder() -> Result<openssl::ssl::SslContextBuilder, openssl::error::ErrorStack> {
    let mut b = SslContext::builder(SslMethod::tls_server())?;
    b.set_options(SslOptions::NO_SSLV2 | SslOptions::NO_SSLV3);
    Ok(b)
}

/// Anonymous (certificate-less) cipher suites. TLS 1.3 has none, so this
/// is TLS 1.2 at most, and OpenSSL's security level must be 0 to allow
/// them. TurboVNC generates DH parameters for each connection; a fixed
/// 2048-bit group (RFC 5114) serves the same purpose without the delay.
fn anon_context(ciphers: Option<&str>) -> Result<SslContext, openssl::error::ErrorStack> {
    let mut b = builder()?;
    b.set_max_proto_version(Some(SslVersion::TLS1_2))?;
    let dh = Dh::get_2048_256()?;
    b.set_tmp_dh(&dh)?;
    b.set_cipher_list(ciphers.unwrap_or("aNULL"))?;
    b.set_security_level(0);
    Ok(b.build())
}

fn x509_context(cert: &Path, key: &Path, ciphers: Option<&str>) -> io::Result<SslContext> {
    let mut b = builder().map_err(|e| ssl_error("TLS", e))?;
    b.set_certificate_chain_file(cert).map_err(|e| {
        ssl_error(
            &format!("unable to load X.509 certificate file {}", cert.display()),
            e,
        )
    })?;
    b.set_private_key_file(key, SslFiletype::PEM).map_err(|e| {
        ssl_error(
            &format!("unable to load X.509 private key file {}", key.display()),
            e,
        )
    })?;
    b.check_private_key()
        .map_err(|e| ssl_error("X.509 certificate and private key", e))?;
    if let Some(c) = ciphers {
        b.set_cipher_list(c)
            .map_err(|e| ssl_error("permitted cipher suites", e))?;
    }
    info!(
        "using X.509 certificate file {} and private key file {}",
        cert.display(),
        key.display()
    );
    Ok(b.build())
}
