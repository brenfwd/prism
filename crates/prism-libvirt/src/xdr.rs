//! Minimal XDR (RFC 4506) encoder and decoder covering the subset used by
//! the libvirt remote protocol.

use crate::Error;

/// Upper bound on any single string in bytes, from
/// `VIR_NET_MESSAGE_STRING_MAX` (4 MiB).
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
    /// Creates an empty encoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the encoded bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    /// Appends an unsigned 32-bit integer in big-endian order.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Appends a signed 32-bit integer in big-endian order.
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Appends an unsigned 64-bit integer (XDR `unsigned hyper`).
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Appends a signed 64-bit integer (XDR `hyper`).
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Appends an IEEE 754 double.
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }

    /// Appends a boolean as a 32-bit 0 or 1.
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u32(u32::from(v))
    }

    /// Appends fixed-length opaque data: the bytes, zero-padded to a multiple
    /// of 4, with no length prefix.
    pub fn opaque_fixed(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self.buf.extend(std::iter::repeat_n(0, pad(v.len())));
        self
    }

    /// Appends variable-length opaque data: a 32-bit length, then the padded
    /// bytes. A length above `u32::MAX` saturates; a buffer that large is
    /// rejected by the client's `MESSAGE_MAX` check before it is sent.
    pub fn opaque(&mut self, v: &[u8]) -> &mut Self {
        self.u32(u32::try_from(v.len()).unwrap_or(u32::MAX));
        self.opaque_fixed(v)
    }

    /// Appends a UTF-8 string as variable-length opaque data.
    pub fn string(&mut self, v: &str) -> &mut Self {
        self.opaque(v.as_bytes())
    }

    /// Appends an optional (pointer) value: a boolean discriminant followed
    /// by the value.
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

    /// Appends an optional string: a presence flag, then the string if present.
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
    /// Creates a decoder over `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// Number of bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    fn len(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u32()?).map_err(|_| Error::Decode("length exceeds usize".into()))
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

    /// Reads an unsigned 32-bit integer. Fails when fewer than 4 bytes remain.
    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a signed 32-bit integer.
    pub fn i32(&mut self) -> Result<i32, Error> {
        Ok(self.u32()? as i32)
    }

    /// Reads an unsigned 64-bit integer.
    pub fn u64(&mut self) -> Result<u64, Error> {
        let hi = u64::from(self.u32()?);
        let lo = u64::from(self.u32()?);
        Ok(hi << 32 | lo)
    }

    /// Reads a signed 64-bit integer.
    pub fn i64(&mut self) -> Result<i64, Error> {
        Ok(self.u64()? as i64)
    }

    /// Reads an IEEE 754 double.
    pub fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// Reads a boolean. Any value other than 0 or 1 is a decode error.
    pub fn bool(&mut self) -> Result<bool, Error> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(Error::Decode(format!("invalid bool {v}"))),
        }
    }

    /// Reads `n` bytes of fixed-length opaque data and skips its padding.
    pub fn opaque_fixed(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let v = self.take(n)?;
        self.take(pad(n))?;
        Ok(v)
    }

    /// Reads variable-length opaque data. Fails when the declared length exceeds `max`.
    pub fn opaque(&mut self, max: usize) -> Result<&'a [u8], Error> {
        let n = self.len()?;
        if n > max {
            return Err(Error::Decode(format!("opaque length {n} exceeds {max}")));
        }
        self.opaque_fixed(n)
    }

    /// Reads a string of at most [`STRING_MAX`] bytes. Fails on invalid UTF-8.
    pub fn string(&mut self) -> Result<String, Error> {
        let b = self.opaque(STRING_MAX)?;
        String::from_utf8(b.to_vec()).map_err(|e| Error::Decode(e.to_string()))
    }

    /// Reads an optional value: a presence flag, then `f` if the flag is set.
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

    /// Reads an optional string.
    pub fn opt_string(&mut self) -> Result<Option<String>, Error> {
        self.option(Self::string)
    }

    /// Reads a variable-length array with element decoder `f`. Fails when the
    /// declared length exceeds `max`.
    pub fn array<T>(
        &mut self,
        max: usize,
        mut f: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let n = self.len()?;
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
