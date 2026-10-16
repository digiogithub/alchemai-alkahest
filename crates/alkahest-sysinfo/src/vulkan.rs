//! Vulkan presence probe without linking the loader.

/// Candidate loader library names per platform.
const CANDIDATES: &[&str] = &[
    "libvulkan.so.1",
    "vulkan-1.dll",
    "libvulkan.1.dylib",
    "libvulkan.dylib",
];

/// Returns whether a Vulkan loader library can be opened.
///
/// This only proves the loader is installed, not that a usable device exists; engines
/// enumerate devices themselves.
pub(crate) fn loader_present() -> bool {
    CANDIDATES.iter().any(|name| open_and_close(name))
}

#[allow(unsafe_code)]
fn open_and_close(name: &str) -> bool {
    // SAFETY: opening a shared library runs its initialisers. The Vulkan loader is a system
    // library whose initialisers are safe to run in any process, and the handle is dropped
    // immediately without resolving any symbol.
    unsafe { libloading::Library::new(name).is_ok() }
}
