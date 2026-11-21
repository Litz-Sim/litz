//! A deterministic TLS 1.3 endpoint and the trust store that goes with it.
//!
//! The router terminates TLS for its HTTPS and secure-WebSocket services, and
//! `Config::client` gives a test, a host or a wasm build the client half of the
//! same stack. Both sides come from `rustls`, with three substitutions:
//!
//! - the random source is a ChaCha20 generator seeded with a fixed key instead
//!   of the operating system's,
//! - the X25519 key exchange draws its private key from that generator too, so
//!   the key shares are reproducible, and
//! - the clock is a fixed instant, so a certificate's validity is a property of
//!   the simulation and not of the day it runs.
//!
//! The cryptography comes from `rustls-rustcrypto`, which is pure Rust and
//! builds for `wasm32-unknown-unknown`; the native build uses the same crate,
//! so a handshake over HTTPS produces the same bytes in the browser and on the
//! command line. `TLS13_CHACHA20_POLY1305_SHA256` is the one offered suite:
//! fewer code paths to keep identical, and no AES implementation to carry into
//! wasm. Within one router, successive connections draw fresh bytes, as they
//! should.
//!
//! The certificate chain is fixed and shipped with the crate: a simulated CA
//! (`certs/ca.pem`) that signs `api.litz.dev` for a century, so a guest whose
//! clock starts at zero and one that has synced over SNTP both accept it.
//! Trust is real — a client that does not load [`ca_der`] rejects the server —
//! and the private key is the simulated server's, not a shared secret.
//!
//! This is not a security boundary. It is a wire-format boundary: firmware
//! runs its own TLS implementation, and the simulator answers it with the same
//! records a host would.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::ClientConnection;
use rustls::crypto::{
    ActiveKeyExchange, CryptoProvider, GetRandomFailed, SecureRandom, SharedSecret,
    SupportedKxGroup,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, Error, NamedGroup, RootCertStore, ServerConfig, ServerConnection};

/// The simulated CA's certificate, PEM, for a firmware's trust store.
pub const CA_PEM: &str = include_str!("../certs/ca.pem");
/// The server certificate, PEM.
pub const SERVER_PEM: &str = include_str!("../certs/server.pem");
/// The name the server certificate is issued for.
pub const SERVER_NAME: &str = "api.litz.dev";

const CA_DER: &[u8] = include_bytes!("../certs/ca.der");
const SERVER_DER: &[u8] = include_bytes!("../certs/server.der");
const SERVER_KEY_DER: &[u8] = include_bytes!("../certs/server.key.der");

/// The last second of 2099, which every shipped certificate outlives.
const NOW: UnixTime = UnixTime::since_unix_epoch(Duration::from_secs(4_102_444_799));

/// The CA's DER encoding, for a caller that builds its own root store.
pub fn ca_der() -> Vec<u8> {
    CA_DER.to_vec()
}

fn ca_certificate() -> CertificateDer<'static> {
    CertificateDer::from(CA_DER.to_vec())
}

/// A ChaCha20 keystream generator. The standard quarter-round construction,
/// seeded from a compile-time constant so the bytes are a pure function of how
/// many were drawn before them.
struct Seeded {
    state: Mutex<Seed>,
}

struct Seed {
    key: [u32; 8],
    counter: u64,
    block: [u8; 64],
    used: usize,
}

impl fmt::Debug for Seeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seeded(chacha20)")
    }
}

impl Seeded {
    fn new(key: [u8; 32]) -> Self {
        let mut words = [0u32; 8];
        for (index, chunk) in key.chunks_exact(4).enumerate() {
            words[index] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Self {
            state: Mutex::new(Seed {
                key: words,
                counter: 0,
                block: [0; 64],
                used: 64,
            }),
        }
    }
}

fn quarter(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(7);
}

fn chacha_block(key: &[u32; 8], counter: u64, out: &mut [u8; 64]) {
    let state = [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        key[0],
        key[1],
        key[2],
        key[3],
        key[4],
        key[5],
        key[6],
        key[7],
        counter as u32,
        (counter >> 32) as u32,
        0,
        0,
    ];
    let mut working = state;
    for _ in 0..10 {
        quarter(&mut working, 0, 4, 8, 12);
        quarter(&mut working, 1, 5, 9, 13);
        quarter(&mut working, 2, 6, 10, 14);
        quarter(&mut working, 3, 7, 11, 15);
        quarter(&mut working, 0, 5, 10, 15);
        quarter(&mut working, 1, 6, 11, 12);
        quarter(&mut working, 2, 7, 8, 13);
        quarter(&mut working, 3, 4, 9, 14);
    }
    for (index, word) in working.iter_mut().enumerate() {
        *word = word.wrapping_add(state[index]);
    }
    for (index, word) in working.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
}

impl SecureRandom for Seeded {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        let mut state = self.state.lock().expect("the generator is not poisoned");
        for slot in buf {
            if state.used == 64 {
                let key = state.key;
                let counter = state.counter;
                let mut block = [0u8; 64];
                chacha_block(&key, counter, &mut block);
                state.block = block;
                state.counter = state.counter.wrapping_add(1);
                state.used = 0;
            }
            *slot = state.block[state.used];
            state.used += 1;
        }
        Ok(())
    }
}

/// A clock that never moves.
#[derive(Debug)]
struct FixedTime;

impl rustls::time_provider::TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(NOW)
    }
}

/// X25519 key exchange over the same fixed generator.
///
/// The provider's own group draws from the host random source, which would make
/// every handshake different. This group exists so the client key share and the
/// server key share come from the seeded stream too.
struct X25519Group {
    random: &'static Seeded,
}

impl fmt::Debug for X25519Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("X25519Group(seeded)")
    }
}

struct X25519Exchange {
    secret: x25519_dalek::StaticSecret,
    public: [u8; 32],
}

impl ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let peer: [u8; 32] = peer
            .try_into()
            .map_err(|_| Error::General("an X25519 key share is 32 bytes".into()))?;
        let shared = self
            .secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        Ok(SharedSecret::from(&shared.to_bytes()[..]))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

impl SupportedKxGroup for X25519Group {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let mut bytes = [0u8; 32];
        self.random
            .fill(&mut bytes)
            .map_err(|_| Error::General("the seeded generator failed".into()))?;
        let secret = x25519_dalek::StaticSecret::from(bytes);
        let public = *x25519_dalek::PublicKey::from(&secret).as_bytes();
        Ok(Box::new(X25519Exchange { secret, public }))
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// A provider whose random source, key exchange and suite list are fixed, using
/// pure-Rust cryptography on every target.
fn provider(seed: u8) -> Arc<CryptoProvider> {
    let random: &'static Seeded = Box::leak(Box::new(Seeded::new([seed; 32])));
    let group: &'static X25519Group = Box::leak(Box::new(X25519Group { random }));
    let mut provider = rustls_rustcrypto::provider();
    provider.cipher_suites = vec![rustls_rustcrypto::TLS13_CHACHA20_POLY1305_SHA256];
    provider.kx_groups = vec![group];
    provider.secure_random = random;
    Arc::new(provider)
}

fn display(error: impl fmt::Display) -> String {
    format!("TLS: {error}")
}

/// The two configurations one router or one host uses. A fresh `Config` starts
/// both generators from their seeds, which is what makes a run reproducible;
/// connections within the config continue the stream.
pub struct Config {
    server: Arc<ServerConfig>,
    client: Arc<ClientConfig>,
}

impl Config {
    pub fn new() -> Result<Self, String> {
        let cert = CertificateDer::from(SERVER_DER.to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY_DER.to_vec()));
        let server = ServerConfig::builder_with_provider(provider(0x5e))
            .with_safe_default_protocol_versions()
            .expect("the provider's cipher suites and groups line up")
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .map_err(display)?;

        let mut roots = RootCertStore::empty();
        roots
            .add(ca_certificate())
            .expect("the shipped CA is a valid certificate");
        let mut client = ClientConfig::builder_with_provider(provider(0xc1))
            .with_safe_default_protocol_versions()
            .expect("the provider's cipher suites and groups line up")
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.time_provider = Arc::new(FixedTime);

        Ok(Self {
            server: Arc::new(server),
            client: Arc::new(client),
        })
    }

    /// The server's TLS record layer for one connection.
    pub fn server(&self) -> Result<Server, String> {
        let conn = ServerConnection::new(Arc::clone(&self.server)).map_err(display)?;
        Ok(Server {
            conn,
            plain: Vec::new(),
            failed: false,
        })
    }

    /// The client's TLS record layer for one connection.
    pub fn client(&self, name: &str) -> Result<Client, String> {
        let name = ServerName::try_from(name.to_string()).map_err(display)?;
        let conn = ClientConnection::new(Arc::clone(&self.client), name).map_err(display)?;
        Ok(Client {
            conn,
            plain: Vec::new(),
            failed: false,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new().expect("the shipped TLS configuration is valid")
    }
}

/// The server half of a TLS connection, driven by the router's flow.
pub struct Server {
    conn: ServerConnection,
    plain: Vec<u8>,
    failed: bool,
}

impl Server {
    /// Accept ciphertext from the guest and collect whatever is readable.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), String> {
        let mut rest = data;
        while !rest.is_empty() {
            match self.conn.read_tls(&mut rest) {
                Ok(0) => break,
                Ok(_) => {
                    if let Err(error) = self.conn.process_new_packets() {
                        self.failed = true;
                        return Err(display(error));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.failed = true;
                    return Err(display(error));
                }
            }
        }
        loop {
            let mut buffer = [0u8; 4096];
            match self.conn.reader().read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => self.plain.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.failed = true;
                    return Err(display(error));
                }
            }
        }
        Ok(())
    }

    /// Ciphertext the handshake or the application produced.
    pub fn output(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while self.conn.wants_write() {
            if self.conn.write_tls(&mut out).is_err() {
                break;
            }
        }
        out
    }

    /// Plaintext the guest sent, drained.
    pub fn take_plain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.plain)
    }

    pub fn handshaking(&self) -> bool {
        self.conn.is_handshaking()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Encrypt application data. An empty result means the handshake has not
    /// finished and the data must wait.
    pub fn send(&mut self, plain: &[u8]) -> Vec<u8> {
        if self.handshaking() {
            return Vec::new();
        }
        if self.conn.writer().write_all(plain).is_err() {
            self.failed = true;
            return Vec::new();
        }
        self.output()
    }
}

/// The client half, used by tests and by a host function that gives a wasm chip
/// a TLS client without reimplementing a cipher suite there.
pub struct Client {
    conn: ClientConnection,
    plain: Vec<u8>,
    failed: bool,
}

impl Client {
    pub fn feed(&mut self, data: &[u8]) -> Result<(), String> {
        let mut rest = data;
        while !rest.is_empty() {
            match self.conn.read_tls(&mut rest) {
                Ok(0) => break,
                Ok(_) => {
                    if let Err(error) = self.conn.process_new_packets() {
                        self.failed = true;
                        return Err(display(error));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.failed = true;
                    return Err(display(error));
                }
            }
        }
        loop {
            let mut buffer = [0u8; 4096];
            match self.conn.reader().read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => self.plain.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.failed = true;
                    return Err(display(error));
                }
            }
        }
        Ok(())
    }

    pub fn output(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while self.conn.wants_write() {
            if self.conn.write_tls(&mut out).is_err() {
                break;
            }
        }
        out
    }

    pub fn take_plain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.plain)
    }

    pub fn handshaking(&self) -> bool {
        self.conn.is_handshaking()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn send(&mut self, plain: &[u8]) -> Vec<u8> {
        if self.conn.writer().write_all(plain).is_err() {
            self.failed = true;
            return Vec::new();
        }
        self.output()
    }
}

/// `getrandom`'s custom backend for `wasm32-unknown-unknown`.
///
/// The provider's default RNG and its default X25519 group are both replaced
/// above, so nothing in the simulator should reach this. It exists because
/// `rustls-rustcrypto` links `getrandom`, and getrandom's own wasm backend
/// would add JavaScript imports to a module the page instantiates with an
/// empty import object. Reporting "unsupported" makes an accidental call fail
/// instead of quietly drawing entropy from somewhere the simulation cannot
/// reproduce.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    _dest: *mut u8,
    _len: usize,
) -> Result<(), getrandom::Error> {
    Err(getrandom::Error::UNSUPPORTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive both ends until the client has sent `request` and seen the
    /// server's answer. Returns what crossed the wire and what each side read.
    fn exchange(request: &[u8], answer: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let config = Config::new().unwrap();
        let mut server = config.server().unwrap();
        let mut client = config.client(SERVER_NAME).unwrap();
        let mut to_server = client.output();
        let mut to_client = Vec::new();
        let mut wire = Vec::new();
        let mut server_plain = Vec::new();
        let mut client_plain = Vec::new();
        let mut sent = false;
        for _ in 0..32 {
            if !to_server.is_empty() {
                wire.extend_from_slice(&to_server);
                server.feed(&to_server).unwrap();
                to_server.clear();
                server_plain.extend(server.take_plain());
                to_client.extend(server.output());
            }
            if !to_client.is_empty() {
                wire.extend_from_slice(&to_client);
                client.feed(&to_client).unwrap();
                to_client.clear();
                client_plain.extend(client.take_plain());
                to_server.extend(client.output());
            }
            if !client.handshaking() && !sent {
                to_server.extend(client.send(request));
                sent = true;
            }
            if server_plain.len() >= request.len() {
                to_client.extend(server.send(answer));
            }
            if client_plain.len() >= answer.len() {
                break;
            }
        }
        (wire, server_plain, client_plain)
    }

    #[test]
    fn a_handshake_carries_a_request_and_an_answer() {
        let (wire, server_plain, client_plain) =
            exchange(b"GET / HTTP/1.1\r\n\r\n", b"HTTP/1.1 200 OK\r\n\r\n");
        assert!(!wire.is_empty());
        assert_eq!(server_plain, b"GET / HTTP/1.1\r\n\r\n");
        assert_eq!(client_plain, b"HTTP/1.1 200 OK\r\n\r\n");
    }

    #[test]
    fn two_configs_exchange_the_same_bytes_for_the_same_conversation() {
        let first = exchange(b"hello", b"world");
        let second = exchange(b"hello", b"world");
        assert_eq!(first.0, second.0, "the handshake is reproducible");
        assert_eq!(first.2, second.2);
    }

    #[test]
    fn the_shipped_chain_parses_and_the_server_starts() {
        assert!(CA_PEM.contains("BEGIN CERTIFICATE"));
        assert!(SERVER_PEM.contains("api.litz.dev") || SERVER_PEM.contains("BEGIN CERTIFICATE"));
        let config = Config::new().expect("the configuration is valid");
        assert!(config.server().is_ok());
        assert!(config.client(SERVER_NAME).is_ok());
    }

    #[test]
    fn the_connection_uses_the_seeded_chacha20_suite() {
        let config = Config::new().unwrap();
        let mut server = config.server().unwrap();
        let mut client = config.client(SERVER_NAME).unwrap();
        let mut to_server = client.output();
        let mut to_client = Vec::new();
        for _ in 0..16 {
            if !to_server.is_empty() {
                server.feed(&to_server).unwrap();
                to_server.clear();
                to_client.extend(server.output());
            }
            if !to_client.is_empty() {
                client.feed(&to_client).unwrap();
                to_client.clear();
                to_server.extend(client.output());
            }
            if !client.handshaking() && !server.handshaking() {
                break;
            }
        }
        assert!(!client.handshaking());
        assert_eq!(
            server
                .conn
                .negotiated_cipher_suite()
                .expect("a suite was negotiated")
                .suite(),
            rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        );
        assert_eq!(
            client
                .conn
                .negotiated_cipher_suite()
                .expect("a suite was negotiated")
                .suite(),
            rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        );
    }
}
