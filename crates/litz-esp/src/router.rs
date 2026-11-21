//! The ESP32 family's radio link plugged into Litz's simulated network.
//!
//! esp32sim owns the 802.11 MAC, the DHCP lease, ARP, ICMP and SNTP, and it
//! exposes one seam past the gateway: a `NatTransport`. This module implements
//! that seam by delegating to [`litz_net::Router`], the chip-independent network
//! every Litz board can carry, so the bytes a test sees are the same whether
//! they reach an ESP32, a C6, or a wasm MCU with its own radio.
//!
//! The adapter is deliberately thin: no service logic lives here. What it adds
//! is the translation between esp32sim's flow states and the router's answers,
//! including the one asymmetry — a port with no service is refused with a
//! reset rather than left to time out.

use std::cell::Cell;
use std::io;
use std::rc::Rc;

use esp_soc::nat::{Connect, FlowId, NatTransport, Resolve};
use litz_net::Answer;

/// The address esp32sim's DHCP lease hands the guest.
pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];

/// A [`NatTransport`] over a [`litz_net::Router`].
pub struct EspRouter {
    router: litz_net::Router,
    /// The chip's simulated time, shared with the adapter so a capture can
    /// timestamp frames. The NAT seam carries no clock of its own.
    clock: Rc<Cell<u64>>,
}

impl EspRouter {
    pub fn new(config: &litz_net::NetworkConfig) -> Self {
        Self::with_clock(config, Rc::new(Cell::new(0)))
    }

    pub fn with_clock(config: &litz_net::NetworkConfig, clock: Rc<Cell<u64>>) -> Self {
        let mut router = config.router();
        router.set_station(GUEST_IP);
        Self { router, clock }
    }

    /// Move the router's clock to the chip's current time before an answer is
    /// recorded.
    fn tick(&mut self) {
        let now = self.clock.get();
        self.router.set_now(now);
    }
}

fn io_error(kind: io::ErrorKind) -> io::Error {
    io::Error::from(kind)
}

impl NatTransport for EspRouter {
    fn tcp_connect(&mut self, dst: std::net::SocketAddrV4) -> Option<FlowId> {
        self.tick();
        Some(self.router.tcp_connect(dst.port()))
    }

    fn tcp_state(&mut self, id: FlowId) -> Connect {
        if self.router.connected(id) {
            Connect::Open
        } else {
            Connect::Failed(io::ErrorKind::ConnectionRefused)
        }
    }

    fn tcp_read(&mut self, id: FlowId, buf: &mut [u8]) -> io::Result<usize> {
        self.tick();
        match self.router.tcp_read(id, buf) {
            Some(n) => Ok(n),
            None => Err(io_error(io::ErrorKind::WouldBlock)),
        }
    }

    fn tcp_write(&mut self, id: FlowId, data: &[u8]) -> io::Result<usize> {
        self.tick();
        self.router
            .tcp_write(id, data)
            .ok_or_else(|| io_error(io::ErrorKind::NotConnected))
    }

    fn tcp_shutdown(&mut self, id: FlowId) {
        self.router.tcp_shutdown(id);
    }

    fn tcp_close(&mut self, id: FlowId, _abort: bool) {
        self.router.tcp_close(id);
    }

    fn udp_open(&mut self, dst: std::net::SocketAddrV4) -> io::Result<FlowId> {
        self.tick();
        self.router
            .udp_open(dst.port())
            .ok_or_else(|| io_error(io::ErrorKind::ConnectionRefused))
    }

    fn udp_send(&mut self, id: FlowId, data: &[u8]) -> io::Result<usize> {
        self.tick();
        self.router
            .udp_send(id, data)
            .ok_or_else(|| io_error(io::ErrorKind::NotConnected))
    }

    fn udp_recv(&mut self, id: FlowId, buf: &mut [u8]) -> io::Result<usize> {
        self.tick();
        match self.router.udp_recv(id, buf) {
            Some(n) => Ok(n),
            None => Err(io_error(io::ErrorKind::WouldBlock)),
        }
    }

    fn udp_close(&mut self, id: FlowId) {
        self.router.udp_close(id);
    }

    fn resolver(&self) -> [u8; 4] {
        litz_net::HOST_IP
    }

    fn resolve(&mut self, name: &str, qtype: u16) -> Option<u32> {
        self.tick();
        self.router.resolve(name, qtype)
    }

    fn resolved(&mut self) -> Option<(u32, Resolve)> {
        self.tick();
        let (id, answer) = self.router.resolved()?;
        let answer = match answer {
            Answer::Addrs(addrs, ttl) => Resolve::Addrs(addrs, ttl),
            Answer::Failed(rcode) => Resolve::Failed(rcode),
        };
        Some((id, answer))
    }
}
