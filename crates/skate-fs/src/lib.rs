//! Filesystem access for every target.
//!
//! Native builds re-export `std::fs` unchanged, plus the `Path` queries
//! (`exists`, `is_file`, `is_dir`, `canonicalize`) as free functions, because
//! `Path`'s own methods call the OS directly and cannot be redirected.
//!
//! The browser build has no filesystem. There, paths under the virtual root
//! [`WEB_ROOT`] map to the web server's data directory: the loader installs the
//! server's file index before `main` runs, prefetches the startup set, and any
//! other file is fetched synchronously on first read. Writes land in an
//! in-memory overlay; small files are persisted to `localStorage` so settings
//! survive a reload. See `web.rs`.
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::*;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

/// Where the server's data directory appears in the browser build.
pub const WEB_ROOT: &str = "/data";

#[cfg(not(target_arch = "wasm32"))]
pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_file(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_dir()
}

/// `std::env::temp_dir`, which panics in the browser; there it is a
/// directory in the virtual filesystem's (unpersisted) overlay.
pub fn temp_dir() -> std::path::PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    return std::env::temp_dir();
    #[cfg(target_arch = "wasm32")]
    return std::path::PathBuf::from("/tmp");
}

/// `std::process::id`, which panics in the browser. Callers use it to tell
/// processes apart in file names and session ids; a page is one process, so
/// the browser build reports a fixed id.
pub fn process_id() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::process::id();
    #[cfg(target_arch = "wasm32")]
    return 1;
}
