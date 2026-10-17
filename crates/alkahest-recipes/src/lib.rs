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
mod error;
mod install;
mod recipe;

pub use archive::{extract, strip_components};
pub use error::{RecipeError, Result};
pub use install::{
    InstallOptions, InstalledEngine, MARKER_FILE, bundled_roots, install, list_installed, locate,
};
pub use recipe::{ArchiveFormat, Asset, Host, Recipe, RecipeKind, builtin_recipes, find_recipe};
