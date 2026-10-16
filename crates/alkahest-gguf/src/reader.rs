//! Streaming GGUF parser.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use crate::error::GgufError;
use crate::ggml::GgmlType;
use crate::value::{Value, ValueType};

/// Upper bounds applied while parsing untrusted files.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum number of metadata key/value pairs.
    pub max_kv: u64,
    /// Maximum number of tensors.
    pub max_tensors: u64,
    /// Maximum byte length of any string (keys, values).
    pub max_string: u64,
    /// Maximum element count of any array (elements beyond `keep_array` are parsed and dropped).
    pub max_array: u64,
    /// Arrays longer than this keep only their length, not their items.
    pub keep_array: u64,
    /// Maximum array nesting depth.
    pub max_depth: u32,
    /// Maximum tensor dimensions (the format allows 4 in practice).
    pub max_dims: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_kv: 1 << 20,
            max_tensors: 1 << 20,
            max_string: 16 << 20,
            max_array: 64 << 20,
            keep_array: 4096,
            max_depth: 4,
            max_dims: 8,
        }
    }
}

/// One entry of the tensor info table.
#[derive(Debug, Clone, PartialEq)]
pub struct TensorInfo {
    /// Tensor name, e.g. `blk.0.attn_q.weight`.
    pub name: String,
    /// Dimensions, innermost first.
    pub shape: Vec<u64>,
    /// Element type.
    pub ggml_type: GgmlType,
    /// Offset in bytes relative to [`Gguf::data_offset`].
    pub offset: u64,
}

impl TensorInfo {
    /// Number of elements (saturating on overflow).
    pub fn element_count(&self) -> u64 {
        self.shape.iter().fold(1u64, |a, &d| a.saturating_mul(d))
    }

    /// Estimated size in bytes of the tensor data, if the type is known.
    pub fn byte_size(&self) -> Option<u64> {
        self.ggml_type.byte_size(self.element_count())
    }
}

/// A parsed GGUF header: metadata and tensor infos, no tensor data.
#[derive(Debug, Clone)]
pub struct Gguf {
    /// Format version (2 or 3).
    pub version: u32,
    /// Metadata key/value pairs (later duplicates replace earlier ones).
    pub metadata: BTreeMap<String, Value>,
    /// Tensor info table in file order.
    pub tensors: Vec<TensorInfo>,
    /// Alignment of the tensor data section (`general.alignment`, default 32).
    pub alignment: u64,
    /// Absolute file offset where tensor data starts (header end rounded up to `alignment`).
    pub data_offset: u64,
}

/// Reader wrapper that counts consumed bytes so the data offset can be computed.
struct Counting<R> {
    inner: R,
    pos: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

fn limit(what: &'static str, value: u64, limit: u64) -> Result<(), GgufError> {
    if value > limit {
        Err(GgufError::LimitExceeded { what, value, limit })
    } else {
        Ok(())
    }
}

struct Parser<R> {
    r: Counting<R>,
    lim: Limits,
}

impl<R: Read> Parser<R> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], GgufError> {
        let mut b = [0u8; N];
        self.r.read_exact(&mut b)?;
        Ok(b)
    }
    fn u32(&mut self) -> Result<u32, GgufError> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64, GgufError> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    /// Reads a length-prefixed string without allocating from the claimed length up front.
    fn string(&mut self) -> Result<String, GgufError> {
        let len = self.u64()?;
        limit("string length", len, self.lim.max_string)?;
        let mut buf = Vec::new();
        let got = (&mut self.r).take(len).read_to_end(&mut buf)?;
        if got as u64 != len {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        String::from_utf8(buf).map_err(|_| GgufError::InvalidUtf8)
    }

    fn value(&mut self, ty: ValueType, depth: u32) -> Result<Value, GgufError> {
        Ok(match ty {
            ValueType::U8 => Value::U8(self.bytes::<1>()?[0]),
            ValueType::I8 => Value::I8(i8::from_le_bytes(self.bytes()?)),
            ValueType::U16 => Value::U16(u16::from_le_bytes(self.bytes()?)),
            ValueType::I16 => Value::I16(i16::from_le_bytes(self.bytes()?)),
            ValueType::U32 => Value::U32(self.u32()?),
            ValueType::I32 => Value::I32(i32::from_le_bytes(self.bytes()?)),
            ValueType::F32 => Value::F32(f32::from_le_bytes(self.bytes()?)),
            ValueType::Bool => Value::Bool(self.bytes::<1>()?[0] != 0),
            ValueType::String => Value::String(self.string()?),
            ValueType::U64 => Value::U64(self.u64()?),
            ValueType::I64 => Value::I64(i64::from_le_bytes(self.bytes()?)),
            ValueType::F64 => Value::F64(f64::from_le_bytes(self.bytes()?)),
            ValueType::Array => {
                if depth >= self.lim.max_depth {
                    return Err(GgufError::Malformed("array nesting too deep"));
                }
                let elem = ValueType::from_id(self.u32()?)?;
                let len = self.u64()?;
                limit("array length", len, self.lim.max_array)?;
                let keep = len <= self.lim.keep_array;
                let mut items = Vec::new();
                for _ in 0..len {
                    let v = self.value(elem, depth + 1)?;
                    if keep {
                        items.push(v);
                    }
                }
                Value::Array { elem, len, items }
            }
        })
    }
}

impl Gguf {
    /// Parses the header, metadata and tensor infos from `reader`.
    pub fn read<R: Read>(reader: R) -> Result<Self, GgufError> {
        Self::read_with_limits(reader, Limits::default())
    }

    /// Like [`Gguf::read`] with explicit [`Limits`].
    pub fn read_with_limits<R: Read>(reader: R, lim: Limits) -> Result<Self, GgufError> {
        let mut p = Parser {
            r: Counting {
                inner: reader,
                pos: 0,
            },
            lim,
        };
        let magic = p.bytes::<4>()?;
        if &magic != b"GGUF" {
            return Err(GgufError::BadMagic(magic));
        }
        let version = p.u32()?;
        if !(2..=3).contains(&version) {
            return Err(GgufError::UnsupportedVersion(version));
        }
        let tensor_count = p.u64()?;
        let kv_count = p.u64()?;
        limit("tensor count", tensor_count, lim.max_tensors)?;
        limit("metadata count", kv_count, lim.max_kv)?;

        let mut metadata = BTreeMap::new();
        for _ in 0..kv_count {
            let key = p.string()?;
            let ty = ValueType::from_id(p.u32()?)?;
            let value = p.value(ty, 0)?;
            metadata.insert(key, value);
        }

        let mut tensors = Vec::new();
        for _ in 0..tensor_count {
            let name = p.string()?;
            let n_dims = p.u32()?;
            limit("tensor dimensions", n_dims.into(), lim.max_dims.into())?;
            let mut shape = Vec::new();
            for _ in 0..n_dims {
                shape.push(p.u64()?);
            }
            let ggml_type = GgmlType::from_id(p.u32()?);
            let offset = p.u64()?;
            tensors.push(TensorInfo {
                name,
                shape,
                ggml_type,
                offset,
            });
        }

        let alignment = metadata
            .get("general.alignment")
            .and_then(Value::as_u64)
            .filter(|a| *a > 0)
            .unwrap_or(32);
        let data_offset = p.r.pos.div_ceil(alignment).saturating_mul(alignment);
        Ok(Self {
            version,
            metadata,
            tensors,
            alignment,
            data_offset,
        })
    }

    /// Opens `path` read-only and parses it through a buffered reader. Tensor data is never
    /// read (a buffered reader streams only the header region).
    pub fn read_file(path: impl AsRef<Path>) -> Result<Self, GgufError> {
        let file = File::open(path)?;
        Self::read(BufReader::with_capacity(256 * 1024, file))
    }
}
