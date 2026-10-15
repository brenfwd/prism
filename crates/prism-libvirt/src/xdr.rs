//! Minimal XDR (RFC 4506) encoder and decoder covering the subset used by
//! the libvirt remote protocol.

use crate::Error;

/// Upper bound on any single string, from `VIR_NET_MESSAGE_STRING_MAX`.
pub const STRING_MAX: usize = 4_194_304;

fn pad(len: usize) -> usize {
    (4 - len % 4) % 4
}

/// Appends XDR-encoded values to a buffer.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u32(u32::from(v))
    }

    /// Fixed-length opaque data (no length prefix).
    pub fn opaque_fixed(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self.buf.extend(std::iter::repeat_n(0, pad(v.len())));
        self
    }

    /// Variable-length opaque data.
    pub fn opaque(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.opaque_fixed(v)
    }

    pub fn string(&mut self, v: &str) -> &mut Self {
        self.opaque(v.as_bytes())
    }

    /// An optional (pointer) value: a boolean discriminant followed by the value.
    pub fn option<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) -> &mut Self {
        match v {
            Some(v) => {
                self.bool(true);
                f(self, v);
            }
            None => {
                self.bool(false);
            }
        }
        self
    }

    pub fn opt_string(&mut self, v: Option<&str>) -> &mut Self {
        self.option(v, |e, s| {
            e.string(s);
        })
    }
}

/// Reads XDR-encoded values from a byte slice.
#[derive(Debug)]
pub struct Decoder<'a> {
    buf: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.buf.len() < n {
            return Err(Error::Decode(format!(
                "need {n} bytes, have {}",
                self.buf.len()
            )));
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(self.u32()? as i32)
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        let hi = u64::from(self.u32()?);
        let lo = u64::from(self.u32()?);
        Ok(hi << 32 | lo)
    }

    pub fn bool(&mut self) -> Result<bool, Error> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(Error::Decode(format!("invalid bool {v}"))),
        }
    }

    pub fn opaque_fixed(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let v = self.take(n)?;
        self.take(pad(n))?;
        Ok(v)
    }

    pub fn opaque(&mut self, max: usize) -> Result<&'a [u8], Error> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(Error::Decode(format!("opaque length {n} exceeds {max}")));
        }
        self.opaque_fixed(n)
    }

    pub fn string(&mut self) -> Result<String, Error> {
        let b = self.opaque(STRING_MAX)?;
        String::from_utf8(b.to_vec()).map_err(|e| Error::Decode(e.to_string()))
    }

    pub fn option<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        if self.bool()? {
            f(self).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn opt_string(&mut self) -> Result<Option<String>, Error> {
        self.option(Self::string)
    }

    /// A variable-length array with element decoder `f`.
    pub fn array<T>(
        &mut self,
        max: usize,
        mut f: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(Error::Decode(format!("array length {n} exceeds {max}")));
        }
        (0..n).map(|_| f(self)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut e = Encoder::new();
        e.u32(7)
            .i32(-2)
            .u64(0x0102_0304_0506_0708)
            .string("abcde")
            .opt_string(None)
            .opt_string(Some("x"))
            .opaque_fixed(&[1, 2, 3]);
        let buf = e.into_inner();
        assert_eq!(buf.len() % 4, 0);
        let mut d = Decoder::new(&buf);
        assert_eq!(d.u32().unwrap(), 7);
        assert_eq!(d.i32().unwrap(), -2);
        assert_eq!(d.u64().unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(d.string().unwrap(), "abcde");
        assert_eq!(d.opt_string().unwrap(), None);
        assert_eq!(d.opt_string().unwrap().as_deref(), Some("x"));
        assert_eq!(d.opaque_fixed(3).unwrap(), &[1, 2, 3]);
        assert_eq!(d.remaining(), 0);
    }

    #[test]
    fn string_padding() {
        let mut e = Encoder::new();
        e.string("abcde");
        assert_eq!(e.into_inner(), b"\0\0\0\x05abcde\0\0\0");
    }

    #[test]
    fn rejects_overlong() {
        let mut e = Encoder::new();
        e.u32(10);
        let buf = e.into_inner();
        assert!(Decoder::new(&buf).opaque(4).is_err());
    }
}
