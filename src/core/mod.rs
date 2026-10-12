//! Archive engine shared by the CLI and the GUI.
//!
//! Format detection ([`detector`]), the backend chain that opens/creates
//! archives ([`backends`]), typed errors ([`error`]), persisted compression
//! profiles ([`config`]) and the filesystem helpers used across the app
//! ([`fm`], [`paths`], [`util`]), the startup release check ([`update`]) and
//! the Dolphin service-menu registration ([`menus`]).

pub mod archive;
pub mod backends;
pub mod config;
pub mod detector;
pub mod error;
pub mod fm;
pub mod menus;
pub mod paths;
pub mod update;
pub mod util;
