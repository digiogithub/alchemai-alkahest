//! HTTPS (ALK-SP-0006.R6): rustls listener with certificate reload, and self-signed
//! certificate generation for `alkahest certs generate`.
//!
//! The server keeps one rustls `ServerConfig` whose certificate resolver reads the current
//! [`CertifiedKey`] from a lock-protected slot. Reloading swaps the slot, so a renewed
//! certificate applies to the next handshake without dropping open connections and without
//! rebuilding the listener. Reload triggers: SIGHUP (Unix) and a periodic check of the
//! files' modification time and size ([`TlsAcceptor::with_reload_interval`]). A reload that
//! fails (half-written file, mismatching key) keeps the previous certificate and logs why.
//!
//! The crypto provider is aws-lc-rs, the one reqwest already links, so TLS adds no second
//! crypto library to the binary.
//!
//! [`generate_self_signed`] creates a throwaway CA and a server certificate signed by it.
//! The CA key is discarded at once (it is never written), so the CA can sign nothing else;
//! clients trust the CA certificate (`ca.pem`, `ALKAHEST_CA_CERT`). A CA plus leaf, rather
//! than one self-signed leaf, is what platform verifiers (macOS, Windows) accept as an extra
//! trust root.

use std::fmt;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::{Mutex, RwLock};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use time::OffsetDateTime;
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// TLS failures: loading, reloading or generating certificates.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A certificate or key file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Cause.
        #[source]
        source: io::Error,
    },
    /// The file holds no usable PEM object of the expected kind.
    #[error("{path}: {reason}")]
    Pem {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// The key does not match the certificate or its type is unsupported.
    #[error("TLS certificate {cert} and key {key} cannot be used together: {reason}")]
    Key {
        /// Certificate file.
        cert: PathBuf,
        /// Key file.
        key: PathBuf,
        /// What rustls reported.
        reason: String,
    },
    /// rustls refused the configuration.
    #[error("TLS configuration: {0}")]
    Config(String),
    /// A generated file could not be written.
    #[error("writing {path}: {source}")]
    Write {
        /// The file.
        path: PathBuf,
        /// Cause.
        #[source]
        source: io::Error,
    },
    /// A file exists and overwriting was not requested.
    #[error("{0} already exists (pass --force to replace it)")]
    Exists(PathBuf),
    /// Certificate generation failed.
    #[error("generating a certificate: {0}")]
    Generate(String),
}

/// Modification time and size of the certificate and key, to notice replaced files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileStamp {
    cert: Option<(SystemTime, u64)>,
    key: Option<(SystemTime, u64)>,
}

fn stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl FileStamp {
    fn read(cert: &Path, key: &Path) -> Self {
        Self {
            cert: stamp_of(cert),
            key: stamp_of(key),
        }
    }
}

/// The certificate the resolver hands to each handshake; swapped on reload.
#[derive(Debug)]
struct CertSlot {
    current: RwLock<Arc<CertifiedKey>>,
}

impl ResolvesServerCert for CertSlot {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().clone())
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn read_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, TlsError> {
    std::fs::read(path)
        .map(Zeroizing::new)
        .map_err(|source| TlsError::Read {
            path: path.to_path_buf(),
            source,
        })
}

/// Loads and checks the pair: at least one certificate, a parseable key that matches the
/// leaf. The key bytes are wiped from memory after parsing.
fn load_pair(
    cert: &Path,
    key: &Path,
    provider: &CryptoProvider,
) -> Result<Arc<CertifiedKey>, TlsError> {
    let cert_pem = read_file(cert)?;
    let chain = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem {
            path: cert.to_path_buf(),
            reason: format!("invalid PEM certificate: {e}"),
        })?;
    if chain.is_empty() {
        return Err(TlsError::Pem {
            path: cert.to_path_buf(),
            reason: "no PEM certificate found".into(),
        });
    }
    warn_if_key_is_readable_by_others(key);
    let key_pem = read_file(key)?;
    let private = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|e| TlsError::Pem {
        path: key.to_path_buf(),
        reason: format!("no usable PEM private key (PKCS#8, PKCS#1 or SEC1): {e}"),
    })?;
    let certified =
        CertifiedKey::from_der(chain, private, provider).map_err(|e| TlsError::Key {
            cert: cert.to_path_buf(),
            key: key.to_path_buf(),
            reason: e.to_string(),
        })?;
    Ok(Arc::new(certified))
}

#[cfg(unix)]
fn warn_if_key_is_readable_by_others(key: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(key)
        && meta.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            key = %key.display(),
            mode = format_args!("{:o}", meta.permissions().mode() & 0o777),
            "the TLS private key is readable by other users; chmod 600 it"
        );
    }
}

#[cfg(not(unix))]
fn warn_if_key_is_readable_by_others(_key: &Path) {}

/// Accepts TLS connections with a reloadable certificate. Cheap to clone.
#[derive(Clone)]
pub struct TlsAcceptor {
    cert: Arc<PathBuf>,
    key: Arc<PathBuf>,
    provider: Arc<CryptoProvider>,
    slot: Arc<CertSlot>,
    stamp: Arc<Mutex<FileStamp>>,
    inner: tokio_rustls::TlsAcceptor,
    reload_interval: Option<Duration>,
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsAcceptor")
            .field("cert", &self.cert)
            .field("key", &self.key)
            .field("reload_interval", &self.reload_interval)
            .finish_non_exhaustive()
    }
}

impl TlsAcceptor {
    /// Loads `cert` (PEM chain, leaf first) and `key` (PEM private key) and builds the
    /// listener configuration: TLS 1.2 and 1.3, ALPN `h2` and `http/1.1`, no client
    /// certificates. Fails when the files are missing, unreadable or do not belong together.
    pub fn from_files(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Result<Self, TlsError> {
        let cert = cert.into();
        let key = key.into();
        let provider = provider();
        let stamp = FileStamp::read(&cert, &key);
        let certified = load_pair(&cert, &key, &provider)?;
        let slot = Arc::new(CertSlot {
            current: RwLock::new(certified),
        });
        let mut config = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsError::Config(e.to_string()))?
            .with_no_client_auth()
            .with_cert_resolver(slot.clone());
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self {
            cert: Arc::new(cert),
            key: Arc::new(key),
            provider,
            slot,
            stamp: Arc::new(Mutex::new(stamp)),
            inner: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
            reload_interval: None,
        })
    }

    /// Checks the files for changes every `interval` while serving (`None` or zero: only
    /// SIGHUP reloads).
    #[must_use]
    pub fn with_reload_interval(mut self, interval: Option<Duration>) -> Self {
        self.reload_interval = interval.filter(|d| !d.is_zero());
        self
    }

    /// Certificate file.
    pub fn cert_path(&self) -> &Path {
        &self.cert
    }

    /// Key file.
    pub fn key_path(&self) -> &Path {
        &self.key
    }

    /// DER of the certificate currently served (the leaf).
    pub fn current_certificate(&self) -> Option<Vec<u8>> {
        self.slot.current.read().cert.first().map(|c| c.to_vec())
    }

    /// Re-reads both files and swaps the certificate for new handshakes. On failure the
    /// previous certificate stays in use.
    pub fn reload(&self) -> Result<(), TlsError> {
        let stamp = FileStamp::read(&self.cert, &self.key);
        let certified = load_pair(&self.cert, &self.key, &self.provider)?;
        *self.slot.current.write() = certified;
        *self.stamp.lock() = stamp;
        tracing::info!(cert = %self.cert.display(), "TLS certificate reloaded");
        Ok(())
    }

    /// Reloads when either file's modification time or size changed since the last load.
    /// Returns whether a reload happened.
    pub fn reload_if_changed(&self) -> Result<bool, TlsError> {
        let now = FileStamp::read(&self.cert, &self.key);
        if now == *self.stamp.lock() {
            return Ok(false);
        }
        match self.reload() {
            Ok(()) => Ok(true),
            Err(e) => {
                // Remember the stamp anyway: a broken file is reported once, not on every
                // tick; the next change triggers another attempt.
                *self.stamp.lock() = now;
                Err(e)
            }
        }
    }

    /// Runs the TLS handshake on an accepted connection.
    pub async fn accept(
        &self,
        stream: TcpStream,
    ) -> io::Result<tokio_rustls::server::TlsStream<TcpStream>> {
        self.inner.accept(stream).await
    }

    /// Installs the SIGHUP handler now and returns the future that reloads on SIGHUP and on
    /// file changes; it runs until dropped. The server spawns it for its lifetime.
    pub fn reloader(&self) -> io::Result<impl std::future::Future<Output = ()> + Send + 'static> {
        let this = self.clone();
        let mut hangup = hangup_signal()?;
        Ok(async move {
            let mut tick = this.reload_interval.map(|d| {
                let mut t = tokio::time::interval(d);
                t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                t
            });
            loop {
                let by_signal = tokio::select! {
                    () = hangup.recv() => true,
                    () = async {
                        match tick.as_mut() {
                            Some(t) => { t.tick().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => false,
                };
                let this = this.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if by_signal {
                        this.reload().map(|()| true)
                    } else {
                        this.reload_if_changed()
                    }
                })
                .await;
                match result {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::error!(
                        error = %e,
                        "TLS certificate reload failed; still serving the previous certificate"
                    ),
                    Err(e) => tracing::error!(error = %e, "TLS reload task failed"),
                }
            }
        })
    }
}

/// SIGHUP on Unix; a never-firing stand-in elsewhere (Windows has no SIGHUP; file changes
/// still reload).
struct Hangup {
    #[cfg(unix)]
    inner: tokio::signal::unix::Signal,
}

impl Hangup {
    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            if self.inner.recv().await.is_none() {
                std::future::pending::<()>().await;
            }
        }
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    }
}

fn hangup_signal() -> io::Result<Hangup> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Hangup {
            inner: signal(SignalKind::hangup())?,
        })
    }
    #[cfg(not(unix))]
    Ok(Hangup {})
}

/// A generated CA and server certificate, PEM encoded.
pub struct GeneratedCert {
    /// CA certificate clients trust (`ca.pem`).
    pub ca_pem: String,
    /// Server certificate followed by the CA certificate (`cert.pem`).
    pub chain_pem: String,
    /// Server private key, PKCS#8 (`key.pem`); wiped from memory on drop.
    pub key_pem: Zeroizing<String>,
    /// Names and addresses the certificate is valid for.
    pub names: Vec<String>,
    /// Expiry.
    pub not_after: OffsetDateTime,
}

impl fmt::Debug for GeneratedCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedCert")
            .field("names", &self.names)
            .field("not_after", &self.not_after)
            .field("key_pem", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Longest validity accepted: Apple platforms refuse server certificates valid for more
/// than 825 days.
pub const MAX_VALID_DAYS: u32 = 825;

/// The names a locally generated certificate covers by default: `localhost`, the loopback
/// addresses and this machine's host name.
pub fn default_names() -> Vec<String> {
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Some(host) = sysinfo_host_name() {
        names.push(host);
    }
    names
}

fn sysinfo_host_name() -> Option<String> {
    let name = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })?;
    let name = name.trim().to_ascii_lowercase();
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'))
    .then_some(name)
}

/// Generates a CA and a server certificate for `names` (DNS names or IP addresses), valid
/// for `valid_days` (at most [`MAX_VALID_DAYS`]). Keys are ECDSA P-256.
pub fn generate_self_signed(names: &[String], valid_days: u32) -> Result<GeneratedCert, TlsError> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    let generate = |e: rcgen::Error| TlsError::Generate(e.to_string());
    if names.is_empty() {
        return Err(TlsError::Generate(
            "at least one host name is needed".into(),
        ));
    }
    if valid_days == 0 || valid_days > MAX_VALID_DAYS {
        return Err(TlsError::Generate(format!(
            "validity must be between 1 and {MAX_VALID_DAYS} days"
        )));
    }
    let now = OffsetDateTime::now_utc();
    // Backdated a little so a client whose clock runs behind still accepts it.
    let not_before = now - time::Duration::hours(1);
    let not_after = now + time::Duration::days(i64::from(valid_days));

    let ca_key = KeyPair::generate().map_err(generate)?;
    let mut ca = CertificateParams::new(Vec::<String>::new()).map_err(generate)?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca.distinguished_name
        .push(DnType::CommonName, "Alkahest local CA");
    ca.distinguished_name
        .push(DnType::OrganizationName, "Alkahest");
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca.not_before = not_before;
    ca.not_after = not_after;
    let ca_cert = ca.self_signed(&ca_key).map_err(generate)?;
    let issuer = Issuer::new(ca, ca_key);

    let leaf_key = KeyPair::generate().map_err(generate)?;
    let mut leaf = CertificateParams::new(names.to_vec()).map_err(generate)?;
    let cn = names
        .iter()
        .find(|n| n.parse::<IpAddr>().is_err())
        .unwrap_or(&names[0]);
    leaf.distinguished_name
        .push(DnType::CommonName, cn.as_str());
    leaf.distinguished_name
        .push(DnType::OrganizationName, "Alkahest");
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf.not_before = not_before;
    leaf.not_after = not_after;
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer).map_err(generate)?;
    // `issuer` (with the CA key) is dropped here: nothing else can ever be signed by it.
    drop(issuer);

    let ca_pem = ca_cert.pem();
    Ok(GeneratedCert {
        chain_pem: format!("{}{}", leaf_cert.pem(), ca_pem),
        ca_pem,
        key_pem: Zeroizing::new(leaf_key.serialize_pem()),
        names: names.to_vec(),
        not_after,
    })
}

/// Where [`write_generated`] puts the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertPaths {
    /// Server certificate chain.
    pub cert: PathBuf,
    /// Server private key (written owner-only).
    pub key: PathBuf,
    /// CA certificate for clients.
    pub ca: PathBuf,
}

/// Writes `generated` to `paths`: certificates world-readable, the key 0600 on Unix (created
/// with that mode, never widened later). Each file goes through a temporary file and a
/// rename, so a running server never reads a half-written file. Refuses to replace existing
/// files unless `overwrite`.
pub fn write_generated(
    generated: &GeneratedCert,
    paths: &CertPaths,
    overwrite: bool,
) -> Result<(), TlsError> {
    if !overwrite {
        for p in [&paths.cert, &paths.key, &paths.ca] {
            if p.exists() {
                return Err(TlsError::Exists(p.clone()));
            }
        }
    }
    // Key first: a server polling for changes sees the new key before the new certificate
    // and the mismatch only delays the reload to the next change (the certificate write).
    write_atomic(&paths.key, generated.key_pem.as_bytes(), true)?;
    write_atomic(&paths.cert, generated.chain_pem.as_bytes(), false)?;
    write_atomic(&paths.ca, generated.ca_pem.as_bytes(), false)?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8], secret: bool) -> Result<(), TlsError> {
    use std::io::Write as _;
    let err = |source: io::Error| TlsError::Write {
        path: path.to_path_buf(),
        source,
    };
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(err)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tls.pem");
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if secret { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = secret;
    let _ = std::fs::remove_file(&tmp);
    let result = options.open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if let Err(e) = result.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["localhost".into(), "127.0.0.1".into()]
    }

    fn write(dir: &Path) -> CertPaths {
        let paths = CertPaths {
            cert: dir.join("tls/cert.pem"),
            key: dir.join("tls/key.pem"),
            ca: dir.join("tls/ca.pem"),
        };
        let generated = generate_self_signed(&names(), 30).unwrap();
        write_generated(&generated, &paths, false).unwrap();
        paths
    }

    #[test]
    fn generated_pair_loads_and_key_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = write(dir.path());
        let acceptor = TlsAcceptor::from_files(&paths.cert, &paths.key).unwrap();
        assert!(acceptor.current_certificate().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Existing files are kept unless overwriting is asked for.
        let again = generate_self_signed(&names(), 30).unwrap();
        assert!(matches!(
            write_generated(&again, &paths, false),
            Err(TlsError::Exists(_))
        ));
        write_generated(&again, &paths, true).unwrap();
    }

    #[test]
    fn reload_follows_file_changes_and_keeps_the_old_certificate_on_errors() {
        let dir = tempfile::tempdir().unwrap();
        let paths = write(dir.path());
        let acceptor = TlsAcceptor::from_files(&paths.cert, &paths.key).unwrap();
        let first = acceptor.current_certificate().unwrap();
        assert!(!acceptor.reload_if_changed().unwrap());

        let next = generate_self_signed(&names(), 30).unwrap();
        write_generated(&next, &paths, true).unwrap();
        assert!(acceptor.reload_if_changed().unwrap());
        let second = acceptor.current_certificate().unwrap();
        assert_ne!(first, second);

        // A broken certificate is refused; the served one stays.
        std::fs::write(&paths.cert, b"not pem").unwrap();
        assert!(acceptor.reload().is_err());
        assert_eq!(acceptor.current_certificate().unwrap(), second);
    }

    #[test]
    fn mismatched_key_and_missing_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path());
        let other = tempfile::tempdir().unwrap();
        let b = write(other.path());
        assert!(matches!(
            TlsAcceptor::from_files(&a.cert, &b.key),
            Err(TlsError::Key { .. })
        ));
        assert!(matches!(
            TlsAcceptor::from_files(dir.path().join("none.pem"), &a.key),
            Err(TlsError::Read { .. })
        ));
        assert!(matches!(
            TlsAcceptor::from_files(&a.key, &a.key),
            Err(TlsError::Pem { .. })
        ));
    }

    #[test]
    fn generation_checks_its_inputs_and_redacts_the_key() {
        assert!(generate_self_signed(&[], 30).is_err());
        assert!(generate_self_signed(&names(), 0).is_err());
        assert!(generate_self_signed(&names(), MAX_VALID_DAYS + 1).is_err());
        let g = generate_self_signed(&names(), 1).unwrap();
        assert!(!format!("{g:?}").contains("PRIVATE KEY"));
        assert!(g.chain_pem.matches("BEGIN CERTIFICATE").count() == 2);
        assert!(default_names().contains(&"localhost".to_string()));
    }
}
