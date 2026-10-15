//! A small DNS server for one switch: authoritative for `<switch>.prism` (A, AAAA and PTR from
//! a caller-provided map) and forwarding every other query to upstream resolvers over UDP,
//! retrying over TCP when the upstream answer is truncated.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hickory_proto::op::{Edns, Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA, PTR};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

use crate::{Error, Result};

/// The authoritative data: the zone apex and the address records under it.
#[derive(Clone, Debug, Default)]
pub struct Zone {
    origin: Option<Name>,
    hosts: HashMap<Name, Vec<IpAddr>>,
    reverse: HashMap<Name, Name>,
    /// Suffixes that are never forwarded (answered NXDOMAIN), e.g. `ts.net`.
    blocked: Vec<Name>,
    ttl: u32,
}

impl Zone {
    /// A zone for `origin` (e.g. `sw1.prism`) with `hosts` mapping relative names (`vm1`) to
    /// addresses. PTR records are derived from the same map.
    pub fn new(origin: &str, hosts: &HashMap<String, Vec<IpAddr>>, ttl: u32) -> Result<Self> {
        let origin = parse_fqdn(origin)?;
        let mut zone = Self {
            origin: Some(origin.clone()),
            hosts: HashMap::new(),
            reverse: HashMap::new(),
            blocked: Vec::new(),
            ttl,
        };
        for (host, addrs) in hosts {
            let name = Name::from_ascii(host)
                .and_then(|n| n.append_domain(&origin))
                .map_err(|e| Error::Invalid(format!("host name {host:?}: {e}")))?
                .to_lowercase();
            for ip in addrs {
                zone.reverse
                    .insert(Name::from(*ip).to_lowercase(), name.clone());
            }
            zone.hosts.insert(name, addrs.clone());
        }
        Ok(zone)
    }

    /// Adds suffixes that must never be forwarded.
    pub fn with_blocked(mut self, suffixes: &[&str]) -> Result<Self> {
        for s in suffixes {
            self.blocked.push(parse_fqdn(s)?);
        }
        Ok(self)
    }

    /// The local answer to `req`, or `None` when it must be forwarded.
    pub fn answer(&self, req: &Message) -> Option<Message> {
        let q = req.queries.first()?;
        let name = q.name().to_lowercase();
        if self.blocked.iter().any(|b| b.zone_of(&name)) {
            return Some(response(req, ResponseCode::NXDomain));
        }
        if let Some(target) = self.reverse.get(&name) {
            let mut resp = response(req, ResponseCode::NoError);
            if matches!(q.query_type(), RecordType::PTR | RecordType::ANY) {
                resp.add_answer(Record::from_rdata(
                    q.name().clone(),
                    self.ttl,
                    RData::PTR(PTR(target.clone())),
                ));
            }
            return Some(resp);
        }
        let origin = self.origin.as_ref()?;
        if !origin.zone_of(&name) {
            return None;
        }
        let Some(addrs) = self.hosts.get(&name) else {
            let code = if name == *origin {
                ResponseCode::NoError
            } else {
                ResponseCode::NXDomain
            };
            return Some(response(req, code));
        };
        let mut resp = response(req, ResponseCode::NoError);
        for ip in addrs {
            let rdata = match (ip, q.query_type()) {
                (IpAddr::V4(a), RecordType::A | RecordType::ANY) => RData::A(A(*a)),
                (IpAddr::V6(a), RecordType::AAAA | RecordType::ANY) => RData::AAAA(AAAA(*a)),
                _ => continue,
            };
            resp.add_answer(Record::from_rdata(q.name().clone(), self.ttl, rdata));
        }
        Some(resp)
    }
}

/// An authoritative response skeleton for `req`.
fn response(req: &Message, code: ResponseCode) -> Message {
    let mut m = Message::response(req.metadata.id, req.metadata.op_code);
    m.metadata.response_code = code;
    m.metadata.authoritative = true;
    m.metadata.recursion_desired = req.metadata.recursion_desired;
    m.metadata.recursion_available = true;
    m.add_queries(req.queries.iter().cloned());
    m
}

fn parse_fqdn(s: &str) -> Result<Name> {
    let mut n = Name::from_ascii(s).map_err(|e| Error::Invalid(format!("domain {s:?}: {e}")))?;
    n.set_fqdn(true);
    Ok(n.to_lowercase())
}

/// Server settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnsConfig {
    /// Address to listen on (UDP and TCP), normally the switch VIP, port 53.
    pub listen: SocketAddr,
    /// Optional `SO_BINDTODEVICE` interface.
    pub interface: Option<String>,
    /// Upstream resolvers, tried in order.
    pub upstreams: Vec<SocketAddr>,
    /// Per-upstream timeout.
    pub upstream_timeout: Duration,
}

/// A bound DNS server.
#[derive(Debug)]
pub struct DnsServer {
    cfg: DnsConfig,
    zone: RwLock<Zone>,
    udp: UdpSocket,
    tcp: TcpListener,
}

fn bind_socket(cfg: &DnsConfig, ty: Type, proto: Protocol) -> Result<Socket> {
    let op = |what: &'static str| move |e| Error::io(format!("dns on {}: {what}", cfg.listen), e);
    let domain = if cfg.listen.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let s = Socket::new(domain, ty, Some(proto)).map_err(op("socket"))?;
    s.set_reuse_address(true).map_err(op("SO_REUSEADDR"))?;
    if let Some(dev) = &cfg.interface {
        s.bind_device(Some(dev.as_bytes()))
            .map_err(op("SO_BINDTODEVICE"))?;
    }
    if cfg.listen.is_ipv6() {
        s.set_only_v6(true).map_err(op("IPV6_V6ONLY"))?;
    }
    s.set_nonblocking(true).map_err(op("O_NONBLOCK"))?;
    s.bind(&cfg.listen.into()).map_err(op("bind"))?;
    Ok(s)
}

impl DnsServer {
    /// Binds UDP and TCP on `cfg.listen`. Must be called inside a tokio runtime; the sockets
    /// belong to the calling thread's network namespace.
    pub fn bind(cfg: DnsConfig, zone: Zone) -> Result<Arc<Self>> {
        let udp = bind_socket(&cfg, Type::DGRAM, Protocol::UDP)?;
        let tcp = bind_socket(&cfg, Type::STREAM, Protocol::TCP)?;
        tcp.listen(128)
            .map_err(|e| Error::io(format!("dns on {}: listen", cfg.listen), e))?;
        let reg = |e| Error::io("dns: register socket", e);
        Ok(Arc::new(Self {
            udp: UdpSocket::from_std(udp.into()).map_err(reg)?,
            tcp: TcpListener::from_std(tcp.into()).map_err(reg)?,
            zone: RwLock::new(zone),
            cfg,
        }))
    }

    /// Replaces the authoritative data.
    pub fn set_zone(&self, zone: Zone) {
        *self
            .zone
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = zone;
    }

    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.udp
            .local_addr()
            .map_err(|e| Error::io("dns: local_addr", e))
    }

    /// Serves UDP and TCP until a socket fails.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let tcp = Arc::clone(&self);
        let tcp_task = tokio::spawn(async move { tcp.serve_tcp().await });
        let udp = self.serve_udp().await;
        tcp_task.abort();
        udp
    }

    async fn serve_udp(self: &Arc<Self>) -> Result<()> {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = self
                .udp
                .recv_from(&mut buf)
                .await
                .map_err(|e| Error::io("dns: udp recv", e))?;
            let query = buf[..n].to_vec();
            let me = Arc::clone(self);
            tokio::spawn(async move {
                if let Some(resp) = me.handle(&query, true).await
                    && let Err(e) = me.udp.send_to(&resp, from).await
                {
                    tracing::debug!(error = %e, %from, "dns: udp send");
                }
            });
        }
    }

    async fn serve_tcp(self: &Arc<Self>) -> Result<()> {
        loop {
            let (stream, from) = self
                .tcp
                .accept()
                .await
                .map_err(|e| Error::io("dns: tcp accept", e))?;
            let me = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(e) = me.serve_tcp_conn(stream).await {
                    tracing::debug!(error = %e, %from, "dns: tcp connection");
                }
            });
        }
    }

    async fn serve_tcp_conn(&self, mut stream: TcpStream) -> std::io::Result<()> {
        loop {
            let Ok(Ok(len)) = timeout(Duration::from_secs(10), stream.read_u16()).await else {
                return Ok(());
            };
            let mut query = vec![0u8; usize::from(len)];
            stream.read_exact(&mut query).await?;
            if let Some(resp) = self.handle(&query, false).await {
                let Ok(len) = u16::try_from(resp.len()) else {
                    return Ok(());
                };
                let mut out = len.to_be_bytes().to_vec();
                out.extend_from_slice(&resp);
                stream.write_all(&out).await?;
            }
        }
    }

    /// Answers one wire-format query. `None` drops it.
    async fn handle(&self, query: &[u8], udp: bool) -> Option<Vec<u8>> {
        let Ok(req) = Message::from_vec(query) else {
            if query.len() < 12 {
                return None;
            }
            let id = u16::from_be_bytes([query[0], query[1]]);
            return Message::error_msg(id, OpCode::Query, ResponseCode::FormErr)
                .to_vec()
                .ok();
        };
        if req.metadata.message_type != MessageType::Query {
            return None;
        }
        if req.metadata.op_code != OpCode::Query || req.queries.len() != 1 {
            let code = if req.metadata.op_code == OpCode::Query {
                ResponseCode::FormErr
            } else {
                ResponseCode::NotImp
            };
            return Message::error_msg(req.metadata.id, req.metadata.op_code, code)
                .to_vec()
                .ok();
        }
        let limit = if udp {
            req.edns.as_ref().map_or(512, |e| e.max_payload().max(512))
        } else {
            u16::MAX
        };
        let local = self
            .zone
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .answer(&req);
        let resp = if let Some(mut m) = local {
            if req.edns.is_some() {
                let mut edns = Edns::new();
                edns.set_max_payload(limit.max(512));
                m.set_edns(edns);
            }
            m.to_vec().ok()?
        } else if let Some(r) = self.forward(query).await {
            r
        } else {
            let mut m = Message::error_msg(
                req.metadata.id,
                req.metadata.op_code,
                ResponseCode::ServFail,
            );
            m.add_queries(req.queries.iter().cloned());
            m.to_vec().ok()?
        };
        if resp.len() > usize::from(limit) {
            return Message::from_vec(&resp).ok()?.truncate().to_vec().ok();
        }
        Some(resp)
    }

    /// Forwards a query to the upstreams in order; the reply keeps the client's ID.
    async fn forward(&self, query: &[u8]) -> Option<Vec<u8>> {
        let id = [query[0], query[1]];
        for upstream in &self.cfg.upstreams {
            let new_id = rand_id();
            let mut q = query.to_vec();
            q[..2].copy_from_slice(&new_id.to_be_bytes());
            let t = self.cfg.upstream_timeout;
            let mut resp = match timeout(t, forward_udp(*upstream, &q, new_id)).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, %upstream, "dns: upstream udp");
                    continue;
                }
                Err(_) => {
                    tracing::debug!(%upstream, "dns: upstream udp timeout");
                    continue;
                }
            };
            if resp[2] & 0x02 != 0 {
                // TC bit: the full answer only fits over TCP.
                match timeout(t, forward_tcp(*upstream, &q)).await {
                    Ok(Ok(r)) if r.len() >= 12 => resp = r,
                    Ok(Ok(_)) => tracing::debug!(%upstream, "dns: short upstream tcp answer"),
                    Ok(Err(e)) => tracing::debug!(error = %e, %upstream, "dns: upstream tcp"),
                    Err(_) => tracing::debug!(%upstream, "dns: upstream tcp timeout"),
                }
            }
            resp[..2].copy_from_slice(&id);
            return Some(resp);
        }
        None
    }
}

fn rand_id() -> u16 {
    let mut b = [0u8; 2];
    if getrandom::fill(&mut b).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        return nanos as u16;
    }
    u16::from_ne_bytes(b)
}

async fn forward_udp(upstream: SocketAddr, query: &[u8], id: u16) -> std::io::Result<Vec<u8>> {
    let bind: SocketAddr = if upstream.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let sock = UdpSocket::bind(bind).await?;
    sock.connect(upstream).await?;
    sock.send(query).await?;
    let mut buf = vec![0u8; 65535];
    loop {
        let n = sock.recv(&mut buf).await?;
        if n >= 12 && u16::from_be_bytes([buf[0], buf[1]]) == id {
            buf.truncate(n);
            return Ok(buf);
        }
    }
}

async fn forward_tcp(upstream: SocketAddr, query: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut s = TcpStream::connect(upstream).await?;
    let len = u16::try_from(query.len()).map_err(std::io::Error::other)?;
    let mut out = len.to_be_bytes().to_vec();
    out.extend_from_slice(query);
    s.write_all(&out).await?;
    let len = s.read_u16().await?;
    let mut buf = vec![0u8; usize::from(len)];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::Query;

    fn zone() -> Zone {
        let mut hosts = HashMap::new();
        hosts.insert(
            "vm1".to_owned(),
            vec!["10.20.0.5".parse().unwrap(), "fd00::5".parse().unwrap()],
        );
        Zone::new("sw1.prism", &hosts, 30)
            .unwrap()
            .with_blocked(&["ts.net"])
            .unwrap()
    }

    fn query(name: &str, ty: RecordType) -> Message {
        let mut m = Message::query();
        m.add_query(Query::query(Name::from_ascii(name).unwrap(), ty));
        m
    }

    #[test]
    fn answers_zone_records() {
        let z = zone();
        let r = z.answer(&query("VM1.sw1.prism.", RecordType::A)).unwrap();
        assert_eq!(r.metadata.response_code, ResponseCode::NoError);
        assert!(r.metadata.authoritative);
        assert_eq!(r.answers.len(), 1);
        assert_eq!(r.answers[0].data, RData::A(A("10.20.0.5".parse().unwrap())));
        let r = z
            .answer(&query("vm1.sw1.prism.", RecordType::AAAA))
            .unwrap();
        assert_eq!(r.answers.len(), 1);
        let r = z.answer(&query("nope.sw1.prism.", RecordType::A)).unwrap();
        assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
        let r = z
            .answer(&query("5.0.20.10.in-addr.arpa.", RecordType::PTR))
            .unwrap();
        assert_eq!(
            r.answers[0].data,
            RData::PTR(PTR(Name::from_ascii("vm1.sw1.prism.").unwrap()))
        );
        let r = z.answer(&query("host.foo.ts.net.", RecordType::A)).unwrap();
        assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
        assert!(z.answer(&query("example.com.", RecordType::A)).is_none());
    }
}
