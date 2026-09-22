//! GTK4/libadwaita interface.
//!
//! The main archive-browser window ([`window`], re-exported as [`build_ui`]),
//! shared dialogs ([`dialogs`]), the in-app progress window
//! ([`progress_window`]) and the headless progress app used by the Dolphin
//! service menus ([`fm_progress`]).

pub mod browser;
pub mod dialogs;
pub mod fm_progress;
pub mod progress_window;
pub mod window;

pub use window::build_ui;
