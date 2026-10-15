//! The copy-paste join blobs: `prism-invite1.` and `prism-reply1.`.
//!
//! Each blob is a prefix, then base64url (no padding) of a deterministic
//! CBOR map with integer keys followed by the first 4 bytes of BLAKE3 over
//! the CBOR. Key 0 is the minor version; unknown keys are ignored. The
//! checksum catches truncated or mistyped pastes; blobs are not secret.

use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value;
use uuid::Uuid;

pub const INVITE_PREFIX: &str = "prism-invite1.";
pub const REPLY_PREFIX: &str = "prism-reply1.";
const MINOR: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    #[error("this is an invite (prism-invite1.…); paste it into `prism join` on the new node")]
    IsInvite,
    #[error("this is a reply (prism-reply1.…); paste it into `prism cluster accept` on a member")]
    IsReply,
    #[error("upgrade prism to read {0} blobs")]
    NewerVersion(String),
    #[error("not a Prism blob; expected {0}…")]
    Unknown(&'static str),
    #[error("blob damaged, copy it again")]
    Damaged,
    #[error("blob is missing field {0}")]
    Missing(u64),
}

/// The sponsor: the member the new node first peers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sponsor {
    pub node_id: Uuid,
    pub name: String,
    pub wg_pubkey: [u8; 32],
    pub mesh_addr: Ipv6Addr,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub cluster_id: Uuid,
    pub cluster_name: String,
    pub invite_id: [u8; 16],
    pub exp: i64,
    pub mesh_prefix: Ipv6Addr,
    pub assigned_addr: Ipv6Addr,
    pub wg_mtu: u16,
    pub sponsor: Sponsor,
    pub feature_level: u32,
    pub version: String,
    pub name_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub cluster_id: Uuid,
    pub invite_id: [u8; 16],
    pub assigned_addr: Ipv6Addr,
    pub node_id: Uuid,
    pub name: String,
    pub wg_pubkey: [u8; 32],
    pub endpoints: Vec<String>,
    pub feature_level: u32,
    pub version: String,
    pub arch: String,
}

/// The first 8 base64 characters of a `WireGuard` public key, shown on both
/// sides so operators can check they match.
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    base64::engine::general_purpose::STANDARD.encode(pubkey)[..8].to_string()
}

fn int(v: u64) -> Value {
    Value::Integer(v.into())
}

fn bytes(b: &[u8]) -> Value {
    Value::Bytes(b.to_vec())
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn strings(v: &[String]) -> Value {
    Value::Array(v.iter().map(|s| text(s)).collect())
}

fn encode(prefix: &str, fields: BTreeMap<u64, Value>) -> String {
    // A BTreeMap gives ascending integer keys: deterministic CBOR.
    let map = Value::Map(fields.into_iter().map(|(k, v)| (int(k), v)).collect());
    let mut cbor = Vec::new();
    ciborium::into_writer(&map, &mut cbor).expect("CBOR into a Vec cannot fail");
    let sum = blake3::hash(&cbor);
    cbor.extend_from_slice(&sum.as_bytes()[..4]);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(cbor))
}

/// Strips whitespace, checks the prefix, version and checksum, and returns
/// the fields by key.
fn decode(blob: &str, prefix: &'static str) -> Result<BTreeMap<u64, Value>, BlobError> {
    let blob: String = blob.chars().filter(|c| !c.is_whitespace()).collect();
    let body = match blob.strip_prefix(prefix) {
        Some(b) => b,
        None if blob.starts_with(INVITE_PREFIX) => return Err(BlobError::IsInvite),
        None if blob.starts_with(REPLY_PREFIX) => return Err(BlobError::IsReply),
        None => {
            let kind = prefix.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
            return Err(match blob.split_once('.') {
                Some((p, _)) if p.starts_with(kind) => BlobError::NewerVersion(p.to_string()),
                _ => BlobError::Unknown(prefix),
            });
        }
    };
    let raw = URL_SAFE_NO_PAD.decode(body).map_err(|_| BlobError::Damaged)?;
    if raw.len() < 5 {
        return Err(BlobError::Damaged);
    }
    let (cbor, sum) = raw.split_at(raw.len() - 4);
    if blake3::hash(cbor).as_bytes()[..4] != *sum {
        return Err(BlobError::Damaged);
    }
    let Value::Map(entries) = ciborium::from_reader::<Value, _>(cbor).map_err(|_| BlobError::Damaged)? else {
        return Err(BlobError::Damaged);
    };
    Ok(entries
        .into_iter()
        .filter_map(|(k, v)| match k {
            Value::Integer(i) => u64::try_from(i).ok().map(|k| (k, v)),
            _ => None,
        })
        .collect())
}

struct Fields(BTreeMap<u64, Value>);

impl Fields {
    fn take(&mut self, k: u64) -> Result<Value, BlobError> {
        self.0.remove(&k).ok_or(BlobError::Missing(k))
    }

    fn u64(&mut self, k: u64) -> Result<u64, BlobError> {
        match self.take(k)? {
            Value::Integer(i) => u64::try_from(i).map_err(|_| BlobError::Damaged),
            _ => Err(BlobError::Damaged),
        }
    }

    fn text(&mut self, k: u64) -> Result<String, BlobError> {
        match self.take(k)? {
            Value::Text(s) => Ok(s),
            _ => Err(BlobError::Damaged),
        }
    }

    fn bytes<const N: usize>(&mut self, k: u64) -> Result<[u8; N], BlobError> {
        match self.take(k)? {
            Value::Bytes(b) => b.try_into().map_err(|_| BlobError::Damaged),
            _ => Err(BlobError::Damaged),
        }
    }

    fn uuid(&mut self, k: u64) -> Result<Uuid, BlobError> {
        Ok(Uuid::from_bytes(self.bytes::<16>(k)?))
    }

    fn ipv6(&mut self, k: u64) -> Result<Ipv6Addr, BlobError> {
        Ok(Ipv6Addr::from(self.bytes::<16>(k)?))
    }

    fn strings(&mut self, k: u64) -> Result<Vec<String>, BlobError> {
        match self.take(k)? {
            Value::Array(a) => a
                .into_iter()
                .map(|v| match v {
                    Value::Text(s) => Ok(s),
                    _ => Err(BlobError::Damaged),
                })
                .collect(),
            _ => Err(BlobError::Damaged),
        }
    }

    fn map(&mut self, k: u64) -> Result<Self, BlobError> {
        match self.take(k)? {
            Value::Map(m) => Ok(Self(
                m.into_iter()
                    .filter_map(|(k, v)| match k {
                        Value::Integer(i) => u64::try_from(i).ok().map(|k| (k, v)),
                        _ => None,
                    })
                    .collect(),
            )),
            _ => Err(BlobError::Damaged),
        }
    }
}

fn versions(feature_level: u32, version: &str, arch: Option<&str>) -> Value {
    let mut v = vec![(int(0), int(u64::from(feature_level))), (int(1), text(version))];
    if let Some(a) = arch {
        v.push((int(2), text(a)));
    }
    Value::Map(v)
}

impl Invite {
    pub fn encode(&self) -> String {
        let s = &self.sponsor;
        let sponsor = Value::Map(vec![
            (int(0), bytes(s.node_id.as_bytes())),
            (int(1), text(&s.name)),
            (int(2), bytes(&s.wg_pubkey)),
            (int(3), bytes(&s.mesh_addr.octets())),
            (int(4), strings(&s.endpoints)),
        ]);
        let mut f = BTreeMap::from([
            (0, int(MINOR)),
            (1, bytes(self.cluster_id.as_bytes())),
            (2, text(&self.cluster_name)),
            (3, bytes(&self.invite_id)),
            (4, Value::Integer(self.exp.into())),
            (5, bytes(&self.mesh_prefix.octets())),
            (6, bytes(&self.assigned_addr.octets())),
            (7, int(u64::from(self.wg_mtu))),
            (8, sponsor),
            (9, versions(self.feature_level, &self.version, None)),
        ]);
        if let Some(n) = &self.name_hint {
            f.insert(10, text(n));
        }
        encode(INVITE_PREFIX, f)
    }

    pub fn decode(blob: &str) -> Result<Self, BlobError> {
        let mut f = Fields(decode(blob, INVITE_PREFIX)?);
        let mut s = f.map(8)?;
        let mut v = f.map(9)?;
        Ok(Self {
            cluster_id: f.uuid(1)?,
            cluster_name: f.text(2)?,
            invite_id: f.bytes(3)?,
            exp: match f.take(4)? {
                Value::Integer(i) => i64::try_from(i).map_err(|_| BlobError::Damaged)?,
                _ => return Err(BlobError::Damaged),
            },
            mesh_prefix: f.ipv6(5)?,
            assigned_addr: f.ipv6(6)?,
            wg_mtu: u16::try_from(f.u64(7)?).map_err(|_| BlobError::Damaged)?,
            sponsor: Sponsor {
                node_id: s.uuid(0)?,
                name: s.text(1)?,
                wg_pubkey: s.bytes(2)?,
                mesh_addr: s.ipv6(3)?,
                endpoints: s.strings(4)?,
            },
            feature_level: u32::try_from(v.u64(0)?).map_err(|_| BlobError::Damaged)?,
            version: v.text(1)?,
            name_hint: f.text(10).ok(),
        })
    }

    pub fn invite_id_hex(&self) -> String {
        hex(&self.invite_id)
    }
}

impl Reply {
    pub fn encode(&self) -> String {
        encode(
            REPLY_PREFIX,
            BTreeMap::from([
                (0, int(MINOR)),
                (1, bytes(self.cluster_id.as_bytes())),
                (2, bytes(&self.invite_id)),
                (3, bytes(&self.assigned_addr.octets())),
                (4, bytes(self.node_id.as_bytes())),
                (5, text(&self.name)),
                (6, bytes(&self.wg_pubkey)),
                (7, strings(&self.endpoints)),
                (8, versions(self.feature_level, &self.version, Some(&self.arch))),
            ]),
        )
    }

    pub fn decode(blob: &str) -> Result<Self, BlobError> {
        let mut f = Fields(decode(blob, REPLY_PREFIX)?);
        let mut v = f.map(8)?;
        Ok(Self {
            cluster_id: f.uuid(1)?,
            invite_id: f.bytes(2)?,
            assigned_addr: f.ipv6(3)?,
            node_id: f.uuid(4)?,
            name: f.text(5)?,
            wg_pubkey: f.bytes(6)?,
            endpoints: f.strings(7)?,
            feature_level: u32::try_from(v.u64(0)?).map_err(|_| BlobError::Damaged)?,
            version: v.text(1)?,
            arch: v.text(2).unwrap_or_default(),
        })
    }

    pub fn invite_id_hex(&self) -> String {
        hex(&self.invite_id)
    }
}

pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite() -> Invite {
        Invite {
            cluster_id: Uuid::from_u128(1),
            cluster_name: "lab".into(),
            invite_id: [7; 16],
            exp: 1_791_939_667,
            mesh_prefix: "fd12:3456:789a::".parse().unwrap(),
            assigned_addr: "fd12:3456:789a:1::3".parse().unwrap(),
            wg_mtu: 1420,
            sponsor: Sponsor {
                node_id: Uuid::from_u128(2),
                name: "kvm-a".into(),
                wg_pubkey: [9; 32],
                mesh_addr: "fd12:3456:789a:1::1".parse().unwrap(),
                endpoints: vec!["192.168.10.11:51820".into(), "100.101.102.103:51820".into()],
            },
            feature_level: 1,
            version: "0.1.0".into(),
            name_hint: Some("kvm-c".into()),
        }
    }

    #[test]
    fn invite_round_trip_and_size() {
        let i = invite();
        let blob = i.encode();
        assert!(blob.starts_with(INVITE_PREFIX));
        assert!(blob.len() < 400, "invite is {} chars", blob.len());
        assert_eq!(i.encode(), blob, "deterministic");
        // Pasted with line breaks and spaces.
        let wrapped: String = blob.chars().enumerate().flat_map(|(n, c)| if n % 60 == 59 { vec![c, '\n', ' '] } else { vec![c] }).collect();
        assert_eq!(Invite::decode(&wrapped).unwrap(), i);
    }

    #[test]
    fn reply_round_trip() {
        let r = Reply {
            cluster_id: Uuid::from_u128(1),
            invite_id: [7; 16],
            assigned_addr: "fd12:3456:789a:1::3".parse().unwrap(),
            node_id: Uuid::from_u128(3),
            name: "kvm-c".into(),
            wg_pubkey: [5; 32],
            endpoints: vec![],
            feature_level: 1,
            version: "0.1.0".into(),
            arch: "x86_64".into(),
        };
        let blob = r.encode();
        assert!(blob.len() < 260, "reply is {} chars", blob.len());
        assert_eq!(Reply::decode(&blob).unwrap(), r);
    }

    #[test]
    fn errors_are_helpful() {
        let blob = invite().encode();
        assert_eq!(Reply::decode(&blob), Err(BlobError::IsInvite));
        let mut damaged = blob.clone();
        damaged.truncate(blob.len() - 3);
        assert_eq!(Invite::decode(&damaged), Err(BlobError::Damaged));
        let flipped = blob.replacen('A', "B", 1);
        if flipped != blob {
            assert_eq!(Invite::decode(&flipped), Err(BlobError::Damaged));
        }
        assert_eq!(Invite::decode("prism-invite2.AAAA"), Err(BlobError::NewerVersion("prism-invite2".into())));
        assert_eq!(Invite::decode("hello"), Err(BlobError::Unknown(INVITE_PREFIX)));
        assert_eq!(fingerprint(&[0; 32]), "AAAAAAAA");
    }
}
