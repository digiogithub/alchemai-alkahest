//! Detecting the CUDA runtime libraries a CUDA engine build needs.
//!
//! Upstream CUDA archives do not contain `libcudart` / `libcublas` (Windows: `cudart64_*.dll`,
//! `cublas64_*.dll`). They come from a system-wide CUDA install or from the recipe's
//! companion archive (`alkahest engines install --with-runtime`). Without them the engine dies
//! at start with a loader error that names no remedy, so the check runs before launch and
//! fails with a message that does.

use std::path::{Path, PathBuf};

use crate::error::{RecipeError, Result};
use crate::recipe::parse_cuda_version;

/// The CUDA major version a variant name asks for (`cuda12.8` is 12), `None` for variants
/// that are not CUDA builds.
pub fn cuda_major_of_variant(variant: &str) -> Option<u32> {
    let rest = variant.strip_prefix("cuda")?;
    parse_cuda_version(rest).map(|(major, _)| major)
}

/// File names that satisfy the runtime requirement of CUDA `major` on `os`, one list per
/// required library (any name of a list is enough). Empty on macOS, which has no CUDA.
pub fn cuda_runtime_libraries(os: &str, major: u32) -> Vec<Vec<String>> {
    match os {
        "linux" => vec![
            vec![format!("libcudart.so.{major}")],
            vec![format!("libcublas.so.{major}")],
        ],
        "windows" => vec![
            vec![format!("cudart64_{major}.dll")],
            vec![format!("cublas64_{major}.dll")],
        ],
        _ => Vec::new(),
    }
}

/// Where the runtime libraries of CUDA `major` are looked for besides the engine's own
/// directories: the loader path variables and the usual system and toolkit locations.
pub fn system_cuda_dirs(os: &str, major: u32) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let env_dirs = |name: &str| -> Vec<PathBuf> {
        std::env::var_os(name)
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default()
    };
    match os {
        "linux" => {
            dirs.extend(env_dirs("LD_LIBRARY_PATH"));
            for d in [
                "/lib/x86_64-linux-gnu",
                "/usr/lib/x86_64-linux-gnu",
                "/lib/aarch64-linux-gnu",
                "/usr/lib/aarch64-linux-gnu",
                "/lib64",
                "/usr/lib64",
                "/usr/lib",
                "/usr/local/lib",
                "/usr/local/cuda/lib64",
                "/opt/cuda/lib64",
                "/usr/lib/wsl/lib",
            ] {
                dirs.push(PathBuf::from(d));
            }
            if let Ok(entries) = std::fs::read_dir("/usr/local") {
                let prefix = format!("cuda-{major}");
                for e in entries.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&prefix) {
                        dirs.push(e.path().join("lib64"));
                    }
                }
            }
        }
        "windows" => {
            dirs.extend(env_dirs("PATH"));
            if let Some(p) = std::env::var_os("CUDA_PATH") {
                dirs.push(PathBuf::from(p).join("bin"));
            }
        }
        _ => {}
    }
    dirs
}

/// The libraries from `needed` that none of `dirs` provides.
pub fn missing_libraries(needed: &[Vec<String>], dirs: &[PathBuf]) -> Vec<String> {
    needed
        .iter()
        .filter(|alternatives| {
            !alternatives
                .iter()
                .any(|name| dirs.iter().any(|d| d.join(name).exists()))
        })
        .map(|alternatives| alternatives.join(" or "))
        .collect()
}

/// Checks that the CUDA runtime a `cudaN` build needs is reachable, and fails with an
/// actionable message when it is not. Variants that are not CUDA builds always pass.
///
/// `install_dir` and `library_dirs` are the engine's own directories (where a companion
/// archive puts the libraries); the system locations of [`system_cuda_dirs`] follow.
///
/// # Errors
///
/// [`RecipeError::CudaRuntimeMissing`] naming the missing libraries and the two ways to fix it.
pub fn check_cuda_runtime(
    variant: &str,
    os: &str,
    install_dir: &Path,
    library_dirs: &[PathBuf],
) -> Result<()> {
    let Some(major) = cuda_major_of_variant(variant) else {
        return Ok(());
    };
    let needed = cuda_runtime_libraries(os, major);
    if needed.is_empty() {
        return Ok(());
    }
    let mut dirs = vec![install_dir.to_path_buf()];
    dirs.extend(library_dirs.iter().cloned());
    dirs.extend(system_cuda_dirs(os, major));
    let missing = missing_libraries(&needed, &dirs);
    if missing.is_empty() {
        return Ok(());
    }
    Err(RecipeError::CudaRuntimeMissing {
        variant: variant.to_string(),
        major,
        missing,
    })
}
