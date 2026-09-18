//! Modal dialogs: password prompt and simple message alerts.

use adw::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::worker::{JobKind, WorkerPool};

/// What the password prompt runs on unlock: list the archive (header-encrypted
/// open) or extract it (data-encrypted entries).
pub(crate) enum PasswordAction {
    Open {
        archive: PathBuf,
    },
    Extract {
        archive: PathBuf,
        dest: PathBuf,
        entries: Option<Vec<String>>,
    },
}

/// Password dialog shared by the in-app flow and the file-manager progress app:
/// same `adw::AlertDialog` + `PasswordEntry` (peek icon, pre-selected text),
/// Unlock disabled while empty. Calls `on_unlock` with the typed password or
/// `on_cancel` when dismissed. No job logic here: the caller decides what to
/// run and can re-show the dialog for a retry.
pub(crate) fn ask_password(
    parent: &impl IsA<gtk::Widget>,
    archive: &Path,
    hint: &str,
    on_unlock: impl Fn(String) + 'static,
    on_cancel: impl Fn() + 'static,
) {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| archive.display().to_string());
    let dialog = adw::AlertDialog::new(
        Some("Protected archive"),
        Some(&format!("{} ({})", hint, name)),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unlock", "Unlock");
    dialog.set_response_appearance("unlock", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("unlock"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("unlock", false);

    let entry = gtk::PasswordEntry::new();
    entry.set_placeholder_text(Some("Password"));
    entry.set_show_peek_icon(true);
    entry.set_margin_top(12);
    entry.set_margin_bottom(12);
    entry.set_margin_start(12);
    entry.set_margin_end(12);
    // Pre-select the previous wrong attempt so a retry just types over it.
    entry.select_region(0, i32::MAX);
    dialog.set_extra_child(Some(&entry));

    let dialog_c = dialog.clone();
    let entry_c = entry.clone();
    entry.connect_changed(move |_| {
        dialog_c.set_response_enabled("unlock", !entry_c.text().is_empty());
    });

    // The PasswordEntry consumes Enter instead of letting the dialog's default
    // response fire: forward Enter to the unlock response explicitly.
    let dialog_enter = dialog.clone();
    let entry_enter = entry.clone();
    entry.connect_activate(move |_| {
        if !entry_enter.text().is_empty() {
            let unlock = "unlock".to_value();
            dialog_enter.emit_by_name_with_values("response", &[unlock]);
        }
    });

    dialog.connect_response(None, move |_, resp| {
        if resp == "unlock" {
            on_unlock(entry.text().to_string());
        } else {
            on_cancel();
        }
    });
    dialog.present(Some(parent));
}

/// Main-app password prompt, shown BEFORE starting the job. On unlock it
/// remembers the password (so later operations on the same archive reuse it)
/// and resubmits the operation with it; the caller re-opens the prompt when the
/// backend rejects the stored password (see `window.rs`). Accepts the real
/// operation parameters so retries keep the correct action and destination.
pub(crate) fn prompt_password(
    parent: &impl IsA<gtk::Widget>,
    action: PasswordAction,
    worker: Rc<RefCell<WorkerPool>>,
    remember: Rc<RefCell<Option<String>>>,
) {
    let (archive, hint) = match &action {
        PasswordAction::Open { archive } => {
            (archive.clone(), "Type the password to open this archive.")
        }
        PasswordAction::Extract { archive, .. } => {
            (archive.clone(), "Type the password to extract it.")
        }
    };
    ask_password(
        parent,
        &archive,
        hint,
        move |pwd| {
            // Remember for the rest of the session (same archive): extraction,
            // re-listing, everything reuses it instead of prompting again.
            *remember.borrow_mut() = Some(pwd.clone());
            match &action {
                PasswordAction::Open { archive } => worker.borrow_mut().submit(JobKind::List {
                    path: archive.clone(),
                    password: Some(pwd),
                }),
                PasswordAction::Extract {
                    archive,
                    dest,
                    entries,
                } => worker.borrow_mut().submit(JobKind::Extract {
                    archive: archive.clone(),
                    dest: dest.clone(),
                    entries: entries.clone(),
                    password: Some(pwd),
                }),
            }
        },
        || {},
    );
}
