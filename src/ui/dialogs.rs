//! Modal dialogs: password prompt and simple message alerts.

use adw::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::ui::browser::AppState;
use crate::worker::{JobKind, WorkerPool};

/// Password prompt for protected archives. Resubmits the extraction with the
/// typed password when unlocking.
pub(crate) fn show_password_dialog(
    parent: &impl gtk::prelude::IsA<gtk::Widget>,
    state: Rc<RefCell<AppState>>,
    worker: Rc<RefCell<WorkerPool>>,
) {
    let dialog = adw::AlertDialog::new(
        Some("Protected archive"),
        Some("Enter the password to open this archive."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unlock", "Unlock");
    dialog.set_response_appearance("unlock", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("unlock"));
    dialog.set_close_response("cancel");

    let entry = gtk::PasswordEntry::new();
    entry.set_placeholder_text(Some("Password"));
    entry.set_show_peek_icon(true);
    entry.set_margin_top(12);
    entry.set_margin_bottom(12);
    entry.set_margin_start(12);
    entry.set_margin_end(12);
    dialog.set_extra_child(Some(&entry));

    let state_c = state.clone();
    let worker_c = worker.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp == "unlock" {
            let pwd = entry.text().to_string();
            if let Some(arch) = state_c.borrow().current_archive.clone() {
                let dest = arch.parent().unwrap_or(Path::new("/tmp")).to_path_buf();
                worker_c.borrow_mut().submit(JobKind::Extract {
                    archive: arch,
                    dest,
                    entries: None,
                    password: Some(pwd),
                });
            }
        }
    });
    dialog.present(Some(parent));
}
