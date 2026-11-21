//! A deterministic IP network behind a simulated access point.
//!
//! A chip's radio backend puts an access point on the air and hands the guest
//! an address, a gateway and a DNS server. That is enough for the link to come
//! up, and it is where a simulator usually stops: the first TCP connection past
//! the gateway either fails or leaves for a real host socket, which makes the
//! test slow, flaky and dependent on the machine it runs on.
//!
//! This crate is the far side of that gateway. A [`Router`] holds a name table
//! and a list of [`Service`]s and answers flows byte for byte, in order, with
//! no clock and no randomness: a firmware image that joins the simulated
//! access point, resolves a name and fetches a page sees the same bytes on
//! every run. The router is transport-agnostic — a radio backend adapts its own
//! seam to these calls — so the same network serves an ESP32, a C6, a wasm MCU
//! or anything else with a TCP stack.
//!
//! DHCP, ARP, ICMP and SNTP belong to the radio backend that owns the link,
//! because they describe the guest's subnet rather than the network beyond it.
//! What this crate adds is the part past the gateway: DNS answers, TCP and UDP.

pub mod capture;
pub mod mqtt;
pub mod tls;
pub mod websocket;

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

/// The address the router answers on. Radio backends that put a DNS server in
/// the DHCP lease point names at this address.
pub const HOST_IP: [u8; 4] = [10, 0, 2, 3];

/// The gateway the backend hands the guest.
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

/// A joinable access point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub ssid: String,
    pub psk: Option<String>,
    pub channel: u8,
    /// A locally administered BSSID when absent, derived by the radio backend
    /// so a scan sees the same address on every run.
    pub bssid: Option<[u8; 6]>,
    /// The network's clock, seconds since 1970, for SNTP answers.
    pub epoch_s: Option<u64>,
}

impl AccessPoint {
    pub fn open(ssid: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            psk: None,
            channel: 6,
            bssid: None,
            epoch_s: None,
        }
    }

    pub fn with_psk(mut self, psk: impl Into<String>) -> Self {
        self.psk = Some(psk.into());
        self
    }

    pub fn on_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }
}

/// A network a scan reports but the guest cannot join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyNetwork {
    pub ssid: String,
    pub channel: u8,
    pub rssi: i8,
    pub secure: bool,
}

impl NearbyNetwork {
    pub fn new(ssid: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            channel: 1,
            rssi: -60,
            secure: true,
        }
    }
}

/// What the router answers on one port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Service {
    /// Accept the connection, write `reply`, then close. With `after_request`
    /// the reply waits until the guest has sent at least one byte, which is
    /// what an HTTP client expects; without it the reply comes first.
    Tcp {
        port: u16,
        reply: Vec<u8>,
        after_request: bool,
    },
    /// Accept the connection and write every received byte back.
    TcpEcho { port: u16 },
    /// Send every received datagram back to its sender.
    UdpEcho { port: u16 },
    /// An MQTT broker: connect, subscribe, publish and retain, answered from
    /// the simulated network. The usual port is 1883.
    Mqtt { port: u16 },
    /// A WebSocket server: the HTTP upgrade, then text and binary echo.
    WebSocket { port: u16 },
    /// Any of the above behind TLS 1.3. The outer port is the one the guest
    /// connects to; the inner service keeps its own behaviour.
    Tls { port: u16, inner: Box<Service> },
}

/// What a flow does once its bytes are plaintext.
#[derive(Clone, Debug)]
enum App {
    Fixed { reply: Vec<u8>, after_request: bool },
    Echo,
    Mqtt,
    WebSocket,
}

impl Service {
    pub fn port(&self) -> u16 {
        match self {
            Service::Tcp { port, .. }
            | Service::TcpEcho { port }
            | Service::UdpEcho { port }
            | Service::Mqtt { port }
            | Service::WebSocket { port }
            | Service::Tls { port, .. } => *port,
        }
    }

    fn app(&self) -> App {
        match self {
            Service::Tcp {
                reply,
                after_request,
                ..
            } => App::Fixed {
                reply: reply.clone(),
                after_request: *after_request,
            },
            Service::TcpEcho { .. } | Service::UdpEcho { .. } => App::Echo,
            Service::Mqtt { .. } => App::Mqtt,
            Service::WebSocket { .. } => App::WebSocket,
            Service::Tls { inner, .. } => inner.app(),
        }
    }

    fn is_tls(&self) -> bool {
        matches!(self, Service::Tls { .. })
    }

    /// The service answers TCP, as opposed to UDP only.
    fn accepts_tcp(&self) -> bool {
        match self {
            Service::UdpEcho { .. } => false,
            Service::Tls { inner, .. } => inner.accepts_tcp(),
            _ => true,
        }
    }

    /// A TCP server whose every request is answered with `body` as
    /// `text/plain`.
    pub fn http(port: u16, body: impl Into<String>) -> Self {
        let body = body.into();
        let reply = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        Service::Tcp {
            port,
            reply: reply.into_bytes(),
            after_request: true,
        }
    }

    /// An MQTT broker on `port`.
    pub fn mqtt(port: u16) -> Self {
        Service::Mqtt { port }
    }

    /// A WebSocket echo server on `port`.
    pub fn websocket(port: u16) -> Self {
        Service::WebSocket { port }
    }

    /// `inner` behind TLS, reachable on `port`.
    pub fn tls(port: u16, inner: Service) -> Self {
        Service::Tls {
            port,
            inner: Box::new(inner),
        }
    }

    /// `body` as `text/plain` over TLS, on `port` (usually 443).
    pub fn https(port: u16, body: impl Into<String>) -> Self {
        Service::tls(port, Service::http(0, body))
    }

    /// A WebSocket echo server over TLS, on `port` (usually 443).
    pub fn wss(port: u16) -> Self {
        Service::tls(port, Service::websocket(0))
    }
}

/// The network behind an access point: what there is to join, what a scan sees,
/// and what the router answers.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// Joinable access points. The first one is the network's own; the guest
    /// joins one at a time.
    pub access_points: Vec<AccessPoint>,
    /// Networks a scan reports but cannot join.
    pub nearby: Vec<NearbyNetwork>,
    /// What the router answers on its ports.
    pub services: Vec<Service>,
    /// Names the router resolves to its own address. A name not listed here is
    /// NXDOMAIN unless `resolve_unknown` is on.
    pub names: BTreeMap<String, [u8; 4]>,
    /// Resolve every name to the router's address.
    pub resolve_unknown: bool,
    /// Where a packet capture is written, when one was asked for. The router
    /// fills it as it answers, so the caller can keep the handle and write the
    /// pcap after the run.
    pub capture: Option<Rc<RefCell<capture::Capture>>>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            access_points: Vec::new(),
            nearby: Vec::new(),
            services: Vec::new(),
            names: BTreeMap::new(),
            resolve_unknown: true,
            capture: None,
        }
    }
}

impl NetworkConfig {
    /// One open access point and no services: a link with nothing to talk to.
    pub fn access_point(ssid: impl Into<String>) -> Self {
        Self {
            access_points: vec![AccessPoint::open(ssid)],
            ..Self::default()
        }
    }

    /// A network a firmware can talk to: one access point, an HTTP server on
    /// port 80, a TCP echo on port 7 and an MQTT broker on 1883.
    pub fn for_testing(ssid: impl Into<String>) -> Self {
        Self {
            access_points: vec![AccessPoint::open(ssid)],
            services: vec![
                Service::http(80, "hello from litz\n"),
                Service::TcpEcho { port: 7 },
                Service::mqtt(1883),
                Service::websocket(81),
                Service::https(443, "hello over tls\n"),
                Service::wss(8443),
            ],
            ..Self::default()
        }
    }

    /// Add a service, replacing the one already on that port. A port carries
    /// one service, so an answer never depends on the order the table was
    /// built in.
    pub fn with_service(mut self, service: Service) -> Self {
        self.services.retain(|s| s.port() != service.port());
        self.services.push(service);
        self
    }

    /// Replace the default services with HTTP on 80, a TCP echo on 7 and an
    /// MQTT broker on 1883 when none are listed, which is how a project that
    /// only names an SSID still has something to connect to.
    pub fn with_default_services(mut self) -> Self {
        if self.services.is_empty() {
            self.services = vec![
                Service::http(80, "hello from litz\n"),
                Service::TcpEcho { port: 7 },
                Service::mqtt(1883),
                Service::websocket(81),
                Service::https(443, "hello over tls\n"),
                Service::wss(8443),
            ];
        }
        self
    }

    /// A router with this configuration and fresh state.
    pub fn router(&self) -> Router {
        Router::new(self)
    }
}

/// An answer to a name lookup, as the radio backend reports it upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// IPv4 answers and their TTL in seconds (may be empty).
    Addrs(Vec<[u8; 4]>, u32),
    /// DNS rcode: 2 SERVFAIL, 3 NXDOMAIN.
    Failed(u8),
}

pub type FlowId = u32;

/// The ephemeral port a flow uses in a capture: the private range, stable for
/// a given flow id.
fn local_port(id: FlowId) -> u16 {
    49152 + (id % 16384) as u16
}

struct Flow {
    service: usize,
    /// What the flow does once its bytes are plaintext.
    app: App,
    /// The TLS record layer, when the service terminates TLS.
    tls: Option<tls::Server>,
    /// The WebSocket handshake and frames, when the service speaks it.
    ws: Option<websocket::Server>,
    /// The local (station) port this flow uses, for the capture.
    local_port: u16,
    /// TCP sequence numbers as a capture sees them.
    client_seq: u32,
    server_seq: u32,
    /// Bytes still to send to the guest, already encrypted when TLS is on.
    out: VecDeque<u8>,
    /// Plaintext from the guest that does not yet form a whole MQTT packet.
    inbox: Vec<u8>,
    /// The guest has sent something, so an `after_request` reply may start.
    requested: bool,
    open: bool,
    /// The fixed reply is waiting for the TLS handshake to finish.
    pending_reply: bool,
    /// The fixed reply has been written to the flow.
    replied: bool,
    /// The whole answer has been read; a further read returns EOF.
    eof: bool,
}

impl Flow {
    fn new(index: usize, service: &Service, local_port: u16) -> Self {
        let app = service.app();
        let mut flow = Flow {
            service: index,
            app: app.clone(),
            tls: None,
            ws: None,
            local_port,
            client_seq: 1,
            server_seq: 1,
            out: VecDeque::new(),
            inbox: Vec::new(),
            requested: false,
            open: true,
            pending_reply: false,
            replied: false,
            eof: false,
        };
        match app {
            App::Fixed {
                reply,
                after_request,
            } => {
                flow.requested = !after_request;
                if flow.requested {
                    if service.is_tls() {
                        flow.pending_reply = true;
                    } else {
                        flow.out.extend(reply);
                        flow.replied = true;
                        flow.eof = flow.out.is_empty();
                    }
                }
            }
            App::Echo | App::Mqtt => flow.requested = true,
            App::WebSocket => flow.ws = Some(websocket::Server::new()),
        }
        flow
    }
}

/// The state behind the gateway: flows, name lookups and the service table.
///
/// It is driven by the radio backend on the guest's behalf, so every method is
/// non-blocking and every answer is immediate. Reads on a flow with nothing
/// buffered return `None` (the backend reports that as "try again"), and a
/// read after a fixed reply has been delivered returns `Some(0)` (end of file).
pub struct Router {
    services: Vec<Service>,
    names: BTreeMap<String, [u8; 4]>,
    resolve_unknown: bool,
    flows: BTreeMap<FlowId, Flow>,
    lookups: BTreeMap<u32, Answer>,
    /// Queries waiting for their answer, kept for the capture.
    queries: BTreeMap<u32, (String, u16)>,
    mqtt: mqtt::Broker,
    /// The server side of the TLS stack. It owns the seeded generators, so
    /// two routers built the same way produce the same handshake bytes.
    tls: tls::Config,
    capture: Option<Rc<RefCell<capture::Capture>>>,
    now_ns: u64,
    station: [u8; 4],
    next_flow: FlowId,
    next_lookup: u32,
}

impl Router {
    pub fn new(config: &NetworkConfig) -> Self {
        Self {
            services: config.services.clone(),
            names: config.names.clone(),
            resolve_unknown: config.resolve_unknown,
            flows: BTreeMap::new(),
            lookups: BTreeMap::new(),
            queries: BTreeMap::new(),
            mqtt: mqtt::Broker::new(),
            tls: tls::Config::new().expect("the shipped TLS configuration is valid"),
            capture: config.capture.clone(),
            now_ns: 0,
            station: [10, 0, 2, 15],
            next_flow: 1,
            next_lookup: 1,
        }
    }

    /// Advance the clock a capture timestamps with. A backend calls this
    /// before each network operation; `--pcap` turns the result into a file.
    pub fn set_now(&mut self, ns: u64) {
        self.now_ns = ns;
    }

    /// The lease the station received. Frames use it as their source address.
    pub fn set_station(&mut self, station: [u8; 4]) {
        self.station = station;
        if let Some(capture) = &self.capture {
            capture.borrow_mut().set_station(station);
        }
    }

    /// The capture buffer, when the network was configured to record.
    pub fn capture(&self) -> Option<Rc<RefCell<capture::Capture>>> {
        self.capture.clone()
    }

    /// A TLS client that trusts the simulated CA and shares this router's
    /// seeded stream, for a test or a wasm host function that terminates TLS
    /// on the guest's behalf.
    pub fn tls_client(&self, name: &str) -> Result<tls::Client, String> {
        self.tls.client(name)
    }

    fn service(&self, port: u16) -> Option<(usize, &Service)> {
        self.services
            .iter()
            .enumerate()
            .find(|(_, s)| s.port() == port)
    }

    fn allocate(&mut self) -> FlowId {
        let id = self.next_flow;
        self.next_flow += 1;
        id
    }

    /// Begin a TCP connection to `port`. The id is returned even when the port
    /// has no service, so the caller can report a refusal rather than a hang;
    /// [`Router::connected`] says which happened.
    pub fn tcp_connect(&mut self, port: u16) -> FlowId {
        let id = self.allocate();
        let found = self
            .service(port)
            .map(|(index, service)| (index, service.clone()));
        let accepted = match found {
            Some((index, service)) if service.accepts_tcp() => {
                let mut flow = Flow::new(index, &service, local_port(id));
                let mut accepted = true;
                if service.is_tls() {
                    match self.tls.server() {
                        Ok(server) => flow.tls = Some(server),
                        Err(_) => {
                            // A target without a TLS backend (the web build)
                            // refuses the port rather than pretending to
                            // handshake.
                            accepted = false;
                            flow.open = false;
                            flow.eof = true;
                        }
                    }
                }
                self.flows.insert(id, flow);
                accepted
            }
            _ => {
                let mut flow = Flow::new(usize::MAX, &Service::TcpEcho { port }, local_port(id));
                flow.open = false;
                flow.requested = false;
                self.flows.insert(id, flow);
                false
            }
        };
        self.record_tcp(id, true, capture::SYN, &[]);
        if accepted {
            self.record_tcp(id, false, capture::SYN | capture::ACK, &[]);
        } else {
            self.record_tcp(id, false, capture::RST | capture::ACK, &[]);
        }
        id
    }

    /// Write one TCP segment of `id` into the capture, then advance that
    /// side's sequence number.
    fn record_tcp(&mut self, id: FlowId, from_station: bool, flags: u8, payload: &[u8]) {
        let Some(capture) = self.capture.clone() else {
            return;
        };
        let Some(flow) = self.flows.get(&id) else {
            return;
        };
        let port = self
            .services
            .get(flow.service)
            .map(Service::port)
            .unwrap_or(0);
        let (src, dst, seq, ack) = if from_station {
            (flow.local_port, port, flow.client_seq, flow.server_seq)
        } else {
            (port, flow.local_port, flow.server_seq, flow.client_seq)
        };
        capture.borrow_mut().tcp(
            self.now_ns,
            from_station,
            src,
            dst,
            seq,
            ack,
            flags,
            payload,
        );
        let advance = payload.len() as u32 + u32::from(flags & (capture::SYN | capture::FIN) != 0);
        if let Some(flow) = self.flows.get_mut(&id) {
            if from_station {
                flow.client_seq = flow.client_seq.wrapping_add(advance);
            } else {
                flow.server_seq = flow.server_seq.wrapping_add(advance);
            }
        }
    }

    /// Whether the connection was accepted.
    pub fn connected(&self, id: FlowId) -> bool {
        self.flows.get(&id).is_some_and(|flow| flow.open)
    }

    /// Read from the router. `None` means nothing is buffered yet;
    /// `Some(0)` means the peer has finished sending.
    pub fn tcp_read(&mut self, id: FlowId, buf: &mut [u8]) -> Option<usize> {
        let flow = self.flows.get_mut(&id)?;
        if flow.out.is_empty() {
            return if flow.eof { Some(0) } else { None };
        }
        let n = buf.len().min(flow.out.len());
        for slot in &mut buf[..n] {
            *slot = flow.out.pop_front().expect("checked the length");
        }
        if flow.out.is_empty() && !flow.tls.as_ref().is_some_and(|tls| tls.handshaking()) {
            let fixed_done = matches!(flow.app, App::Fixed { .. }) && flow.replied;
            let ws_done = flow.ws.as_ref().is_some_and(|ws| ws.closed());
            if fixed_done || ws_done {
                flow.eof = true;
            }
        }
        let payload = buf[..n].to_vec();
        self.record_tcp(id, false, capture::PSH | capture::ACK, &payload);
        Some(n)
    }

    /// Write from the guest. A fixed reply starts once the first byte has
    /// arrived when the service waits for a request; an MQTT broker consumes
    /// whole packets and answers through the same flow.
    pub fn tcp_write(&mut self, id: FlowId, data: &[u8]) -> Option<usize> {
        if !self.flows.get(&id)?.open {
            return Some(data.len());
        }
        self.record_tcp(id, true, capture::PSH | capture::ACK, data);

        // The TLS record layer turns ciphertext into plaintext; everything
        // after this point works on the plaintext and encrypts its answers.
        let mut wire = Vec::new();
        let mut plain = Some(data.to_vec());
        let mut completed = true;
        {
            let flow = self.flows.get_mut(&id)?;
            if let Some(tls) = &mut flow.tls {
                if tls.feed(data).is_err() {
                    flow.open = false;
                    flow.eof = true;
                    return Some(data.len());
                }
                wire = tls.output();
                completed = !tls.handshaking();
                plain = if completed {
                    let bytes = tls.take_plain();
                    Some(bytes)
                } else {
                    tls.take_plain();
                    None
                };
            }
        }
        {
            let flow = self.flows.get_mut(&id)?;
            flow.out.extend(wire);
            if completed && flow.pending_reply {
                flow.pending_reply = false;
                flow.replied = true;
                let reply = match &flow.app {
                    App::Fixed { reply, .. } => reply.clone(),
                    _ => Vec::new(),
                };
                let wire = match &mut flow.tls {
                    Some(tls) => {
                        let mut out = tls.send(&reply);
                        out.extend(tls.output());
                        out
                    }
                    None => reply,
                };
                flow.out.extend(wire);
            }
        }
        if let Some(plain) = plain {
            self.app_write(id, &plain);
        }
        Some(data.len())
    }

    /// What the service does with plaintext from the guest.
    fn app_write(&mut self, id: FlowId, plain: &[u8]) {
        let Some(app) = self.flows.get(&id).map(|flow| flow.app.clone()) else {
            return;
        };
        // A TLS handshake finishing is not an application request: the last
        // handshake flight arrives with no plaintext, and a request/response
        // service must not answer it.
        if plain.is_empty() && matches!(app, App::Fixed { .. }) {
            return;
        }
        match app {
            App::Fixed { reply, .. } => {
                let requested = self.flows.get(&id).is_some_and(|flow| flow.requested);
                if !requested {
                    if let Some(flow) = self.flows.get_mut(&id) {
                        flow.requested = true;
                        flow.replied = true;
                    }
                    self.push(id, &reply);
                }
            }
            App::Echo => self.push(id, plain),
            App::Mqtt => self.mqtt_write(id, plain),
            App::WebSocket => {
                let output = {
                    let Some(flow) = self.flows.get_mut(&id) else {
                        return;
                    };
                    let Some(ws) = flow.ws.as_mut() else {
                        return;
                    };
                    ws.feed(plain);
                    ws.take_output()
                };
                self.push(id, &output);
            }
        }
    }

    fn mqtt_write(&mut self, id: FlowId, data: &[u8]) {
        let mut reply = Vec::new();
        let mut deliveries = Vec::new();
        let closed = {
            let Some(flow) = self.flows.get_mut(&id) else {
                return;
            };
            flow.inbox.extend_from_slice(data);
            self.mqtt
                .feed(id, &mut flow.inbox, &mut reply, &mut deliveries)
                == mqtt::Event::Closed
        };
        self.push(id, &reply);
        self.deliver_mqtt(deliveries);
        if closed {
            if let Some(flow) = self.flows.get_mut(&id) {
                flow.open = false;
                flow.eof = true;
            }
            let published = self.mqtt.remove(id);
            self.deliver_mqtt(published);
        }
    }

    /// Encrypt `plain` for the flow when it terminates TLS and append the
    /// result to its output.
    fn push(&mut self, id: FlowId, plain: &[u8]) {
        let Some(flow) = self.flows.get_mut(&id) else {
            return;
        };
        if plain.is_empty() {
            return;
        }
        let wire = match &mut flow.tls {
            Some(tls) => {
                let mut out = tls.send(plain);
                out.extend(tls.output());
                out
            }
            None => plain.to_vec(),
        };
        flow.out.extend(wire);
    }

    /// The guest sent FIN. A fixed reply continues to be readable; an echo
    /// returns EOF once it has been drained.
    pub fn tcp_shutdown(&mut self, id: FlowId) {
        if let Some(flow) = self.flows.get_mut(&id) {
            if !flow.out.is_empty() || !matches!(flow.app, App::Echo) {
                return;
            }
            flow.eof = true;
        }
    }

    pub fn tcp_close(&mut self, id: FlowId) {
        self.record_tcp(id, true, capture::FIN | capture::ACK, &[]);
        let published = self.mqtt.remove(id);
        self.flows.remove(&id);
        self.deliver_mqtt(published);
    }

    /// Write will publications the broker produced into the flows they
    /// reached.
    fn deliver_mqtt(&mut self, packets: Vec<(u32, Vec<u8>)>) {
        for (to, packet) in packets {
            self.push(to, &packet);
        }
    }

    /// Open a UDP flow to `port`, when a UDP service answers there.
    pub fn udp_open(&mut self, port: u16) -> Option<FlowId> {
        let (index, _) = self.service(port)?;
        if !matches!(self.services[index], Service::UdpEcho { .. }) {
            return None;
        }
        let id = self.allocate();
        self.flows.insert(
            id,
            Flow {
                service: index,
                app: App::Echo,
                tls: None,
                ws: None,
                local_port: local_port(id),
                client_seq: 1,
                server_seq: 1,
                out: VecDeque::new(),
                inbox: Vec::new(),
                requested: true,
                open: true,
                pending_reply: false,
                replied: false,
                eof: false,
            },
        );
        Some(id)
    }

    pub fn udp_send(&mut self, id: FlowId, data: &[u8]) -> Option<usize> {
        let flow = self.flows.get_mut(&id)?;
        flow.out.extend(data.iter().copied());
        let port = self
            .services
            .get(flow.service)
            .map(Service::port)
            .unwrap_or(0);
        let local = flow.local_port;
        if let Some(capture) = self.capture.clone() {
            capture
                .borrow_mut()
                .udp(self.now_ns, true, local, port, data);
        }
        Some(data.len())
    }

    /// Read a datagram. `None` means nothing has come back yet.
    pub fn udp_recv(&mut self, id: FlowId, buf: &mut [u8]) -> Option<usize> {
        let flow = self.flows.get_mut(&id)?;
        if flow.out.is_empty() {
            return None;
        }
        let n = buf.len().min(flow.out.len());
        for slot in &mut buf[..n] {
            *slot = flow.out.pop_front().expect("checked the length");
        }
        let payload = buf[..n].to_vec();
        let port = self
            .services
            .get(flow.service)
            .map(Service::port)
            .unwrap_or(0);
        let local = flow.local_port;
        if let Some(capture) = self.capture.clone() {
            capture
                .borrow_mut()
                .udp(self.now_ns, false, port, local, &payload);
        }
        Some(n)
    }

    pub fn udp_close(&mut self, id: FlowId) {
        self.flows.remove(&id);
    }

    /// Begin a name lookup. The answer is taken by [`Router::resolved`]; an
    /// empty id means the query type is one the router leaves to the backend.
    pub fn resolve(&mut self, name: &str, qtype: u16) -> Option<u32> {
        if qtype != 1 && qtype != 28 {
            return None;
        }
        let answer = match self.names.get(name) {
            Some(ip) => Answer::Addrs(vec![*ip], 60),
            // An AAAA answer for a host known only as IPv4 is empty, not a
            // failure: the guest then uses the A record.
            None if qtype == 28 => Answer::Addrs(Vec::new(), 60),
            None if self.resolve_unknown => Answer::Addrs(vec![HOST_IP], 60),
            None => Answer::Failed(3),
        };
        let id = self.next_lookup;
        self.next_lookup += 1;
        self.lookups.insert(id, answer);
        self.queries.insert(id, (name.to_string(), qtype));
        if let Some(capture) = self.capture.clone() {
            capture
                .borrow_mut()
                .dns_query(self.now_ns, id as u16, name, qtype);
        }
        Some(id)
    }

    /// The oldest finished lookup, if any.
    pub fn resolved(&mut self) -> Option<(u32, Answer)> {
        let id = *self.lookups.keys().next()?;
        let answer = self.lookups.remove(&id)?;
        if let (Some(capture), Some((name, qtype))) =
            (self.capture.clone(), self.queries.remove(&id))
        {
            capture
                .borrow_mut()
                .dns_answer(self.now_ns, id as u16, &name, qtype, &answer);
        }
        Some((id, answer))
    }

    /// How many TCP flows the router still holds, for a report.
    pub fn open_flows(&self) -> usize {
        self.flows.values().filter(|flow| flow.open).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tcp_service_waits_for_the_request_then_replies_and_closes() {
        let config = NetworkConfig::access_point("litz").with_service(Service::http(80, "ok"));
        let mut router = config.router();
        let id = router.tcp_connect(80);
        assert!(router.connected(id));
        let mut buf = [0u8; 64];
        assert_eq!(
            router.tcp_read(id, &mut buf),
            None,
            "nothing before a request"
        );
        router.tcp_write(id, b"GET / HTTP/1.0\r\n\r\n");
        let mut reply = Vec::new();
        while let Some(n) = router.tcp_read(id, &mut buf) {
            if n == 0 {
                break;
            }
            reply.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8(reply).unwrap();
        assert!(text.starts_with("HTTP/1.0 200 OK"), "{text}");
        assert!(text.ends_with("ok"), "{text}");
    }

    #[test]
    fn a_port_with_no_service_is_refused() {
        let mut router = NetworkConfig::access_point("litz").router();
        let id = router.tcp_connect(9);
        assert!(!router.connected(id));
    }

    #[test]
    fn a_tcp_echo_service_returns_what_it_receives() {
        let config = NetworkConfig::access_point("litz").with_service(Service::TcpEcho { port: 7 });
        let mut router = config.router();
        let id = router.tcp_connect(7);
        router.tcp_write(id, b"ping");
        let mut buf = [0u8; 8];
        let n = router.tcp_read(id, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
    }

    #[test]
    fn a_udp_echo_service_returns_the_datagram() {
        let config =
            NetworkConfig::access_point("litz").with_service(Service::UdpEcho { port: 5000 });
        let mut router = config.router();
        let id = router.udp_open(5000).unwrap();
        router.udp_send(id, b"pulse");
        let mut buf = [0u8; 8];
        let n = router.udp_recv(id, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"pulse");
        assert!(router.udp_open(5001).is_none());
    }

    #[test]
    fn names_resolve_to_the_router_when_unknown_names_are_allowed() {
        let mut router = NetworkConfig::default().router();
        let id = router.resolve("example.com", 1).unwrap();
        assert_eq!(
            router.resolved(),
            Some((id, Answer::Addrs(vec![HOST_IP], 60)))
        );
    }

    #[test]
    fn a_strict_router_answers_nxdomain_for_an_unknown_name() {
        let mut config = NetworkConfig {
            resolve_unknown: false,
            ..Default::default()
        };
        config.names.insert("litz.local".into(), HOST_IP);
        let mut router = config.router();
        let known = router.resolve("litz.local", 1).unwrap();
        assert_eq!(
            router.resolved(),
            Some((known, Answer::Addrs(vec![HOST_IP], 60)))
        );
        let id = router.resolve("nowhere.invalid", 1).unwrap();
        assert_eq!(router.resolved(), Some((id, Answer::Failed(3))));
    }

    #[test]
    fn one_service_per_port_and_the_last_one_wins() {
        let config = NetworkConfig::access_point("litz")
            .with_service(Service::http(80, "first"))
            .with_service(Service::http(80, "second"));
        assert_eq!(config.services.len(), 1);
        assert_eq!(config.services[0].port(), 80);
    }

    /// Run the TLS handshake over a router flow, returning the flow, the
    /// client and whatever plaintext the server sent during it.
    fn handshake(router: &mut Router, port: u16) -> (FlowId, tls::Client, Vec<u8>) {
        let id = router.tcp_connect(port);
        let mut client = router.tls_client(tls::SERVER_NAME).unwrap();
        let mut plain = Vec::new();
        let mut pending = client.output();
        for _ in 0..32 {
            if !pending.is_empty() {
                router.tcp_write(id, &pending);
                pending.clear();
            }
            let mut buf = [0u8; 4096];
            while let Some(n) = router.tcp_read(id, &mut buf) {
                if n == 0 {
                    break;
                }
                client.feed(&buf[..n]).unwrap();
            }
            plain.extend(client.take_plain());
            pending = client.output();
            if !client.handshaking() && pending.is_empty() {
                break;
            }
        }
        assert!(!client.handshaking(), "the handshake completes");
        (id, client, plain)
    }

    /// Send `plain` through the client and collect what comes back.
    fn exchange(
        router: &mut Router,
        id: FlowId,
        client: &mut tls::Client,
        plain: &[u8],
    ) -> Vec<u8> {
        let wire = client.send(plain);
        router.tcp_write(id, &wire);
        let mut answer = Vec::new();
        for _ in 0..32 {
            let mut buf = [0u8; 4096];
            while let Some(n) = router.tcp_read(id, &mut buf) {
                if n == 0 {
                    break;
                }
                client.feed(&buf[..n]).unwrap();
            }
            answer.extend(client.take_plain());
            let more = client.output();
            if more.is_empty() {
                break;
            }
            router.tcp_write(id, &more);
        }
        answer
    }

    #[test]
    fn https_answers_a_request_through_tls() {
        let config =
            NetworkConfig::access_point("litz").with_service(Service::https(443, "secret"));
        let mut router = config.router();
        let (id, mut client, _) = handshake(&mut router, 443);
        let answer = exchange(&mut router, id, &mut client, b"GET / HTTP/1.1\r\n\r\n");
        let text = String::from_utf8(answer).unwrap();
        assert!(text.starts_with("HTTP/1.0 200 OK"), "{text}");
        assert!(text.ends_with("secret"), "{text}");
    }

    #[test]
    fn mqtt_runs_over_tls() {
        let config =
            NetworkConfig::access_point("litz").with_service(Service::tls(8883, Service::mqtt(0)));
        let mut router = config.router();
        let (id, mut client, _) = handshake(&mut router, 8883);
        let connect = [
            0x10, 0x0c, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x02, 0x00, 0x3c, 0x00, 0x00,
        ];
        let answer = exchange(&mut router, id, &mut client, &connect);
        assert_eq!(answer[0], 0x20, "CONNACK");
        assert_eq!(answer[3], 0x00, "connection accepted");
    }

    #[test]
    fn a_client_rejects_a_name_the_certificate_does_not_cover() {
        let config =
            NetworkConfig::access_point("litz").with_service(Service::https(443, "secret"));
        let mut router = config.router();
        let id = router.tcp_connect(443);
        let mut client = router.tls_client("wrong.example").unwrap();
        let hello = client.output();
        router.tcp_write(id, &hello);
        let mut buf = [0u8; 4096];
        while let Some(n) = router.tcp_read(id, &mut buf) {
            if n == 0 || client.feed(&buf[..n]).is_err() {
                break;
            }
        }
        assert!(
            client.failed(),
            "the certificate names api.litz.dev, not the requested host"
        );
    }

    #[test]
    fn two_https_runs_capture_the_same_bytes() {
        fn run() -> Vec<u8> {
            let capture = Rc::new(RefCell::new(capture::Capture::new([10, 0, 2, 15])));
            let mut config =
                NetworkConfig::access_point("litz").with_service(Service::https(443, "ok"));
            config.capture = Some(Rc::clone(&capture));
            let mut router = config.router();
            router.set_now(1_000);
            let (id, mut client, _) = handshake(&mut router, 443);
            let answer = exchange(&mut router, id, &mut client, b"GET / HTTP/1.1\r\n\r\n");
            assert!(String::from_utf8_lossy(&answer).contains("ok"));
            router.set_now(2_000);
            router.tcp_close(id);
            capture.borrow().to_pcap()
        }
        let first = run();
        let second = run();
        assert_eq!(
            first, second,
            "the seeded TLS stack makes the capture reproducible"
        );
    }

    #[test]
    fn a_websocket_echoes_text_after_the_upgrade() {
        let config = NetworkConfig::access_point("litz").with_service(Service::websocket(80));
        let mut router = config.router();
        let id = router.tcp_connect(80);
        router.tcp_write(id, b"GET /chat HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n");
        let mut buf = [0u8; 512];
        let n = router.tcp_read(id, &mut buf).unwrap();
        let response = String::from_utf8(buf[..n].to_vec()).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 101 Switching Protocols"),
            "{response}"
        );
        router.tcp_write(id, &websocket::masked_frame(0x1, b"hi", [7, 7, 7, 7]));
        let mut out = Vec::new();
        while let Some(n) = router.tcp_read(id, &mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, websocket::frame(0x1, b"hi"));
    }

    #[test]
    fn a_websocket_over_tls_echoes_inside_the_record_layer() {
        let config = NetworkConfig::access_point("litz").with_service(Service::wss(443));
        let mut router = config.router();
        let (id, mut client, _) = handshake(&mut router, 443);
        let upgrade = b"GET /chat HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        let answer = exchange(&mut router, id, &mut client, upgrade);
        let text = String::from_utf8_lossy(&answer);
        assert!(text.starts_with("HTTP/1.1 101"), "{text}");
        let echo = exchange(
            &mut router,
            id,
            &mut client,
            &websocket::masked_frame(0x1, b"hi", [1, 3, 3, 7]),
        );
        assert_eq!(echo, websocket::frame(0x1, b"hi"));
    }

    #[test]
    fn a_router_capture_holds_the_handshake_the_payload_and_the_answer() {
        let capture = Rc::new(RefCell::new(capture::Capture::new([10, 0, 2, 15])));
        let mut config = NetworkConfig::access_point("litz").with_service(Service::http(80, "ok"));
        config.capture = Some(Rc::clone(&capture));
        let mut router = config.router();
        router.set_now(100);
        let id = router.tcp_connect(80);
        router.set_now(200);
        router.tcp_write(id, b"GET / HTTP/1.0\r\n\r\n");
        router.set_now(300);
        let mut buf = [0u8; 64];
        while let Some(n) = router.tcp_read(id, &mut buf) {
            if n == 0 {
                break;
            }
        }
        router.set_now(400);
        router.tcp_close(id);
        assert!(
            capture.borrow().frames() >= 5,
            "SYN, SYN-ACK, request, reply, FIN: {}",
            capture.borrow().frames()
        );
        let pcap = capture.borrow().to_pcap();
        assert!(pcap.windows(4).any(|w| w == [10, 0, 2, 15]));
        assert!(
            pcap.windows(15).any(|w| w == b"GET / HTTP/1.0\r"),
            "the request payload is in the capture"
        );
    }

    #[test]
    fn a_banner_service_replies_before_the_guest_speaks() {
        let config = NetworkConfig::access_point("litz").with_service(Service::Tcp {
            port: 9000,
            reply: b"welcome\n".to_vec(),
            after_request: false,
        });
        let mut router = config.router();
        let id = router.tcp_connect(9000);
        let mut buf = [0u8; 16];
        let n = router.tcp_read(id, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"welcome\n");
    }
}
