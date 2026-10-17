//! `cargo xtask tokens`: generate the desktop theme constants from `design/tokens.json`
//! (the Alembic design system) and check WCAG contrast.
//!
//! - `tokens`: write [`OUTPUT`] (colours of both themes, model kinds, processor-state pills,
//!   fonts, type scale, spacing, radii, sizes and the overlay shadow).
//! - `tokens --check`: fail when the generated file is stale (CI).
//! - `tokens --contrast`: fail when a foreground token drops below its WCAG 2.x minimum on the
//!   surfaces it is drawn on ([`contrast_rules`]).
//!
//! The generated module is plain Rust data (`u32` colours as `0xRRGGBBAA`, `f32` pixels), so it
//! compiles without GPUI and the contrast rules can be unit tested; the `src/ui/` facade turns
//! the numbers into GPUI colours and pixels.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};

/// Token source, relative to the workspace root.
pub const TOKENS: &str = "design/tokens.json";

/// Generated module, relative to the workspace root.
pub const OUTPUT: &str = "crates/alkahest-desktop/src/theme/tokens.rs";

/// Runs `cargo xtask tokens [--check] [--contrast]` in `root`. Returns `Ok(true)` when every
/// requested step passed.
pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut check = false;
    let mut contrast = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "--contrast" => contrast = true,
            other => bail!("unknown `tokens` option `{other}` (use --check and/or --contrast)"),
        }
    }
    let tokens = read_tokens(&root.join(TOKENS))?;
    let generated = generate(&tokens)?;
    let mut ok = true;
    if check {
        let have = std::fs::read_to_string(root.join(OUTPUT)).unwrap_or_default();
        if have == generated {
            println!("tokens: {OUTPUT} is up to date");
        } else {
            eprintln!("tokens: {OUTPUT} is stale: run `cargo xtask tokens` and commit the result");
            ok = false;
        }
    }
    if contrast {
        let failures = check_contrast(&tokens)?;
        if failures.is_empty() {
            println!("tokens: WCAG contrast OK");
        } else {
            for f in &failures {
                eprintln!("tokens: contrast: {f}");
            }
            ok = false;
        }
    }
    if !check && !contrast {
        let target = root.join(OUTPUT);
        std::fs::write(&target, generated)
            .with_context(|| format!("writing {}", target.display()))?;
        println!("tokens: wrote {OUTPUT}");
    }
    Ok(ok)
}

/// Reads and parses the token file.
pub fn read_tokens(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn object<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur
            .get(part)
            .ok_or_else(|| anyhow!("tokens.json has no `{path}`"))?;
    }
    cur.as_object()
        .ok_or_else(|| anyhow!("tokens.json `{path}` is not an object"))
}

/// The `$value` of a token object.
fn value<'a>(token: &'a Value, path: &str) -> Result<&'a Value> {
    token
        .get("$value")
        .ok_or_else(|| anyhow!("token `{path}` has no $value"))
}

fn str_value<'a>(token: &'a Value, path: &str) -> Result<&'a str> {
    value(token, path)?
        .as_str()
        .ok_or_else(|| anyhow!("token `{path}` is not a string"))
}

/// Children of a token group, skipping `$`-prefixed metadata keys.
fn children(group: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    group.iter().filter(|(k, _)| !k.starts_with('$'))
}

/// `#RRGGBB` / `#RRGGBBAA` as `0xRRGGBBAA`.
pub fn parse_color(hex: &str) -> Result<u32> {
    let h = hex.trim().trim_start_matches('#');
    let full = match h.len() {
        6 => format!("{h}FF"),
        8 => h.to_string(),
        _ => bail!("`{hex}` is not #RRGGBB or #RRGGBBAA"),
    };
    u32::from_str_radix(&full, 16).with_context(|| format!("`{hex}` is not a hex colour"))
}

/// `"12.5px"` -> 12.5.
fn parse_px(v: &str) -> Result<f64> {
    v.trim()
        .strip_suffix("px")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("`{v}` is not a px dimension"))
}

/// `"-0.02em"` -> -0.02, `"0"` -> 0.
fn parse_em(v: &str) -> Result<f64> {
    let t = v.trim();
    if t == "0" {
        return Ok(0.0);
    }
    t.strip_suffix("em")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("`{v}` is not an em length"))
}

/// `focusRing` -> `focus_ring`, `monoLarge` -> `mono_large`.
pub fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Constant name for a token key: `focusRing` -> `FOCUS_RING`; `2xl` -> `XXL` (leading digit
/// `n` before `xl` repeats the `X`); a bare number gets `prefix` (`1` -> `S1`).
fn const_name(key: &str, prefix: &str) -> String {
    let digits: String = key.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return snake(key).to_ascii_uppercase();
    }
    let rest = &key[digits.len()..];
    if rest.is_empty() {
        return format!("{prefix}{digits}");
    }
    match (digits.parse::<usize>(), rest) {
        (Ok(n), "xl") => format!("{}L", "X".repeat(n)),
        _ => format!("{prefix}{}", snake(key).to_ascii_uppercase()),
    }
}

/// Flattens a colour group into `(dotted path, colour, description)` leaves.
fn color_leaves(
    group: &Map<String, Value>,
    prefix: &str,
    out: &mut Vec<(String, u32, Option<String>)>,
) -> Result<()> {
    for (k, v) in children(group) {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if v.get("$value").is_some() {
            let desc = v
                .get("$description")
                .and_then(Value::as_str)
                .map(str::to_string);
            out.push((path.clone(), parse_color(str_value(v, &path)?)?, desc));
        } else if let Some(obj) = v.as_object() {
            color_leaves(obj, &path, out)?;
        }
    }
    Ok(())
}

/// The colours of one theme (`color.dark` / `color.light`), by dotted path.
pub fn theme_colors(tokens: &Value, theme: &str) -> Result<BTreeMap<String, u32>> {
    let mut leaves = Vec::new();
    color_leaves(object(tokens, &format!("color.{theme}"))?, "", &mut leaves)?;
    Ok(leaves.into_iter().map(|(p, c, _)| (p, c)).collect())
}

fn hex(c: u32) -> String {
    format!("0x{c:08X}")
}

/// Rust float literal (`232` -> `232.0`).
fn float(v: f64) -> String {
    format!("{v:?}")
}

/// Renders the whole generated module.
pub fn generate(tokens: &Value) -> Result<String> {
    let mut rs = String::new();
    rs.push_str(
        "//! Alembic design tokens, GENERATED by `cargo xtask tokens` from `design/tokens.json`.\n\
         //! Do not edit by hand: change the token file and regenerate (CI runs `tokens --check`).\n\
         //! Colours are `0xRRGGBBAA`; dimensions are logical pixels.\n\n",
    );
    palette(tokens, &mut rs)?;
    brand(tokens, &mut rs)?;
    kinds(tokens, &mut rs)?;
    processor_states(tokens, &mut rs)?;
    fonts(tokens, &mut rs)?;
    type_scale(tokens, &mut rs)?;
    dimensions(tokens, "space", "S", "Spacing scale.", &mut rs)?;
    dimensions(tokens, "radius", "R", "Corner radii.", &mut rs)?;
    sizes(tokens, &mut rs)?;
    shadow(tokens, &mut rs)?;
    Ok(rs)
}

fn palette(tokens: &Value, rs: &mut String) -> Result<()> {
    let mut dark = Vec::new();
    color_leaves(object(tokens, "color.dark")?, "", &mut dark)?;
    let mut light = Vec::new();
    color_leaves(object(tokens, "color.light")?, "", &mut light)?;
    let dark_keys: Vec<&String> = dark.iter().map(|l| &l.0).collect();
    let light_keys: Vec<&String> = light.iter().map(|l| &l.0).collect();
    if dark_keys != light_keys {
        bail!("`color.dark` and `color.light` must define the same tokens");
    }
    rs.push_str(
        "/// The semantic colours of one theme.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct Palette {\n",
    );
    for (path, _, desc) in &dark {
        let doc = match desc {
            Some(d) => format!("`{path}`: {d}."),
            None => format!("`{path}`."),
        };
        writeln!(rs, "    /// {doc}")?;
        writeln!(
            rs,
            "    pub {}: u32,",
            path.split('.').map(snake).collect::<Vec<_>>().join("_")
        )?;
    }
    rs.push_str("}\n\n");
    for (name, doc, leaves) in [
        ("DARK", "The dark theme (default).", &dark),
        ("LIGHT", "The light theme.", &light),
    ] {
        writeln!(rs, "/// {doc}")?;
        writeln!(rs, "pub const {name}: Palette = Palette {{")?;
        for (path, color, _) in leaves.iter() {
            let field = path.split('.').map(snake).collect::<Vec<_>>().join("_");
            writeln!(rs, "    {field}: {},", hex(*color))?;
        }
        rs.push_str("};\n\n");
    }
    Ok(())
}

fn brand(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str("/// Brand colours (logo, app icon).\npub mod brand {\n");
    for (k, v) in children(object(tokens, "color.brand")?) {
        let path = format!("color.brand.{k}");
        let doc = v
            .get("$description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("`{path}`."), |d| format!("`{path}`: {d}."));
        writeln!(rs, "    /// {doc}")?;
        writeln!(
            rs,
            "    pub const {}: u32 = {};",
            const_name(k, "C"),
            hex(parse_color(str_value(v, &path)?)?)
        )?;
    }
    rs.push_str("}\n\n");
    Ok(())
}

fn kinds(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str(
        "/// A model kind badge: glyph + label + colour on a tint (never colour alone).\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct KindToken {\n\
         \x20   /// Token key (`chat`, `embeddings`, ...).\n\
         \x20   pub key: &'static str,\n\
         \x20   /// Glyph drawn before the label.\n\
         \x20   pub glyph: &'static str,\n\
         \x20   /// Label text.\n\
         \x20   pub label: &'static str,\n\
         \x20   /// Text and glyph colour.\n\
         \x20   pub fg: u32,\n\
         \x20   /// Badge background (the foreground at 15 % alpha).\n\
         \x20   pub tint: u32,\n\
         }\n\n\
         /// Every model kind, in token order.\n\
         pub const KINDS: &[KindToken] = &[\n",
    );
    for (k, v) in children(object(tokens, "kind")?) {
        let p = format!("kind.{k}");
        let field = |f: &str| -> Result<&Value> {
            v.get(f)
                .ok_or_else(|| anyhow!("token `{p}.{f}` is missing"))
        };
        writeln!(rs, "    KindToken {{")?;
        writeln!(rs, "        key: {:?},", k)?;
        writeln!(rs, "        glyph: {:?},", str_value(field("glyph")?, &p)?)?;
        writeln!(rs, "        label: {:?},", str_value(field("label")?, &p)?)?;
        writeln!(
            rs,
            "        fg: {},",
            hex(parse_color(str_value(field("fg")?, &p)?)?)
        )?;
        writeln!(
            rs,
            "        tint: {},",
            hex(parse_color(str_value(field("tint")?, &p)?)?)
        )?;
        writeln!(rs, "    }},")?;
    }
    rs.push_str("];\n\n");
    Ok(())
}

fn processor_states(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str(
        "/// A processor-state pill (Library and Running): dot + label on a background.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub struct ProcessorStateToken {\n\
         \x20   /// Token key (`gpu`, `split`, `cpu`, `loading`, `off`).\n\
         \x20   pub key: &'static str,\n\
         \x20   /// Pill background.\n\
         \x20   pub bg: u32,\n\
         \x20   /// Label colour.\n\
         \x20   pub fg: u32,\n\
         \x20   /// Status dot colour.\n\
         \x20   pub dot: u32,\n\
         \x20   /// Outline, when the pill has one.\n\
         \x20   pub border: Option<u32>,\n\
         \x20   /// Label template (`{gpu}`, `{cpu}`, `{pct}` are placeholders).\n\
         \x20   pub label: &'static str,\n\
         }\n\n\
         /// Every processor state, in token order.\n\
         pub const PROCESSOR_STATES: &[ProcessorStateToken] = &[\n",
    );
    for (k, v) in children(object(tokens, "processorState")?) {
        let p = format!("processorState.{k}");
        let color = |f: &str| -> Result<Option<u32>> {
            v.get(f)
                .map(|t| str_value(t, &format!("{p}.{f}")).and_then(parse_color))
                .transpose()
        };
        let req = |f: &str| -> Result<u32> {
            color(f)?.ok_or_else(|| anyhow!("token `{p}.{f}` is missing"))
        };
        let label = v
            .get("label")
            .ok_or_else(|| anyhow!("token `{p}.label` is missing"))?;
        writeln!(rs, "    ProcessorStateToken {{")?;
        writeln!(rs, "        key: {:?},", k)?;
        writeln!(rs, "        bg: {},", hex(req("bg")?))?;
        writeln!(rs, "        fg: {},", hex(req("fg")?))?;
        writeln!(rs, "        dot: {},", hex(req("dot")?))?;
        match color("border")? {
            Some(c) => writeln!(rs, "        border: Some({}),", hex(c))?,
            None => writeln!(rs, "        border: None,")?,
        }
        writeln!(rs, "        label: {:?},", str_value(label, &p)?)?;
        writeln!(rs, "    }},")?;
    }
    rs.push_str("];\n\n");
    Ok(())
}

/// First family of a `fontFamily` token.
fn family(tokens: &Value, key: &str) -> Result<String> {
    let path = format!("font.family.{key}");
    let t = object(tokens, "font.family")?
        .get(key)
        .ok_or_else(|| anyhow!("token `{path}` is missing"))?;
    value(t, &path)?
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("token `{path}` is not a list of families"))
}

fn fonts(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str("/// Font families (embedded, OFL-1.1) and weights.\npub mod font {\n");
    writeln!(rs, "    /// UI family (`font.family.ui`).")?;
    writeln!(rs, "    pub const UI: &str = {:?};", family(tokens, "ui")?)?;
    writeln!(
        rs,
        "    /// Numbers, sizes, paths, quants, ids (`font.family.mono`)."
    )?;
    writeln!(
        rs,
        "    pub const MONO: &str = {:?};",
        family(tokens, "mono")?
    )?;
    for (k, v) in children(object(tokens, "font.weight")?) {
        let path = format!("font.weight.{k}");
        let w = value(v, &path)?
            .as_u64()
            .ok_or_else(|| anyhow!("token `{path}` is not an integer"))?;
        writeln!(rs, "    /// `{path}`.")?;
        writeln!(
            rs,
            "    pub const WEIGHT_{}: u16 = {w};",
            const_name(k, "W")
        )?;
    }
    rs.push_str("}\n\n");
    Ok(())
}

fn type_scale(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str(
        "/// Which embedded family a type style uses.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         pub enum Family {\n\
         \x20   /// [`font::UI`].\n\
         \x20   Ui,\n\
         \x20   /// [`font::MONO`].\n\
         \x20   Mono,\n\
         }\n\n\
         /// One step of the type scale.\n\
         #[derive(Debug, Clone, Copy, PartialEq)]\n\
         pub struct TypeStyle {\n\
         \x20   /// Font family.\n\
         \x20   pub family: Family,\n\
         \x20   /// Font size in pixels.\n\
         \x20   pub size: f32,\n\
         \x20   /// Font weight (400 regular .. 700 bold).\n\
         \x20   pub weight: u16,\n\
         \x20   /// Line height as a multiple of the size.\n\
         \x20   pub line_height: f32,\n\
         \x20   /// Letter spacing in em.\n\
         \x20   pub letter_spacing: f32,\n\
         \x20   /// Whether the text is set in capitals.\n\
         \x20   pub uppercase: bool,\n\
         }\n\n\
         /// The type scale.\n\
         pub mod text {\n\
         \x20   use super::{Family, TypeStyle};\n\n",
    );
    for (k, v) in children(object(tokens, "type")?) {
        let path = format!("type.{k}");
        let t = value(v, &path)?;
        let get = |f: &str| t.get(f).ok_or_else(|| anyhow!("`{path}.{f}` is missing"));
        let fam = match get("fontFamily")?.as_str() {
            Some("{font.family.ui}") => "Ui",
            Some("{font.family.mono}") => "Mono",
            other => bail!("`{path}.fontFamily` is not a family alias: {other:?}"),
        };
        let size = parse_px(get("fontSize")?.as_str().unwrap_or_default())?;
        let weight = get("fontWeight")?
            .as_u64()
            .ok_or_else(|| anyhow!("`{path}.fontWeight` is not an integer"))?;
        let line = get("lineHeight")?
            .as_f64()
            .ok_or_else(|| anyhow!("`{path}.lineHeight` is not a number"))?;
        let spacing = parse_em(get("letterSpacing")?.as_str().unwrap_or_default())?;
        let upper = t.get("textTransform").and_then(Value::as_str) == Some("uppercase");
        let doc = v
            .get("$description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("`{path}`."), |d| format!("`{path}`: {d}."));
        writeln!(rs, "    /// {doc}")?;
        writeln!(
            rs,
            "    pub const {}: TypeStyle = TypeStyle {{",
            const_name(k, "T")
        )?;
        writeln!(rs, "        family: Family::{fam},")?;
        writeln!(rs, "        size: {},", float(size))?;
        writeln!(rs, "        weight: {weight},")?;
        writeln!(rs, "        line_height: {},", float(line))?;
        writeln!(rs, "        letter_spacing: {},", float(spacing))?;
        writeln!(rs, "        uppercase: {upper},")?;
        writeln!(rs, "    }};")?;
    }
    rs.push_str("}\n\n");
    Ok(())
}

fn dimensions(tokens: &Value, group: &str, prefix: &str, doc: &str, rs: &mut String) -> Result<()> {
    writeln!(rs, "/// {doc}")?;
    writeln!(rs, "pub mod {group} {{")?;
    for (k, v) in children(object(tokens, group)?) {
        let path = format!("{group}.{k}");
        let px = parse_px(str_value(v, &path)?)?;
        let doc = v
            .get("$description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("`{path}`."), |d| format!("`{path}`: {d}."));
        writeln!(rs, "    /// {doc}")?;
        writeln!(
            rs,
            "    pub const {}: f32 = {};",
            const_name(k, prefix),
            float(px)
        )?;
    }
    rs.push_str("}\n\n");
    Ok(())
}

fn sizes(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str("/// Component and window sizes.\npub mod size {\n");
    for (k, v) in children(object(tokens, "size")?) {
        let path = format!("size.{k}");
        let raw = str_value(v, &path)?;
        let doc = v
            .get("$description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("`{path}`."), |d| format!("`{path}`: {d}."));
        writeln!(rs, "    /// {doc}")?;
        let name = const_name(k, "Z");
        if let Some((a, b)) = raw.split_once(' ') {
            // `24px 32px`: vertical, horizontal (CSS shorthand).
            writeln!(
                rs,
                "    pub const {name}: (f32, f32) = ({}, {});",
                float(parse_px(a)?),
                float(parse_px(b)?)
            )?;
        } else if let Some((w, h)) = raw.strip_suffix("px").and_then(|r| r.split_once('x')) {
            // `38x22px`: width x height.
            let w: f64 = w.parse().with_context(|| format!("`{path}` width"))?;
            let h: f64 = h.parse().with_context(|| format!("`{path}` height"))?;
            writeln!(
                rs,
                "    pub const {name}: (f32, f32) = ({}, {});",
                float(w),
                float(h)
            )?;
        } else {
            writeln!(rs, "    pub const {name}: f32 = {};", float(parse_px(raw)?))?;
        }
    }
    rs.push_str("}\n\n");
    Ok(())
}

fn shadow(tokens: &Value, rs: &mut String) -> Result<()> {
    rs.push_str(
        "/// A drop shadow.\n\
         #[derive(Debug, Clone, Copy, PartialEq)]\n\
         pub struct Shadow {\n\
         \x20   /// Horizontal offset.\n\
         \x20   pub offset_x: f32,\n\
         \x20   /// Vertical offset.\n\
         \x20   pub offset_y: f32,\n\
         \x20   /// Blur radius.\n\
         \x20   pub blur: f32,\n\
         \x20   /// Spread radius.\n\
         \x20   pub spread: f32,\n\
         \x20   /// Shadow colour.\n\
         \x20   pub color: u32,\n\
         }\n\n",
    );
    for (k, v) in children(object(tokens, "shadow")?) {
        let path = format!("shadow.{k}");
        let t = value(v, &path)?;
        let px = |f: &str| -> Result<f64> {
            parse_px(
                t.get(f)
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`{path}.{f}` is missing"))?,
            )
        };
        let color = t
            .get("color")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("`{path}.color` is missing"))?;
        let doc = v
            .get("$description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("`{path}`."), |d| format!("`{path}`: {d}."));
        writeln!(rs, "/// {doc}")?;
        writeln!(
            rs,
            "pub const SHADOW_{}: Shadow = Shadow {{",
            const_name(k, "S")
        )?;
        writeln!(rs, "    offset_x: {},", float(px("offsetX")?))?;
        writeln!(rs, "    offset_y: {},", float(px("offsetY")?))?;
        writeln!(rs, "    blur: {},", float(px("blur")?))?;
        writeln!(rs, "    spread: {},", float(px("spread")?))?;
        writeln!(rs, "    color: {},", hex(parse_color(color)?))?;
        writeln!(rs, "}};")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Contrast

/// WCAG 2.x relative luminance of an opaque `0xRRGGBBAA` colour (alpha ignored).
pub fn luminance(c: u32) -> f64 {
    let channel = |shift: u32| {
        let v = f64::from((c >> shift) & 0xFF) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
}

/// Composites a translucent `top` over an opaque `bottom` (both `0xRRGGBBAA`).
pub fn over(top: u32, bottom: u32) -> u32 {
    let a = f64::from(top & 0xFF) / 255.0;
    let mix = |shift: u32| {
        let t = f64::from((top >> shift) & 0xFF);
        let b = f64::from((bottom >> shift) & 0xFF);
        // In 0..=255 by construction, so the cast cannot truncate meaningfully.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = (t * a + b * (1.0 - a)).round() as u32;
        v.min(255)
    };
    (mix(24) << 24) | (mix(16) << 16) | (mix(8) << 8) | 0xFF
}

/// WCAG contrast ratio of two opaque colours.
pub fn ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// One contrast requirement: `fg` drawn on `bg` needs at least `min`.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// What is checked (`dark text.muted on bg.raised`).
    pub name: String,
    /// Foreground colour (opaque).
    pub fg: u32,
    /// Background colour (opaque, translucent ones already composited).
    pub bg: u32,
    /// Minimum ratio: 4.5 for body text, 3.0 for large or non-body text.
    pub min: f64,
}

/// Every contrast requirement the design system promises.
///
/// - Body text (`text.primary/secondary/muted`): 4.5:1 on every surface.
/// - `text.faint` ("axis ticks, token names; not for body text"): 3:1 on every surface.
/// - Status text (`accent/ok/info/danger.text`): 4.5:1 on `bg.raised` and on its own subtle.
/// - `text.onAccent` on the brass fill: 4.5:1.
/// - Kind badges and processor-state pills (dark only, as designed): label on its tint
///   composited over `bg.raised`, 4.5:1.
pub fn contrast_rules(tokens: &Value) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for theme in ["dark", "light"] {
        let c = theme_colors(tokens, theme)?;
        let get = |k: &str| {
            c.get(k)
                .copied()
                .ok_or_else(|| anyhow!("`color.{theme}.{k}` is missing"))
        };
        let surfaces = ["bg.base", "bg.sidebar", "bg.raised", "bg.overlay"];
        for (fg, min) in [
            ("text.primary", 4.5),
            ("text.secondary", 4.5),
            ("text.muted", 4.5),
            ("text.faint", 3.0),
        ] {
            for bg in surfaces {
                rules.push(Rule {
                    name: format!("{theme} {fg} on {bg}"),
                    fg: get(fg)?,
                    bg: get(bg)?,
                    min,
                });
            }
        }
        for status in ["accent", "ok", "info", "danger"] {
            let fg = format!("{status}.text");
            for bg in ["bg.raised".to_string(), format!("{status}.subtle")] {
                rules.push(Rule {
                    name: format!("{theme} {fg} on {bg}"),
                    fg: get(&fg)?,
                    bg: get(&bg)?,
                    min: 4.5,
                });
            }
        }
        rules.push(Rule {
            name: format!("{theme} text.onAccent on accent.default"),
            fg: get("text.onAccent")?,
            bg: get("accent.default")?,
            min: 4.5,
        });
    }
    let dark = theme_colors(tokens, "dark")?;
    let raised = dark
        .get("bg.raised")
        .copied()
        .ok_or_else(|| anyhow!("`color.dark.bg.raised` is missing"))?;
    for (k, v) in children(object(tokens, "kind")?) {
        let col = |f: &str| -> Result<u32> {
            let p = format!("kind.{k}.{f}");
            parse_color(str_value(
                v.get(f).ok_or_else(|| anyhow!("`{p}` is missing"))?,
                &p,
            )?)
        };
        rules.push(Rule {
            name: format!("dark kind.{k}.fg on its tint"),
            fg: col("fg")?,
            bg: over(col("tint")?, raised),
            min: 4.5,
        });
    }
    for (k, v) in children(object(tokens, "processorState")?) {
        let col = |f: &str| -> Result<u32> {
            let p = format!("processorState.{k}.{f}");
            parse_color(str_value(
                v.get(f).ok_or_else(|| anyhow!("`{p}` is missing"))?,
                &p,
            )?)
        };
        rules.push(Rule {
            name: format!("dark processorState.{k}.fg on its bg"),
            fg: col("fg")?,
            bg: over(col("bg")?, raised),
            min: 4.5,
        });
    }
    Ok(rules)
}

/// Failed rules, formatted for the console.
pub fn check_contrast(tokens: &Value) -> Result<Vec<String>> {
    Ok(contrast_rules(tokens)?
        .into_iter()
        .filter_map(|r| {
            let got = ratio(r.fg, r.bg);
            (got < r.min).then(|| format!("{}: {got:.2}:1 < {:.1}:1", r.name, r.min))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn real_tokens() -> Value {
        read_tokens(&root().join(TOKENS)).unwrap()
    }

    #[test]
    fn names_are_rust_identifiers() {
        assert_eq!(snake("focusRing"), "focus_ring");
        assert_eq!(const_name("onAccent", "C"), "ON_ACCENT");
        assert_eq!(const_name("2xl", "R"), "XXL");
        assert_eq!(const_name("12", "S"), "S12");
        assert_eq!(const_name("monoLarge", "T"), "MONO_LARGE");
    }

    #[test]
    fn colours_parse_with_and_without_alpha() {
        assert_eq!(parse_color("#0F1214").unwrap(), 0x0F12_14FF);
        assert_eq!(parse_color("#E3A63B2E").unwrap(), 0xE3A6_3B2E);
        assert!(parse_color("#123").is_err());
    }

    #[test]
    fn wcag_ratio_matches_known_values() {
        let r = ratio(0x0000_00FF, 0xFFFF_FFFF);
        assert!((r - 21.0).abs() < 0.01, "{r}");
        assert!((ratio(0x7777_77FF, 0xFFFF_FFFF) - 4.48).abs() < 0.01);
        assert_eq!(over(0xFFFF_FF00, 0x1020_30FF), 0x1020_30FF);
        assert_eq!(over(0xFFFF_FFFF, 0x1020_30FF), 0xFFFF_FFFF);
    }

    #[test]
    fn generated_file_is_up_to_date() {
        let want = generate(&real_tokens()).unwrap();
        let have = std::fs::read_to_string(root().join(OUTPUT)).unwrap();
        assert!(have == want, "run `cargo xtask tokens`");
    }

    #[test]
    fn alembic_tokens_pass_the_contrast_rules() {
        let failures = check_contrast(&real_tokens()).unwrap();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_low_contrast_token_is_reported() {
        let mut t = real_tokens();
        t["color"]["dark"]["text"]["muted"]["$value"] = Value::from("#30363C");
        let failures = check_contrast(&t).unwrap();
        assert!(failures.iter().any(|f| f.contains("dark text.muted")));
    }

    #[test]
    fn mismatched_themes_are_rejected() {
        let mut t = real_tokens();
        t["color"]["light"]["bg"]
            .as_object_mut()
            .unwrap()
            .remove("code");
        assert!(generate(&t).is_err());
    }
}
