//! Built-in engine ids, their aliases, and the naming rules of fork engines (ALK-US-0132).
//!
//! The built-in engines are the adapter families Alkahest knows how to drive. A recipe whose id
//! is not one of them is a *fork engine*: it must say which family's adapter runs it with
//! `base = "<family id>"` (for example a TurboQuant build of llama.cpp with
//! `base = "llama-cpp"`). A fork keeps its own id, install directory and overrides, and is never
//! chosen automatically; a model opts in with its `engine` setting.
//!
//! The alias table lives here, below the engine crate, so that recipe validation and
//! `alkahest_engine::EngineId::from_str` share one definition.

/// Canonical ids of the built-in engines, in display order.
pub const BUILTIN_ENGINE_IDS: [&str; 5] = [
    "llama-cpp",
    "prism-llama-cpp",
    "vllm",
    "whisper-cpp",
    "stable-diffusion-cpp",
];

/// Spellings users type for the built-in engines, mapped to the canonical id. The keys are in
/// normalised form (see [`normalize_engine_name`]).
const ALIASES: &[(&str, &str)] = &[
    ("llama-cpp", "llama-cpp"),
    ("llamacpp", "llama-cpp"),
    ("llama", "llama-cpp"),
    ("llama-server", "llama-cpp"),
    ("prism-llama-cpp", "prism-llama-cpp"),
    ("prism", "prism-llama-cpp"),
    ("prismml", "prism-llama-cpp"),
    ("prism-llama", "prism-llama-cpp"),
    ("vllm", "vllm"),
    ("whisper-cpp", "whisper-cpp"),
    ("whisper", "whisper-cpp"),
    ("whispercpp", "whisper-cpp"),
    ("stable-diffusion-cpp", "stable-diffusion-cpp"),
    ("sd", "stable-diffusion-cpp"),
    ("sd-cpp", "stable-diffusion-cpp"),
    ("stable-diffusion", "stable-diffusion-cpp"),
    ("sdcpp", "stable-diffusion-cpp"),
];

/// The form engine names are compared in: trimmed, lower case, `_` and `.` read as `-`
/// (`llama.cpp` and `llama_cpp` are `llama-cpp`).
pub fn normalize_engine_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(['_', '.'], "-")
}

/// The canonical built-in engine id `name` refers to, accepting the aliases users type
/// (`llama.cpp`, `prism`, `sd`); `None` when `name` is not a built-in engine.
pub fn builtin_engine_id(name: &str) -> Option<&'static str> {
    let norm = normalize_engine_name(name);
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == norm)
        .map(|(_, id)| *id)
}

/// Checks the id of a fork engine: lower-case letters, digits, `-`, `_` and `.`, starting with a
/// letter, at most 64 characters, and not a built-in id or alias in any spelling
/// (`llama.cpp` is taken by mainline). The error explains which rule was broken.
pub fn check_fork_id(id: &str) -> std::result::Result<(), String> {
    if let Some(builtin) = builtin_engine_id(id) {
        return Err(format!(
            "`{id}` is the built-in engine `{builtin}` (or one of its aliases); a fork needs \
             its own id, for example `{builtin}-myfork`"
        ));
    }
    let shape_ok = (1..=64).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !shape_ok {
        return Err(format!(
            "fork engine id `{id}` must start with a lower-case letter and use only lower-case \
             letters, digits, `-`, `_` and `.` (at most 64 characters)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_canonical_ids() {
        assert_eq!(builtin_engine_id("llama.cpp"), Some("llama-cpp"));
        assert_eq!(builtin_engine_id(" Prism "), Some("prism-llama-cpp"));
        assert_eq!(builtin_engine_id("SD"), Some("stable-diffusion-cpp"));
        assert_eq!(builtin_engine_id("llama-cpp-turboquant"), None);
        for id in BUILTIN_ENGINE_IDS {
            assert_eq!(builtin_engine_id(id), Some(id));
        }
    }

    #[test]
    fn fork_ids_must_not_shadow_builtins() {
        assert!(check_fork_id("llama-cpp-turboquant").is_ok());
        assert!(check_fork_id("ik-llama.cpp").is_ok());
        for taken in ["llama-cpp", "llama.cpp", "LLAMA_CPP", "prism", "sd", "vllm"] {
            assert!(check_fork_id(taken).is_err(), "{taken}");
        }
        assert!(check_fork_id("Turbo").is_err());
        assert!(check_fork_id("9lives").is_err());
        assert!(check_fork_id("a@b").is_err());
    }
}
