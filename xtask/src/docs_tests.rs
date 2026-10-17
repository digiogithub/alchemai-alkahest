//! Unit tests of the documentation generator's parsers (`docs.rs`).

use std::collections::BTreeMap;

use super::{env_rows, long_flag, parse_help, parse_structs};

const HELP: &str = "Short about\n\nUsage: alkahest pull [OPTIONS] <MODEL>\n\nArguments:\n  <MODEL>  Model reference\n\nOptions:\n      --host <HOST>  Server address\n      --force        Download again\n          more detail\n  -h, --help         Print help\n";

#[test]
fn help_page_is_split_into_parts() {
    let h = parse_help(HELP);
    assert_eq!(h.about, "Short about");
    assert_eq!(h.usage, "alkahest pull [OPTIONS] <MODEL>");
    assert_eq!(h.arguments.len(), 1);
    assert_eq!(h.options.len(), 3);
    assert!(
        h.options[1].contains("more detail"),
        "a continuation line joins its entry"
    );
    assert_eq!(long_flag(&h.options[2]), Some("--help"));
}

#[test]
fn structs_and_enums_are_parsed_with_their_docs() {
    let src = "/// Server.\npub struct S {\n    /// The port.\n    pub port: u16,\n    /// Name.\n    pub name: Option<String>,\n}\n/// Mode.\npub enum M {\n    /// A.\n    Alpha,\n    /// B.\n    Beta,\n}\n";
    let (mut s, mut e) = (BTreeMap::new(), BTreeMap::new());
    parse_structs(src, &mut s, &mut e);
    assert_eq!(s["S"].len(), 2);
    assert_eq!(s["S"][0].doc, "The port.");
    assert_eq!(e["M"], vec!["alpha", "beta"]);
}

#[test]
fn environment_variables_are_mapped_to_the_key_they_set() {
    let src = "if let Some(v) = get(\"ALKAHEST_KEEP_ALIVE\") {\n    self.scheduler.keep_alive = parse(&v);\n}\n";
    assert_eq!(
        env_rows(src),
        vec!["| `ALKAHEST_KEEP_ALIVE` | `scheduler.keep_alive` |"]
    );
}
