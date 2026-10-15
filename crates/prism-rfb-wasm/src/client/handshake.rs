//! The RFB handshake: protocol version, security and initialisation
//! (RFC 6143 sections 7.1 and 7.3).

use super::{Action, Client, State, be16, be32, latin1};
use crate::Error;

impl Client {
    pub(super) fn version(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(v) = input.get(..12) else {
            return Ok(None);
        };
        let bad = || {
            Error::new(format!(
                "not an RFB server: {:?}",
                String::from_utf8_lossy(v)
            ))
        };
        if &v[..4] != b"RFB " || v[7] != b'.' || v[11] != b'\n' {
            return Err(bad());
        }
        let num = |s: &[u8]| {
            std::str::from_utf8(s)
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
        };
        let (major, minor) = (
            num(&v[4..7]).ok_or_else(bad)?,
            num(&v[8..11]).ok_or_else(bad)?,
        );
        if major != 3 || minor < 3 {
            return Err(bad());
        }
        self.minor = match minor {
            3..=6 => 3,
            7 => 7,
            _ => 8,
        };
        self.send(format!("RFB 003.00{}\n", self.minor).into_bytes());
        self.state = State::SecurityTypes;
        Ok(Some(12))
    }

    /// Reads a u32-length-prefixed reason string at `input[at..]`.
    pub(super) fn reason(input: &[u8], at: usize) -> Option<(String, usize)> {
        let len = be32(input.get(at..at + 4)?) as usize;
        let text = input.get(at + 4..at + 4 + len.min(4096))?;
        Some((latin1(text), at + 4 + len))
    }

    pub(super) fn security_types(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        if self.minor == 3 {
            let Some(t) = input.get(..4).map(be32) else {
                return Ok(None);
            };
            return match t {
                1 => {
                    self.client_init();
                    Ok(Some(4))
                }
                0 => match Self::reason(input, 4) {
                    Some((r, _)) => Err(Error::new(format!("server refused connection: {r}"))),
                    None => Ok(None),
                },
                t => Err(Error::new(format!("unsupported security type {t}"))),
            };
        }
        let Some(&n) = input.first() else {
            return Ok(None);
        };
        if n == 0 {
            return match Self::reason(input, 1) {
                Some((r, _)) => Err(Error::new(format!("server refused connection: {r}"))),
                None => Ok(None),
            };
        }
        let Some(types) = input.get(1..1 + usize::from(n)) else {
            return Ok(None);
        };
        if !types.contains(&1) {
            return Err(Error::new(format!(
                "server requires authentication (security types {types:?}); only None is supported"
            )));
        }
        self.send(vec![1]);
        if self.minor >= 8 {
            self.state = State::SecurityResult;
        } else {
            self.client_init();
        }
        Ok(Some(1 + usize::from(n)))
    }

    pub(super) fn security_result(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(r) = input.get(..4).map(be32) else {
            return Ok(None);
        };
        if r == 0 {
            self.client_init();
            return Ok(Some(4));
        }
        match Self::reason(input, 4) {
            Some((r, _)) => Err(Error::new(format!("security handshake failed: {r}"))),
            None => Ok(None),
        }
    }

    pub(super) fn client_init(&mut self) {
        self.send(vec![u8::from(self.opts.shared)]);
        self.state = State::ServerInit;
    }

    pub(super) fn server_init(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(head) = input.get(..24) else {
            return Ok(None);
        };
        let name_len = be32(&head[20..24]) as usize;
        if name_len > 1 << 16 {
            return Err(Error::new("desktop name too long"));
        }
        let Some(name) = input.get(24..24 + name_len) else {
            return Ok(None);
        };
        let name = String::from_utf8_lossy(name).into_owned();
        self.resize(be16(&head[0..2]), be16(&head[2..4]))?;
        self.actions.push(Action::Connected(name));
        // SetPixelFormat: 32 bpp, depth 24, little-endian true colour with
        // red in the lowest byte, so wire pixels are R, G, B, X in memory.
        let mut msg = vec![0, 0, 0, 0, 32, 24, 0, 1];
        msg.extend(255u16.to_be_bytes());
        msg.extend(255u16.to_be_bytes());
        msg.extend(255u16.to_be_bytes());
        msg.extend([0, 8, 16, 0, 0, 0]);
        self.send(msg);
        self.send(self.set_encodings_msg());
        self.request(false);
        self.need_full = false;
        self.state = State::Message;
        Ok(Some(24 + name_len))
    }
}
