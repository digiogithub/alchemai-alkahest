//! Reproducible release archives written in-process, so the same code runs on Linux, macOS and
//! Windows (macOS `tar` has no `--sort`/`--owner`, Windows has no `tar.gz` habit): sorted
//! entries, root ownership, a fixed modification time (`SOURCE_DATE_EPOCH`, else the epoch) and
//! a fixed gzip header.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

/// Entries of `stage` as `(path relative to stage's parent, absolute path)`, directories first
/// by sort order; `stage`'s own name is the archive's top-level directory.
fn entries(stage: &Path, follow_links: bool) -> Result<Vec<(String, PathBuf)>> {
    let base = stage.parent().context("stage has no parent")?;
    let mut out = Vec::new();
    for e in WalkDir::new(stage)
        .follow_links(follow_links)
        .sort_by_file_name()
    {
        let e = e?;
        let rel = e.path().strip_prefix(base)?;
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((name, e.into_path()));
    }
    Ok(out)
}

/// Unix permission bits of a file; `0o644` where the platform has none.
fn mode_of(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

/// Writes `<stage>` as a `.tar.gz` at `out` and returns `out`.
pub fn write_tar_gz(stage: &Path, out: &Path, source_date: Option<u64>) -> Result<PathBuf> {
    let mtime = source_date.unwrap_or(0);
    let file = BufWriter::new(File::create(out).with_context(|| out.display().to_string())?);
    let gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(file, flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    for (name, path) in entries(stage, false)? {
        let meta = fs::symlink_metadata(&path)?;
        let mut h = tar::Header::new_gnu();
        h.set_mtime(mtime);
        h.set_uid(0);
        h.set_gid(0);
        h.set_username("root")?;
        h.set_groupname("root")?;
        h.set_mode(mode_of(&meta));
        if meta.is_dir() {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
            h.set_mode(0o755);
            tar.append_data(&mut h, format!("{name}/"), std::io::empty())?;
        } else if meta.file_type().is_symlink() {
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            tar.append_link(&mut h, &name, fs::read_link(&path)?)?;
        } else {
            h.set_entry_type(tar::EntryType::Regular);
            h.set_size(meta.len());
            tar.append_data(&mut h, &name, File::open(&path)?)?;
        }
    }
    tar.into_inner()?.finish()?.flush()?;
    Ok(out.to_path_buf())
}

/// Writes `<stage>` as a `.zip` at `out` (symlinks are followed) and returns `out`.
pub fn write_zip(stage: &Path, out: &Path, source_date: Option<u64>) -> Result<PathBuf> {
    use zip::write::SimpleFileOptions;
    let file = BufWriter::new(File::create(out).with_context(|| out.display().to_string())?);
    let mut zip = zip::ZipWriter::new(file);
    let stamp = zip_datetime(source_date.unwrap_or(0))?;
    for (name, path) in entries(stage, true)? {
        let meta = fs::metadata(&path)?;
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(stamp)
            .unix_permissions(if meta.is_dir() { 0o755 } else { mode_of(&meta) });
        if meta.is_dir() {
            zip.add_directory(format!("{name}/"), opts)?;
        } else {
            zip.start_file(&name, opts)?;
            std::io::copy(&mut File::open(&path)?, &mut zip)?;
        }
    }
    zip.finish()?.flush()?;
    Ok(out.to_path_buf())
}

/// The zip timestamp for a Unix time. Zip cannot represent dates before 1980, so earlier times
/// (including the default epoch 0) become 1980-01-01 00:00:00.
fn zip_datetime(epoch: u64) -> Result<zip::DateTime> {
    const MIN: u64 = 315_532_800; // 1980-01-01T00:00:00Z
    let t = epoch.max(MIN);
    let (days, secs) = ((t / 86_400) as i64, t % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    match zip::DateTime::from_date_and_time(
        year as u16,
        month as u8,
        day as u8,
        (secs / 3_600) as u8,
        (secs % 3_600 / 60) as u8,
        (secs % 60) as u8,
    ) {
        Ok(d) => Ok(d),
        Err(_) => bail!("unrepresentable zip timestamp {epoch}"),
    }
}

/// Names inside a `.tar.gz` (files and symlinks only).
#[cfg(test)]
pub fn list_tar_gz(path: &Path) -> Result<Vec<String>> {
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(File::open(path)?));
    let mut names = Vec::new();
    for e in ar.entries()? {
        let e = e?;
        if !e.header().entry_type().is_dir() {
            names.push(e.path()?.to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

/// Names inside a `.zip` (files only).
#[cfg(test)]
pub fn list_zip(path: &Path) -> Result<Vec<String>> {
    let ar = zip::ZipArchive::new(File::open(path)?)?;
    Ok(ar
        .file_names()
        .map(|n| n.map(|n| n.into_owned()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|n| !n.ends_with('/'))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_timestamps_are_civil_dates() {
        // 2026-10-17T12:34:56Z
        let d = zip_datetime(1_792_240_496).unwrap();
        assert_eq!(
            (
                d.year(),
                d.month(),
                d.day(),
                d.hour(),
                d.minute(),
                d.second()
            ),
            (2026, 10, 17, 12, 34, 56)
        );
        let min = zip_datetime(0).unwrap();
        assert_eq!((min.year(), min.month(), min.day()), (1980, 1, 1));
    }

    #[test]
    fn tar_is_reproducible_and_keeps_modes_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("pkg");
        fs::create_dir_all(stage.join("engines")).unwrap();
        fs::write(stage.join("engines/server"), b"bin").unwrap();
        fs::write(stage.join("NOTICE"), b"n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                stage.join("engines/server"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            std::os::unix::fs::symlink("server", stage.join("engines/link")).unwrap();
        }
        let a = write_tar_gz(&stage, &tmp.path().join("a.tar.gz"), Some(1_700_000_000)).unwrap();
        let b = write_tar_gz(&stage, &tmp.path().join("b.tar.gz"), Some(1_700_000_000)).unwrap();
        assert_eq!(fs::read(a.clone()).unwrap(), fs::read(b).unwrap());
        let names = list_tar_gz(&a).unwrap();
        assert!(names.contains(&"pkg/NOTICE".to_owned()), "{names:?}");
        #[cfg(unix)]
        {
            assert!(names.contains(&"pkg/engines/link".to_owned()));
            let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(File::open(&a).unwrap()));
            let server = ar
                .entries()
                .unwrap()
                .map(Result::unwrap)
                .find(|e| e.path().unwrap().ends_with("engines/server"))
                .unwrap();
            assert_eq!(server.header().mode().unwrap() & 0o777, 0o755);
            assert_eq!(server.header().uid().unwrap(), 0);
        }
    }
}
