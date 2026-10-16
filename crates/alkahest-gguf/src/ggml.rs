//! ggml tensor type table and `general.file_type` names.
//!
//! Ids of the mainline types follow `ggml.h`. `Q2_0 = 42` and `PQ2_0 = 142` come from the
//! story acceptance criteria. `PTQ1_0 = 143` is an UNCONFIRMED guess (the PrismML fork source
//! was not available offline); it is isolated in [`GgmlType::PTQ1_0_ID`] so one constant fixes
//! it. Block geometry of the three Bonsai types is likewise approximate (it only feeds memory
//! estimates, never decoding).

use std::fmt;

/// Which engine family can load a quantisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantFamily {
    /// Loadable by mainline llama.cpp.
    Mainline,
    /// Only the PrismML llama.cpp fork understands it.
    PrismMl,
    /// Unknown type id; assume mainline cannot load it.
    Unknown,
}

/// A ggml tensor element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GgmlType {
    /// 32-bit float.
    F32,
    /// 16-bit float.
    F16,
    /// 4-bit, scale only.
    Q4_0,
    /// 4-bit, scale and min.
    Q4_1,
    /// 5-bit, scale only.
    Q5_0,
    /// 5-bit, scale and min.
    Q5_1,
    /// 8-bit, scale only.
    Q8_0,
    /// 8-bit, scale and sum.
    Q8_1,
    /// 2-bit k-quant.
    Q2K,
    /// 3-bit k-quant.
    Q3K,
    /// 4-bit k-quant.
    Q4K,
    /// 5-bit k-quant.
    Q5K,
    /// 6-bit k-quant.
    Q6K,
    /// 8-bit k-quant.
    Q8K,
    /// i-quant 2.06 bpw.
    IQ2XXS,
    /// i-quant 2.31 bpw.
    IQ2XS,
    /// i-quant 3.06 bpw.
    IQ3XXS,
    /// i-quant 1.56 bpw.
    IQ1S,
    /// i-quant 4-bit non-linear.
    IQ4NL,
    /// i-quant 3.44 bpw.
    IQ3S,
    /// i-quant 2.5 bpw.
    IQ2S,
    /// i-quant 4.25 bpw.
    IQ4XS,
    /// signed 8-bit integer.
    I8,
    /// signed 16-bit integer.
    I16,
    /// signed 32-bit integer.
    I32,
    /// signed 64-bit integer.
    I64,
    /// 64-bit float.
    F64,
    /// i-quant 1.75 bpw.
    IQ1M,
    /// bfloat16.
    BF16,
    /// ternary 1.69 bpw.
    TQ1_0,
    /// ternary 2.06 bpw.
    TQ2_0,
    /// microscaling FP4.
    MXFP4,
    /// 2-bit quantisation (mainline).
    Q2_0,
    /// Packed Q2_0 (PrismML fork only).
    PQ2_0,
    /// Dense ternary 1.75 bpw (PrismML fork only).
    PTQ1_0,
    /// A type id this table does not know.
    Unknown(u32),
}

impl GgmlType {
    /// Type id of `Q2_0`.
    pub const Q2_0_ID: u32 = 42;
    /// Type id of `PQ2_0`.
    pub const PQ2_0_ID: u32 = 142;
    /// Type id of `PTQ1_0`. UNCONFIRMED, see the module docs.
    pub const PTQ1_0_ID: u32 = 143;

    /// Decodes a wire type id.
    pub fn from_id(id: u32) -> Self {
        match id {
            0 => Self::F32,
            1 => Self::F16,
            2 => Self::Q4_0,
            3 => Self::Q4_1,
            6 => Self::Q5_0,
            7 => Self::Q5_1,
            8 => Self::Q8_0,
            9 => Self::Q8_1,
            10 => Self::Q2K,
            11 => Self::Q3K,
            12 => Self::Q4K,
            13 => Self::Q5K,
            14 => Self::Q6K,
            15 => Self::Q8K,
            16 => Self::IQ2XXS,
            17 => Self::IQ2XS,
            18 => Self::IQ3XXS,
            19 => Self::IQ1S,
            20 => Self::IQ4NL,
            21 => Self::IQ3S,
            22 => Self::IQ2S,
            23 => Self::IQ4XS,
            24 => Self::I8,
            25 => Self::I16,
            26 => Self::I32,
            27 => Self::I64,
            28 => Self::F64,
            29 => Self::IQ1M,
            30 => Self::BF16,
            34 => Self::TQ1_0,
            35 => Self::TQ2_0,
            39 => Self::MXFP4,
            Self::Q2_0_ID => Self::Q2_0,
            Self::PQ2_0_ID => Self::PQ2_0,
            Self::PTQ1_0_ID => Self::PTQ1_0,
            other => Self::Unknown(other),
        }
    }

    /// The wire type id.
    pub fn id(self) -> u32 {
        match self {
            Self::F32 => 0,
            Self::F16 => 1,
            Self::Q4_0 => 2,
            Self::Q4_1 => 3,
            Self::Q5_0 => 6,
            Self::Q5_1 => 7,
            Self::Q8_0 => 8,
            Self::Q8_1 => 9,
            Self::Q2K => 10,
            Self::Q3K => 11,
            Self::Q4K => 12,
            Self::Q5K => 13,
            Self::Q6K => 14,
            Self::Q8K => 15,
            Self::IQ2XXS => 16,
            Self::IQ2XS => 17,
            Self::IQ3XXS => 18,
            Self::IQ1S => 19,
            Self::IQ4NL => 20,
            Self::IQ3S => 21,
            Self::IQ2S => 22,
            Self::IQ4XS => 23,
            Self::I8 => 24,
            Self::I16 => 25,
            Self::I32 => 26,
            Self::I64 => 27,
            Self::F64 => 28,
            Self::IQ1M => 29,
            Self::BF16 => 30,
            Self::TQ1_0 => 34,
            Self::TQ2_0 => 35,
            Self::MXFP4 => 39,
            Self::Q2_0 => Self::Q2_0_ID,
            Self::PQ2_0 => Self::PQ2_0_ID,
            Self::PTQ1_0 => Self::PTQ1_0_ID,
            Self::Unknown(id) => id,
        }
    }

    /// Canonical upper-case name (`Q4_K`, `PQ2_0`, ...); `UNKNOWN(<id>)` for unknown ids.
    pub fn name(self) -> String {
        let s = match self {
            Self::Q2K => "Q2_K",
            Self::Q3K => "Q3_K",
            Self::Q4K => "Q4_K",
            Self::Q5K => "Q5_K",
            Self::Q6K => "Q6_K",
            Self::Q8K => "Q8_K",
            Self::IQ2XXS => "IQ2_XXS",
            Self::IQ2XS => "IQ2_XS",
            Self::IQ3XXS => "IQ3_XXS",
            Self::IQ1S => "IQ1_S",
            Self::IQ4NL => "IQ4_NL",
            Self::IQ3S => "IQ3_S",
            Self::IQ2S => "IQ2_S",
            Self::IQ4XS => "IQ4_XS",
            Self::IQ1M => "IQ1_M",
            Self::Unknown(id) => return format!("UNKNOWN({id})"),
            other => return format!("{other:?}"),
        };
        s.to_owned()
    }

    /// Engine family able to load tensors of this type.
    pub fn family(self) -> QuantFamily {
        match self {
            Self::PQ2_0 | Self::PTQ1_0 => QuantFamily::PrismMl,
            Self::Unknown(_) => QuantFamily::Unknown,
            _ => QuantFamily::Mainline,
        }
    }

    /// `(elements per block, bytes per block)`, or `None` for unknown types. Bonsai types are
    /// approximations good enough for memory estimates.
    pub fn block_layout(self) -> Option<(u64, u64)> {
        Some(match self {
            Self::F32 | Self::I32 => (1, 4),
            Self::F16 | Self::BF16 | Self::I16 => (1, 2),
            Self::F64 | Self::I64 => (1, 8),
            Self::I8 => (1, 1),
            Self::Q4_0 | Self::IQ4NL => (32, 18),
            Self::Q4_1 => (32, 20),
            Self::Q5_0 => (32, 22),
            Self::Q5_1 => (32, 24),
            Self::Q8_0 => (32, 34),
            Self::Q8_1 => (32, 36),
            Self::Q2K => (256, 84),
            Self::Q3K | Self::IQ3S => (256, 110),
            Self::Q4K => (256, 144),
            Self::Q5K => (256, 176),
            Self::Q6K => (256, 210),
            Self::Q8K => (256, 292),
            Self::IQ2XXS | Self::TQ2_0 => (256, 66),
            Self::IQ2XS => (256, 74),
            Self::IQ3XXS => (256, 98),
            Self::IQ1S => (256, 50),
            Self::IQ2S => (256, 82),
            Self::IQ4XS => (256, 136),
            Self::IQ1M => (256, 56),
            Self::TQ1_0 => (256, 54),
            Self::MXFP4 => (32, 17),
            Self::Q2_0 => (32, 10),
            Self::PQ2_0 => (256, 68),
            Self::PTQ1_0 => (256, 56),
            Self::Unknown(_) => return None,
        })
    }

    /// Bytes needed for `elements` elements (rounded up to whole blocks), or `None` if the
    /// type is unknown or the size overflows.
    pub fn byte_size(self, elements: u64) -> Option<u64> {
        let (block, bytes) = self.block_layout()?;
        elements.div_ceil(block).checked_mul(bytes)
    }
}

impl fmt::Display for GgmlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// Human name of a `general.file_type` value (`LLAMA_FTYPE_*`), e.g. 15 gives `Q4_K_M`.
pub fn file_type_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4_MOE",
        _ => return None,
    })
}
