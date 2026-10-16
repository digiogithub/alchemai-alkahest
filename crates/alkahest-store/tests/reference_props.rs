//! Property tests for model references (ALK-SP-0001.R3): every value formats back to a string
//! that parses to the same value, and no input makes the parser panic.

use alkahest_store::{Digest, HfRef, HfSelector, ModelName, ModelRef, Origin, ScopedRef};
use proptest::prelude::*;

/// An Ollama name part: first char alphanumeric or `_`, then the part's extra characters.
fn part(extra: &'static str, max: usize) -> impl Strategy<Value = String> {
    let class = format!("[A-Za-z0-9_][A-Za-z0-9_{extra}]{{0,{}}}", max - 1);
    proptest::string::string_regex(&class).unwrap_or_else(|_| unreachable!())
}

fn host() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("registry.ollama.ai".to_string()),
        Just("ollama.com".to_string()),
        Just("localhost:5000".to_string()),
        part(".-", 30).prop_filter("not a Hugging Face host", |h| {
            let l = h.to_ascii_lowercase();
            !["hf.co", "huggingface.co", "www.huggingface.co"].contains(&l.as_str())
        }),
    ]
}

fn model_name() -> impl Strategy<Value = ModelName> {
    (host(), part("-", 20), part(".-", 30), part(".-", 20))
        .prop_filter_map("valid name", |(h, n, m, t)| ModelName::new(h, n, m, t).ok())
}

fn digest() -> impl Strategy<Value = Digest> {
    any::<[u8; 32]>().prop_map(Digest::from_bytes)
}

fn hf_ref() -> impl Strategy<Value = HfRef> {
    let selector = prop_oneof![
        Just(None),
        part("-", 12).prop_map(|q| Some(HfSelector::Quant(q))),
        (part(".-", 20)).prop_map(|f| Some(HfSelector::File(format!("{f}.gguf")))),
        (part("-", 8), part(".-", 20))
            .prop_map(|(d, f)| Some(HfSelector::File(format!("{d}/{f}.GGUF")))),
    ];
    (part(".-", 20), part(".-", 30), selector)
        .prop_filter_map("valid hf ref", |(o, r, s)| HfRef::new(o, r, s).ok())
}

fn model_ref() -> impl Strategy<Value = ModelRef> {
    prop_oneof![
        (model_name(), proptest::option::of(digest()))
            .prop_map(|(name, digest)| ModelRef::Name { name, digest }),
        hf_ref().prop_map(ModelRef::HuggingFace),
        digest().prop_map(ModelRef::Digest),
    ]
}

proptest! {
    #[test]
    fn model_name_round_trips(n in model_name()) {
        prop_assert_eq!(n.to_string().parse::<ModelName>().unwrap(), n.clone());
        // The short form resolves to the same name through the defaults.
        prop_assert_eq!(n.display_short().parse::<ModelName>().unwrap(), n.clone());
        // The on-disk path decodes back to the name.
        let path = n.relative_path();
        let parts: Vec<String> = path.iter().map(|c| c.to_string_lossy().into_owned()).collect();
        prop_assert_eq!(parts.len(), 4);
        let back = ModelName::from_relative_parts(&parts[0], &parts[1], &parts[2], &parts[3]).unwrap();
        prop_assert_eq!(back, n);
    }

    #[test]
    fn model_ref_round_trips(r in model_ref()) {
        prop_assert_eq!(r.to_string().parse::<ModelRef>().unwrap(), r);
    }

    #[test]
    fn scoped_ref_round_trips(
        origin in proptest::option::of(prop_oneof![Just(Origin::Ollama), Just(Origin::Alkahest)]),
        reference in model_ref(),
    ) {
        let s = ScopedRef { origin, reference };
        prop_assert_eq!(s.to_string().parse::<ScopedRef>().unwrap(), s);
    }

    #[test]
    fn hf_store_name_maps_back(h in hf_ref()) {
        if let Ok(name) = h.to_model_name() {
            let back = name.to_hf().unwrap();
            prop_assert_eq!(back.repo_id(), h.repo_id());
            if !matches!(h.selector(), Some(HfSelector::File(f)) if f.contains('/')) {
                prop_assert_eq!(back, h);
            }
        }
    }

    #[test]
    fn parser_never_panics(s in "\\PC{0,80}") {
        let _ = s.parse::<ScopedRef>();
        let _ = s.parse::<ModelName>();
        let _ = s.parse::<Digest>();
    }

    #[test]
    fn parsed_names_are_safe_path_components(s in "[A-Za-z0-9_./:@-]{1,60}") {
        if let Ok(r) = s.parse::<ModelRef>()
            && let Ok(Some(name)) = r.store_name()
        {
            for c in name.relative_path().components() {
                prop_assert!(matches!(c, std::path::Component::Normal(_)), "{c:?} in {s}");
            }
        }
    }
}
