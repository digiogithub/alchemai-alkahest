//! GGUF header and metadata reader, plus the quantisation and format table (including PQ2_0 and
//! PTQ1_0) used for engine selection and memory estimates.
//!
//! The reader streams the header, the metadata key/value section and the tensor info table from
//! any [`std::io::Read`]; tensor data is never loaded. Every length read from the file is
//! bounded by [`Limits`] and nothing is pre-allocated from an untrusted count, so a malicious
//! file cannot force a large allocation.

mod accessors;
mod classify;
mod error;
mod ggml;
mod reader;
mod value;

pub use accessors::PoolingType;
pub use classify::{Classification, ClassifyInput, ModelFormat, ModelKind, classify};
pub use error::GgufError;
pub use ggml::{GgmlType, QuantFamily, file_type_name};
pub use reader::{Gguf, Limits, TensorInfo};
pub use value::{Value, ValueType};
