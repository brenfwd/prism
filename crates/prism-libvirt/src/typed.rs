//! `remote_typed_param`: libvirt's tagged parameter values.

use crate::Error;
use crate::xdr::{Decoder, Encoder};

pub const TYPED_PARAMS_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Int(i32),
    UInt(u32),
    LLong(i64),
    ULLong(u64),
    Double(f64),
    Bool(bool),
    String(String),
}

/// A named parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedParam {
    pub field: String,
    pub value: TypedValue,
}

impl TypedParam {
    pub fn new(field: impl Into<String>, value: TypedValue) -> Self {
        Self {
            field: field.into(),
            value,
        }
    }

    pub fn string(field: impl Into<String>, v: impl Into<String>) -> Self {
        Self::new(field, TypedValue::String(v.into()))
    }

    pub fn ullong(field: impl Into<String>, v: u64) -> Self {
        Self::new(field, TypedValue::ULLong(v))
    }

    pub fn uint(field: impl Into<String>, v: u32) -> Self {
        Self::new(field, TypedValue::UInt(v))
    }

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

pub fn encode_params(e: &mut Encoder, params: &[TypedParam]) {
    e.u32(params.len() as u32);
    for p in params {
        p.encode(e);
    }
}

pub fn decode_params(d: &mut Decoder<'_>, max: usize) -> Result<Vec<TypedParam>, Error> {
    d.array(max, TypedParam::decode)
}

/// Convenience lookups over a parameter list.
pub trait Params {
    fn get(&self, field: &str) -> Option<&TypedValue>;

    fn u64(&self, field: &str) -> Option<u64> {
        match self.get(field)? {
            TypedValue::ULLong(v) => Some(*v),
            TypedValue::UInt(v) => Some(u64::from(*v)),
            TypedValue::LLong(v) => u64::try_from(*v).ok(),
            TypedValue::Int(v) => u64::try_from(*v).ok(),
            _ => None,
        }
    }

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
}
