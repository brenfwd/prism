//! `remote_typed_param`: libvirt's tagged parameter values.

use crate::Error;
use crate::xdr::{Decoder, Encoder};

/// Most parameters one list may hold.
pub const TYPED_PARAMS_MAX: usize = 64;

/// The value of a [`TypedParam`], tagged with its libvirt type.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    /// `VIR_TYPED_PARAM_INT`.
    Int(i32),
    /// `VIR_TYPED_PARAM_UINT`.
    UInt(u32),
    /// `VIR_TYPED_PARAM_LLONG`.
    LLong(i64),
    /// `VIR_TYPED_PARAM_ULLONG`.
    ULLong(u64),
    /// `VIR_TYPED_PARAM_DOUBLE`.
    Double(f64),
    /// `VIR_TYPED_PARAM_BOOLEAN`.
    Bool(bool),
    /// `VIR_TYPED_PARAM_STRING`.
    String(String),
}

/// A named parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedParam {
    /// Parameter name, such as `bandwidth`.
    pub field: String,
    /// The tagged value.
    pub value: TypedValue,
}

impl TypedParam {
    /// Creates a parameter from a name and a value.
    pub fn new(field: impl Into<String>, value: TypedValue) -> Self {
        Self {
            field: field.into(),
            value,
        }
    }

    /// Creates a string parameter.
    pub fn string(field: impl Into<String>, v: impl Into<String>) -> Self {
        Self::new(field, TypedValue::String(v.into()))
    }

    /// Creates an unsigned 64-bit parameter.
    pub fn ullong(field: impl Into<String>, v: u64) -> Self {
        Self::new(field, TypedValue::ULLong(v))
    }

    /// Creates an unsigned 32-bit parameter.
    pub fn uint(field: impl Into<String>, v: u32) -> Self {
        Self::new(field, TypedValue::UInt(v))
    }

    /// Appends the field name, the type tag and the value.
    pub fn encode(&self, e: &mut Encoder) {
        e.string(&self.field);
        match &self.value {
            TypedValue::Int(v) => e.i32(1).i32(*v),
            TypedValue::UInt(v) => e.i32(2).u32(*v),
            TypedValue::LLong(v) => e.i32(3).i64(*v),
            TypedValue::ULLong(v) => e.i32(4).u64(*v),
            TypedValue::Double(v) => e.i32(5).f64(*v),
            TypedValue::Bool(v) => e.i32(6).i32(i32::from(*v)),
            TypedValue::String(v) => e.i32(7).string(v),
        };
    }

    /// Reads one parameter. Fails on an unknown type tag.
    pub fn decode(d: &mut Decoder<'_>) -> Result<Self, Error> {
        let field = d.string()?;
        let value = match d.i32()? {
            1 => TypedValue::Int(d.i32()?),
            2 => TypedValue::UInt(d.u32()?),
            3 => TypedValue::LLong(d.i64()?),
            4 => TypedValue::ULLong(d.u64()?),
            5 => TypedValue::Double(d.f64()?),
            6 => TypedValue::Bool(d.i32()? != 0),
            7 => TypedValue::String(d.string()?),
            t => return Err(Error::Decode(format!("bad typed param type {t}"))),
        };
        Ok(Self { field, value })
    }
}

/// Appends a parameter list: a count, then each parameter.
pub fn encode_params(e: &mut Encoder, params: &[TypedParam]) {
    // A list long enough to saturate is rejected by the client's message
    // size check.
    e.u32(u32::try_from(params.len()).unwrap_or(u32::MAX));
    for p in params {
        p.encode(e);
    }
}

/// Reads a parameter list of at most `max` entries.
pub fn decode_params(d: &mut Decoder<'_>, max: usize) -> Result<Vec<TypedParam>, Error> {
    d.array(max, TypedParam::decode)
}

/// Convenience lookups over a parameter list.
pub trait Params {
    /// The value of the first parameter named `field`.
    fn get(&self, field: &str) -> Option<&TypedValue>;

    /// The parameter as a `u64`, converting any integer type that fits.
    fn u64(&self, field: &str) -> Option<u64> {
        match self.get(field)? {
            TypedValue::ULLong(v) => Some(*v),
            TypedValue::UInt(v) => Some(u64::from(*v)),
            TypedValue::LLong(v) => u64::try_from(*v).ok(),
            TypedValue::Int(v) => u64::try_from(*v).ok(),
            _ => None,
        }
    }

    /// The parameter as a string slice, if it is a string.
    fn str(&self, field: &str) -> Option<&str> {
        match self.get(field)? {
            TypedValue::String(s) => Some(s),
            _ => None,
        }
    }
}

impl Params for [TypedParam] {
    fn get(&self, field: &str) -> Option<&TypedValue> {
        self.iter().find(|p| p.field == field).map(|p| &p.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let params = vec![
            TypedParam::ullong("a", 7),
            TypedParam::string("b", "x"),
            TypedParam::new("c", TypedValue::Double(1.5)),
            TypedParam::new("d", TypedValue::Bool(true)),
        ];
        let mut e = Encoder::new();
        encode_params(&mut e, &params);
        let buf = e.into_inner();
        let back = decode_params(&mut Decoder::new(&buf), 16).unwrap();
        assert_eq!(back, params);
        assert_eq!(back.u64("a"), Some(7));
        assert_eq!(back.str("b"), Some("x"));
    }

    #[test]
    fn u64_converts_only_values_that_fit() {
        let params = [
            TypedParam::new("neg", TypedValue::Int(-1)),
            TypedParam::new("small", TypedValue::UInt(5)),
            TypedParam::string("s", "x"),
        ];
        assert_eq!(params.u64("neg"), None);
        assert_eq!(params.u64("small"), Some(5));
        assert_eq!(params.u64("s"), None);
        assert_eq!(params.u64("missing"), None);
    }

    #[test]
    fn rejects_unknown_type_tag() {
        let mut e = Encoder::new();
        e.string("x").i32(99);
        let buf = e.into_inner();
        assert!(TypedParam::decode(&mut Decoder::new(&buf)).is_err());
    }
}
