//! LocalSend (protocol v2.1): native sender and receiver.
//!
//! LocalSend is an open REST protocol, so Nook talks to LocalSend apps (and to
//! other Nook windows) directly. Nothing is shelled out and no LocalSend window
//! is involved: discovery runs over the protocol's multicast group, transfers
//! use the documented `/api/localsend/v2/*` endpoints, and every step is driven
//! from Nook's own UI.
//!
//! ```text
//!   discover ──► announce on 224.0.0.167:53317 ──► peers answer POST /register
//!   send     ──► POST /prepare-upload          ──► sessionId + per-file tokens
//!            ──► POST /upload?sessionId&fileId&token   (raw bytes)
//!   receive  ──► our own HTTPS server answers /register, /prepare-upload, /upload
//! ```
//!
//! All traffic is HTTPS with a self-signed certificate, as LocalSend does by
//! default: the receiver's fingerprint is the SHA-256 of its certificate. The
//! client accepts self-signed certificates (there is no CA on a home network)
//! and compares the presented certificate against the fingerprint the device
//! announced, so a changed certificate is at least reported.
//!
//! Every blocking operation runs on its own thread. Progress and incoming
//! requests reach the UI through an [`async_channel`] of [`Event`]s, which the
//! GTK main loop drains, so nothing here touches a widget.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::Config;

/// Default LocalSend port for both the HTTPS server and multicast discovery.
pub const DEFAULT_PORT: u16 = 53317;
/// Multicast group used for discovery (LocalSend default).
const MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
/// Protocol version announced to peers.
pub const PROTOCOL_VERSION: &str = "2.1";
/// Files larger than this are sent without a SHA-256, because hashing them
/// would double the time-to-first-byte for little benefit on a local link.
const HASH_LIMIT: u64 = 512 * 1024 * 1024;

/* --------------------------------- model -------------------------------- */

fn default_version() -> String {
    PROTOCOL_VERSION.to_string()
}

fn default_protocol() -> String {
    "https".to_string()
}

/// A LocalSend device, in the wire format used by announcements and `/register`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub alias: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub device_model: Option<String>,
    #[serde(default)]
    pub device_type: Option<String>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub download: bool,
    /// Only present in multicast announcements.
    #[serde(default)]
    pub announce: bool,
    /// Not on the wire: the address the device was reached from.
    #[serde(skip)]
    pub host: String,
}

impl Device {
    /// Human label used in the picker when the alias is empty.
    pub fn label(&self) -> String {
        if self.alias.trim().is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            self.alias.clone()
        }
    }

    pub fn kind(&self) -> &str {
        self.device_type.as_deref().unwrap_or("desktop")
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// One file announced in `prepare-upload`, as the receiver sees it.
#[derive(Clone, Debug)]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
}

/// A transfer waiting for the user to accept or decline it.
pub struct Incoming {
    pub from: String,
    pub device: Device,
    pub files: Vec<IncomingFile>,
    pub total_bytes: u64,
    /// Answer channel; send `true` to accept, `false` to decline.
    pub decide: async_channel::Sender<bool>,
}

impl Incoming {
    pub fn accept(&self) {
        let _ = self.decide.try_send(true);
    }

    pub fn decline(&self) {
        let _ = self.decide.try_send(false);
    }
}

/// Everything the transfer service reports to the UI.
pub enum Event {
    /// Somebody wants to send files to us.
    Incoming(Incoming),
    /// Files arrived and were written to disk.
    Saved { from: String, files: Vec<PathBuf> },
    /// Something worth a toast that is not an error.
    Notice(String),
}

/// Progress of one outgoing file.
#[derive(Clone, Debug)]
pub struct Progress {
    pub file: String,
    pub index: usize,
    pub count: usize,
    pub file_done: u64,
    pub file_size: u64,
    pub total_done: u64,
    pub total: u64,
}

/* ------------------------------- settings -------------------------------- */

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Nook".to_string())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Certificate fingerprints, in the format LocalSend uses on the wire:
/// uppercase hex. File checksums stay lowercase ([`hex`]), as the protocol
/// defines them.
fn fingerprint_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// This device's TLS identity: one self-signed certificate, used as the
/// server certificate *and* as the client certificate. LocalSend receivers
/// run mTLS and refuse peers that present no client certificate, and the
/// announced fingerprint is the SHA-256 of exactly this certificate, so the
/// server certificate, the client certificate and the announced fingerprint
/// must always come from the same key pair.
struct Identity {
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    /// SHA-256 of the certificate in DER form, uppercase hex.
    fingerprint: String,
}

/// The identity of this process, created on the first `Service::start`.
static IDENTITY: OnceLock<Identity> = OnceLock::new();

fn identity() -> Result<&'static Identity, String> {
    if let Some(identity) = IDENTITY.get() {
        return Ok(identity);
    }
    let certified = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "nook.local".to_string(),
    ])
    .map_err(|err| format!("could not create a certificate: {err}"))?;
    let identity = Identity {
        cert: certified.cert.der().clone(),
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()),
        ),
        fingerprint: fingerprint_hex(&Sha256::digest(certified.cert.der().as_ref())),
    };
    let _ = IDENTITY.set(identity);
    IDENTITY.get().ok_or_else(|| "could not create a certificate".to_string())
}

fn random_token() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(now.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(gtk::glib::uuid_string_random().as_bytes());
    hex(&hasher.finalize())[..32].to_string()
}

/* --------------------------------- state --------------------------------- */

struct SessionFile {
    id: String,
    path: PathBuf,
    token: String,
    sha256: Option<String>,
    /// Size announced in `prepare-upload`; the upload is only complete when
    /// exactly this many bytes have been written.
    size: u64,
    done: bool,
}

struct Session {
    from: String,
    files: Vec<SessionFile>,
}

struct Inner {
    alias: String,
    fingerprint: String,
    port: u16,
    events: async_channel::Sender<Event>,
    peers: Mutex<HashMap<String, Device>>,
    trusted: Mutex<Vec<String>>,
    auto_accept: AtomicBool,
    receive_dir: Mutex<PathBuf>,
    sessions: Mutex<HashMap<String, Session>>,
    tls: Arc<rustls::ServerConfig>,
    udp: Option<Arc<UdpSocket>>,
    shutdown: AtomicBool,
}

impl Inner {
    fn device(&self, announce: bool) -> Device {
        Device {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION.to_string(),
            device_model: Some("Nook".to_string()),
            device_type: Some("desktop".to_string()),
            fingerprint: self.fingerprint.clone(),
            port: self.port,
            protocol: "https".to_string(),
            download: false,
            announce,
            host: String::new(),
        }
    }

    fn device_json(&self, announce: bool) -> Vec<u8> {
        serde_json::to_vec(&self.device(announce)).unwrap_or_default()
    }

    fn remember(&self, device: &Device) {
        if device.fingerprint.is_empty() || device.fingerprint.eq_ignore_ascii_case(&self.fingerprint)
        {
            return;
        }
        self.peers
            .lock()
            .unwrap()
            .insert(device.fingerprint.clone(), device.clone());
    }
}

/// Handle to the running transfer service. Cheap to clone; dropping it does
/// not stop the threads, [`Service::shutdown`] does.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    /// Start the LocalSend receiver and discovery. Returns an error when the
    /// certificate or the sockets cannot be created; the rest of Nook keeps
    /// working in that case.
    pub fn start(cfg: &Config, events: async_channel::Sender<Event>) -> Result<Service, String> {
        install_provider();
        let alias = cfg
            .transfer_alias
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(hostname);

        let identity = identity()?;
        let fingerprint = identity.fingerprint.clone();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())
            .map_err(|err| format!("could not configure TLS: {err}"))?;
        // Build the client configuration now, while the identity is in place:
        // every outgoing connection has to present this certificate.
        let _ = client_config();

        // The announced port stays the default when possible; when LocalSend or
        // another copy of Nook already holds it, the operating system picks one
        // and we announce that instead (the protocol carries the port).
        let listener = TcpListener::bind(("0.0.0.0", cfg.transfer_port))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .map_err(|err| format!("could not open the transfer port: {err}"))?;
        let port = listener.local_addr().map_err(|err| err.to_string())?.port();
        let _ = listener.set_nonblocking(true);

        let receive_dir = cfg
            .transfer_receive_dir
            .clone()
            .map(|dir| crate::settings::expand_path(&dir))
            .unwrap_or_else(|| crate::settings::home_dir().join("Downloads"));

        let inner = Arc::new(Inner {
            alias,
            fingerprint,
            port,
            events,
            peers: Mutex::new(HashMap::new()),
            trusted: Mutex::new(cfg.transfer_trusted.clone()),
            auto_accept: AtomicBool::new(cfg.transfer_auto_accept),
            receive_dir: Mutex::new(receive_dir),
            sessions: Mutex::new(HashMap::new()),
            tls: Arc::new(tls),
            udp: bind_discovery(cfg.transfer_port),
            shutdown: AtomicBool::new(false),
        });
        let service = Service {
            inner: inner.clone(),
        };

        // Accept loop: every connection is handled on its own thread, so a
        // slow upload never blocks discovery or the UI.
        {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("nook-localsend".into())
                .spawn(move || {
                    while !inner.shutdown.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, addr)) => {
                                let inner = inner.clone();
                                let _ = std::thread::Builder::new()
                                    .name("nook-localsend-conn".into())
                                    .spawn(move || handle_connection(inner, stream, addr));
                            }
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(200));
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(500)),
                        }
                    }
                })
                .map_err(|err| err.to_string())?;
        }

        // Discovery listener.
        if let Some(udp) = inner.udp.clone() {
            let inner = inner.clone();
            let _ = std::thread::Builder::new()
                .name("nook-localsend-udp".into())
                .spawn(move || udp_loop(inner, udp));
        }

        // Periodic announcement, so devices that start later still find us.
        {
            let inner = inner.clone();
            let _ = std::thread::Builder::new()
                .name("nook-localsend-announce".into())
                .spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(10));
                    if inner.shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                    announce(&inner);
                });
        }

        announce(&inner);
        Ok(service)
    }

    pub fn alias(&self) -> String {
        self.inner.alias.clone()
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn fingerprint(&self) -> String {
        self.inner.fingerprint.clone()
    }

    pub fn device(&self) -> Device {
        self.inner.device(false)
    }

    /// Every device discovered so far, best first (named devices first).
    pub fn peers(&self) -> Vec<Device> {
        let mut devices: Vec<Device> = self.inner.peers.lock().unwrap().values().cloned().collect();
        devices.sort_by(|a, b| a.label().to_lowercase().cmp(&b.label().to_lowercase()));
        devices
    }

    /// Warn when a device shows up with a different certificate than the one
    /// it announced — the normal way to spot an unexpected peer.
    fn verify_peer(&self, device: &Device) {
        if device.fingerprint.is_empty() {
            return;
        }
        if let Ok((_, Some(actual))) = connect(device) {
            if !actual.eq_ignore_ascii_case(&device.fingerprint) {
                let _ = self.inner.events.try_send(Event::Notice(format!(
                    "{} presented a different certificate than it announced",
                    device.label()
                )));
            }
        }
    }

    /// Broadcast our presence; peers answer with `POST /register`.
    pub fn announce(&self) {
        announce(&self.inner);
    }

    /// Apply settings that can change without restarting the service.
    pub fn update(&self, cfg: &Config) {
        self.inner
            .auto_accept
            .store(cfg.transfer_auto_accept, Ordering::Relaxed);
        *self.inner.trusted.lock().unwrap() = cfg.transfer_trusted.clone();
        let dir = cfg
            .transfer_receive_dir
            .clone()
            .map(|dir| crate::settings::expand_path(&dir))
            .unwrap_or_else(|| crate::settings::home_dir().join("Downloads"));
        *self.inner.receive_dir.lock().unwrap() = dir;
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
    }

    /// Add a device by address, for networks where multicast is blocked.
    pub fn add_by_address(&self, address: &str) -> Result<Device, String> {
        let (host, port) = split_address(address)?;
        // Probe both schemes: LocalSend may run with encryption on or off.
        let mut last = String::new();
        for protocol in ["https", "http"] {
            let probe = Device {
                alias: host.clone(),
                version: PROTOCOL_VERSION.to_string(),
                device_model: None,
                device_type: Some("desktop".to_string()),
                fingerprint: String::new(),
                port,
                protocol: protocol.to_string(),
                download: false,
                announce: false,
                host: host.clone(),
            };
            match register_with(&self.inner, &probe) {
                Ok(device) => {
                    self.inner.remember(&device);
                    return Ok(device);
                }
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    /// Send `paths` (files and folders) to `device`. Runs on the calling thread
    /// and reports progress through `progress`; returns the number of files sent.
    pub fn send_files(
        &self,
        device: &Device,
        paths: &[PathBuf],
        pin: Option<&str>,
        progress: &(dyn Fn(Progress) + Send + Sync),
        cancelled: &AtomicBool,
    ) -> Result<usize, String> {
        self.verify_peer(device);
        let outgoing = collect_files(paths);
        if outgoing.is_empty() {
            return Err("There is nothing to send".into());
        }
        let total_bytes: u64 = outgoing.iter().map(|file| file.size).sum();

        let mut files_json = serde_json::Map::new();
        for (index, file) in outgoing.iter().enumerate() {
            let sha = if file.size <= HASH_LIMIT {
                sha256_file(&file.path).ok()
            } else {
                None
            };
            let id = format!("nook-{index}-{}", random_token());
            files_json.insert(
                id.clone(),
                serde_json::json!({
                    "id": id,
                    "fileName": file.name,
                    "size": file.size,
                    "fileType": file.mime,
                    "sha256": sha,
                    "metadata": { "modified": modified_iso(&file.path) },
                }),
            );
        }

        let body = serde_json::json!({
            "info": self.device(),
            "files": serde_json::Value::Object(files_json),
        });
        let body = serde_json::to_vec(&body).map_err(|err| err.to_string())?;
        let path = match pin {
            Some(pin) if !pin.trim().is_empty() => {
                format!(
                    "/api/localsend/v2/prepare-upload?pin={}",
                    url_encode(pin.trim())
                )
            }
            _ => "/api/localsend/v2/prepare-upload".to_string(),
        };
        let (status, body) = request_json(device, "POST", &path, Some(&body))?;
        match status {
            200 => {}
            204 => return Ok(0),
            401 => return Err("This device asks for a PIN".into()),
            403 => return Err(format!("{} declined the transfer", device.label())),
            409 => return Err(format!("{} is busy with another transfer", device.label())),
            413 => return Err("The device refused the transfer as too large".into()),
            429 => return Err("Too many requests: wait a moment and try again".into()),
            500 => return Err("The device reported an internal error".into()),
            other => return Err(format!("The device answered HTTP {other}")),
        }

        let response: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| "The device sent an invalid session")?;
        let session = response
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or("The device did not open a session")?
            .to_string();
        let tokens = response
            .get("files")
            .and_then(|v| v.as_object())
            .ok_or("The device did not accept any file")?
            .clone();

        let mut sent = 0usize;
        let mut total_done = 0u64;
        for (index, file) in outgoing.iter().enumerate() {
            let Some((id, token)) = tokens
                .iter()
                .find(|(id, _)| id.starts_with(&format!("nook-{index}-")))
                .and_then(|(id, token)| token.as_str().map(|t| (id.clone(), t.to_string())))
            else {
                total_done += file.size;
                continue;
            };
            if cancelled.load(Ordering::Relaxed) {
                cancel_session(device, &session);
                return Err("Transfer cancelled".into());
            }
            let upload_path = format!(
                "/api/localsend/v2/upload?sessionId={}&fileId={}&token={}",
                url_encode(&session),
                url_encode(&id),
                url_encode(&token)
            );
            upload_file(
                device,
                &upload_path,
                file,
                &mut |done| {
                    progress(Progress {
                        file: file.name.clone(),
                        index,
                        count: outgoing.len(),
                        file_done: done,
                        file_size: file.size,
                        total_done: total_done + done,
                        total: total_bytes,
                    });
                },
                cancelled,
            )?;
            total_done += file.size;
            sent += 1;
        }
        Ok(sent)
    }
}

/* ------------------------------ discovery -------------------------------- */

fn bind_discovery(port: u16) -> Option<Arc<UdpSocket>> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .ok()?;
    let _ = socket.set_reuse_address(true);
    #[cfg(unix)]
    let _ = socket.set_reuse_port(true);
    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port);
    socket.bind(&addr.into()).ok()?;
    let _ = socket.join_multicast_v4(&MULTICAST, &Ipv4Addr::UNSPECIFIED);
    let _ = socket.set_multicast_loop_v4(false);
    socket
        .set_read_timeout(Some(Duration::from_millis(400)))
        .ok()?;
    let udp: UdpSocket = socket.into();
    Some(Arc::new(udp))
}

fn announce(inner: &Inner) {
    let Some(udp) = inner.udp.as_ref() else {
        return;
    };
    let json = inner.device_json(true);
    let target = SocketAddr::new(MULTICAST.into(), DEFAULT_PORT);
    let _ = udp.send_to(&json, target);
    // Also unset loopback so another Nook on the same machine hears it.
    let local = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), DEFAULT_PORT);
    let _ = udp.send_to(&json, local);
}

fn udp_loop(inner: Arc<Inner>, udp: Arc<UdpSocket>) {
    let mut buffer = [0u8; 64 * 1024];
    while !inner.shutdown.load(Ordering::Relaxed) {
        match udp.recv_from(&mut buffer) {
            Ok((len, from)) => {
                let Ok(mut device) = serde_json::from_slice::<Device>(&buffer[..len]) else {
                    continue;
                };
                if device.port == 0 {
                    device.port = DEFAULT_PORT;
                }
                device.host = from.ip().to_string();
                inner.remember(&device);
                if device.announce {
                    // Unicast answer (works when the peer cannot receive
                    // multicast) plus a full /register so both sides know each
                    // other, exactly as the protocol describes.
                    let reply = inner.device_json(false);
                    let _ = udp.send_to(&reply, from);
                    let inner = inner.clone();
                    let peer = device.clone();
                    let _ = std::thread::Builder::new()
                        .name("nook-localsend-register".into())
                        .spawn(move || {
                            let _ = register_with(&inner, &peer);
                        });
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// `POST /register`: tell a device about us and learn who it is.
fn register_with(inner: &Arc<Inner>, device: &Device) -> Result<Device, String> {
    let body = inner.device_json(false);
    let (status, body) = request_json(device, "POST", "/api/localsend/v2/register", Some(&body))?;
    if status != 200 {
        return Err(format!("Device answered HTTP {status}"));
    }
    let mut answered: Device = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
    answered.host = device.host.clone();
    if answered.port == 0 {
        answered.port = device.port;
    }
    if answered.protocol.is_empty() {
        answered.protocol = device.protocol.clone();
    }
    inner.remember(&answered);
    Ok(answered)
}

/* --------------------------------- HTTP ---------------------------------- */

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn install_provider() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Accepts any certificate: LocalSend peers use self-signed certificates and
/// identity is established by the announced SHA-256 fingerprint instead.
#[derive(Debug)]
struct AcceptAny(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            install_provider();
            let provider = rustls::crypto::ring::default_provider();
            let builder = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)));
            // mTLS: LocalSend receivers demand a client certificate. A client
            // that presents none is rejected during the handshake with the
            // `certificate_required` alert, before a single HTTP byte moves.
            let config = match IDENTITY.get() {
                Some(identity) => builder
                    .with_client_auth_cert(vec![identity.cert.clone()], identity.key.clone_key())
                    .expect("the identity's certificate and key are already paired"),
                None => builder.with_no_client_auth(),
            };
            Arc::new(config)
        })
        .clone()
}

fn connect(device: &Device) -> Result<(Box<dyn Stream>, Option<String>), String> {
    let address = format!("{}:{}", device.host, device.port);
    let tcp = TcpStream::connect(&address)
        .map_err(|err| format!("Cannot reach {} ({address}): {err}", device.label()))?;
    let _ = tcp.set_nodelay(true);
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(120)));
    if device.protocol == "http" {
        return Ok((Box::new(tcp), None));
    }
    let name = rustls::pki_types::ServerName::try_from(device.host.clone())
        .map_err(|_| "Invalid device address".to_string())?;
    let config = client_config();
    let mut connection = rustls::ClientConnection::new(config, name)
        .map_err(|err| format!("TLS setup failed: {err}"))?;
    let mut tcp = tcp;
    connection
        .complete_io(&mut tcp)
        .map_err(|err| format!("TLS handshake with {} failed: {err}", device.label()))?;
    let fingerprint = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint_hex(cert.as_ref()));
    Ok((
        Box::new(rustls::StreamOwned::new(connection, tcp)),
        fingerprint,
    ))
}

fn read_head(stream: &mut dyn Stream) -> io::Result<String> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while head.len() < 32 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn header_value(head: &str, name: &str) -> Option<String> {
    for line in head.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn read_response(stream: &mut dyn Stream) -> Result<(u16, Vec<u8>), String> {
    let head = read_head(stream).map_err(|err| format!("No response: {err}"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "Invalid HTTP response".to_string())?;
    let length = header_value(&head, "content-length").and_then(|v| v.parse::<usize>().ok());
    let mut body = Vec::new();
    match length {
        Some(length) => {
            body.resize(length, 0);
            stream
                .read_exact(&mut body)
                .map_err(|err| format!("Truncated response: {err}"))?;
        }
        None => {
            stream
                .read_to_end(&mut body)
                .map_err(|err| format!("Truncated response: {err}"))?;
        }
    }
    Ok((status, body))
}

fn write_head(
    stream: &mut dyn Stream,
    method: &str,
    path: &str,
    host: &str,
    length: Option<u64>,
    json: bool,
) -> io::Result<()> {
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {host}\r\n")?;
    write!(stream, "Accept: application/json\r\nConnection: close\r\n")?;
    if json {
        write!(stream, "Content-Type: application/json\r\n")?;
    }
    if let Some(length) = length {
        write!(stream, "Content-Length: {length}\r\n")?;
    }
    stream.write_all(b"\r\n")
}

/// One request, no keep-alive: connection per call keeps the code simple and
/// transfers are local, where the handshake costs nothing worth saving.
fn request_json(
    device: &Device,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let (mut stream, _) = connect(device)?;
    write_head(
        &mut *stream,
        method,
        path,
        &device.address(),
        body.map(|b| b.len() as u64),
        true,
    )
    .map_err(|err| err.to_string())?;
    if let Some(body) = body {
        stream.write_all(body).map_err(|err| err.to_string())?;
    }
    stream.flush().map_err(|err| err.to_string())?;
    read_response(&mut *stream)
}

fn upload_file(
    device: &Device,
    path: &str,
    file: &OutgoingFile,
    progress: &mut dyn FnMut(u64),
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let (mut stream, _) = connect(device)?;
    write_head(
        &mut *stream,
        "POST",
        path,
        &device.address(),
        Some(file.size),
        false,
    )
    .map_err(|err| err.to_string())?;
    let mut source = std::fs::File::open(&file.path)
        .map_err(|err| format!("Cannot read {}: {err}", file.path.display()))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Transfer cancelled".into());
        }
        let read = source
            .read(&mut buffer)
            .map_err(|err| format!("Cannot read {}: {err}", file.path.display()))?;
        if read == 0 {
            break;
        }
        stream
            .write_all(&buffer[..read])
            .map_err(|err| format!("Connection lost: {err}"))?;
        done += read as u64;
        progress(done);
    }
    stream.flush().map_err(|err| err.to_string())?;
    let (status, _) = read_response(&mut *stream)?;
    match status {
        200 | 204 => Ok(()),
        401 => Err("The PIN is wrong or expired".into()),
        403 => Err("The device rejected this file".into()),
        409 => Err("The device is busy with another transfer".into()),
        422 => Err("The device reported a checksum mismatch".into()),
        other => Err(format!("The device answered HTTP {other}")),
    }
}

fn cancel_session(device: &Device, session: &str) {
    let path = format!("/api/localsend/v2/cancel?sessionId={}", url_encode(session));
    let _ = request_json(device, "POST", &path, None);
}

/* ----------------------------- incoming side ----------------------------- */

fn handle_connection(inner: Arc<Inner>, tcp: TcpStream, from: SocketAddr) {
    let _ = tcp.set_nodelay(true);
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(120)));
    let Ok(connection) = rustls::ServerConnection::new(inner.tls.clone()) else {
        return;
    };
    let mut stream = rustls::StreamOwned::new(connection, tcp);
    let head = match read_head(&mut stream) {
        Ok(head) if !head.is_empty() => head,
        _ => return,
    };
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = split_target(&target);

    let outcome = match (method.as_str(), path.as_str()) {
        ("GET", "/api/localsend/v2/info") => (200, inner.device_json(false)),
        ("POST", "/api/localsend/v2/register") => match read_body(&mut stream, &head) {
            Ok(body) => {
                if let Ok(mut device) = serde_json::from_slice::<Device>(&body) {
                    device.host = from.ip().to_string();
                    if device.port == 0 {
                        device.port = from.port();
                    }
                    if device.protocol.is_empty() {
                        device.protocol = "https".to_string();
                    }
                    inner.remember(&device);
                }
                (200, inner.device_json(false))
            }
            Err(err) => (400, error_json(&err.to_string())),
        },
        ("POST", "/api/localsend/v2/prepare-upload") => match read_body(&mut stream, &head) {
            Ok(body) => prepare_upload(&inner, &body, from),
            Err(err) => (400, error_json(&err.to_string())),
        },
        ("POST", "/api/localsend/v2/upload") => {
            match upload_body(&inner, &mut stream, &head, &query) {
                Ok(()) => (200, Vec::new()),
                Err((status, message)) => (status, error_json(&message)),
            }
        }
        ("POST", "/api/localsend/v2/cancel") => {
            if let Some(session) = query.get("sessionId") {
                cancel_incoming(&inner, session);
            }
            (200, Vec::new())
        }
        _ => (404, error_json("Unknown endpoint")),
    };
    respond(&mut stream, outcome.0, &outcome.1);
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            params.insert(key.to_string(), url_decode(value));
        }
    }
    (path.to_string(), params)
}

fn read_body(stream: &mut dyn Stream, head: &str) -> Result<Vec<u8>, io::Error> {
    let length = header_value(head, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > 8 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Body too large",
        ));
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut body)?;
    }
    Ok(body)
}

fn respond(stream: &mut dyn Stream, status: u16, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn error_json(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "message": message })).unwrap_or_default()
}

fn prepare_upload(inner: &Arc<Inner>, body: &[u8], from: SocketAddr) -> (u16, Vec<u8>) {
    let Ok(request) = serde_json::from_slice::<PrepareRequest>(body) else {
        return (400, error_json("Invalid body"));
    };
    let mut device = request.info;
    device.host = from.ip().to_string();
    if device.port == 0 {
        device.port = from.port();
    }
    inner.remember(&device);

    if request.files.is_empty() {
        return (204, Vec::new());
    }
    let total: u64 = request.files.values().map(|file| file.size).sum();
    let trusted = inner
        .trusted
        .lock()
        .unwrap()
        .iter()
        .any(|fingerprint| fingerprint.eq_ignore_ascii_case(&device.fingerprint));
    let auto = inner.auto_accept.load(Ordering::Relaxed) && trusted;
    let accepted = if auto {
        true
    } else {
        let (tx, rx) = async_channel::bounded::<bool>(1);
        let timeout_sender = tx.clone();
        let incoming = Incoming {
            from: device.label(),
            device: device.clone(),
            files: request
                .files
                .values()
                .map(|file| IncomingFile {
                    name: friendly_name(&file.file_name),
                    size: file.size,
                })
                .collect(),
            total_bytes: total,
            decide: tx,
        };
        let _ = inner.events.try_send(Event::Incoming(incoming));
        // The dialog answers within a minute; silence means decline, so a
        // forgotten request never pins a connection thread forever.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(90));
            let _ = timeout_sender.try_send(false);
        });
        match rx.recv_blocking() {
            Ok(answer) => answer,
            Err(_) => false,
        }
    };
    if !accepted {
        return (403, error_json("Declined"));
    }

    let dir = inner.receive_dir.lock().unwrap().clone();
    let mut session_files = Vec::new();
    let mut tokens = serde_json::Map::new();
    for (id, file) in &request.files {
        let name = safe_relative(&file.file_name);
        let path = unique_path(&dir.join(&name));
        let token = random_token();
        tokens.insert(id.clone(), serde_json::Value::String(token.clone()));
        session_files.push(SessionFile {
            id: id.clone(),
            path,
            token,
            sha256: file.sha256.clone(),
            size: file.size,
            done: false,
        });
    }
    let session_id = random_token();
    inner.sessions.lock().unwrap().insert(
        session_id.clone(),
        Session {
            from: device.label(),
            files: session_files,
        },
    );
    let body = serde_json::json!({ "sessionId": session_id, "files": tokens });
    (200, serde_json::to_vec(&body).unwrap_or_default())
}

/// Decodes `Transfer-Encoding: chunked`, how LocalSend peers send file
/// contents: the body is streamed from disk, so it never has a length the
/// sender could put in a `Content-Length` header.
struct ChunkedStream<'a> {
    stream: &'a mut dyn Stream,
    /// Bytes left in the chunk being read.
    remaining: u64,
    /// Set once the terminating zero-length chunk and its trailers are read.
    finished: bool,
}

impl<'a> ChunkedStream<'a> {
    fn new(stream: &'a mut dyn Stream) -> Self {
        Self {
            stream,
            remaining: 0,
            finished: false,
        }
    }

    /// One CRLF-terminated line of the chunk framing.
    fn read_line(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::with_capacity(32);
        let mut byte = [0u8; 1];
        while line.len() <= 1024 {
            match self.stream.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    line.push(byte[0]);
                    if line.ends_with(b"\r\n") {
                        return Ok(line);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid chunk framing",
        ))
    }
}

impl Read for ChunkedStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        if self.remaining == 0 {
            let line = String::from_utf8_lossy(&self.read_line()?).into_owned();
            let size = line
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let size = u64::from_str_radix(&size, 16).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size")
            })?;
            if size == 0 {
                // Trailer section, terminated by an empty line.
                loop {
                    let trailer = self.read_line()?;
                    if trailer == b"\r\n" || trailer.is_empty() {
                        break;
                    }
                }
                self.finished = true;
                return Ok(0);
            }
            self.remaining = size;
        }

        let want = (buffer.len() as u64).min(self.remaining) as usize;
        let read = self.stream.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated chunk",
            ));
        }
        self.remaining -= read as u64;
        if self.remaining == 0 {
            let mut terminator = [0u8; 2];
            self.stream.read_exact(&mut terminator)?;
            if &terminator != b"\r\n" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing chunk terminator",
                ));
            }
        }
        Ok(read)
    }
}

fn upload_body(
    inner: &Arc<Inner>,
    stream: &mut dyn Stream,
    head: &str,
    query: &HashMap<String, String>,
) -> Result<(), (u16, String)> {
    let session_id = query
        .get("sessionId")
        .ok_or((400, "Missing sessionId".into()))?;
    let file_id = query.get("fileId").ok_or((400, "Missing fileId".into()))?;
    let token = query.get("token").ok_or((400, "Missing token".into()))?;
    let chunked = header_value(head, "transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let length = header_value(head, "content-length").and_then(|value| value.parse::<u64>().ok());
    if !chunked && length.is_none() {
        return Err((400, "Missing Content-Length".into()));
    }

    let (path, sha, size, from_alias) = {
        let mut sessions = inner.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(session_id.as_str()) else {
            return Err((409, "Unknown session".into()));
        };
        let Some(file) = session.files.iter_mut().find(|f| &f.id == file_id) else {
            return Err((400, "Unknown file".into()));
        };
        if &file.token != token {
            return Err((403, "Invalid token".into()));
        }
        (
            file.path.clone(),
            file.sha256.clone(),
            file.size,
            session.from.clone(),
        )
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| (500, err.to_string()))?;
    }
    let mut target = std::fs::File::create(&path).map_err(|err| (500, err.to_string()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut reader: Box<dyn Read + '_> = if chunked {
        Box::new(ChunkedStream::new(stream))
    } else {
        Box::new(stream.take(length.unwrap_or(0)))
    };
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| (500, err.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        target
            .write_all(&buffer[..read])
            .map_err(|err| (500, err.to_string()))?;
        done += read as u64;
    }
    if done != size {
        return Err((400, "Truncated upload".into()));
    }
    if let Some(expected) = sha {
        if !expected.is_empty() && hex(&hasher.finalize()) != expected.to_lowercase() {
            let _ = std::fs::remove_file(&path);
            return Err((422, "Checksum mismatch".into()));
        }
    }

    let finished = {
        let mut sessions = inner.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(session_id.as_str()) else {
            return Err((409, "Unknown session".into()));
        };
        if let Some(file) = session.files.iter_mut().find(|f| &f.id == file_id) {
            file.done = true;
        }
        session.files.iter().all(|file| file.done)
    };
    if finished {
        let session = inner.sessions.lock().unwrap().remove(session_id.as_str());
        if let Some(session) = session {
            let files = session.files.iter().map(|file| file.path.clone()).collect();
            let _ = inner.events.try_send(Event::Saved {
                from: from_alias,
                files,
            });
        }
    }
    Ok(())
}

fn cancel_incoming(inner: &Arc<Inner>, session_id: &str) {
    let session = inner.sessions.lock().unwrap().remove(session_id);
    if let Some(session) = session {
        for file in session.files {
            if file.done {
                let _ = std::fs::remove_file(&file.path);
            }
        }
    }
}

/* -------------------------------- helpers -------------------------------- */

struct OutgoingFile {
    path: PathBuf,
    /// Name on the wire; folders keep their relative path with `/`.
    name: String,
    size: u64,
    mime: String,
}

#[derive(Deserialize)]
struct PrepareRequest {
    info: Device,
    #[serde(default)]
    files: HashMap<String, PrepareFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareFile {
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    sha256: Option<String>,
}

fn collect_files(paths: &[PathBuf]) -> Vec<OutgoingFile> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            collect_dir(path, path, &mut out);
        } else if let Some(file) = outgoing_file(path, &friendly_name(&file_name(path))) {
            out.push(file);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn collect_dir(root: &Path, dir: &Path, out: &mut Vec<OutgoingFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_dir(root, &path, out);
        } else if kind.is_file() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let name = format!(
                "{}/{}",
                file_name(root),
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            );
            if let Some(file) = outgoing_file(&path, &name) {
                out.push(file);
            }
        }
    }
}

fn outgoing_file(path: &Path, name: &str) -> Option<OutgoingFile> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(OutgoingFile {
        path: path.to_path_buf(),
        name: name.to_string(),
        size: meta.len(),
        mime: guess_mime(name),
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn friendly_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

/// Only the last path segment is used in dialogs; the full relative path is
/// kept for the transfer itself.
fn safe_relative(name: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        let clean: String = part.chars().filter(|c| *c != '\0').collect();
        if !clean.is_empty() {
            out.push(clean);
        }
    }
    if out.as_os_str().is_empty() {
        out.push("received-file");
    }
    out
}

/// `photo (2).jpg` style unique name, so an incoming transfer never overwrites
/// something already in the folder.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for index in 2..10_000 {
        let candidate = parent.join(format!("{stem} ({index}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// The file's modification time as RFC 3339 in UTC, the format the protocol
/// defines for `metadata.modified`; receivers apply it to the saved file.
fn modified_iso(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let seconds = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(unix_to_rfc3339(seconds as i64))
}

fn unix_to_rfc3339(timestamp: i64) -> String {
    let days = timestamp.div_euclid(86_400);
    let second_of_day = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a calendar
/// date, without pulling in a date-time dependency.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        year_of_era + era * 400 + i64::from(month <= 2),
        month as u32,
        day as u32,
    )
}

fn split_address(address: &str) -> Result<(String, u16), String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("Enter a device address".into());
    }
    match address.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port
                .parse::<u16>()
                .map_err(|_| "The port must be a number".to_string())?;
            Ok((host.trim_matches(['[', ']']).to_string(), port))
        }
        _ => Ok((address.to_string(), DEFAULT_PORT)),
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Small extension-based MIME table: the receiver uses it for previews, and it
/// avoids touching the theme's MIME database from a worker thread.
fn guess_mime(name: &str) -> String {
    let lower = name.to_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    let mime = match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "apk" => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn config_in(dir: &Path) -> Config {
        let mut config = Config::default();
        config.transfer_enabled = true;
        // Port 0 lets the OS pick, so tests never clash with a running LocalSend.
        config.transfer_port = 0;
        config.transfer_receive_dir = Some(dir.join("inbox").to_string_lossy().to_string());
        config
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nook-localsend-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn send_and_receive_round_trip() {
        let root = temp_dir("roundtrip");
        let send_dir = root.join("send");
        std::fs::create_dir_all(send_dir.join("folder")).unwrap();
        std::fs::write(send_dir.join("photo.txt"), b"holiday photo bytes").unwrap();
        std::fs::write(send_dir.join("folder/nested.txt"), b"nested").unwrap();

        let (receiving_events, incoming_rx) = async_channel::unbounded::<Event>();
        let receiver = Service::start(&config_in(&root), receiving_events).unwrap();

        // Answer the incoming request the way the dialog does.
        {
            let incoming_rx = incoming_rx.clone();
            std::thread::spawn(move || {
                if let Ok(event) = incoming_rx.recv_blocking() {
                    if let Event::Incoming(request) = event {
                        assert_eq!(request.files.len(), 2);
                        assert_eq!(request.total_bytes, 19 + 6);
                        request.accept();
                    }
                }
            });
        }

        let (sending_events, sending_rx) = async_channel::unbounded::<Event>();
        let sender = Service::start(&config_in(&root.join("sender")), sending_events).unwrap();

        let device = Device {
            alias: "Receiver".into(),
            version: PROTOCOL_VERSION.into(),
            device_model: Some("Nook".into()),
            device_type: Some("desktop".into()),
            fingerprint: receiver.fingerprint(),
            port: receiver.port(),
            protocol: "https".into(),
            download: false,
            announce: false,
            host: "127.0.0.1".into(),
        };

        let cancelled = AtomicBool::new(false);
        let sent = sender
            .send_files(
                &device,
                &[send_dir.join("photo.txt"), send_dir.join("folder")],
                None,
                &|_progress| {},
                &cancelled,
            )
            .expect("send succeeds");
        assert_eq!(sent, 2);

        // Wait for the receiver to report the finished transfer.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "receiver never reported the files");
            match sending_rx.try_recv() {
                Ok(_) => {}
                Err(_) => {}
            }
            match incoming_rx.try_recv() {
                Ok(Event::Saved { files, .. }) => {
                    assert_eq!(files.len(), 2);
                    let inbox = root.join("inbox");
                    assert_eq!(
                        std::fs::read(inbox.join("photo.txt")).unwrap(),
                        b"holiday photo bytes"
                    );
                    assert_eq!(
                        std::fs::read(inbox.join("folder/nested.txt")).unwrap(),
                        b"nested"
                    );
                    break;
                }
                Ok(_) => {}
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => panic!("receiver event loop closed"),
            }
        }
        receiver.shutdown();
        sender.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A LocalSend receiver (1.18 and newer) runs mTLS: it asks for a client
    /// certificate and aborts the handshake with `certificate_required` when
    /// the sender presents none. This verifier does the same.
    struct RequireClientCert {
        provider: rustls::crypto::CryptoProvider,
        seen: Arc<Mutex<Option<String>>>,
    }

    impl std::fmt::Debug for RequireClientCert {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("RequireClientCert")
        }
    }

    impl rustls::server::danger::ClientCertVerifier for RequireClientCert {
        fn offer_client_auth(&self) -> bool {
            true
        }

        fn client_auth_mandatory(&self) -> bool {
            true
        }

        fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
            &[]
        }

        fn verify_client_cert(
            &self,
            cert: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
            *self.seen.lock().unwrap() = Some(fingerprint_hex(&Sha256::digest(cert.as_ref())));
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// Talking to a peer that demands a client certificate must succeed, and
    /// the certificate Nook presents must be the one it announced, in the
    /// uppercase hex LocalSend computes for a certificate fingerprint.
    #[test]
    fn tls_client_presents_the_announced_certificate() {
        let root = temp_dir("mtls");
        let (events, _rx) = async_channel::unbounded::<Event>();
        let service = Service::start(&config_in(&root), events).unwrap();

        let server_cert =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let seen = Arc::new(Mutex::new(None));
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(Arc::new(RequireClientCert {
                provider: rustls::crypto::ring::default_provider(),
                seen: seen.clone(),
            }))
            .with_single_cert(
                vec![server_cert.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(
                        server_cert.key_pair.serialize_der(),
                    ),
                ),
            )
            .unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let reply = serde_json::to_vec(&service.device()).unwrap();
        let answer = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut stream =
                rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(config)).unwrap(), tcp);
            let head = read_head(&mut stream).expect("a readable request");
            assert!(
                head.starts_with("POST /api/localsend/v2/register"),
                "unexpected request: {head}"
            );
            respond(&mut stream, 200, &reply);
        });

        let device = Device {
            alias: "Receiver".into(),
            version: PROTOCOL_VERSION.into(),
            device_model: None,
            device_type: Some("desktop".into()),
            fingerprint: service.fingerprint(),
            port: address.port(),
            protocol: "https".into(),
            download: false,
            announce: false,
            host: "127.0.0.1".into(),
        };
        let (status, _) = request_json(
            &device,
            "POST",
            "/api/localsend/v2/register",
            Some(&serde_json::to_vec(&service.device()).unwrap()),
        )
        .expect("a peer demanding a client certificate must accept us");
        assert_eq!(status, 200);
        answer.join().unwrap();

        let presented = seen.lock().unwrap().clone();
        assert_eq!(
            presented.as_deref(),
            Some(service.fingerprint().as_str()),
            "the presented certificate must be the announced one, in uppercase hex"
        );
        assert!(
            service.fingerprint().chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)),
            "fingerprints are uppercase hex: {}",
            service.fingerprint()
        );
        service.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Temporary end-to-end check against LocalSend's own server binary
    /// (`target/probe/lsrecv`). Skipped unless LOCALSEND_TEST_PORT is set.
    #[test]
    fn send_to_a_real_localsend_receiver() {
        let Ok(port) = std::env::var("LOCALSEND_TEST_PORT") else {
            eprintln!("LOCALSEND_TEST_PORT not set, skipping");
            return;
        };
        let port: u16 = port.parse().unwrap();
        let dest = PathBuf::from(
            std::env::var("LOCALSEND_TEST_DEST").unwrap_or_else(|_| "received".into()),
        );

        let root = temp_dir("realpeer");
        let send_dir = root.join("send");
        std::fs::create_dir_all(send_dir.join("folder")).unwrap();
        std::fs::write(send_dir.join("hello.txt"), b"hello from nook\n").unwrap();
        std::fs::write(send_dir.join("folder/note.md"), b"# nested note\n").unwrap();

        let (events, _rx) = async_channel::unbounded::<Event>();
        let sender = Service::start(&config_in(&root), events).unwrap();
        eprintln!("nook fingerprint: {}", sender.fingerprint());

        let device = sender
            .add_by_address(&format!("127.0.0.1:{port}"))
            .expect("register with the real LocalSend server over mTLS");
        eprintln!("discovered: {} {} {}", device.label(), device.protocol, device.fingerprint);

        let seen = Mutex::new(Vec::<String>::new());
        let cancelled = AtomicBool::new(false);
        let sent = sender
            .send_files(
                &device,
                &[send_dir.join("hello.txt"), send_dir.join("folder")],
                None,
                &|progress| {
                    if progress.file_done == progress.file_size {
                        seen.lock().unwrap().push(format!(
                            "{} {}/{}",
                            progress.file, progress.index, progress.count
                        ));
                    }
                },
                &cancelled,
            )
            .expect("sending to a real LocalSend receiver succeeds");
        assert_eq!(sent, 2, "both files were accepted");
        eprintln!("progress: {:?}", seen.lock().unwrap());

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if std::fs::read(dest.join("hello.txt")).is_ok()
                && std::fs::read(dest.join("folder/note.md")).is_ok()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "files never arrived in {}",
                dest.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(std::fs::read(dest.join("hello.txt")).unwrap(), b"hello from nook\n");
        assert_eq!(std::fs::read(dest.join("folder/note.md")).unwrap(), b"# nested note\n");
        sender.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Temporary end-to-end check: LocalSend's own v2 client sends to Nook's
    /// receiver. Skipped unless LOCALSEND_TEST_LISTEN_PORT is set.
    #[test]
    fn receive_from_a_real_localsend_sender() {
        let Ok(port) = std::env::var("LOCALSEND_TEST_LISTEN_PORT") else {
            eprintln!("LOCALSEND_TEST_LISTEN_PORT not set, skipping");
            return;
        };
        let port: u16 = port.parse().unwrap();

        let root = temp_dir("realsender");
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("from-phone.txt"), b"sent by LocalSend's own client\n").unwrap();

        let mut config = config_in(&root);
        config.transfer_port = port;
        let (events, rx) = async_channel::unbounded::<Event>();
        let receiver = Service::start(&config, events).unwrap();
        assert_eq!(receiver.port(), port, "the test port must be free");

        // Accept the prompt, the way the dialog does.
        std::thread::spawn(move || {
            while let Ok(event) = rx.recv_blocking() {
                match event {
                    Event::Incoming(request) => {
                        eprintln!(
                            "incoming from {} files={:?}",
                            request.from,
                            request
                                .files
                                .iter()
                                .map(|file| file.name.clone())
                                .collect::<Vec<_>>()
                        );
                        request.accept();
                    }
                    Event::Saved { from, files } => {
                        eprintln!("saved from {from}: {} files", files.len())
                    }
                    Event::Notice(notice) => eprintln!("notice: {notice}"),
                }
            }
        });

        let output = std::process::Command::new("target/probe/lsrecv/target/debug/lssend")
            .args([
                "127.0.0.1",
                &port.to_string(),
                source.join("from-phone.txt").to_str().unwrap(),
            ])
            .output()
            .expect("lssend must run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("lssend stdout:\n{stdout}\nlssend stderr:\n{stderr}");
        assert!(output.status.success(), "lssend failed: {stderr}");
        assert!(stdout.contains("DONE"), "the transfer finished: {stdout}");

        assert_eq!(
            std::fs::read(root.join("inbox/from-phone.txt")).unwrap(),
            b"sent by LocalSend's own client\n"
        );
        receiver.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn modification_times_are_rfc3339() {
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_to_rfc3339(1_600_000_000), "2020-09-13T12:26:40Z");
        assert_eq!(unix_to_rfc3339(1_767_225_600), "2026-01-01T00:00:00Z");
        // A leap day, and a negative (pre-1970) timestamp.
        assert_eq!(unix_to_rfc3339(1_582_934_400), "2020-02-29T00:00:00Z");
        assert_eq!(unix_to_rfc3339(-86_400), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn incoming_names_stay_inside_the_receive_folder() {
        assert_eq!(
            safe_relative("../../etc/passwd"),
            PathBuf::from("etc/passwd")
        );
        assert_eq!(
            safe_relative("/abs/path.txt"),
            PathBuf::from("abs/path.txt")
        );
        assert_eq!(
            safe_relative("folder/sub/a.txt"),
            PathBuf::from("folder/sub/a.txt")
        );
        assert_eq!(safe_relative(".."), PathBuf::from("received-file"));
        assert_eq!(safe_relative(""), PathBuf::from("received-file"));
    }

    #[test]
    fn existing_files_get_a_unique_name() {
        let dir = temp_dir("unique");
        let file = dir.join("report.pdf");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(unique_path(&file), dir.join("report (2).pdf"));
        std::fs::write(dir.join("report (2).pdf"), b"x").unwrap();
        assert_eq!(unique_path(&file), dir.join("report (3).pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_keep_their_relative_paths() {
        let dir = temp_dir("collect");
        std::fs::create_dir_all(dir.join("album/2026")).unwrap();
        std::fs::write(dir.join("album/2026/a.png"), b"x").unwrap();
        let files = collect_files(&[dir.join("album")]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "album/2026/a.png");
        assert_eq!(files[0].mime, "image/png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn addresses_accept_host_and_port() {
        assert_eq!(
            split_address("192.168.1.20").unwrap(),
            ("192.168.1.20".to_string(), DEFAULT_PORT)
        );
        assert_eq!(
            split_address("phone.local:1234").unwrap(),
            ("phone.local".to_string(), 1234)
        );
        assert!(split_address("  ").is_err());
        assert!(split_address("host:not-a-port").is_err());
    }

    #[test]
    fn query_values_round_trip() {
        for value in ["session id", "a&b=c", "ümlaut", "token-1_2.3~"] {
            assert_eq!(url_decode(&url_encode(value)), value);
        }
    }
}
