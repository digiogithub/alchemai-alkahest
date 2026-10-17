//! Wire types of `POST /v1/systemone` (the Jev / TypeSafe System One contract as Ollama
//! >= 0.35 and llama-server serve it).
//!
//! Questions and answers are JSON objects whose key order is meaningful (answers come back
//! in question order), so both are ordered lists here, serialised as objects.

use std::fmt;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

use crate::json::Json;

/// Body of `POST /v1/systemone`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemOneRequest {
    /// Model name (`tev1:0.8b`).
    #[serde(default)]
    pub model: String,
    /// Ollama extension: runner override. Accepted and ignored; Alkahest picks the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// What the questions are about: a string, an object or an array. `None` when absent;
    /// `Some(Json::Null)` when sent as `null`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub state: Option<Json>,
    /// Images, base64 (Ollama) or data URLs (llama-server). Only models with image input
    /// accept them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Videos; part of the Ollama shape, never supported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub videos: Vec<Json>,
    /// Questions by id, in request order.
    #[serde(default)]
    pub questions: Questions,
    /// How long the model stays loaded (`"5m"`, seconds, `-1`); interpreted by the API layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<serde_json::Value>,
}

/// Deserialises a field so that an explicit `null` is `Some(Json::Null)`, not `None`:
/// several encodings treat "absent" and "null" differently.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(d).map(Some)
}

/// The question type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum QuestionType {
    /// Yes / no; the answer is the probability of yes. The default when `type` is omitted.
    #[default]
    Noul,
    /// One of several labelled options.
    Choice,
    /// An ordered scale of 2 to 10 levels.
    Score,
    /// Anything else, kept so the compiler can reject it with a precise message.
    Other(String),
}

impl QuestionType {
    /// The wire name.
    pub fn as_str(&self) -> &str {
        match self {
            QuestionType::Noul => "noul",
            QuestionType::Choice => "choice",
            QuestionType::Score => "score",
            QuestionType::Other(s) => s,
        }
    }

    /// The numeric type id model-native decision heads use (0 noul, 1 choice, 2 score).
    pub fn head_id(&self) -> u8 {
        match self {
            QuestionType::Choice => 1,
            QuestionType::Score => 2,
            _ => 0,
        }
    }
}

impl fmt::Display for QuestionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for QuestionType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QuestionType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Option::<String>::deserialize(d)?.unwrap_or_default();
        Ok(match s.as_str() {
            "" | "noul" => QuestionType::Noul,
            "choice" => QuestionType::Choice,
            "score" => QuestionType::Score,
            _ => QuestionType::Other(s),
        })
    }
}

/// One question.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Question {
    /// Question type; `noul` when omitted.
    #[serde(rename = "type", default)]
    pub kind: QuestionType,
    /// The question: a string, an object or an array.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub instructions: Option<Json>,
    /// Allowed answers: `{option: description|null}` for choice, `[level, ...]` for score,
    /// optional `{"true": .., "false": ..}` for noul.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub criteria: Option<Json>,
}

impl Question {
    /// A question of `kind` with string instructions and no criteria.
    pub fn new(kind: QuestionType, instructions: impl Into<String>) -> Self {
        Self {
            kind,
            instructions: Some(Json::String(instructions.into())),
            criteria: None,
        }
    }

    /// The same question with `criteria`.
    pub fn with_criteria(mut self, criteria: Json) -> Self {
        self.criteria = Some(criteria);
        self
    }
}

/// Questions by id in request order (a JSON object on the wire).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Questions(pub Vec<(String, Question)>);

impl Questions {
    /// Number of questions.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Questions in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Question)> {
        self.0.iter().map(|(k, q)| (k.as_str(), q))
    }

    /// Appends or replaces (keeping the position) the question `id`.
    pub fn insert(&mut self, id: impl Into<String>, q: Question) {
        let id = id.into();
        match self.0.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = q,
            None => self.0.push((id, q)),
        }
    }
}

impl Serialize for Questions {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, q) in &self.0 {
            map.serialize_entry(k, q)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Questions {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Questions;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object mapping question ids to questions")
            }
            fn visit_unit<E>(self) -> Result<Questions, E> {
                Ok(Questions::default())
            }
            fn visit_none<E>(self) -> Result<Questions, E> {
                Ok(Questions::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Questions, A::Error> {
                let mut out = Questions::default();
                while let Some((k, q)) = map.next_entry::<String, Question>()? {
                    out.insert(k, q);
                }
                Ok(out)
            }
        }
        d.deserialize_any(V)
    }
}

/// Token accounting of a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens of all questions, shared tokens counted once per question.
    #[serde(default)]
    pub input_tokens: u64,
    /// Tokens generated internally to read the candidate distribution (one per question on
    /// the prompt-scoring path, 0 for native heads).
    #[serde(default)]
    pub output_tokens: u64,
}

/// An ordered `{key: value}` object.
pub type OrderedMap<V> = Vec<(String, V)>;

/// One answer; the variant follows the question type.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// `{"type":"noul","noul":p}`: probability that the answer is yes.
    Noul {
        /// Probability of `true`.
        noul: f64,
    },
    /// `{"type":"choice","choice":..,"probabilities":{..},"confidence":..}`.
    Choice {
        /// The most probable option (first one on ties).
        choice: String,
        /// Probability of each option, request order; they sum to 1.
        probabilities: OrderedMap<f64>,
        /// 0 when all options are equally likely, 1 when one has all the mass.
        confidence: f64,
    },
    /// `{"type":"score","score":..,"legend":{..},"probabilities":{..},"confidence":..}`.
    Score {
        /// Expected level index (can lie between levels).
        score: f64,
        /// Description of each level index (`"0"`, `"1"`, ...).
        legend: OrderedMap<Json>,
        /// Probability of each level index.
        probabilities: OrderedMap<f64>,
        /// 0 to 1.
        confidence: f64,
    },
}

impl Answer {
    /// The `type` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Answer::Noul { .. } => "noul",
            Answer::Choice { .. } => "choice",
            Answer::Score { .. } => "score",
        }
    }
}

/// Serialises an ordered map as a JSON object.
struct ObjectOf<'a, V>(&'a [(String, V)]);

impl<V: Serialize> Serialize for ObjectOf<'_, V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl Serialize for Answer {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Answer::Noul { noul } => {
                let mut st = s.serialize_struct("Answer", 2)?;
                st.serialize_field("type", "noul")?;
                st.serialize_field("noul", noul)?;
                st.end()
            }
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                let mut st = s.serialize_struct("Answer", 4)?;
                st.serialize_field("type", "choice")?;
                st.serialize_field("choice", choice)?;
                st.serialize_field("probabilities", &ObjectOf(probabilities))?;
                st.serialize_field("confidence", confidence)?;
                st.end()
            }
            Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => {
                let mut st = s.serialize_struct("Answer", 5)?;
                st.serialize_field("type", "score")?;
                st.serialize_field("score", score)?;
                st.serialize_field("legend", &ObjectOf(legend))?;
                st.serialize_field("probabilities", &ObjectOf(probabilities))?;
                st.serialize_field("confidence", confidence)?;
                st.end()
            }
        }
    }
}

impl TryFrom<&Json> for Answer {
    type Error = String;

    /// Reads an answer object (from llama-server's native `/v1/systemone` or a client).
    fn try_from(v: &Json) -> Result<Self, String> {
        let num = |key: &str| -> Result<f64, String> {
            match v.get(key) {
                Some(Json::Number(n)) => n.as_f64().ok_or_else(|| format!("`{key}` not finite")),
                _ => Err(format!("answer lacks numeric `{key}`")),
            }
        };
        let probs = || -> Result<OrderedMap<f64>, String> {
            let m = v
                .get("probabilities")
                .and_then(Json::as_object)
                .ok_or("answer lacks `probabilities`")?;
            m.iter()
                .map(|(k, p)| match p {
                    Json::Number(n) => n
                        .as_f64()
                        .map(|f| (k.clone(), f))
                        .ok_or_else(|| "probability not finite".to_string()),
                    _ => Err("probability is not a number".to_string()),
                })
                .collect()
        };
        match v.get("type").and_then(Json::as_str) {
            Some("noul") => Ok(Answer::Noul { noul: num("noul")? }),
            Some("choice") => Ok(Answer::Choice {
                choice: v
                    .get("choice")
                    .and_then(Json::as_str)
                    .ok_or("answer lacks `choice`")?
                    .to_owned(),
                probabilities: probs()?,
                confidence: num("confidence")?,
            }),
            Some("score") => Ok(Answer::Score {
                score: num("score")?,
                legend: v
                    .get("legend")
                    .and_then(Json::as_object)
                    .map(<[_]>::to_vec)
                    .unwrap_or_default(),
                probabilities: probs()?,
                confidence: num("confidence")?,
            }),
            other => Err(format!("unknown answer type {other:?}")),
        }
    }
}

impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        Answer::try_from(&v).map_err(de::Error::custom)
    }
}

/// Answers by question id, in question order (a JSON object on the wire).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answers(pub Vec<(String, Answer)>);

impl Answers {
    /// The answer to question `id`.
    pub fn get(&self, id: &str) -> Option<&Answer> {
        self.0.iter().find_map(|(k, a)| (k == id).then_some(a))
    }

    /// Number of answers.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Answers {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        ObjectOf(&self.0).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Answers {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        let m = v
            .as_object()
            .ok_or_else(|| de::Error::custom("answers must be an object"))?;
        m.iter()
            .map(|(k, a)| Answer::try_from(a).map(|a| (k.clone(), a)))
            .collect::<Result<Vec<_>, _>>()
            .map(Answers)
            .map_err(de::Error::custom)
    }
}

/// Body of a successful `POST /v1/systemone` response.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SystemOneResponse {
    /// The model as the request named it.
    pub model: String,
    /// Answers by question id, request order.
    pub answers: Answers,
    /// Token accounting.
    #[serde(default)]
    pub usage: Usage,
    /// Ollama extension: prompt tokens restored from cache instead of evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_eval_cached_count: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_keeps_question_order_and_null_criteria() {
        let text = r#"{"model":"m","state":{"b":1,"a":2},"questions":{"z":{"type":"choice","instructions":"Pick","criteria":{"x":null,"y":"why"}},"a":{"instructions":"Yes?"},"n":{"type":"noul","instructions":"N","criteria":null}},"keep_alive":"5m"}"#;
        let req: SystemOneRequest = serde_json::from_str(text).unwrap();
        let ids: Vec<_> = req.questions.iter().map(|(k, _)| k).collect();
        assert_eq!(ids, ["z", "a", "n"]);
        assert_eq!(req.questions.0[1].1.kind, QuestionType::Noul);
        assert_eq!(req.questions.0[2].1.criteria, Some(Json::Null));
        assert_eq!(req.questions.0[1].1.criteria, None);
        let back = serde_json::to_string(&req).unwrap();
        assert!(back.starts_with(r#"{"model":"m","state":{"b":1,"a":2},"questions":{"z":"#));
    }

    #[test]
    fn unknown_type_is_kept_for_the_compiler() {
        let q: Question = serde_json::from_str(r#"{"type":"rank","instructions":"x"}"#).unwrap();
        assert_eq!(q.kind, QuestionType::Other("rank".into()));
    }

    #[test]
    fn response_matches_reference_shape() {
        // Shape of the llama-server README example (values shortened there too).
        let text = r#"{"model":"openjev","answers":{"route":{"type":"choice","choice":"billing","probabilities":{"billing":0.9998,"shipping":0.0001,"technical":0.0001},"confidence":0.9997},"angry":{"type":"noul","noul":0.6328},"urgency":{"type":"score","score":2.0858,"legend":{"0":"can wait","1":"this week","2":"today","3":"right now"},"probabilities":{"0":0.0023,"1":0.116,"2":0.6753,"3":0.2064},"confidence":0.673}},"usage":{"input_tokens":239,"output_tokens":0}}"#;
        let resp: SystemOneResponse = serde_json::from_str(text).unwrap();
        assert_eq!(resp.answers.len(), 3);
        assert_eq!(serde_json::to_string(&resp).unwrap(), text);
    }
}
