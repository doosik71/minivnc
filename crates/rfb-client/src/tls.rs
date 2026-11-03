//! TLS for the VeNCrypt security types: a connection that can switch to
//! TLS mid-handshake, and the TurboVNC viewer's certificate checks
//! (`CSecurityTLS.java`).

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use openssl::asn1::Asn1Time;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::ssl::{ErrorCode, Ssl, SslContext, SslMethod, SslStream, SslVersion};
use openssl::stack::Stack;
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::verify::X509VerifyFlags;
use openssl::x509::{X509, X509NameRef, X509Ref, X509StoreContext, X509VerifyResult};

use crate::{ClientError, ConnectOptions, Result};

/// A connection to the server; clones share it, so one thread can read
/// while others write. Writes are never interleaved. After
/// [`Transport::start_tls`] everything goes through TLS.
#[derive(Clone)]
pub struct Stream(Arc<Inner>);

struct Inner {
    tcp: TcpStream,
    tls: Mutex<Option<SslStream<TcpStream>>>,
}

impl Stream {
    pub fn new(tcp: TcpStream) -> Self {
        Self(Arc::new(Inner {
            tcp,
            tls: Mutex::new(None),
        }))
    }

    /// The underlying TCP connection (for socket options and addresses).
    pub fn tcp(&self) -> &TcpStream {
        &self.0.tcp
    }

    /// Closes the connection, waking a blocked reader.
    pub fn shutdown(&self) -> io::Result<()> {
        self.0.tcp.shutdown(Shutdown::Both)
    }

    fn lock(&self) -> MutexGuard<'_, Option<SslStream<TcpStream>>> {
        self.0.tls.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A byte stream the client can switch to TLS.
pub trait Transport: Read + Write {
    /// Runs the client side of a TLS handshake with `ssl` and returns the
    /// server's certificate chain, leaf first (empty when anonymous).
    fn start_tls(&mut self, ssl: Ssl) -> io::Result<Vec<X509>>;
}

impl Transport for Stream {
    fn start_tls(&mut self, ssl: Ssl) -> io::Result<Vec<X509>> {
        let mut tls = self.lock();
        assert!(tls.is_none(), "TLS started twice");
        let mut s = SslStream::new(ssl, self.0.tcp.try_clone()?).map_err(io::Error::other)?;
        s.connect()
            .map_err(|e| io::Error::other(format!("TLS handshake failed: {e}")))?;
        let chain = s
            .ssl()
            .peer_cert_chain()
            .map(|c| c.iter().map(X509Ref::to_owned).collect())
            .unwrap_or_default();
        *tls = Some(s);
        Ok(chain)
    }
}

fn ssl_io_error(e: openssl::ssl::Error) -> io::Error {
    match e.into_io_error() {
        Ok(e) => e,
        Err(e) => io::Error::other(e),
    }
}

impl Read for Stream {
    /// With TLS, waits for data without holding the lock, so writers are
    /// not held up by a reader waiting for the server. (A TLS record
    /// without application data, such as a TLS 1.3 key update, still
    /// makes `SSL_read` wait for the next one with the lock held: the safe
    /// API cannot clear `SSL_MODE_AUTO_RETRY`.)
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut tls = self.lock();
            let Some(s) = tls.as_mut() else {
                drop(tls);
                return (&self.0.tcp).read(buf);
            };
            if s.ssl().pending() == 0 {
                drop(tls);
                // Returns at end of stream too; SSL_read then reports it.
                self.0.tcp.peek(&mut [0])?;
                tls = self.lock();
            }
            if let Some(n) = tls_read(tls.as_mut().expect("TLS started"), buf)? {
                return Ok(n);
            }
        }
    }
}

/// One `SSL_read` after the socket became readable: `None` if OpenSSL
/// asks to try again.
fn tls_read(s: &mut SslStream<TcpStream>, buf: &mut [u8]) -> io::Result<Option<usize>> {
    match s.ssl_read(buf) {
        Ok(n) => Ok(Some(n)),
        Err(e) if e.code() == ErrorCode::ZERO_RETURN => Ok(Some(0)),
        Err(e) if e.code() == ErrorCode::SYSCALL && e.io_error().is_none() => Ok(Some(0)),
        Err(e) if e.code() == ErrorCode::WANT_READ && e.io_error().is_none() => Ok(None),
        Err(e) => Err(ssl_io_error(e)),
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut tls = self.lock();
        match tls.as_mut() {
            Some(s) => s.write_all(buf),
            None => (&self.0.tcp).write_all(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn ssl_error(e: openssl::error::ErrorStack) -> ClientError {
    ClientError::Io(io::Error::other(e))
}

/// A TLS client session: anonymous cipher suites (TLS 1.2 at most, as
/// TLS 1.3 has none), or the defaults for X.509.
pub(crate) fn new_ssl(anon: bool) -> Result<Ssl> {
    let mut b = SslContext::builder(SslMethod::tls_client()).map_err(ssl_error)?;
    if anon {
        b.set_max_proto_version(Some(SslVersion::TLS1_2))
            .map_err(ssl_error)?;
        // The TurboVNC viewer prefers ECDH over DH.
        b.set_cipher_list("aECDH+aNULL:aNULL").map_err(ssl_error)?;
        b.set_security_level(0);
    }
    Ssl::new(&b.build()).map_err(ssl_error)
}

fn not_trusted() -> ClientError {
    ClientError::NotTrusted("X.509 certificate not trusted".into())
}

fn ask(options: &ConnectOptions, question: &str) -> Result<()> {
    match options.confirm {
        Some(confirm) if confirm(question)? => Ok(()),
        _ => Err(not_trusted()),
    }
}

fn name_text(name: &X509NameRef) -> String {
    name.entries()
        .map(|e| {
            let key = e.object().nid().short_name().unwrap_or("?");
            let value = e.data().to_string().unwrap_or_default();
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn fingerprint(cert: &X509Ref) -> String {
    cert.digest(MessageDigest::sha256())
        .map(|d| {
            d.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":")
        })
        .unwrap_or_default()
}

fn certificate_info(cert: &X509Ref) -> String {
    let serial = cert
        .serial_number()
        .to_bn()
        .and_then(|n| n.to_hex_str().map(|s| s.to_string()))
        .unwrap_or_default();
    format!(
        "  Subject: {}\n  Issuer: {}\n  Serial Number: {serial}\n  \
         Not Valid Before: {}\n  Not Valid After: {}\n  SHA-256 Fingerprint: {}",
        name_text(cert.subject_name()),
        name_text(cert.issuer_name()),
        cert.not_before(),
        cert.not_after(),
        fingerprint(cert)
    )
}

/// The certificate names `host`, as the TurboVNC viewer checks: a DNS or
/// IP subject alternative name, or without those the common name.
fn host_matches(cert: &X509Ref, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let ip: Option<IpAddr> = host.parse().ok();
    match cert.subject_alt_names() {
        Some(sans) => sans.iter().any(|n| {
            n.dnsname().is_some_and(|d| d.eq_ignore_ascii_case(&host))
                || n.ipaddress().is_some_and(|a| match (ip, a.len()) {
                    (Some(IpAddr::V4(v4)), 4) => v4.octets() == a,
                    (Some(IpAddr::V6(v6)), 16) => v6.octets() == a,
                    _ => false,
                })
        }),
        None => cert
            .subject_name()
            .entries_by_nid(Nid::COMMONNAME)
            .any(|e| {
                e.data()
                    .to_string()
                    .is_ok_and(|cn| cn.eq_ignore_ascii_case(&host))
            }),
    }
}

fn read_certs(path: &Path) -> Vec<X509> {
    fs::read(path)
        .ok()
        .and_then(|pem| X509::stack_from_pem(&pem).ok())
        .unwrap_or_default()
}

/// Checks the server's certificate as the TurboVNC viewer does: validity
/// first, then certificates the user accepted before, the host name, and
/// the system and `x509_ca` authorities. An expired certificate, a host
/// name mismatch or an unknown authority is put to the user; a
/// certificate from an unknown authority is saved once accepted.
pub(crate) fn verify(chain: &[X509], options: &ConnectOptions) -> Result<()> {
    let leaf = chain
        .first()
        .ok_or_else(|| ClientError::NotTrusted("the server sent no certificate".into()))?;
    let info = certificate_info(leaf);
    let now = Asn1Time::days_from_now(0).map_err(ssl_error)?;
    if leaf.not_before() > now {
        return Err(ClientError::NotTrusted(format!(
            "X.509 certificate is not valid yet\n\n{info}"
        )));
    }
    if leaf.not_after() < now {
        ask(
            options,
            &format!("X.509 certificate has expired\n\n{info}\n\nDo you want to continue?"),
        )?;
    }

    let saved = options
        .saved_certs
        .as_deref()
        .map(read_certs)
        .unwrap_or_default();
    let der = leaf.to_der().map_err(ssl_error)?;
    if saved.iter().any(|c| c.to_der().is_ok_and(|d| d == der)) {
        return Ok(());
    }

    if !host_matches(leaf, &options.host) {
        ask(
            options,
            "X.509 hostname verification failed.  Do you want to continue?",
        )?;
    }

    let mut store = X509StoreBuilder::new().map_err(ssl_error)?;
    store.set_default_paths().map_err(ssl_error)?;
    // Expiry was handled above.
    store
        .set_flags(X509VerifyFlags::NO_CHECK_TIME)
        .map_err(ssl_error)?;
    if let Some(ca) = &options.x509_ca {
        for cert in read_certs(ca) {
            store.add_cert(cert).map_err(ssl_error)?;
        }
    }
    let store = store.build();
    let mut untrusted = Stack::new().map_err(ssl_error)?;
    for cert in &chain[1..] {
        untrusted.push(cert.clone()).map_err(ssl_error)?;
    }
    let mut ctx = X509StoreContext::new().map_err(ssl_error)?;
    let result = ctx
        .init(&store, leaf, &untrusted, |c| {
            c.verify_cert()?;
            Ok(c.error())
        })
        .map_err(ssl_error)?;
    if result == X509VerifyResult::OK {
        return Ok(());
    }
    let unknown_issuer = [
        2,  // X509_V_ERR_UNABLE_TO_GET_ISSUER_CERT
        18, // X509_V_ERR_DEPTH_ZERO_SELF_SIGNED_CERT
        19, // X509_V_ERR_SELF_SIGNED_CERT_IN_CHAIN
        20, // X509_V_ERR_UNABLE_TO_GET_ISSUER_CERT_LOCALLY
        21, // X509_V_ERR_UNABLE_TO_VERIFY_LEAF_SIGNATURE
    ];
    if !unknown_issuer.contains(&result.as_raw()) {
        return Err(ClientError::NotTrusted(format!(
            "X.509 certificate verification failed: {}\n\n{info}",
            result.error_string()
        )));
    }
    ask(
        options,
        &format!(
            "X.509 certificate has been signed by an unknown authority\n\n{info}\n\n\
             Do you want to save it and continue?"
        ),
    )?;
    if let Some(path) = &options.saved_certs {
        save_cert(path, leaf)?;
    }
    Ok(())
}

fn save_cert(path: &Path, cert: &X509Ref) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let pem = cert.to_pem().map_err(ssl_error)?;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&pem)?;
    Ok(())
}
