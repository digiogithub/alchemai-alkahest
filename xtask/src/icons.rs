//! `cargo xtask icons`: render the platform icon sets from `design/brand/` into
//! [`OUT_DIR`].
//!
//! - App icon PNGs at 16..1024 px: `app-icon.svg` (ink tile, brass mark) from 64 px up,
//!   `mark-small.svg` (drop removed, heavier stroke) at 48 px and below, as the brand rules in
//!   `design/README.md` require.
//! - `alkahest.ico` (Windows: 16, 24, 32, 48, 64, 128, 256) and `alkahest.icns` (macOS:
//!   16..1024, PNG-compressed entries), written as plain containers around the PNGs.
//! - Tray icons from `mark-tray.svg` (`currentColor`): `tray-template-*.png` in black for the
//!   macOS template image, `tray-light-*.png` in bone for dark panels (Linux, Windows).
//!
//! Rasterizing is done by `resvg` (Apache-2.0 OR MIT), pinned in the workspace and used only
//! here. The SVGs contain no text, so no font database is needed.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use resvg::{tiny_skia, usvg};

/// Source directory, relative to the workspace root.
pub const BRAND_DIR: &str = "design/brand";

/// Output directory, relative to the workspace root.
pub const OUT_DIR: &str = "packaging/icons";

/// App icon sizes written as PNG.
pub const APP_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256, 512, 1024];

/// Sizes at or below this use `mark-small.svg`.
pub const SMALL_MAX: u32 = 48;

/// Sizes in the Windows `.ico`.
pub const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// Tray icon sizes.
pub const TRAY_SIZES: &[u32] = &[16, 22, 24, 32, 44, 64];

/// `.icns` entry types for PNG payloads, by pixel size.
const ICNS_TYPES: &[(&[u8; 4], u32)] = &[
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// Tray tint for dark panels (`color.brand.bone`).
const TRAY_LIGHT: &str = "#E8E6E1";

/// Runs `cargo xtask icons` in `root`.
pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    if let Some(arg) = args.first() {
        bail!("unknown `icons` option `{arg}`");
    }
    let brand = root.join(BRAND_DIR);
    let out = root.join(OUT_DIR);
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let app = read(&brand.join("app-icon.svg"))?;
    let small = read(&brand.join("mark-small.svg"))?;
    let tray = read(&brand.join("mark-tray.svg"))?;

    let mut pngs = std::collections::BTreeMap::new();
    for &size in APP_SIZES {
        let svg = if size <= SMALL_MAX { &small } else { &app };
        let png = render(svg, size)?;
        write(&out.join(format!("alkahest-{size}.png")), &png)?;
        pngs.insert(size, png);
    }
    let ico_entries: Vec<(u32, &[u8])> = ICO_SIZES
        .iter()
        .filter_map(|s| pngs.get(s).map(|p| (*s, p.as_slice())))
        .collect();
    write(&out.join("alkahest.ico"), &ico(&ico_entries)?)?;
    let icns_entries: Vec<(&[u8; 4], &[u8])> = ICNS_TYPES
        .iter()
        .filter_map(|(ty, s)| pngs.get(s).map(|p| (*ty, p.as_slice())))
        .collect();
    write(&out.join("alkahest.icns"), &icns(&icns_entries)?)?;

    let template = tray.replace("currentColor", "#000000");
    let light = tray.replace("currentColor", TRAY_LIGHT);
    for &size in TRAY_SIZES {
        write(
            &out.join(format!("tray-template-{size}.png")),
            &render(&template, size)?,
        )?;
        write(
            &out.join(format!("tray-light-{size}.png")),
            &render(&light, size)?,
        )?;
    }
    println!(
        "icons: wrote {OUT_DIR}/ ({} files)",
        APP_SIZES.len() + 2 + 2 * TRAY_SIZES.len()
    );
    Ok(true)
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Renders `svg` into a `size`×`size` PNG, scaled to fit and centred.
pub fn render(svg: &str, size: u32) -> Result<Vec<u8>> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .map_err(|e| anyhow!("parsing SVG: {e}"))?;
    let mut pixmap =
        tiny_skia::Pixmap::new(size, size).ok_or_else(|| anyhow!("invalid icon size {size}"))?;
    let view = tree.size();
    // Pixel sizes are small integers, exactly representable as f32.
    #[allow(clippy::cast_precision_loss)]
    let target = size as f32;
    let scale = (target / view.width()).min(target / view.height());
    let dx = (target - view.width() * scale) / 2.0;
    let dy = (target - view.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|e| anyhow!("encoding PNG: {e}"))
}

/// A Windows `.ico` holding PNG entries (supported since Windows Vista).
pub fn ico(entries: &[(u32, &[u8])]) -> Result<Vec<u8>> {
    let count = u16::try_from(entries.len()).context("too many ico entries")?;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * u32::from(count);
    for (size, png) in entries {
        // 256 is stored as 0 in the one-byte fields.
        let dim = u8::try_from(*size % 256).context("ico entries must be at most 256 px")?;
        if *size > 256 {
            bail!("ico entries must be at most 256 px");
        }
        let len = u32::try_from(png.len()).context("ico entry too large")?;
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += len;
    }
    for (_, png) in entries {
        out.extend_from_slice(png);
    }
    Ok(out)
}

/// A macOS `.icns` holding PNG entries.
pub fn icns(entries: &[(&[u8; 4], &[u8])]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for (ty, png) in entries {
        let len = u32::try_from(png.len() + 8).context("icns entry too large")?;
        body.extend_from_slice(*ty);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(png);
    }
    let total = u32::try_from(body.len() + 8).context("icns too large")?;
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn brand(name: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        std::fs::read_to_string(root.join(BRAND_DIR).join(name)).unwrap()
    }

    #[test]
    fn renders_the_app_icon_at_the_requested_size() {
        let png = render(&brand("app-icon.svg"), 64).unwrap();
        assert!(png.starts_with(PNG_MAGIC));
        // IHDR width and height, big endian, right after the signature and chunk header.
        assert_eq!(&png[16..20], &64u32.to_be_bytes());
        assert_eq!(&png[20..24], &64u32.to_be_bytes());
    }

    #[test]
    fn ico_directory_points_at_each_png() {
        let a = render(&brand("mark-small.svg"), 16).unwrap();
        let b = render(&brand("app-icon.svg"), 256).unwrap();
        let ico = ico(&[(16, &a), (256, &b)]).unwrap();
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0]);
        assert_eq!(ico[6], 16);
        assert_eq!(ico[6 + 16], 0, "256 px is stored as 0");
        let first = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(first, 6 + 32);
        assert!(ico[first..].starts_with(PNG_MAGIC));
    }

    #[test]
    fn icns_length_fields_are_consistent() {
        let a = render(&brand("app-icon.svg"), 128).unwrap();
        let icns = icns(&[(b"ic07", &a)]).unwrap();
        assert_eq!(&icns[0..4], b"icns");
        let total = u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize;
        assert_eq!(total, icns.len());
        assert_eq!(&icns[8..12], b"ic07");
        assert!(icns[16..].starts_with(PNG_MAGIC));
    }

    #[test]
    fn tray_icon_is_tintable() {
        let svg = brand("mark-tray.svg").replace("currentColor", TRAY_LIGHT);
        assert!(render(&svg, 22).unwrap().starts_with(PNG_MAGIC));
    }
}
