//! Model card (`README.md`) parsing: YAML front matter plus Markdown body.

use serde_json::Value;

use crate::error::{HfError, Result};

/// A parsed model card.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCard {
    /// Front matter as JSON (`Null` when the card has none).
    pub front_matter: Value,
    /// Markdown after the front matter.
    pub body: String,
}

impl ModelCard {
    /// Splits `readme` at the `---` fences and parses the YAML between them. A README without
    /// front matter yields `Null` and the whole text as body. A fence that never closes is
    /// treated as plain text; invalid YAML is an error.
    pub fn parse(readme: &str) -> Result<Self> {
        let text = readme.strip_prefix('\u{feff}').unwrap_or(readme);
        let Some(rest) = text
            .strip_prefix("---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
        else {
            return Ok(Self {
                front_matter: Value::Null,
                body: text.to_string(),
            });
        };
        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            if line.trim_end() == "---" {
                let yaml = &rest[..offset];
                let body = &rest[offset + line.len()..];
                let front_matter = if yaml.trim().is_empty() {
                    Value::Null
                } else {
                    serde_norway::from_str::<Value>(yaml)
                        .map_err(|e| HfError::FrontMatter(e.to_string()))?
                };
                return Ok(Self {
                    front_matter,
                    body: body.trim_start_matches(['\r', '\n']).to_string(),
                });
            }
            offset += line.len();
        }
        Ok(Self {
            front_matter: Value::Null,
            body: text.to_string(),
        })
    }

    /// First value of a field that may be a string or a list of strings.
    fn first_string(&self, key: &str) -> Option<String> {
        match self.front_matter.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Array(a) => a.first().and_then(Value::as_str).map(str::to_string),
            _ => None,
        }
    }

    /// `license` (SPDX-like id or `other`).
    pub fn license(&self) -> Option<String> {
        self.first_string("license")
    }

    /// `base_model` (first one when a list).
    pub fn base_model(&self) -> Option<String> {
        self.first_string("base_model")
    }

    /// `pipeline_tag`.
    pub fn pipeline_tag(&self) -> Option<String> {
        self.first_string("pipeline_tag")
    }

    /// `tags` as strings.
    pub fn tags(&self) -> Vec<String> {
        match self.front_matter.get("tags") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_and_body() {
        let card = ModelCard::parse(
            "---\nlicense: apache-2.0\nbase_model:\n  - meta/llama\ntags:\n- gguf\n- chat\npipeline_tag: text-generation\n---\n\n# Title\ntext\n",
        )
        .unwrap();
        assert_eq!(card.license().as_deref(), Some("apache-2.0"));
        assert_eq!(card.base_model().as_deref(), Some("meta/llama"));
        assert_eq!(card.pipeline_tag().as_deref(), Some("text-generation"));
        assert_eq!(card.tags(), ["gguf", "chat"]);
        assert_eq!(card.body, "# Title\ntext\n");
    }

    #[test]
    fn readme_without_front_matter_is_all_body() {
        let card = ModelCard::parse("# Just docs\n---\nnot yaml\n").unwrap();
        assert_eq!(card.front_matter, Value::Null);
        assert!(card.body.starts_with("# Just docs"));
        assert!(card.license().is_none());
    }

    #[test]
    fn unclosed_fence_is_plain_text() {
        let card = ModelCard::parse("---\nlicense: mit\nno end").unwrap();
        assert_eq!(card.front_matter, Value::Null);
    }

    #[test]
    fn invalid_yaml_is_an_error() {
        let err = ModelCard::parse("---\nkey: [unclosed\n---\nbody").unwrap_err();
        assert!(matches!(err, HfError::FrontMatter(_)));
    }

    #[test]
    fn crlf_line_endings_work() {
        let card = ModelCard::parse("---\r\nlicense: mit\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(card.license().as_deref(), Some("mit"));
        assert_eq!(card.body, "body\r\n");
    }
}
