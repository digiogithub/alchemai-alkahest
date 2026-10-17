//! Decision request compiler and logit-based scoring behind the decision API.
//!
//! Decision models answer typed questions about a `state` with probabilities in one pass,
//! without generating text (spec ALK-SP-0007). This crate owns:
//!
//! - the `/v1/systemone` wire types ([`SystemOneRequest`], [`SystemOneResponse`]);
//! - choosing how a model is served from its metadata ([`DecisionMetadata::plan`]);
//! - the compiler that validates a request and builds scoring input for an [`Encoding`]
//!   ([`compile`]) and turns candidate scores back into answers ([`Compiled::answer`]).

#![cfg_attr(test, allow(clippy::float_cmp))]

pub mod compile;
pub mod encoding;
pub mod error;
pub mod json;
pub mod types;

pub use compile::{
    ChatMessage, Choice, ChoiceValue, Compiled, CompiledField, MAX_QUESTIONS, PointerRow,
    PromptRow, RowQuestion, ScoreInput, ScoreOutput, SegmentField, compile, validate_limits,
};
pub use encoding::{DecisionBackend, DecisionMetadata, DecisionPlan, Encoding, TEV1_SYSTEM};
pub use error::{DecisionError, Result};
pub use json::Json;
pub use types::{
    Answer, Answers, Question, QuestionType, Questions, SystemOneRequest, SystemOneResponse, Usage,
};
