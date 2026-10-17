//! An order-preserving JSON value and the text renderings decision models were trained on.
//!
//! Decision prompts depend on the order of object keys (questions are answered in request
//! order, a state object is shown to the model as written), and `serde_json::Value` sorts
//! keys unless the workspace-wide `preserve_order` feature is enabled. Enabling that feature
//! would change map ordering for every crate in the build, so this crate carries its own
//! small value type instead.

use std::fmt::{self, Write as _};

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::Number;

/// Nesting limit of every renderer: deeper input is rejected instead of recursing without
/// bound (the same 64 levels the reference implementations use).
pub const MAX_DEPTH: usize = 64;

/// A JSON value whose objects keep their keys in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number as `serde_json` parsed it (integer or `f64`).
    Number(Number),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, keys in document order. A repeated key keeps its first position and its
    /// last value, as ordered-map decoders do.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// The string, when this is a JSON string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// The members, when this is an object.
    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(m) => Some(m),
            _ => None,
        }
    }

    /// The elements, when this is an array.
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// `true` for `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// The value of `key` in an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_object()?
            .iter()
            .find_map(|(k, v)| (k == key).then_some(v))
    }

    /// Parses JSON text, keeping object key order.
    pub fn parse(text: &str) -> serde_json::Result<Json> {
        serde_json::from_str(text)
    }

    /// Compact standard JSON (what an HTTP body carries), keys in order.
    pub fn to_json_string(&self) -> String {
        // Serialising an in-memory tree of strings and numbers cannot fail; an empty string
        // would only show up if serde_json ever started rejecting finite numbers.
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json::String(s.to_owned())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Self {
        Json::String(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json::Bool(b)
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => s.serialize_unit(),
            Json::Bool(b) => s.serialize_bool(*b),
            Json::Number(n) => n.serialize(s),
            Json::String(v) => s.serialize_str(v),
            Json::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Json::Object(m) => {
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        Number::from_f64(v)
            .map(Json::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Json, D::Error> {
        Json::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(v) = seq.next_element()? {
            out.push(v);
        }
        Ok(Json::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut out: Vec<(String, Json)> = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            match out.iter_mut().find(|(existing, _)| *existing == k) {
                Some(slot) => slot.1 = v,
                None => out.push((k, v)),
            }
        }
        Ok(Json::Object(out))
    }
}

/// Why a value could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RenderError(pub String);

fn too_deep() -> RenderError {
    RenderError(format!("content nesting exceeds {MAX_DEPTH} levels"))
}

/// Separators and layout of a Python `json.dumps` call.
#[derive(Debug, Clone, Copy)]
struct PyStyle {
    indent: usize,
    item_sep: &'static str,
    key_sep: &'static str,
    sort_keys: bool,
}

/// `json.dumps(v, ensure_ascii=False)`, indented by `indent` spaces when `indent > 0`
/// (Python then drops the space after `,`).
pub fn python_json(v: &Json, indent: usize) -> Result<String, RenderError> {
    let style = PyStyle {
        indent,
        item_sep: if indent > 0 { "," } else { ", " },
        key_sep: ": ",
        sort_keys: false,
    };
    let mut out = String::new();
    py_render(v, style, 0, &mut out)?;
    Ok(out)
}

/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"), sort_keys=True)`: the compact
/// form the clef encoder uses, whose reference decodes objects into maps and therefore sorts
/// keys (byte order).
pub fn python_json_compact_sorted(v: &Json) -> Result<String, RenderError> {
    let style = PyStyle {
        indent: 0,
        item_sep: ",",
        key_sep: ":",
        sort_keys: true,
    };
    let mut out = String::new();
    py_render(v, style, 0, &mut out)?;
    Ok(out)
}

fn py_render(v: &Json, st: PyStyle, depth: usize, out: &mut String) -> Result<(), RenderError> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    let newline = |out: &mut String, level: usize| {
        if st.indent > 0 {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', st.indent * level));
        }
    };
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&py_number(n)),
        Json::String(s) => py_string(s, out),
        Json::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(st.item_sep);
                }
                newline(out, depth + 1);
                py_render(item, st, depth + 1, out)?;
            }
            if !a.is_empty() {
                newline(out, depth);
            }
            out.push(']');
        }
        Json::Object(m) => {
            let mut members: Vec<&(String, Json)> = m.iter().collect();
            if st.sort_keys {
                members.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            }
            out.push('{');
            for (i, (k, item)) in members.iter().enumerate() {
                if i > 0 {
                    out.push_str(st.item_sep);
                }
                newline(out, depth + 1);
                py_string(k, out);
                out.push_str(st.key_sep);
                py_render(item, st, depth + 1, out)?;
            }
            if !members.is_empty() {
                newline(out, depth);
            }
            out.push('}');
        }
    }
    Ok(())
}

/// A JSON string with Python's `ensure_ascii=False` escaping: only quotes, backslashes and
/// control characters are escaped, everything else (including `<`, `&`, U+2028) is literal.
pub(crate) fn py_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A number the way Python prints it: integers verbatim, floats with `repr` (always a
/// fraction or an exponent; exponent form outside `[1e-4, 1e16)`).
fn py_number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f.is_infinite() {
        return if f < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    let a = f.abs();
    if a >= 1e16 || (a != 0.0 && a < 1e-4) {
        exp_form(f)
    } else {
        fixed_form(f)
    }
}

/// Shortest round-trip digits without exponent, with a `.0` when integral.
fn fixed_form(f: f64) -> String {
    let mut s = format!("{f}");
    if !s.contains('.') {
        s.push_str(".0");
    }
    s
}

/// Shortest round-trip digits in exponent form with a signed, at least two-digit exponent
/// (`1e-05`, `1.5e+16`), as both Python `repr` and Go `strconv.FormatFloat(f, 'e', -1, 64)`
/// print it.
fn exp_form(f: f64) -> String {
    let s = format!("{f:e}");
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exp.unsigned_abs())
}

/// The kev rendering: the indented `key: value` / `- item` lines Kev was trained on, with
/// Python scalars (`True`, `False`, empty for `null`).
pub fn kev_text(v: &Json) -> Result<String, RenderError> {
    Ok(kev_value(v, 0)?.0)
}

fn kev_value(v: &Json, depth: usize) -> Result<(String, bool), RenderError> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    let pad = "  ".repeat(depth);
    match v {
        Json::Object(m) => {
            let mut lines = Vec::with_capacity(m.len());
            for (k, item) in m {
                let (value, nested) = kev_value(item, depth + 1)?;
                let sep = if nested { ":\n" } else { ": " };
                lines.push(format!("{pad}{k}{sep}{value}"));
            }
            Ok((lines.join("\n"), true))
        }
        Json::Array(a) => {
            let mut lines = Vec::with_capacity(a.len());
            for item in a {
                let (value, _) = kev_value(item, depth + 1)?;
                lines.push(format!("{pad}- {}", value.trim_start()));
            }
            Ok((lines.join("\n"), true))
        }
        scalar => Ok((kev_scalar(scalar), false)),
    }
}

fn kev_scalar(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::String(s) => s.clone(),
        Json::Number(n) => {
            if n.is_i64() || n.is_u64() {
                return n.to_string();
            }
            let f = n.as_f64().unwrap_or(0.0);
            let a = f.abs();
            if f == 0.0 || (1e-4..1e16).contains(&a) {
                fixed_form(f)
            } else {
                exp_form(f)
            }
        }
        // Containers are handled by `kev_value`.
        other => other.to_json_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(s: &str) -> Json {
        Json::parse(s).unwrap()
    }

    #[test]
    fn keeps_object_key_order() {
        let v = j(r#"{"z":1,"a":{"y":true,"b":null}}"#);
        assert_eq!(v.to_json_string(), r#"{"z":1,"a":{"y":true,"b":null}}"#);
    }

    #[test]
    fn repeated_key_keeps_first_position_last_value() {
        let v = j(r#"{"a":1,"b":2,"a":3}"#);
        assert_eq!(v.to_json_string(), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn python_separators_and_escaping() {
        let v = j(r#"{"t":"Paid €20 & <tag> \"q\"\n","n":[1,2.0,1e-5,1e16,0.5]}"#);
        assert_eq!(
            python_json(&v, 0).unwrap(),
            r#"{"t": "Paid €20 & <tag> \"q\"\n", "n": [1, 2.0, 1e-05, 1e+16, 0.5]}"#
        );
    }

    #[test]
    fn python_indent_drops_space_after_comma() {
        let v = j(r#"{"a":[1,{"b":"c"}],"e":{}}"#);
        assert_eq!(
            python_json(&v, 2).unwrap(),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": \"c\"\n    }\n  ],\n  \"e\": {}\n}"
        );
    }

    #[test]
    fn compact_sorted_sorts_keys() {
        let v = j(r#"{"option_id":"x","description":{"z":1,"a":2}}"#);
        assert_eq!(
            python_json_compact_sorted(&v).unwrap(),
            r#"{"description":{"a":2,"z":1},"option_id":"x"}"#
        );
    }

    #[test]
    fn control_characters_use_unicode_escapes() {
        let mut out = String::new();
        py_string("a\u{1}b", &mut out);
        assert_eq!(out, r#""a\u0001b""#);
    }

    #[test]
    fn kev_lines() {
        let v =
            j(r#"{"user":{"name":"Ann","vip":true},"items":[{"sku":1},"x"],"note":null,"p":0.25}"#);
        assert_eq!(
            kev_text(&v).unwrap(),
            "user:\n  name: Ann\n  vip: True\nitems:\n  - sku: 1\n  - x\nnote: \np: 0.25"
        );
        assert_eq!(kev_text(&j("3.0")).unwrap(), "3.0");
        assert_eq!(kev_text(&j("0.00001")).unwrap(), "1e-05");
    }

    #[test]
    fn rejects_runaway_nesting() {
        let deep = format!("{}{}", "[".repeat(70), "]".repeat(70));
        let v = j(&deep);
        assert!(python_json(&v, 0).is_err());
        assert!(kev_text(&v).is_err());
    }
}
