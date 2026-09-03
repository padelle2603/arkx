use adw::prelude::*;
use gtk4 as gtk;
use gtk::{glib, gio, gdk};
use std::cell::RefCell;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::time::{Instant, Duration};

use crate::core::paths;
use crate::ui::browser::{
    AppState, create_empty_state, create_table_header, filter_list, get_all_descendants,
    get_children, navigate_up, populate_current_view, truncate_middle, update_breadcrumb,
};
use crate::ui::dialogs;
use crate::worker::{WorkerPool, JobKind, WorkerEvent, JobResult};
use crate::ui::progress_window::ProgressWindow;

/// Handles shared by every UI callback. Cloning is cheap (Rc + refcounted widgets).
#[derive(Clone)]
struct Ui {
    state: Rc<RefCell<AppState>>,
    worker: Rc<RefCell<WorkerPool>>,
    is_busy: Rc<RefCell<bool>>,
    last_extract: Rc<RefCell<Option<(PathBuf, PathBuf, Instant)>>>,
    last_progress: Rc<RefCell<(f32, Instant)>>,
    progress_window: Rc<RefCell<Option<Rc<RefCell<ProgressWindow>>>>>,
    breadcrumb: gtk::Box,
    list: gtk::ListBox,
    empty: gtk::Box,
    status_left: gtk::Label,
    status_right: gtk::Label,
    info: gtk::Label,
    extract_all: gtk::Button,
    extract_sel: gtk::Button,
    back: gtk::Button,
    open: gtk::Button,
    window: adw::ApplicationWindow,
}

// Builds the main UI
pub fn build_ui(app: &adw::Application) {
    // CSS distintivo — responsive + frame + non generico
    let css = r#"
        window { background: #09090b; }
        .app-frame { margin: 12px; border: 1px solid #27272a; border-radius: 12px; background: #0f0f0f; box-shadow: 0 8px 24px rgba(0,0,0,0.5), 0 1px 3px rgba(0,0,0,0.3); overflow: hidden; }
        .app-header { background: #1e1e1e; border-bottom: 1px solid #2a2a2a; padding: 6px 8px; }
        .header-btn { padding: 6px 10px; border-radius: 8px; }
        .header-btn.suggested-action { background: #2563eb; color: white; }
        .header-btn.suggested-action:hover { background: #1d4ed8; }
        .file-table { background: #0f0f0f; }
        .file-row { padding: 8px 12px; border-bottom: 1px solid #1f1f23; min-height: 44px; }
        .file-row:hover { background: #1e1e2e; }
        .file-row:selected { background: #2563eb; color: white; }
        .file-row:selected label { color: white; }
        .status-bar { background: #1e1e1e; border-top: 1px solid #27272a; padding: 6px 12px; font-size: 12px; color: #a1a1aa; }
        .progress-bar trough { min-height: 6px; background: #27272a; border-radius: 3px; }
        .progress-bar progress { background: #3b82f6; border-radius: 3px; transition: width 100ms ease; }
        .empty-state { padding: 48px; color: #71717a; }
        .empty-state title { font-size: 18px; font-weight: 600; color: #e4e4e7; margin-bottom: 8px; }
        .search-entry { background: #27272a; border: 1px solid #3f3f46; border-radius: 8px; padding: 6px 12px; min-width: 140px; }
        .search-entry:focus { border-color: #3b82f6; }
        .icon-folder { color: #f59e0b; }
        .icon-file { color: #71717a; }
        .icon-archive { color: #3b82f6; }
        .badge { background: #27272a; border-radius: 12px; padding: 2px 8px; font-size: 11px; color: #a1a1aa; }
        .badge-encrypted { background: #dc2626; color: white; }
        .breadcrumb { background: #18181b; border-bottom: 1px solid #27272a; padding: 6px 12px; }
        .breadcrumb-btn { padding: 4px 8px; border-radius: 6px; }
        .breadcrumb-btn:hover { background: #27272a; }
        .breadcrumb-sep { color: #52525b; margin: 0 2px; }
        .table-header { background: #18181b; border-bottom: 1px solid #27272a; padding: 6px 12px; font-size: 11px; font-weight: 600; color: #a1a1aa; letter-spacing: 0.5px; }
        /* Scrollbar sempre visibili quando serve */
        scrollbar { opacity: 1; }
        scrollbar slider { min-width: 8px; min-height: 8px; background: #3f3f46; border-radius: 4px; }
        scrollbar slider:hover { background: #52525b; }
        scrolledwindow { border: none; }
        /* Responsive helpers */
        .narrow .hide-narrow { display: none; }
        .narrow .header-btn label { font-size: 12px; }
        .narrow .file-row { padding: 6px 8px; }
        .medium .hide-medium { display: none; }
        .header-wrap { flex-wrap: wrap; }
        .narrow .app-frame { margin: 6px; border-radius: 8px; }
    "#;
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Arkx"));
    window.set_default_size(1050, 680);

    // Global state
    let state = Rc::new(RefCell::new(AppState {
        current_archive: None,
        current_info: None,
        current_path: String::new(),
        selected_entries: Vec::new(),
        filter_text: String::new(),
    }));

    let worker = Rc::new(RefCell::new(WorkerPool::new()));
    let is_busy = Rc::new(RefCell::new(false));
    let last_extract = Rc::new(RefCell::new(None::<(PathBuf, PathBuf, Instant)>));
    let last_progress = Rc::new(RefCell::new((0.0f32, Instant::now())));

    // Root layout with a ToolbarView for responsiveness
    let toolbar_view = adw::ToolbarView::new();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // === HEADER BAR === (Open/Create live in drag&drop, empty-state and Ctrl+O)
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&create_title_widget()));
    header.add_css_class("app-header");

    let extract_all_btn = gtk::Button::new();
    let extract_all_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    extract_all_box.append(&gtk::Image::from_icon_name("folder-download-symbolic"));
    extract_all_box.append(&gtk::Label::new(Some("Extract all")));
    extract_all_btn.set_child(Some(&extract_all_box));
    extract_all_btn.set_tooltip_text(Some("Extract the whole archive into the archive folder"));
    extract_all_btn.add_css_class("header-btn");
    extract_all_btn.add_css_class("suggested-action");
    extract_all_btn.set_sensitive(false);
    header.pack_start(&extract_all_btn);

    let extract_sel_btn = gtk::Button::new();
    let sel_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    sel_box.append(&gtk::Image::from_icon_name("document-save-as-symbolic"));
    sel_box.append(&gtk::Label::new(Some("Extract selected")));
    extract_sel_btn.set_child(Some(&sel_box));
    extract_sel_btn.set_tooltip_text(Some("Extract only the selected items (right click)"));
    extract_sel_btn.add_css_class("header-btn");
    extract_sel_btn.set_sensitive(false);
    header.pack_start(&extract_sel_btn);

    // Search
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Filter…"));
    search_entry.set_hexpand(false);
    search_entry.set_width_request(180);
    search_entry.add_css_class("search-entry");
    header.pack_end(&search_entry);

    toolbar_view.add_top_bar(&header);
    toolbar_view.add_css_class("app-frame");

    // Hidden Ctrl+O helper (kept alive in the widget tree)
    let open_btn = gtk::Button::new();
    open_btn.set_visible(false);
    root.append(&open_btn);

    // Content area
    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.add_css_class("file-table");
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);

    // Clickable file-manager-style breadcrumb with an X scrollbar when needed
    let breadcrumb_scroll = gtk::ScrolledWindow::new();
    breadcrumb_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    breadcrumb_scroll.set_propagate_natural_height(true);
    breadcrumb_scroll.set_overlay_scrolling(false);
    breadcrumb_scroll.set_has_frame(false);
    breadcrumb_scroll.add_css_class("breadcrumb");
    let breadcrumb_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    breadcrumb_box.set_hexpand(true);
    breadcrumb_box.set_margin_top(4);
    breadcrumb_box.set_margin_bottom(4);
    // Back button
    let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    back_btn.set_tooltip_text(Some("Back (Alt+←)"));
    back_btn.add_css_class("flat");
    back_btn.add_css_class("circular");
    back_btn.set_sensitive(false);
    breadcrumb_box.append(&back_btn);
    let breadcrumb_content = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    breadcrumb_content.set_hexpand(true);
    breadcrumb_box.append(&breadcrumb_content);
    let info_label = gtk::Label::new(None);
    info_label.add_css_class("badge");
    info_label.set_visible(false);
    breadcrumb_box.append(&info_label);
    breadcrumb_scroll.set_child(Some(&breadcrumb_box));
    content_box.append(&breadcrumb_scroll);

    // Responsive table header wrapped in a horizontal ScrolledWindow synced
    // with the list for the X scrollbar
    let table_header = create_table_header();
    let header_scroll = gtk::ScrolledWindow::new();
    header_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    header_scroll.set_overlay_scrolling(false);
    header_scroll.set_has_frame(false);
    header_scroll.set_propagate_natural_width(true);
    header_scroll.set_child(Some(&table_header));
    content_box.append(&header_scroll);

    // File list (ScrolledWindow + ListBox) with automatic X/Y scrollbars
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hexpand(true);
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_overlay_scrolling(false);
    scrolled.set_has_frame(true);
    scrolled.set_propagate_natural_width(true);
    scrolled.set_propagate_natural_height(true);
    scrolled.set_min_content_height(200);
    scrolled.set_min_content_width(400);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Multiple);
    list_box.add_css_class("file-table");
    list_box.set_activate_on_single_click(false);
    list_box.set_halign(gtk::Align::Fill);
    list_box.set_hexpand(true);
    // Sync the header ↔ list horizontal scrolling for the X scrollbar
    header_scroll.set_hadjustment(Some(&scrolled.hadjustment()));

    // Empty state with an Open button (Open was removed from the header)
    let empty_state = create_empty_state();
    let empty_open_btn = gtk::Button::new();
    let empty_open_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    empty_open_box.append(&gtk::Image::from_icon_name("document-open-symbolic"));
    empty_open_box.append(&gtk::Label::new(Some("Open archive")));
    empty_open_btn.set_child(Some(&empty_open_box));
    empty_open_btn.add_css_class("suggested-action");
    empty_open_btn.add_css_class("pill");
    empty_open_btn.set_margin_top(12);
    empty_state.append(&empty_open_btn);
    let open_btn_clone_for_empty = open_btn.clone();
    empty_open_btn.connect_clicked(move |_| {
        open_btn_clone_for_empty.emit_clicked();
    });

    let empty_overlay = gtk::Overlay::new();
    empty_overlay.set_child(Some(&scrolled));
    empty_overlay.add_overlay(&empty_state);
    scrolled.set_child(Some(&list_box));

    content_box.append(&empty_overlay);

    // Separate progress window (dismissed by the user: Close button or Esc).
    // Holder for the current progress window
    let progress_window: Rc<RefCell<Option<Rc<RefCell<crate::ui::progress_window::ProgressWindow>>>>> = Rc::new(RefCell::new(None));

    // Responsive status bar
    let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status_bar.add_css_class("status-bar");
    let status_left = gtk::Label::new(Some(""));
    status_left.set_xalign(0.0);
    status_left.set_hexpand(true);
    status_left.set_ellipsize(pango::EllipsizeMode::End);
    let status_right = gtk::Label::new(Some(""));
    status_right.add_css_class("dim-label");
    status_right.add_css_class("hide-narrow");
    status_bar.append(&status_left);
    status_bar.append(&status_right);
    content_box.append(&status_bar);

    toolbar_view.set_content(Some(&content_box));
    root.append(&toolbar_view);
    window.set_content(Some(&root));

    // Breakpoints for responsiveness
    let bp_narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 650px").unwrap());
    let root_clone_a = root.clone();
    let root_clone_b = root.clone();
    bp_narrow.connect_apply(move |_| {
        root_clone_a.add_css_class("narrow");
    });
    bp_narrow.connect_unapply(move |_| {
        root_clone_b.remove_css_class("narrow");
    });
    window.add_breakpoint(bp_narrow);

    let bp_medium = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 900px").unwrap());
    let root_clone2_a = root.clone();
    let root_clone2_b = root.clone();
    bp_medium.connect_apply(move |_| {
        root_clone2_a.add_css_class("medium");
    });
    bp_medium.connect_unapply(move |_| {
        root_clone2_b.remove_css_class("medium");
    });
    window.add_breakpoint(bp_medium);

    // Actions for the context menu (popovers are built per row on right click)
    let action_extract_here = gio::SimpleAction::new("extract-selected-here", None);
    let action_extract_to = gio::SimpleAction::new("extract-selected-to", None);
    let action_copy = gio::SimpleAction::new("copy-path", None);
    window.add_action(&action_extract_here);
    window.add_action(&action_extract_to);
    window.add_action(&action_copy);

    // Bundle shared handles for the callbacks below.
    let ui = Ui {
        state: state.clone(),
        worker: worker.clone(),
        is_busy: is_busy.clone(),
        last_extract: last_extract.clone(),
        last_progress: last_progress.clone(),
        progress_window: progress_window.clone(),
        breadcrumb: breadcrumb_content.clone(),
        list: list_box.clone(),
        empty: empty_state.clone(),
        status_left: status_left.clone(),
        status_right: status_right.clone(),
        info: info_label.clone(),
        extract_all: extract_all_btn.clone(),
        extract_sel: extract_sel_btn.clone(),
        back: back_btn.clone(),
        open: open_btn.clone(),
        window: window.clone(),
    };

    // === DRAG & DROP ===
    let drop_target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
    let ui_drop = ui.clone();
    drop_target.connect_drop(move |_, value, _, _| {
        if let Ok(file) = value.get::<gio::File>() {
            if let Some(path) = file.path() {
                open_archive(path, ui_drop.clone());
                return true;
            }
        }
        false
    });
    root.add_controller(drop_target);

    // === ACTIONS ===
    // Open file
    let ui_open = ui.clone();
    open_btn.connect_clicked(move |_| {
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Open archive");
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Archives"));
        filter.add_mime_type("application/zip");
        filter.add_mime_type("application/x-7z-compressed");
        filter.add_mime_type("application/vnd.rar");
        filter.add_mime_type("application/x-rar");
        filter.add_mime_type("application/x-tar");
        filter.add_mime_type("application/x-compressed-tar");
        filter.add_mime_type("application/x-bzip-compressed-tar");
        filter.add_mime_type("application/x-tarz");
        filter.add_mime_type("application/x-xz-compressed-tar");
        filter.add_mime_type("application/x-lzma-compressed-tar");
        filter.add_mime_type("application/x-lzip-compressed-tar");
        filter.add_mime_type("application/x-tzo");
        filter.add_mime_type("application/x-lrzip-compressed-tar");
        filter.add_mime_type("application/x-lz4-compressed-tar");
        filter.add_mime_type("application/x-zstd-compressed-tar");
        filter.add_mime_type("application/gzip");
        filter.add_mime_type("application/x-bzip");
        filter.add_mime_type("application/x-bzip2");
        filter.add_mime_type("application/x-xz");
        filter.add_mime_type("application/x-lzma");
        filter.add_mime_type("application/x-compress");
        filter.add_mime_type("application/zstd");
        filter.add_mime_type("application/x-lz4");
        filter.add_mime_type("application/x-iso9660-image");
        filter.add_mime_type("application/x-cd-image");
        filter.add_mime_type("application/x-iso9660-appimage");
        filter.add_mime_type("application/vnd.ms-cab-compressed");
        filter.add_mime_type("application/x-bcpio");
        filter.add_mime_type("application/x-cpio");
        filter.add_mime_type("application/x-cpio-compressed");
        filter.add_mime_type("application/x-sv4cpio");
        filter.add_mime_type("application/x-sv4crc");
        filter.add_mime_type("application/x-xar");
        filter.add_mime_type("application/x-archive");
        filter.add_mime_type("application/x-source-rpm");
        filter.add_mime_type("application/x-rpm");
        filter.add_mime_type("application/x-lha");
        filter.add_pattern("*.zip");
        filter.add_pattern("*.7z");
        filter.add_pattern("*.rar");
        filter.add_pattern("*.tar");
        filter.add_pattern("*.tar.gz");
        filter.add_pattern("*.tgz");
        filter.add_pattern("*.tar.bz2");
        filter.add_pattern("*.tbz2");
        filter.add_pattern("*.tar.xz");
        filter.add_pattern("*.txz");
        filter.add_pattern("*.tar.zst");
        filter.add_pattern("*.tzst");
        filter.add_pattern("*.tar.lz4");
        filter.add_pattern("*.tar.Z");
        filter.add_pattern("*.taz");
        filter.add_pattern("*.tar.lzma");
        filter.add_pattern("*.tlz");
        filter.add_pattern("*.tar.lz");
        filter.add_pattern("*.tzo");
        filter.add_pattern("*.tar.lzo");
        filter.add_pattern("*.tar.lrz");
        filter.add_pattern("*.gz");
        filter.add_pattern("*.bz2");
        filter.add_pattern("*.xz");
        filter.add_pattern("*.lzma");
        filter.add_pattern("*.Z");
        filter.add_pattern("*.zst");
        filter.add_pattern("*.lz4");
        filter.add_pattern("*.iso");
        filter.add_pattern("*.img");
        filter.add_pattern("*.AppImage");
        filter.add_pattern("*.appimage");
        filter.add_pattern("*.cab");
        filter.add_pattern("*.cpio");
        filter.add_pattern("*.bcpio");
        filter.add_pattern("*.xar");
        filter.add_pattern("*.xip");
        filter.add_pattern("*.a");
        filter.add_pattern("*.ar");
        filter.add_pattern("*.lha");
        filter.add_pattern("*.lzh");
        filter.add_pattern("*.rpm");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));

        let ui_dlg = ui_open.clone();
        let parent = ui_open.window.clone();
        dialog.open(Some(&parent), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    open_archive(path, ui_dlg.clone());
                }
            }
        });
    });

    // Live filter (current view only)
    let ui_filter = ui.clone();
    search_entry.connect_search_changed(move |entry| {
        let text = entry.text().to_string().to_lowercase();
        ui_filter.state.borrow_mut().filter_text = text.clone();
        filter_list(&ui_filter.list, &text);
    });

    // Extract all
    let ui_all = ui.clone();
    extract_all_btn.connect_clicked(move |_| {
        if *ui_all.is_busy.borrow() {
            ui_all.status_left.set_text("An operation is already in progress…");
            return;
        }
        let st = ui_all.state.borrow();
        if let Some(archive) = st.current_archive.clone() {
            let dest = archive.parent().unwrap_or(Path::new("/tmp")).to_path_buf();
            drop(st);
            // Fast duplicate guard (fixes double-extraction bugs)
            {
                let mut le = ui_all.last_extract.borrow_mut();
                if let Some((la, ld, t)) = &*le {
                    if la == &archive && ld == &dest && t.elapsed().as_secs() < 2 {
                        eprintln!("[guard] duplicate Extract-all ignored");
                        return;
                    }
                }
                *le = Some((archive.clone(), dest.clone(), Instant::now()));
            }
            // Extract all: entries = None
            start_extract(archive, dest, Vec::new(), None, ui_all.clone());
        }
    });

    // Extract selected (header button)
    let ui_sel = ui.clone();
    extract_sel_btn.connect_clicked(move |_| {
        if *ui_sel.is_busy.borrow() {
            ui_sel.status_left.set_text("An operation is already in progress…");
            return;
        }
        let st = ui_sel.state.borrow();
        if let Some(archive) = st.current_archive.clone() {
            let dest = archive.parent().unwrap_or(Path::new("/tmp")).to_path_buf();
            let selected = st.selected_entries.clone();
            let info_opt = st.current_info.clone();
            drop(st);
            if selected.is_empty() {
                ui_sel.status_left.set_text("No items selected");
                return;
            }
            let expanded = if let Some(info) = info_opt {
                get_all_descendants(&info, &selected)
            } else {
                selected
            };
            {
                let mut le = ui_sel.last_extract.borrow_mut();
                if let Some((la, ld, t)) = &*le {
                    if la == &archive && ld == &dest && t.elapsed().as_secs() < 2 {
                        eprintln!("[guard] duplicate Extract-selected ignored");
                        return;
                    }
                }
                *le = Some((archive.clone(), dest.clone(), Instant::now()));
            }
            start_extract(archive, dest, expanded, None, ui_sel.clone());
        }
    });

    // Back button
    let ui_back = ui.clone();
    back_btn.connect_clicked(move |_| {
        let mut st = ui_back.state.borrow_mut();
        if st.current_path.is_empty() { return; }
        // Drop the last component.
        st.current_path = navigate_up(&st.current_path);
        let cur = st.current_path.clone();
        let info_opt = st.current_info.clone();
        let filter = st.filter_text.clone();
        drop(st);
        if let Some(info) = info_opt {
            update_breadcrumb(&ui_back.breadcrumb, &cur, &info, ui_back.state.clone(), ui_back.list.clone(), ui_back.status_left.clone(), ui_back.status_right.clone());
            populate_current_view(&ui_back.list, &info, &cur, &filter);
            ui_back.back.set_sensitive(!cur.is_empty());
            ui_back.status_left.set_text(&format!("Folder: /{}", if cur.is_empty() { "".to_string() } else { cur.clone() }));
        }
    });

    // Archive creation lives in the CLI (`arkx a`); the header stays clean.

    // Row selection → update selected state + button sensitivity (skip __UP__)
    let ui_rows = ui.clone();
    list_box.connect_selected_rows_changed(move |lb| {
        let mut selected = Vec::new();
        for row in lb.selected_rows() {
            if let Some(p) = row.tooltip_text() {
                let s = p.to_string();
                if s == "__UP__" { continue; }
                selected.push(s);
            }
        }
        let has_sel = !selected.is_empty();
        ui_rows.state.borrow_mut().selected_entries = selected;
        ui_rows.extract_sel.set_sensitive(has_sel);
    });

    // Double click / row activation for file-manager navigation
    let ui_nav = ui.clone();
    list_box.connect_row_activated(move |_lb, row| {
        if let Some(path) = row.tooltip_text() {
            let path_str = path.to_string();
            let st = ui_nav.state.borrow();
            let info_opt = st.current_info.clone();
            let cur = st.current_path.clone();
            drop(st);
            if path_str == "__UP__" {
                // Go up one level.
                let mut st_mut = ui_nav.state.borrow_mut();
                st_mut.current_path = navigate_up(&st_mut.current_path);
                let new_path = st_mut.current_path.clone();
                let filter_c = st_mut.filter_text.clone();
                let info_c = st_mut.current_info.clone().unwrap();
                drop(st_mut);
                update_breadcrumb(&ui_nav.breadcrumb, &new_path, &info_c, ui_nav.state.clone(), ui_nav.list.clone(), ui_nav.status_left.clone(), ui_nav.status_right.clone());
                populate_current_view(&ui_nav.list, &info_c, &new_path, &filter_c);
                ui_nav.back.set_sensitive(!new_path.is_empty());
                ui_nav.status_left.set_text(&format!("Folder: /{}", if new_path.is_empty() { "".to_string() } else { new_path.clone() }));
                return;
            }
            if let Some(info) = info_opt {
                // Find the matching entry among the children.
                let children = get_children(&info, &cur);
                if let Some(entry) = children.iter().find(|e| e.path == path_str) {
                    if entry.is_dir {
                        // Dive in.
                        let mut st_mut = ui_nav.state.borrow_mut();
                        st_mut.current_path = paths::with_trailing_slash(&entry.path.clone());
                        let new_path = st_mut.current_path.clone();
                        let filter_c = st_mut.filter_text.clone();
                        let info_c = st_mut.current_info.clone().unwrap();
                        drop(st_mut);
                        update_breadcrumb(&ui_nav.breadcrumb, &new_path, &info_c, ui_nav.state.clone(), ui_nav.list.clone(), ui_nav.status_left.clone(), ui_nav.status_right.clone());
                        populate_current_view(&ui_nav.list, &info_c, &new_path, &filter_c);
                        ui_nav.back.set_sensitive(true);
                        ui_nav.status_left.set_text(&format!("Opened: /{}", new_path));
                    } else {
                        // Plain file: show info for now.
                        ui_nav.status_left.set_text(&format!("File: {} ({}), double-click to extract", entry.file_name(), humansize::format_size(entry.size, humansize::BINARY)));
                    }
                }
            }
        }
    });

    // Right-click context menu
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    let ui_menu = ui.clone();
    let list_gesture = list_box.clone();
    gesture.connect_pressed(move |gesture, _n, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        // Find the row under the cursor.
        let picked = list_gesture.pick(x, y, gtk::PickFlags::DEFAULT);
        let mut target_path: Option<String> = None;
        let mut current = picked;
        while let Some(w) = current {
            if let Ok(row) = w.clone().downcast::<gtk::ListBoxRow>() {
                if let Some(t) = row.tooltip_text() {
                    target_path = Some(t.to_string());
                    // Select the row if not already selected.
                    if !row.is_selected() {
                        list_gesture.select_row(Some(&row));
                    }
                    break;
                }
            }
            current = w.parent();
        }
        if let Some(ref tp) = target_path {
            if tp == "__UP__" {
                // No menu on "..".
                return;
            }
        }
        if target_path.is_none() {
            // Click on empty space: only proceed when something is selected
            // (actions read state.selected_entries when activated).
            let st = ui_menu.state.borrow();
            if st.selected_entries.is_empty() {
                return;
            }
        }
        // Build the contextual popover menu (win.* actions read
        // state.selected_entries when activated).
        let menu = gio::Menu::new();
        menu.append(Some("Extract selected here"), Some("win.extract-selected-here"));
        menu.append(Some("Extract selected to…"), Some("win.extract-selected-to"));
        menu.append(Some("Copy path"), Some("win.copy-path"));
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(&list_gesture);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.set_has_arrow(false);
        popover.popup();
    });
    list_box.add_controller(gesture);

    // Wire the context-menu actions (global) with a fast duplicate guard
    let ui_here = ui.clone();
    action_extract_here.connect_activate(move |_, _| {
        if *ui_here.is_busy.borrow() { ui_here.status_left.set_text("An operation is already in progress…"); return; }
        let st = ui_here.state.borrow();
        if let Some(archive) = st.current_archive.clone() {
            let dest = archive.parent().unwrap_or(Path::new("/tmp")).to_path_buf();
            let selected = st.selected_entries.clone();
            let info_opt = st.current_info.clone();
            drop(st);
            if selected.is_empty() { ui_here.status_left.set_text("No selection"); return; }
            {
                let mut le = ui_here.last_extract.borrow_mut();
                if let Some((la, ld, t)) = &*le {
                    if la == &archive && ld == &dest && t.elapsed().as_secs() < 2 {
                        eprintln!("[guard] duplicate context Extract-here ignored");
                        return;
                    }
                }
                *le = Some((archive.clone(), dest.clone(), Instant::now()));
            }
            let expanded = if let Some(info) = info_opt { get_all_descendants(&info, &selected) } else { selected };
            start_extract(archive, dest, expanded, None, ui_here.clone());
        }
    });

    let ui_to = ui.clone();
    action_extract_to.connect_activate(move |_, _| {
        if *ui_to.is_busy.borrow() { ui_to.status_left.set_text("An operation is already in progress…"); return; }
        let st = ui_to.state.borrow();
        let archive = match st.current_archive.clone() {
            Some(a) => a,
            None => return,
        };
        let selected = st.selected_entries.clone();
        let info_opt = st.current_info.clone();
        drop(st);
        if selected.is_empty() { ui_to.status_left.set_text("No selection"); return; }
        let expanded = if let Some(info) = info_opt { get_all_descendants(&info, &selected) } else { selected };
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Extract selected to…");
        let ui_dlg = ui_to.clone();
        let parent = ui_to.window.clone();
        dialog.select_folder(Some(&parent), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(dest) = file.path() {
                    start_extract(archive, dest, expanded, None, ui_dlg.clone());
                }
            }
        });
    });

    let ui_copy = ui.clone();
    action_copy.connect_activate(move |_, _| {
        let st = ui_copy.state.borrow();
        if let Some(sel) = st.selected_entries.first() {
            let display = gdk::Display::default().unwrap();
            let clipboard = display.clipboard();
            clipboard.set_text(&glib::GString::from(sel.clone()));
        }
    });

    // Shortcuts
    let shortcut_controller = gtk::ShortcutController::new();
    let ui_open_key = ui.clone();
    let action_open = gio::SimpleAction::new("open", None);
    action_open.connect_activate(move |_, _| { ui_open_key.open.emit_clicked(); });
    window.add_action(&action_open);
    let ui_ctrl_o = ui.clone();
    shortcut_controller.add_shortcut(gtk::Shortcut::new(
        Some(gtk::ShortcutTrigger::parse_string("<Control>o").unwrap()),
        Some(gtk::CallbackAction::new(move |_, _| {
            ui_ctrl_o.open.emit_clicked();
            glib::Propagation::Stop
        })),
    ));
    // Back with Alt+Left
    let ui_keys = ui.clone();
    shortcut_controller.add_shortcut(gtk::Shortcut::new(
        Some(gtk::ShortcutTrigger::parse_string("<Alt>Left").unwrap()),
        Some(gtk::CallbackAction::new(move |_, _| {
            ui_keys.back.emit_clicked();
            glib::Propagation::Stop
        })),
    ));
    window.add_controller(shortcut_controller);

    window.present();

    // Poll worker events every 50ms (throttled so the UI is not spammed).
    // The whole loop works off one shared `Ui` snapshot plus small locals.
    let ui_poll = ui.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let mut events = Vec::new();
        while let Some(evt) = ui_poll.worker.borrow().try_recv() {
            events.push(evt);
        }
        // Local aliases keep the match arms readable.
        let status_left = &ui_poll.status_left;
        for evt in events {
            match evt {
                WorkerEvent::Started { kind } => {
                    *ui_poll.is_busy.borrow_mut() = true;
                    *ui_poll.last_progress.borrow_mut() = (0.0, Instant::now());
                    ui_poll.extract_all.set_sensitive(false);
                    ui_poll.extract_sel.set_sensitive(false);
                    status_left.set_text("Working…");
                    // Open the window immediately at 0% for extractions (not List).
                    // A previous outcome still on screen is closed first.
                    if kind == "extract" {
                        let stale = ui_poll.progress_window.borrow().as_ref().cloned();
                        if let Some(old) = stale {
                            old.borrow().close();
                        }
                        *ui_poll.progress_window.borrow_mut() = None;
                        let w = ProgressWindow::new(&ui_poll.window, "Extraction in progress", "Preparing… 0%");
                        w.borrow().reset();
                        let w_clone = w.clone();
                        let worker_c = ui_poll.worker.clone();
                        w.borrow().on_cancel(move || {
                            worker_c.borrow().cancel_all();
                            w_clone.borrow().close();
                        });
                        *ui_poll.progress_window.borrow_mut() = Some(w.clone());
                    }
                }
                WorkerEvent::Progress { info } => {
                    // Unknown total (e.g. encrypted headers, empty Size): pulsing bar,
                    // never a fake %. Every event steps the pulse.
                    if info.total == 0 {
                        if ui_poll.progress_window.borrow().is_none() {
                            let subtitle = truncate_middle(&info.file, 50);
                            let w = ProgressWindow::new(&ui_poll.window, "Extraction in progress", &subtitle);
                            w.borrow().pulse(&info.file);
                            let w_clone = w.clone();
                            let worker_c = ui_poll.worker.clone();
                            w.borrow().on_cancel(move || {
                                worker_c.borrow().cancel_all();
                                w_clone.borrow().close();
                            });
                            *ui_poll.progress_window.borrow_mut() = Some(w.clone());
                        }
                        if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                            pw.borrow().pulse(&info.file);
                        }
                        status_left.set_text(&format!("{} — …", truncate_middle(&info.file, 40)));
                        continue;
                    }
                    // Real 0%→100% progress straight from the backend, monotonic guard only.
                    let pct = info.percent.clamp(0.0, 100.0);
                    let (last_pct, _) = *ui_poll.last_progress.borrow();
                    // Ignore regressions and duplicates.
                    if pct + 0.01 < last_pct {
                        continue;
                    }
                    if (pct - last_pct).abs() < 0.01 && pct > 0.0 && pct < 100.0 {
                        continue;
                    }
                    *ui_poll.last_progress.borrow_mut() = (pct, Instant::now());
                    // Fallback: Started lost (ultrafast job), create at the real pct.
                    if ui_poll.progress_window.borrow().is_none() {
                        let subtitle = truncate_middle(&info.file, 50);
                        let w = ProgressWindow::new(&ui_poll.window, "Extraction in progress", &subtitle);
                        w.borrow().set_progress(&info);
                        let w_clone = w.clone();
                        let worker_c = ui_poll.worker.clone();
                        w.borrow().on_cancel(move || {
                            worker_c.borrow().cancel_all();
                            w_clone.borrow().close();
                        });
                        *ui_poll.progress_window.borrow_mut() = Some(w.clone());
                    }
                    if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                        pw.borrow().set_progress(&info);
                    }
                    status_left.set_text(&format!("{} — {}", truncate_middle(&info.file, 40), crate::core::util::format_percent(pct, info.current, info.total)));
                }
                WorkerEvent::Finished { result } => {
                    *ui_poll.last_progress.borrow_mut() = (100.0, Instant::now());
                    // The outcome stays on screen: the user dismisses it with
                    // Close, Esc or the window X. No auto-close timers.
                    match &result {
                        Ok(JobResult::Extract) => {
                            if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                                pw.borrow().set_complete();
                                let holder = ui_poll.progress_window.clone();
                                pw.borrow().finish_dismiss(move || {
                                    *holder.borrow_mut() = None;
                                });
                            } else {
                                // Ultrafast extraction without Progress: still show the outcome.
                                let w = ProgressWindow::new(&ui_poll.window, "Extraction", "Completed ✓");
                                w.borrow().set_complete();
                                let holder = ui_poll.progress_window.clone();
                                *ui_poll.progress_window.borrow_mut() = Some(w.clone());
                                w.borrow().finish_dismiss(move || {
                                    *holder.borrow_mut() = None;
                                });
                            }
                            *ui_poll.is_busy.borrow_mut() = false;
                            ui_poll.extract_all.set_sensitive(true);
                            if !ui_poll.state.borrow().selected_entries.is_empty() {
                                ui_poll.extract_sel.set_sensitive(true);
                            }
                            status_left.set_text("Extraction complete ✓");
                        }
                        _ => {
                            // For List, close any leftover window
                            if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                                pw.borrow().close();
                                *ui_poll.progress_window.borrow_mut() = None;
                            }
                            *ui_poll.is_busy.borrow_mut() = false;
                            ui_poll.extract_all.set_sensitive(ui_poll.state.borrow().current_info.is_some());
                            ui_poll.extract_sel.set_sensitive(!ui_poll.state.borrow().selected_entries.is_empty());
                        }
                    }
                    match result {
                        Ok(JobResult::List(info)) => {
                            let archive_path = PathBuf::from(&info.path);
                            let mut st = ui_poll.state.borrow_mut();
                            st.current_info = Some(info.clone());
                            st.current_path = String::new();
                            st.selected_entries.clear();
                            let filter = st.filter_text.clone();
                            drop(st);
                            // Refresh the root breadcrumb
                            update_breadcrumb(&ui_poll.breadcrumb, "", &info, ui_poll.state.clone(), ui_poll.list.clone(), ui_poll.status_left.clone(), ui_poll.status_right.clone());
                            // Populate the current view (top level only)
                            populate_current_view(&ui_poll.list, &info, "", &filter);
                            ui_poll.empty.set_visible(info.entries.is_empty());
                            status_left.set_text("");
                            let total_h = humansize::format_size(info.total_size, humansize::BINARY);
                            let packed_h = humansize::format_size(info.total_packed, humansize::BINARY);
                            let ratio = if info.total_size > 0 { 100.0 * (1.0 - info.total_packed as f32 / info.total_size as f32) } else { 0.0 };
                            // Show archive info + path (root breadcrumb carries the archive name)
                            ui_poll.info.set_text(&info.format);
                            ui_poll.info.set_visible(true);
                            if info.has_encrypted {
                                ui_poll.info.add_css_class("badge-encrypted");
                                ui_poll.info.set_text(&format!("{} • encrypted", info.format));
                            }
                            ui_poll.status_right.set_text(&format!("{} files • {} folders • {} → {} ({:.0}%)", info.num_files, info.num_dirs, total_h, packed_h, ratio));
                            ui_poll.extract_all.set_sensitive(true);
                            ui_poll.extract_sel.set_sensitive(false);
                            ui_poll.back.set_sensitive(false);
                            // Full-path tooltip on the breadcrumb
                            ui_poll.breadcrumb.set_tooltip_text(Some(&archive_path.display().to_string()));
                        }
                        Ok(JobResult::Extract) => {
                            // Outcome already shown in the progress window; status only.
                            status_left.set_text("Extraction complete ✓");
                        }
                        Err(msg) => {
                            // Outcome shown in the progress window when one exists;
                            // password errors still open the unlock prompt.
                            status_left.set_text(&format!("Error: {}", truncate_middle(&msg, 80)));
                            if msg.contains("Password") || msg.contains("encrypted") || msg.contains("Wrong") {
                                dialogs::show_password_dialog(status_left, ui_poll.state.clone(), ui_poll.worker.clone());
                            } else if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                                pw.borrow().set_error(&msg);
                                let holder = ui_poll.progress_window.clone();
                                pw.borrow().finish_dismiss(move || {
                                    *holder.borrow_mut() = None;
                                });
                            }
                        }
                    }
                }
                WorkerEvent::Error { msg } => {
                    // Permanent error outcome; the user dismisses it.
                    if let Some(pw) = ui_poll.progress_window.borrow().as_ref() {
                        pw.borrow().set_error(&msg);
                        let holder = ui_poll.progress_window.clone();
                        pw.borrow().finish_dismiss(move || {
                            *holder.borrow_mut() = None;
                        });
                    }
                    *ui_poll.is_busy.borrow_mut() = false;
                    ui_poll.extract_all.set_sensitive(ui_poll.state.borrow().current_info.is_some());
                    ui_poll.extract_sel.set_sensitive(!ui_poll.state.borrow().selected_entries.is_empty());
                    status_left.set_text(&format!("Error: {}", msg));
                }
            }
        }
        glib::ControlFlow::Continue
    });

    // Launched with a file argument
    if let Some(arg) = std::env::args().nth(1) {
        let p = PathBuf::from(arg);
        if p.exists() && p.is_file() {
            open_archive(p, ui.clone());
        }
    }
}

fn create_title_widget() -> gtk::Box {
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let icon = gtk::Image::from_icon_name("package-x-generic-symbolic");
    let title = gtk::Label::new(Some("Arkx"));
    title.add_css_class("title");
    bx.append(&icon);
    bx.append(&title);
    bx
}

fn open_archive(path: PathBuf, ui: Ui) {
    if !path.exists() {
        ui.status_left.set_text(&format!("File not found: {}", path.display()));
        return;
    }
    // Reset the breadcrumb to the filename right away;
    // the List result will fill it in.
    ui.state.borrow_mut().current_archive = Some(path.clone());
    ui.state.borrow_mut().current_path = String::new();
    ui.status_left.set_text(&format!("Opening {}…", path.display()));
    ui.worker.borrow_mut().submit(JobKind::List { path });
}

fn start_extract(
    archive: PathBuf,
    dest: PathBuf,
    entries: Vec<String>,
    password: Option<String>,
    ui: Ui,
) {
    let entries_opt = if entries.is_empty() { None } else { Some(entries) };
    if let Some(ref sel) = entries_opt {
        ui.status_left.set_text(&format!("Extracting {} items to {}…", sel.len(), dest.display()));
    } else {
        ui.status_left.set_text(&format!("Extracting everything to {}…", dest.display()));
    }
    ui.worker.borrow_mut().submit(JobKind::Extract { archive, dest, entries: entries_opt, password });
}
