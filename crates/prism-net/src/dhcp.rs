//! An embedded `DHCPv4` server for one switch bridge with a static lease table.
//!
//! It answers only for MACs in the table and stays silent for every other client, so several
//! nodes can serve the same L2 segment without conflicting offers.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use dhcproto::v4::{DhcpOption, Flags, HType, Message, MessageType, Opcode, OptionCode};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use ipnet::Ipv4Net;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::{Error, MacAddr, Result};

/// UDP port DHCP servers listen on (RFC 2131).
pub const SERVER_PORT: u16 = 67;
/// UDP port DHCP clients listen on; broadcast replies go here.
pub const CLIENT_PORT: u16 = 68;

/// A static lease and the options sent with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The address handed to the client.
    pub ip: Ipv4Addr,
    /// Length of the subnet mask sent as option 1; values above 32 are treated as 32.
    pub prefix_len: u8,
    /// Default gateway (option 3), normally the egress node's VIP.
    pub router: Option<Ipv4Addr>,
    /// DNS servers (option 6).
    pub dns: Vec<Ipv4Addr>,
    /// Search domain suffix (option 15).
    pub domain: Option<String>,
    /// Host name for the client to adopt (option 12).
    pub hostname: Option<String>,
    /// Interface MTU in bytes (option 26).
    pub mtu: Option<u16>,
    /// Classless static routes (option 121). When non-empty, a default route via `router` is
    /// added to it, because clients ignore option 3 when option 121 is present.
    pub routes: Vec<(Ipv4Net, Ipv4Addr)>,
    /// How long the client may keep the address. Sent in seconds, capped at `u32::MAX`; renewal
    /// is offered at half and rebinding at seven eighths of it.
    pub lease_time: Duration,
}

/// The shared, updatable lease table, keyed by client MAC.
pub type LeaseTable = Arc<RwLock<HashMap<MacAddr, Lease>>>;

/// Where and as whom the server listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DhcpConfig {
    /// Interface to bind to (`SO_BINDTODEVICE`), the switch bridge.
    pub interface: String,
    /// Server identifier (option 54): the node's address on the bridge.
    pub server_ip: Ipv4Addr,
    /// Listen port, normally [`SERVER_PORT`]; replies go to `client_port`.
    pub port: u16,
    /// UDP port replies to unconfigured clients are sent to, normally [`CLIENT_PORT`].
    pub client_port: u16,
}

impl DhcpConfig {
    /// A configuration listening on [`SERVER_PORT`] and replying to [`CLIENT_PORT`].
    pub fn new(interface: impl Into<String>, server_ip: Ipv4Addr) -> Self {
        Self {
            interface: interface.into(),
            server_ip,
            port: SERVER_PORT,
            client_port: CLIENT_PORT,
        }
    }
}

/// A bound DHCP server. Call [`DhcpServer::run`] to serve.
#[derive(Debug)]
pub struct DhcpServer {
    cfg: DhcpConfig,
    leases: LeaseTable,
    socket: UdpSocket,
}

/// What the server decided for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    /// The DHCP message to send.
    pub message: Message,
    /// Where to send it: the relay for relayed requests, the client for renewals, otherwise
    /// the limited broadcast address.
    pub to: SocketAddrV4,
}

impl DhcpServer {
    /// Binds `0.0.0.0:<port>` on the interface. Must be called inside a tokio runtime; the
    /// socket belongs to the calling thread's network namespace.
    pub fn bind(cfg: DhcpConfig, leases: LeaseTable) -> Result<Self> {
        let op = |what: &str| {
            let what = format!("dhcp on {}: {what}", cfg.interface);
            move |e| Error::io(what, e)
        };
        let sock =
            Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(op("socket"))?;
        sock.set_reuse_address(true).map_err(op("SO_REUSEADDR"))?;
        sock.set_broadcast(true).map_err(op("SO_BROADCAST"))?;
        sock.bind_device(Some(cfg.interface.as_bytes()))
            .map_err(op("SO_BINDTODEVICE"))?;
        sock.set_nonblocking(true).map_err(op("O_NONBLOCK"))?;
        let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, cfg.port));
        sock.bind(&addr.into()).map_err(op("bind"))?;
        let socket = UdpSocket::from_std(sock.into()).map_err(op("register"))?;
        Ok(Self {
            cfg,
            leases,
            socket,
        })
    }

    /// The lease table; updates take effect for the next request.
    pub fn leases(&self) -> &LeaseTable {
        &self.leases
    }

    /// Serves until the socket fails.
    pub async fn run(&self) -> Result<()> {
        let mut buf = vec![0u8; 1500];
        loop {
            let (n, from) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| Error::io(format!("dhcp on {}: recv", self.cfg.interface), e))?;
            let Ok(req) = Message::decode(&mut Decoder::new(&buf[..n])) else {
                tracing::debug!(%from, "dhcp: undecodable packet");
                continue;
            };
            let reply = {
                let leases = self
                    .leases
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                respond(&self.cfg, &leases, &req)
            };
            let Some(reply) = reply else { continue };
            let mut out = Vec::with_capacity(576);
            if let Err(e) = reply.message.encode(&mut Encoder::new(&mut out)) {
                tracing::warn!(error = %e, "dhcp: encode reply");
                continue;
            }
            if let Err(e) = self.socket.send_to(&out, reply.to).await {
                tracing::warn!(error = %e, to = %reply.to, "dhcp: send reply");
            }
        }
    }
}

fn mac_of(req: &Message) -> Option<MacAddr> {
    if req.htype() != HType::Eth || req.hlen() != 6 {
        return None;
    }
    <[u8; 6]>::try_from(&req.chaddr()[..6]).ok().map(MacAddr)
}

/// Decides the reply to `req`. `None` means stay silent: the MAC has no lease, the request
/// names another server, or the message type needs no answer (RELEASE, DECLINE).
pub(crate) fn respond<S: std::hash::BuildHasher>(
    cfg: &DhcpConfig,
    leases: &HashMap<MacAddr, Lease, S>,
    req: &Message,
) -> Option<Reply> {
    if req.opcode() != Opcode::BootRequest {
        return None;
    }
    let mac = mac_of(req)?;
    let Some(lease) = leases.get(&mac) else {
        tracing::trace!(%mac, "dhcp: ignoring unknown client");
        return None;
    };
    let opts = req.opts();
    let kind = opts.msg_type()?;
    let server_id = match opts.get(OptionCode::ServerIdentifier) {
        Some(DhcpOption::ServerIdentifier(ip)) => Some(*ip),
        _ => None,
    };
    let requested = match opts.get(OptionCode::RequestedIpAddress) {
        Some(DhcpOption::RequestedIpAddress(ip)) => Some(*ip),
        _ => None,
    };
    match kind {
        MessageType::Discover => Some(reply(cfg, req, lease, MessageType::Offer)),
        MessageType::Request => {
            if server_id.is_some_and(|s| s != cfg.server_ip) {
                return None; // the client picked another server
            }
            let wanted = requested.or_else(|| Some(req.ciaddr()).filter(|ip| !ip.is_unspecified()));
            if wanted == Some(lease.ip) {
                Some(reply(cfg, req, lease, MessageType::Ack))
            } else {
                Some(nak(cfg, req))
            }
        }
        MessageType::Inform => Some(reply(cfg, req, lease, MessageType::Ack)),
        MessageType::Release | MessageType::Decline => {
            tracing::debug!(%mac, ?kind, "dhcp: static lease, nothing to release");
            None
        }
        _ => None,
    }
}

fn base_reply(cfg: &DhcpConfig, req: &Message, kind: MessageType) -> Message {
    let mut m = Message::default();
    m.set_opcode(Opcode::BootReply)
        .set_htype(req.htype())
        .set_xid(req.xid())
        .set_flags(req.flags())
        .set_giaddr(req.giaddr())
        .set_chaddr(&req.chaddr()[..usize::from(req.hlen()).min(16)]);
    let opts = m.opts_mut();
    opts.insert(DhcpOption::MessageType(kind));
    opts.insert(DhcpOption::ServerIdentifier(cfg.server_ip));
    m
}

/// How a reply may be addressed when the request carries no relay address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// Unicast to the client's `ciaddr` when it has one (renewals, INFORM).
    Unicast,
    /// Always broadcast; used for NAK, which must not be sent to an address the client
    /// is not entitled to.
    Broadcast,
}

fn destination(cfg: &DhcpConfig, req: &Message, delivery: Delivery) -> SocketAddrV4 {
    if !req.giaddr().is_unspecified() {
        return SocketAddrV4::new(req.giaddr(), cfg.port);
    }
    if delivery == Delivery::Unicast && !req.ciaddr().is_unspecified() {
        return SocketAddrV4::new(req.ciaddr(), cfg.client_port);
    }
    // Without the client's address in our ARP table we cannot unicast to yiaddr, so replies
    // to unconfigured clients are broadcast on the bound interface.
    SocketAddrV4::new(Ipv4Addr::BROADCAST, cfg.client_port)
}

fn reply(cfg: &DhcpConfig, req: &Message, lease: &Lease, kind: MessageType) -> Reply {
    let inform = req.opts().has_msg_type(MessageType::Inform);
    let mut m = base_reply(cfg, req, kind);
    if !inform {
        m.set_yiaddr(lease.ip);
    }
    m.set_ciaddr(req.ciaddr());
    let net = Ipv4Net::new(lease.ip, lease.prefix_len.min(32))
        .unwrap_or_else(|_| Ipv4Net::from(lease.ip));
    let opts = m.opts_mut();
    if !inform {
        let secs = u32::try_from(lease.lease_time.as_secs()).unwrap_or(u32::MAX);
        opts.insert(DhcpOption::AddressLeaseTime(secs));
        opts.insert(DhcpOption::Renewal(secs / 2));
        opts.insert(DhcpOption::Rebinding(secs / 8 * 7));
    }
    opts.insert(DhcpOption::SubnetMask(net.netmask()));
    opts.insert(DhcpOption::BroadcastAddr(net.broadcast()));
    if let Some(r) = lease.router {
        opts.insert(DhcpOption::Router(vec![r]));
    }
    if !lease.dns.is_empty() {
        opts.insert(DhcpOption::DomainNameServer(lease.dns.clone()));
    }
    if let Some(d) = &lease.domain {
        opts.insert(DhcpOption::DomainName(d.clone()));
    }
    if let Some(h) = &lease.hostname {
        opts.insert(DhcpOption::Hostname(h.clone()));
    }
    if let Some(mtu) = lease.mtu {
        opts.insert(DhcpOption::InterfaceMtu(mtu));
    }
    if !lease.routes.is_empty() {
        let mut routes = lease.routes.clone();
        if let Some(r) = lease.router
            && !routes.iter().any(|(n, _)| n.prefix_len() == 0)
        {
            routes.push((Ipv4Net::default(), r));
        }
        opts.insert(DhcpOption::ClasslessStaticRoute(routes));
    }
    Reply {
        to: destination(cfg, req, Delivery::Unicast),
        message: m,
    }
}

fn nak(cfg: &DhcpConfig, req: &Message) -> Reply {
    let mut m = base_reply(cfg, req, MessageType::Nak);
    m.set_flags(Flags::default().set_broadcast());
    Reply {
        to: destination(cfg, req, Delivery::Broadcast),
        message: m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x52, 0x54, 0, 1, 2, 3];

    fn table() -> HashMap<MacAddr, Lease> {
        let mut t = HashMap::new();
        t.insert(
            MacAddr(MAC),
            Lease {
                ip: Ipv4Addr::new(10, 20, 0, 5),
                prefix_len: 24,
                router: Some(Ipv4Addr::new(10, 20, 0, 1)),
                dns: vec![Ipv4Addr::new(10, 20, 0, 1)],
                domain: Some("sw1.prism".into()),
                hostname: Some("vm1".into()),
                mtu: Some(1350),
                routes: Vec::new(),
                lease_time: Duration::from_secs(3600),
            },
        );
        t
    }

    fn request(mac: [u8; 6], kind: MessageType, extra: &[DhcpOption]) -> Message {
        let mut m = Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &mac,
        );
        m.opts_mut().insert(DhcpOption::MessageType(kind));
        for o in extra {
            m.opts_mut().insert(o.clone());
        }
        m
    }

    fn cfg() -> DhcpConfig {
        DhcpConfig::new("ps1", Ipv4Addr::new(10, 20, 0, 1))
    }

    #[test]
    fn offers_static_lease_with_options() {
        let r = respond(&cfg(), &table(), &request(MAC, MessageType::Discover, &[])).unwrap();
        assert_eq!(r.message.yiaddr(), Ipv4Addr::new(10, 20, 0, 5));
        assert_eq!(r.to, SocketAddrV4::new(Ipv4Addr::BROADCAST, CLIENT_PORT));
        let o = r.message.opts();
        assert_eq!(o.msg_type(), Some(MessageType::Offer));
        assert_eq!(
            o.get(OptionCode::InterfaceMtu),
            Some(&DhcpOption::InterfaceMtu(1350))
        );
        assert_eq!(
            o.get(OptionCode::SubnetMask),
            Some(&DhcpOption::SubnetMask(Ipv4Addr::new(255, 255, 255, 0)))
        );
        // Encodes and decodes.
        let mut buf = Vec::new();
        r.message.encode(&mut Encoder::new(&mut buf)).unwrap();
        let back = Message::decode(&mut Decoder::new(&buf)).unwrap();
        assert_eq!(back.yiaddr(), Ipv4Addr::new(10, 20, 0, 5));
    }

    #[test]
    fn destination_by_delivery() {
        let mut req = request(MAC, MessageType::Request, &[]);
        let client = SocketAddrV4::new(Ipv4Addr::BROADCAST, CLIENT_PORT);
        assert_eq!(destination(&cfg(), &req, Delivery::Unicast), client);
        req.set_ciaddr(Ipv4Addr::new(10, 20, 0, 5));
        assert_eq!(
            destination(&cfg(), &req, Delivery::Unicast),
            SocketAddrV4::new(Ipv4Addr::new(10, 20, 0, 5), CLIENT_PORT)
        );
        assert_eq!(destination(&cfg(), &req, Delivery::Broadcast), client);
        req.set_giaddr(Ipv4Addr::new(10, 20, 0, 254));
        assert_eq!(
            destination(&cfg(), &req, Delivery::Broadcast),
            SocketAddrV4::new(Ipv4Addr::new(10, 20, 0, 254), SERVER_PORT)
        );
    }

    #[test]
    fn ignores_unknown_macs() {
        let req = request([2, 0, 0, 0, 0, 9], MessageType::Discover, &[]);
        assert!(respond(&cfg(), &table(), &req).is_none());
    }

    #[test]
    fn acks_matching_request_and_naks_others() {
        let ok = request(
            MAC,
            MessageType::Request,
            &[
                DhcpOption::RequestedIpAddress(Ipv4Addr::new(10, 20, 0, 5)),
                DhcpOption::ServerIdentifier(Ipv4Addr::new(10, 20, 0, 1)),
            ],
        );
        let r = respond(&cfg(), &table(), &ok).unwrap();
        assert_eq!(r.message.opts().msg_type(), Some(MessageType::Ack));

        let other_server = request(
            MAC,
            MessageType::Request,
            &[DhcpOption::ServerIdentifier(Ipv4Addr::new(10, 20, 0, 2))],
        );
        assert!(respond(&cfg(), &table(), &other_server).is_none());

        let wrong = request(
            MAC,
            MessageType::Request,
            &[DhcpOption::RequestedIpAddress(Ipv4Addr::new(10, 20, 0, 9))],
        );
        let r = respond(&cfg(), &table(), &wrong).unwrap();
        assert_eq!(r.message.opts().msg_type(), Some(MessageType::Nak));
    }

    #[test]
    fn inform_and_release() {
        let mut inform = request(MAC, MessageType::Inform, &[]);
        inform.set_ciaddr(Ipv4Addr::new(10, 20, 0, 5));
        let r = respond(&cfg(), &table(), &inform).unwrap();
        assert_eq!(r.message.yiaddr(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(
            r.to,
            SocketAddrV4::new(Ipv4Addr::new(10, 20, 0, 5), CLIENT_PORT)
        );
        assert!(r.message.opts().get(OptionCode::AddressLeaseTime).is_none());
        assert!(respond(&cfg(), &table(), &request(MAC, MessageType::Release, &[])).is_none());
    }
}
