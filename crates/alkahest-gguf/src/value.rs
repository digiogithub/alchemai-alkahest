//! Metadata value model.

use crate::error::GgufError;

/// Wire type id of a metadata value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// `uint8`
    U8,
    /// `int8`
    I8,
    /// `uint16`
    U16,
    /// `int16`
    I16,
    /// `uint32`
    U32,
    /// `int32`
    I32,
    /// `float32`
    F32,
    /// `bool`
    Bool,
    /// `string`
    String,
    /// `array`
    Array,
    /// `uint64`
    U64,
    /// `int64`
    I64,
    /// `float64`
    F64,
}

impl ValueType {
    /// Decodes a wire type id.
    pub fn from_id(id: u32) -> Result<Self, GgufError> {
        Ok(match id {
            0 => Self::U8,
            1 => Self::I8,
            2 => Self::U16,
            3 => Self::I16,
            4 => Self::U32,
            5 => Self::I32,
            6 => Self::F32,
            7 => Self::Bool,
            8 => Self::String,
            9 => Self::Array,
            10 => Self::U64,
            11 => Self::I64,
            12 => Self::F64,
            other => return Err(GgufError::UnknownValueType(other)),
        })
    }
}

/// A decoded metadata value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `uint8`
    U8(u8),
    /// `int8`
    I8(i8),
    /// `uint16`
    U16(u16),
    /// `int16`
    I16(i16),
    /// `uint32`
    U32(u32),
    /// `int32`
    I32(i32),
    /// `uint64`
    U64(u64),
    /// `int64`
    I64(i64),
    /// `float32`
    F32(f32),
    /// `float64`
    F64(f64),
    /// `bool`
    Bool(bool),
    /// `string`
    String(String),
    /// An array. `items` is empty when `len` exceeds the configured keep limit (large
    /// tokenizer vocabularies); `len` is always the true element count.
    Array {
        /// Element type.
        elem: ValueType,
        /// True number of elements in the file.
        len: u64,
        /// Decoded elements, or empty when the array was too large to keep.
        items: Vec<Value>,
    },
}

impl Value {
    /// Any integer variant as `u64` (negative values yield `None`).
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Self::U8(v) => Some(v.into()),
            Self::U16(v) => Some(v.into()),
            Self::U32(v) => Some(v.into()),
            Self::U64(v) => Some(v),
            Self::I8(v) => u64::try_from(v).ok(),
            Self::I16(v) => u64::try_from(v).ok(),
            Self::I32(v) => u64::try_from(v).ok(),
            Self::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    /// Any numeric variant as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::F32(v) => Some(v.into()),
            Self::F64(v) => Some(v),
            _ => self.as_u64().map(|v| v as f64),
        }
    }

    /// The string, if this is a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The bool, if this is a bool value.
    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// True element count if this is an array.
    pub fn array_len(&self) -> Option<u64> {
        match self {
            Self::Array { len, .. } => Some(*len),
            _ => None,
        }
    }
}
