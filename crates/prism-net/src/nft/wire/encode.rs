//! Encoder for one netlink message with nested attributes.

use super::consts::NLA_F_NESTED;
use crate::{Error, Result};

/// Builds one netlink message; attribute helpers handle nesting and padding.
///
/// Attribute lengths are 16-bit, so a string or value over 65531 bytes (or a nest over 65535)
/// cannot be encoded. The first such failure is remembered and returned by [`Msg::finish`],
/// which keeps the chained builder calls free of `?`.
pub(super) struct Msg {
    buf: Vec<u8>,
    nests: Vec<usize>,
    error: Option<Error>,
}

impl Msg {
    pub(super) fn new(kind: u16, flags: u16, seq: u32, family: u8, res_id: u16) -> Self {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&0u32.to_ne_bytes()); // length, patched in finish()
        buf.extend_from_slice(&kind.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes()); // port id: kernel
        buf.extend_from_slice(&[family, 0]); // nfgenmsg: family, NFNETLINK_V0
        buf.extend_from_slice(&res_id.to_be_bytes());
        Self {
            buf,
            nests: Vec::new(),
            error: None,
        }
    }

    fn pad(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    fn fail(&mut self, what: &str) {
        self.error
            .get_or_insert_with(|| Error::Invalid(format!("nftables {what} exceeds 65535 bytes")));
    }

    pub(super) fn attr(&mut self, kind: u16, data: &[u8]) -> &mut Self {
        let Ok(len) = u16::try_from(4 + data.len()) else {
            self.fail("attribute");
            return self;
        };
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        self.buf.extend_from_slice(data);
        self.pad();
        self
    }

    pub(super) fn str(&mut self, kind: u16, s: &str) -> &mut Self {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.attr(kind, &data)
    }

    pub(super) fn be32(&mut self, kind: u16, v: u32) -> &mut Self {
        self.attr(kind, &v.to_be_bytes())
    }

    pub(super) fn begin(&mut self, kind: u16) -> &mut Self {
        self.nests.push(self.buf.len());
        self.buf.extend_from_slice(&0u16.to_ne_bytes());
        self.buf
            .extend_from_slice(&(kind | NLA_F_NESTED).to_ne_bytes());
        self
    }

    pub(super) fn end(&mut self) -> &mut Self {
        let Some(start) = self.nests.pop() else {
            debug_assert!(false, "end() without begin()");
            return self;
        };
        match u16::try_from(self.buf.len() - start) {
            Ok(len) => self.buf[start..start + 2].copy_from_slice(&len.to_ne_bytes()),
            Err(_) => self.fail("nested attribute"),
        }
        self
    }

    /// Patches the message length and returns the bytes, or the first encoding error.
    pub(super) fn finish(mut self) -> Result<Vec<u8>> {
        if let Some(e) = self.error {
            return Err(e);
        }
        debug_assert!(self.nests.is_empty(), "unbalanced nest");
        let len = u32::try_from(self.buf.len())
            .map_err(|_| Error::Invalid("nftables message exceeds 4 GiB".into()))?;
        self.buf[..4].copy_from_slice(&len.to_ne_bytes());
        Ok(self.buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_attributes_with_padding_and_nesting() {
        let mut m = Msg::new(1, 0, 7, 1, 0);
        m.str(1, "abc").begin(2).be32(1, 5).end();
        let out = m.finish().unwrap();
        assert_eq!(out.len(), 16 + 4 + 8 + 4 + 8);
        assert_eq!(
            u32::from_ne_bytes(out[0..4].try_into().unwrap()) as usize,
            out.len()
        );
        // "abc\0" attr: len 8, type 1.
        assert_eq!(&out[20..24], &[8, 0, 1, 0]);
        assert_eq!(&out[24..28], b"abc\0");
        // nested attr: len 12, type 2|NESTED.
        assert_eq!(&out[28..32], &[12, 0, 2, 0x80]);
        assert_eq!(&out[36..40], &5u32.to_be_bytes());
    }

    #[test]
    fn oversized_attribute_is_an_error() {
        let mut m = Msg::new(1, 0, 7, 1, 0);
        m.attr(1, &vec![0u8; 70_000]);
        assert!(matches!(m.finish(), Err(Error::Invalid(_))));
    }
}
