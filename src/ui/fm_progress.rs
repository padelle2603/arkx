//! Standalone compression-progress app for file-manager launches.
//!
//! `arkx compress --progress …` (Dolphin ServiceMenu) shows the same
//! `ProgressWindow` as the main app: live 0%→100% bar with speed, ETA and
//! Details pane, plus Cancel. Differences from the in-app flow:
//!
//! * no parent window — the progress window is top-level;
//! * on success the window **auto-closes** and a desktop notification +
//!   file-manager highlight confirm the result (choice: no extra click);
//! * on error the window **stays** with the message and a Close button,
//!   so failures are never missed;
//! * on Cancel the backend is killed and the partial archive deleted.
//!
//! A dedicated application id avoids merging into a running Arkx main
//! window (GApplication single-instance).

use adw::prelude::*;
use gtk4 as gtk;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::core::archive::ProgressInfo;
use crate::core::backends::BackendManager;
use crate::ui::progress_window::ProgressWindow;

/// True when a graphical session is reachable (Wayland or X11).
pub fn has_display() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

enum FmEvent {
    Progress(ProgressInfo),
    Done(Result<(), String>),
}

/// Run the compression showing the in-app progress window. Blocks until the
/// window is dismissed (or auto-closed on success). Never panics on headless
/// systems — callers must check [`has_display`] first.
pub fn run_compress_with_progress(
    dest: PathBuf,
    sources: Vec<PathBuf>,
    level: u8,
    password: Option<String>,
) -> anyhow::Result<()> {
    let app = adw::Application::builder()
        .application_id("io.github.padelle.arkx.fm-progress")
        .build();

    app.connect_activate(move |app| {
        activate(app, dest.clone(), sources.clone(), level, password.clone());
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn short_name(dest: &std::path::Path) -> String {
    let name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivio".to_string());
    if name.chars().count() > 50 {
        let chars: Vec<char> = name.chars().collect();
        let half = (50 - 3) / 2;
        let start: String = chars[..half].iter().collect();
        let end: String = chars[chars.len() - half..].iter().collect();
        format!("{}...{}", start, end)
    } else {
        name
    }
}

fn activate(
    app: &adw::Application,
    dest: PathBuf,
    sources: Vec<PathBuf>,
    level: u8,
    password: Option<String>,
) {
    let pw = ProgressWindow::new_standalone("Compression in progress", &short_name(&dest));
    pw.borrow().reset();
    pw.borrow().set_operation("Compressing");
    // Keep GApplication::run alive while the window is open.
    pw.borrow().register_with_app(app);

    let manager = Arc::new(BackendManager::new());
    let (tx, rx) = mpsc::channel::<FmEvent>();

    // Worker thread: identical backend call as the headless CLI path.
    {
        let tx_prog = tx.clone();
        let manager = manager.clone();
        let (dest, sources, password) = (dest.clone(), sources.clone(), password.clone());
        std::thread::spawn(move || {
            let res = manager.create(
                &dest,
                &sources,
                level,
                password.as_deref(),
                Some(Box::new(move |info| {
                    let _ = tx_prog.send(FmEvent::Progress(info));
                })),
            );
            let _ = tx.send(FmEvent::Done(res.map_err(|e| e.to_string())));
        });
    }

    // Cancel path: abort the backend (kills 7z, deletes the partial file),
    // then close quietly — no notification for an explicit cancel.
    let settled = Rc::new(Cell::new(false));
    {
        let pw_c = pw.clone();
        let settled_c = settled.clone();
        let manager_c = manager.clone();
        pw.borrow().on_cancel(move || {
            if settled_c.get() {
                return;
            }
            settled_c.set(true);
            manager_c.cancel_all();
            pw_c.borrow().close();
        });
    }

    // If the window is closed via X/Esc after settling (error outcome), quit.
    // (on_cancel already routes Esc/X through the cancel closure above.)
    let app_c = app.clone();
    let settled_c = settled.clone();
    let pw_c = pw.clone();
    let dest_c = dest.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        let mut done = false;
        while let Ok(evt) = rx.try_recv() {
            match evt {
                FmEvent::Progress(info) => {
                    if info.total == 0 {
                        pw_c.borrow().pulse(&info.file);
                    } else {
                        pw_c.borrow().set_progress(&info);
                    }
                }
                FmEvent::Done(res) => {
                    done = true;
                    if settled_c.get() {
                        break;
                    }
                    settled_c.set(true);
                    match res {
                        Ok(()) => {
                            // Auto-close + notify (no extra click on success).
                            crate::core::fm::notify_created(&dest_c);
                            crate::core::fm::reveal_in_file_manager(&dest_c);
                            pw_c.borrow().close();
                            app_c.quit();
                        }
                        Err(msg) => {
                            if msg.contains("Cancelled") {
                                pw_c.borrow().close();
                                app_c.quit();
                            } else {
                                // Stay on screen: the user dismisses with Close.
                                pw_c.borrow().set_error(&msg);
                                let app_q = app_c.clone();
                                pw_c.borrow().finish_dismiss(move || {
                                    app_q.quit();
                                });
                            }
                        }
                    }
                    break;
                }
            }
        }
        if done {
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
}
