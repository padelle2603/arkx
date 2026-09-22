//! Archive engine shared by the CLI and the GUI.
//!
//! Format detection ([`detector`]), the backend chain that opens/creates
//! archives ([`backends`]), typed errors ([`error`]), persisted compression
//! profiles ([`config`]) and the filesystem helpers used across the app
//! ([`fm`], [`paths`], [`util`]).

pub mod archive;
pub mod backends;
pub mod config;
pub mod detector;
pub mod error;
pub mod fm;
pub mod paths;
pub mod util;
