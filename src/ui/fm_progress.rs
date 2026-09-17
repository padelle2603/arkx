//! Standalone progress app for file-manager launches.
//!
//! `arkx compress --progress …` and `arkx extract --progress …`
//! (Dolphin ServiceMenu) show the same `ProgressWindow` as the main app:
//! live 0%→100% bar with speed, ETA and Details pane, plus Cancel.
//! Differences from the in-app flow:
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
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::core::archive::ProgressInfo;
use crate::core::backends::BackendManager;
use crate::core::error::ArkxError;
use crate::ui::progress_window::ProgressWindow;

/// True when a graphical session is reachable (Wayland or X11).
pub fn has_display() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

enum FmEvent {
    Progress(ProgressInfo),
    Done(Result<(), ArkxError>),
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

/// Run the extraction showing the in-app progress window. Blocks until the
/// window is dismissed (or auto-closed on success). Never panics on headless
/// systems — callers must check [`has_display`] first.
pub fn run_extract_with_progress(
    archive: PathBuf,
    dest: PathBuf,
    password: Option<String>,
) -> anyhow::Result<()> {
    let app = adw::Application::builder()
        .application_id("io.github.padelle.arkx.fm-progress")
        .build();

    app.connect_activate(move |app| {
        activate_extract(app, archive.clone(), dest.clone(), password.clone());
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}

fn short_name(dest: &std::path::Path) -> String {
    let name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archivio".to_string());
    crate::core::util::truncate_middle(&name, 50)
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
            let _ = tx.send(FmEvent::Done(res));
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
                        Err(err) => {
                            if matches!(err, ArkxError::Cancelled) {
                                pw_c.borrow().close();
                                app_c.quit();
                            } else {
                                // Stay on screen: the user dismisses with Close.
                                pw_c.borrow().set_error(&err.to_string());
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

/// Shared state for one Dolphin extraction, reused across password retries.
#[derive(Clone)]
struct ExtractCtx {
    app: adw::Application,
    pw: Rc<RefCell<ProgressWindow>>,
    manager: Arc<BackendManager>,
    archive: PathBuf,
    dest: PathBuf,
    settled: Rc<Cell<bool>>,
}

fn activate_extract(
    app: &adw::Application,
    archive: PathBuf,
    dest: PathBuf,
    password: Option<String>,
) {
    let pw = ProgressWindow::new_standalone("Extraction in progress", &short_name(&archive));
    pw.borrow().reset();
    pw.borrow().set_operation("Extracting");
    pw.borrow().register_with_app(app);

    let ctx = ExtractCtx {
        app: app.clone(),
        pw: pw.clone(),
        manager: Arc::new(BackendManager::new()),
        archive,
        dest,
        settled: Rc::new(Cell::new(false)),
    };

    // Cancel path: abort the backend (kills 7z), then close quietly.
    {
        let ctx_c = ctx.clone();
        pw.borrow().on_cancel(move || {
            if ctx_c.settled.get() {
                return;
            }
            ctx_c.settled.set(true);
            ctx_c.manager.cancel_all();
            ctx_c.pw.borrow().close();
        });
    }

    spawn_extract_attempt(&ctx, password);
}

/// Run one extraction attempt in a worker thread and poll its events. On a
/// wrong password it asks for it (same dialog as the in-app open flow) and
/// retries; the window is never put in the error state for that case, so it
/// stays alive under the dialog.
fn spawn_extract_attempt(ctx: &ExtractCtx, password: Option<String>) {
    if ctx.settled.get() {
        return;
    }
    ctx.pw.borrow().reset();
    ctx.pw.borrow().set_operation("Extracting");

    let (tx, rx) = mpsc::channel::<FmEvent>();
    {
        let tx_prog = tx.clone();
        let manager = ctx.manager.clone();
        let (archive, dest, password) = (ctx.archive.clone(), ctx.dest.clone(), password.clone());
        std::thread::spawn(move || {
            let res = manager.extract(
                &archive,
                &dest,
                None,
                password.as_deref(),
                Some(Box::new(move |info| {
                    let _ = tx_prog.send(FmEvent::Progress(info));
                })),
            );
            let _ = tx.send(FmEvent::Done(res));
        });
    }

    let ctx_poll = ctx.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        let mut done = false;
        while let Ok(evt) = rx.try_recv() {
            match evt {
                FmEvent::Progress(info) => {
                    if info.total == 0 {
                        ctx_poll.pw.borrow().pulse(&info.file);
                    } else {
                        ctx_poll.pw.borrow().set_progress(&info);
                    }
                }
                FmEvent::Done(res) => {
                    done = true;
                    if ctx_poll.settled.get() {
                        break;
                    }
                    match res {
                        Ok(()) => {
                            ctx_poll.settled.set(true);
                            // Auto-close + reveal dest folder + notify.
                            crate::core::fm::reveal_in_file_manager(&ctx_poll.dest);
                            let _ = std::process::Command::new("notify-send")
                                .args([
                                    "Arkx",
                                    &format!("Extracted to {}", ctx_poll.dest.display()),
                                    "--icon=arkx",
                                ])
                                .output();
                            ctx_poll.pw.borrow().close();
                            ctx_poll.app.quit();
                        }
                        Err(err) if matches!(&err, ArkxError::WrongPassword) => {
                            // Encrypted archive: ask for the password (same dialog
                            // as opening in the app) and retry. The window is kept
                            // as-is so GApplication does not quit meanwhile.
                            let parent = ctx_poll.pw.borrow().root();
                            let ctx_retry = ctx_poll.clone();
                            let ctx_cancel = ctx_poll.clone();
                            crate::ui::dialogs::ask_password(
                                &parent,
                                &ctx_poll.archive,
                                "Type the password to extract it.",
                                move |pwd| spawn_extract_attempt(&ctx_retry, Some(pwd)),
                                move || {
                                    if ctx_cancel.settled.get() {
                                        return;
                                    }
                                    ctx_cancel.settled.set(true);
                                    ctx_cancel.pw.borrow().close();
                                    ctx_cancel.app.quit();
                                },
                            );
                        }
                        Err(err) => {
                            if matches!(err, ArkxError::Cancelled) {
                                ctx_poll.settled.set(true);
                                ctx_poll.pw.borrow().close();
                                ctx_poll.app.quit();
                            } else {
                                ctx_poll.settled.set(true);
                                let app_q = ctx_poll.app.clone();
                                ctx_poll.pw.borrow().set_error(&err.to_string());
                                ctx_poll.pw.borrow().finish_dismiss(move || {
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
