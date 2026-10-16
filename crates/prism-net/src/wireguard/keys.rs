//! Curve25519 key types: base64 parsing and formatting, clamping, zeroing on drop.

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::{Error, Result};

/// Length of a Curve25519 key in bytes.
const KEY_LEN: usize = 32;

/// A Curve25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

/// A Curve25519 private key. Debug output is redacted; memory is zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateKey([u8; KEY_LEN]);

/// Decodes a standard-base64 key, ignoring surrounding whitespace. Fails with
/// [`Error::InvalidKey`] on bad base64 or any length other than 32 bytes.
fn decode_key(s: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = B64
        .decode(s.trim())
        .map_err(|e| Error::InvalidKey(e.to_string()))?;
    <[u8; KEY_LEN]>::try_from(bytes.as_slice())
        .map_err(|_| Error::InvalidKey(format!("expected {KEY_LEN} bytes, got {}", bytes.len())))
}

impl PublicKey {
    /// Wraps the 32 raw bytes of a public key without checking them.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The key in standard base64 with padding, as `wg show` prints it.
    pub fn to_base64(&self) -> String {
        B64.encode(self.0)
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        decode_key(s).map(Self)
    }
}

impl PrivateKey {
    /// Generates a new clamped key from the OS CSPRNG.
    pub fn generate() -> Self {
        Self::from_bytes(x25519_dalek::StaticSecret::random().to_bytes())
    }

    /// Wraps raw key bytes, applying Curve25519 clamping as `wg genkey` does.
    pub fn from_bytes(mut bytes: [u8; KEY_LEN]) -> Self {
        bytes[0] &= 0b1111_1000;
        bytes[31] &= 0b0111_1111;
        bytes[31] |= 0b0100_0000;
        Self(bytes)
    }

    /// The raw, clamped key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The public key that belongs to this private key.
    pub fn public_key(&self) -> PublicKey {
        let secret = x25519_dalek::StaticSecret::from(self.0);
        PublicKey(x25519_dalek::PublicKey::from(&secret).to_bytes())
    }

    /// Base64, the format of `wg genkey` and `PrivateKey =` in wg-quick files.
    pub fn to_base64(&self) -> String {
        B64.encode(self.0)
    }
}

impl FromStr for PrivateKey {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        decode_key(s).map(Self::from_bytes)
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateKey(<redacted>)")
    }
}

impl Drop for PrivateKey {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

/// A private key and its public key.
#[derive(Clone, Debug)]
pub struct KeyPair {
    /// The secret half.
    pub private: PrivateKey,
    /// The half peers need, always `private.public_key()`.
    pub public: PublicKey,
}

impl KeyPair {
    /// Generates a fresh key pair from the OS random source.
    pub fn generate() -> Self {
        let private = PrivateKey::generate();
        let public = private.public_key();
        Self { private, public }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_roundtrip_and_match_known_vector() {
        // RFC 7748 section 6.1 (Alice), already clamped-compatible.
        let private: PrivateKey = B64
            .encode([
                0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2,
                0x66, 0x45, 0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5,
                0x1d, 0xb9, 0x2c, 0x2a,
            ])
            .parse()
            .unwrap();
        assert_eq!(
            private.public_key().as_bytes(),
            &[
                0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54, 0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e,
                0xf7, 0x5a, 0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4, 0xeb, 0xa4, 0xa9, 0x8e,
                0xaa, 0x9b, 0x4e, 0x6a
            ]
        );
        let kp = KeyPair::generate();
        let back: PrivateKey = kp.private.to_base64().parse().unwrap();
        assert_eq!(back.public_key(), kp.public);
        let pk: PublicKey = kp.public.to_string().parse().unwrap();
        assert_eq!(pk, kp.public);
        assert_eq!(kp.private.as_bytes()[0] & 7, 0);
        assert!(format!("{:?}", kp.private).contains("redacted"));
        assert!("short".parse::<PublicKey>().is_err());
    }
}
