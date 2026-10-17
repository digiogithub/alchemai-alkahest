//! Engine recipes: download a prebuilt engine or build one from source for a given accelerator,
//! described by TOML files under `recipes/`.
//!
//! This is the minimal slice needed by the bundled CPU llama.cpp (ALK-US-0026,
//! ALK-SP-0003.R6); the full recipe system (accelerator probing, source builds, recipe
//! updates: ALK-US-0049) extends the same types.
//!
//! - [`Recipe`] is the data model: an engine id, an accelerator `variant`, a pinned upstream
//!   `tag`, and per-platform [`Asset`]s with a SHA-256. [`builtin_recipes`] returns the recipes
//!   compiled into the binary.
//! - [`install`] downloads the asset for a [`Host`] with `alkahest-fetch` (resumable, digest
//!   verified), unpacks it into `<engines_dir>/<id>/<tag>-<variant>/` and writes a marker file.
//!   The directory appears atomically: a failed install leaves no half-unpacked engine.
//! - [`list_installed`] and [`locate`] find installed engines, in the engines directory or in
//!   directories shipped next to the binary ([`bundled_roots`]).
//!
//! # Running an installed engine
//!
//! The Linux and macOS llama.cpp archives carry `RUNPATH=$ORIGIN` (Linux) or `@loader_path`
//! (macOS) and load their `libggml-cpu-*` backends from the executable's directory, so the
//! server runs from the install directory without `LD_LIBRARY_PATH`. The supervisor should
//! still set the child's working directory to [`InstalledEngine::dir`]. On Windows the DLLs
//! sit next to the executable, which the loader searches first.

mod archive;
mod build;
mod catalog;
mod error;
mod install;
mod recipe;
mod runtime;

pub use archive::{extract, strip_components};
pub use build::BuildTools;
pub use catalog::{Catalog, DiscoveryIssue};
pub use error::{RecipeError, Result};
pub use install::{
    ENGINE_LICENSE_FILE, InstallOptions, InstalledEngine, MARKER_FILE, bundled_roots,
    engine_license_text, install, list_installed, locate, remove_installed,
};
pub use recipe::{
    AcceleratorKind, ArchiveFormat, Asset, Companion, Host, Recipe, SourceBuild, VenvBuild,
    parse_recipes, select_variant,
};
pub use runtime::{
    check_cuda_runtime, cuda_major_of_variant, cuda_runtime_libraries, missing_libraries,
    system_cuda_dirs,
};

/// The built-in recipe for `id` and `variant` (`None` means the CPU variant, the default that
/// works on every host), newest version first.
///
/// # Errors
///
/// [`RecipeError::UnknownEngine`] or [`RecipeError::UnknownVariant`].
pub fn find_recipe(id: &str, variant: Option<&str>) -> Result<Recipe> {
    let catalog = Catalog::builtin();
    let host = Host::current();
    match variant {
        Some(v) => catalog.resolve(id, None, Some(v), None, &host).cloned(),
        // Engines with a CPU build default to it; the others (vLLM) follow the hardware.
        None => catalog
            .resolve(id, None, Some("cpu"), None, &host)
            .or_else(|_| catalog.resolve(id, None, None, Some(&alkahest_sysinfo::detect()), &host))
            .cloned(),
    }
}

/// The recipes compiled into the binary, one per engine, version and variant.
pub fn builtin_recipes() -> Vec<Recipe> {
    Catalog::builtin().recipes().to_vec()
}
