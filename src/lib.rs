//! Arkx — fast multi-threaded archive manager for Linux.
//!
//! Ships as a single binary that launches either the GTK4/libadwaita GUI or
//! the CLI depending on invocation: `arkx` / `arkx archive.zip` → GUI,
//! `arkx l/... x/a/r …` → CLI. Both share the same engine.
//!
//! ```
//! use arkx::core::{backends::BackendManager, detector::detect_format};
//!
//! let fmt = detect_format(std::path::Path::new("archive.zip"));
//! let backend = BackendManager::new();
//! // `backend.detect_and_list(&path)` returns the archive listing, etc.
//! ```
//!
//! Module layout:
//! - [`core`] — engine: format detection, backends (native + 7z/bsdtar
//!   fallback), typed errors, config and filesystem helpers
//! - [`worker`] — background job pool that keeps the UI responsive/cancellable
//! - [`ui`] — GTK4/libadwaita windows, dialogs and integration points

pub mod core;
pub mod ui;
pub mod worker;
