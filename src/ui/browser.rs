//! Archive browser: state, hierarchy helpers and the file-table widgets.
//!
//! Pure navigation logic (`get_children`, `get_all_descendants`) lives next to
//! the widgets that render it, keeping `window.rs` to shell + event loop.

use gtk4 as gtk;
use gtk::prelude::*;
use pango;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use crate::core::archive::{ArchiveEntry, ArchiveInfo};
use crate::core::paths;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Browser state: opened archive, current folder and selection.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    pub(crate) current_archive: Option<PathBuf>,
    pub(crate) current_info: Option<ArchiveInfo>,
    /// "" = root, otherwise "src/", "src/subdir/", ...
    pub(crate) current_path: String,
    pub(crate) selected_entries: Vec<String>,
    pub(crate) filter_text: String,
}

// ---------------------------------------------------------------------------
// Hierarchy helpers
// ---------------------------------------------------------------------------

/// Immediate children of `current_path`, deduped for folders.
pub(crate) fn get_children(info: &ArchiveInfo, current_path: &str) -> Vec<ArchiveEntry> {
    let cur = paths::with_trailing_slash(&paths::normalize(current_path));
    let cur_norm = cur.as_str();

    let mut map: HashMap<String, ArchiveEntry> = HashMap::new();
    let mut explicit_dirs: HashSet<String> = HashSet::new();

    // First pass: collect explicit dirs to reuse their metadata.
    for e in &info.entries {
        let ep = paths::normalize(&e.path);
        if e.is_dir {
            let key = paths::with_trailing_slash(&ep);
            explicit_dirs.insert(key);
        }
    }

    for e in &info.entries {
        let ep_raw = paths::normalize(&e.path);
        if ep_raw.is_empty() {
            continue;
        }
        // Skip the entry that exactly matches the current folder.
        let ep_with_slash = if e.is_dir {
            paths::with_trailing_slash(&ep_raw)
        } else {
            ep_raw.clone()
        };
        if ep_with_slash == cur_norm {
            continue;
        }
        if cur_norm.is_empty() {
            // root: take the first component
        } else if !ep_raw.starts_with(cur_norm) && !ep_with_slash.starts_with(cur_norm) {
            continue;
        }
        let suffix = if cur_norm.is_empty() {
            ep_raw.as_str()
        } else {
            // e.g. ep "src/file.txt" under cur "src/" → suffix "file.txt";
            // dir entry "src/subdir/" → suffix "subdir/"
            if let Some(suffix) = ep_raw
                .strip_prefix(cur_norm)
                .or_else(|| ep_with_slash.strip_prefix(cur_norm))
            {
                suffix
            } else {
                continue;
            }
        };
        if suffix.is_empty() {
            continue;
        }

        // Ignore empty entries such as "./"
        if suffix == "/" || suffix.is_empty() {
            continue;
        }

        if let Some(slash_pos) = suffix.find('/') {
            // Inside a subfolder → the child is a folder.
            let child_dir = &suffix[..=slash_pos]; // include /
            let child_path = format!("{}{}", cur_norm, child_dir);
            if map.contains_key(&child_path) {
                continue;
            }
            // Look for explicit metadata for this dir.
            let mut synthetic = None;
            for orig in &info.entries {
                let op = paths::normalize(&orig.path);
                let op_slash = paths::with_trailing_slash(&op);
                if op_slash == child_path && orig.is_dir {
                    synthetic = Some(orig.clone());
                    break;
                }
                // Also without trailing slash.
                if op == child_path.trim_end_matches('/') && orig.is_dir {
                    synthetic = Some(orig.clone());
                    break;
                }
            }
            let entry = if let Some(mut ex) = synthetic {
                // Fix up the path.
                ex.path = child_path.clone();
                ex.is_dir = true;
                ex
            } else {
                ArchiveEntry {
                    path: child_path.clone(),
                    is_dir: true,
                    size: 0,
                    packed_size: 0,
                    modified: None,
                    mode: Some(0o755),
                    crc32: None,
                    method: None,
                    encrypted: false,
                }
            };
            map.insert(child_path, entry);
        } else {
            // Immediate file.
            let child_path = format!("{}{}", cur_norm, suffix);
            // Dedup: skip if already present as a file (shouldn't happen).
            if map.contains_key(&child_path) {
                continue;
            }
            // Use the original entry with a normalized path.
            let mut cloned = e.clone();
            cloned.path = child_path.clone();
            // If the original was a dir under the same file name (rare), skip.
            if cloned.is_dir {
                continue;
            }
            map.insert(child_path, cloned);
        }
    }

    let mut vec: Vec<ArchiveEntry> = map.into_values().collect();
    vec.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.path.cmp(&b.path)));
    vec
}

/// Expand selected folders to every descendant entry (by prefix).
pub(crate) fn get_all_descendants(info: &ArchiveInfo, selected_paths: &[String]) -> Vec<String> {
    let mut expanded = HashSet::new();
    for sel in selected_paths {
        let sel_norm = paths::normalize(sel);
        let sel_slash = paths::with_trailing_slash(&sel_norm);
        let sel_is_dir = sel.ends_with('/')
            || info
                .entries
                .iter()
                .any(|e| paths::normalize(&e.path) == sel_norm && e.is_dir)
            || {
                // A synthetic dir always carries its slash.
                sel_slash != sel_norm
            };
        if sel_is_dir {
            let prefix = sel_slash;
            let mut found = false;
            for e in &info.entries {
                let ep = paths::normalize(&e.path);
                if ep == prefix.trim_end_matches('/') {
                    continue;
                }
                if ep.starts_with(&prefix) || paths::with_trailing_slash(&ep).starts_with(&prefix) {
                    expanded.insert(e.path.clone());
                    found = true;
                }
            }
            if !found {
                // Fallback: keep the selection itself.
                expanded.insert(sel.clone());
            } else {
                // Also keep the dir itself when explicit.
                expanded.insert(sel.clone());
            }
        } else {
            expanded.insert(sel.clone());
        }
    }
    expanded.into_iter().collect()
}

/// Parent of a `current_path` ("src/sub/" → "src/"). Root stays root.
pub(crate) fn navigate_up(current_path: &str) -> String {
    let p = current_path.trim_end_matches('/').to_string();
    if let Some(pos) = p.rfind('/') {
        format!("{}/", &p[..pos])
    } else {
        String::new()
    }
}

pub(crate) fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let half = (max - 3) / 2;
    format!("{}...{}", &s[..half], &s[s.len() - half..])
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// Refresh the clickable file-manager-style breadcrumb.
pub(crate) fn update_breadcrumb(
    breadcrumb_box: &gtk::Box,
    current_path: &str,
    info: &ArchiveInfo,
    state: Rc<RefCell<AppState>>,
    list_box: gtk::ListBox,
    status_left: gtk::Label,
    status_right: gtk::Label,
) {
    // Clear (breadcrumb_box only holds the trail, the back button lives outside).
    while let Some(child) = breadcrumb_box.first_child() {
        breadcrumb_box.remove(&child);
    }
    let archive_name = PathBuf::from(&info.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or("Archive".into());

    // Root button.
    let root_btn = gtk::Button::with_label(&archive_name);
    root_btn.add_css_class("flat");
    root_btn.add_css_class("breadcrumb-btn");
    root_btn.set_tooltip_text(Some(&info.path));
    let state_c = state.clone();
    let list_c = list_box.clone();
    let bc_c = breadcrumb_box.clone();
    let sl_c = status_left.clone();
    let sr_c = status_right.clone();
    let info_c = info.clone();
    root_btn.connect_clicked(move |_| {
        let mut st = state_c.borrow_mut();
        st.current_path = String::new();
        let f = st.filter_text.clone();
        drop(st);
        update_breadcrumb(&bc_c, "", &info_c, state_c.clone(), list_c.clone(), sl_c.clone(), sr_c.clone());
        populate_current_view(&list_c, &info_c, "", &f);
        sl_c.set_text("Root");
    });
    breadcrumb_box.append(&root_btn);

    if current_path.is_empty() {
        return;
    }
    let parts: Vec<&str> = current_path.trim_end_matches('/').split('/').collect();
    let mut acc = String::new();
    for part in parts.iter() {
        if part.is_empty() {
            continue;
        }
        acc.push_str(part);
        acc.push('/');
        let sep = gtk::Label::new(Some("›"));
        sep.add_css_class("breadcrumb-sep");
        breadcrumb_box.append(&sep);
        let btn = gtk::Button::with_label(part);
        btn.add_css_class("flat");
        btn.add_css_class("breadcrumb-btn");
        let target = acc.clone();
        let state_cc = state.clone();
        let list_cc = list_box.clone();
        let bc_cc = breadcrumb_box.clone();
        let sl_cc = status_left.clone();
        let sr_cc = status_right.clone();
        let info_cc = info.clone();
        btn.connect_clicked(move |_| {
            let mut st = state_cc.borrow_mut();
            st.current_path = target.clone();
            let f = st.filter_text.clone();
            drop(st);
            update_breadcrumb(&bc_cc, &target, &info_cc, state_cc.clone(), list_cc.clone(), sl_cc.clone(), sr_cc.clone());
            populate_current_view(&list_cc, &info_cc, &target, &f);
            sl_cc.set_text(&format!("Folder: /{}", target));
        });
        breadcrumb_box.append(&btn);
    }
}

/// Responsive table header (NAME / SIZE / DATE / METHOD).
pub(crate) fn create_table_header() -> gtk::Box {
    let hdr = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hdr.set_margin_top(4);
    hdr.set_margin_bottom(4);
    hdr.set_margin_start(12);
    hdr.set_margin_end(12);
    hdr.add_css_class("table-header");
    hdr.add_css_class("hide-narrow"); // hidden on narrow; rows go vertical via CSS
    // Responsive columns: flexible name, fixed others hidden on narrow.
    let name_lbl = gtk::Label::new(Some("NAME"));
    name_lbl.set_xalign(0.0);
    name_lbl.set_hexpand(true);
    name_lbl.add_css_class("heading");
    hdr.append(&name_lbl);

    let size_lbl = gtk::Label::new(Some("SIZE"));
    size_lbl.set_xalign(1.0);
    size_lbl.set_width_request(90);
    size_lbl.add_css_class("heading");
    hdr.append(&size_lbl);

    let date_lbl = gtk::Label::new(Some("DATE"));
    date_lbl.set_xalign(0.5);
    date_lbl.set_width_request(110);
    date_lbl.add_css_class("heading");
    date_lbl.add_css_class("hide-narrow");
    hdr.append(&date_lbl);

    let method_lbl = gtk::Label::new(Some("METHOD"));
    method_lbl.set_xalign(0.5);
    method_lbl.set_width_request(90);
    method_lbl.add_css_class("heading");
    method_lbl.add_css_class("hide-medium");
    method_lbl.add_css_class("hide-narrow");
    hdr.append(&method_lbl);

    hdr
}

/// Empty state shown before any archive is opened.
pub(crate) fn create_empty_state() -> gtk::Box {
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::Center);
    bx.set_visible(true);
    bx.add_css_class("empty-state");

    let icon = gtk::Image::from_icon_name("package-x-generic-symbolic");
    icon.set_pixel_size(64);
    icon.add_css_class("dim-label");

    let title = gtk::Label::new(Some("Drag an archive here"));
    title.add_css_class("title-2");
    let subtitle = gtk::Label::new(Some(
        "Supports ZIP, 7Z, RAR, TAR.GZ, TAR.XZ, ISO and 20+ more formats\nBrowse like a file manager • Right-click to extract",
    ));
    subtitle.set_justify(gtk::Justification::Center);
    subtitle.add_css_class("dim-label");
    subtitle.set_wrap(true);

    let hint = gtk::Label::new(Some("Drop a file here, press Ctrl+O or use the button below"));
    hint.add_css_class("dim-label");

    bx.append(&icon);
    bx.append(&title);
    bx.append(&subtitle);
    bx.append(&hint);
    bx
}

/// Fill the list with the current folder's children (max 5000 rows).
pub(crate) fn populate_current_view(list_box: &gtk::ListBox, info: &ArchiveInfo, current_path: &str, filter: &str) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let filter_lower = filter.to_lowercase();
    let children = get_children(info, current_path);
    let mut visible = 0;

    for entry in children {
        if !filter_lower.is_empty()
            && !entry.path.to_lowercase().contains(&filter_lower)
            && !entry.file_name().to_lowercase().contains(&filter_lower)
        {
            continue;
        }
        visible += 1;
        if visible > 5000 {
            break;
        }
        let row = create_file_row(&entry);
        list_box.append(&row);
    }

    // ".." row to go up when not at root.
    if !current_path.is_empty() {
        let up_row = create_up_row();
        list_box.prepend(&up_row);
        visible += 1;
    }

    if visible == 0 || (visible == 1 && !current_path.is_empty() && list_box.first_child().is_some()) {
        // Only ".." and nothing else: show the filtered-empty message.
        if !filter.is_empty() {
            let lbl = gtk::Label::new(Some(&format!(
                "No results for “{}” in /{}",
                filter,
                if current_path.is_empty() {
                    "".to_string()
                } else {
                    current_path.to_string()
                }
            )));
            lbl.add_css_class("dim-label");
            lbl.set_margin_top(24);
            lbl.set_wrap(true);
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&lbl));
            r.set_selectable(false);
            list_box.append(&r);
        }
    }
}

fn create_up_row() -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");
    row.set_tooltip_text(Some(".."));
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bx.set_margin_top(6);
    bx.set_margin_bottom(6);
    bx.set_margin_start(8);
    bx.set_margin_end(8);
    let icon = gtk::Image::from_icon_name("go-up-symbolic");
    icon.set_pixel_size(16);
    icon.add_css_class("icon-folder");
    let label = gtk::Label::new(Some(".. (go back)"));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.add_css_class("dim-label");
    bx.append(&icon);
    bx.append(&label);
    row.set_child(Some(&bx));
    // Special path for "up".
    row.set_tooltip_text(Some("__UP__"));
    row
}

fn create_file_row(entry: &ArchiveEntry) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("file-row");

    let is_up = entry.path == "__UP__";

    // Responsive: extra columns collapse on narrow via CSS hide classes.
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bx.set_margin_top(6);
    bx.set_margin_bottom(6);
    bx.set_margin_start(8);
    bx.set_margin_end(8);
    bx.set_hexpand(true);

    // Icon.
    let icon_name = if is_up {
        "go-up-symbolic"
    } else if entry.is_dir {
        "folder-symbolic"
    } else if entry.path.ends_with(".jpg") || entry.path.ends_with(".png") || entry.path.ends_with(".webp") || entry.path.ends_with(".gif") {
        "image-x-generic-symbolic"
    } else if entry.path.ends_with(".mp4") || entry.path.ends_with(".mkv") || entry.path.ends_with(".webm") {
        "video-x-generic-symbolic"
    } else if entry.path.ends_with(".mp3") || entry.path.ends_with(".flac") || entry.path.ends_with(".ogg") {
        "audio-x-generic-symbolic"
    } else if entry.path.ends_with(".pdf") {
        "application-pdf-symbolic"
    } else if entry.path.ends_with(".zip") || entry.path.ends_with(".7z") || entry.path.ends_with(".rar") || entry.path.ends_with(".tar") || entry.path.ends_with(".gz") {
        "package-x-generic-symbolic"
    } else {
        "text-x-generic-symbolic"
    };
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);
    if entry.is_dir {
        icon.add_css_class("icon-folder");
    } else {
        icon.add_css_class("icon-file");
    }
    if entry.encrypted {
        icon.add_css_class("icon-archive");
    }

    // Name (basename only, file-manager feel).
    let display_name = if entry.is_dir {
        // Dirs show only the last component.
        entry.path.trim_end_matches('/').rsplit('/').next().unwrap_or(&entry.path).to_string() + "/"
    } else {
        entry.file_name().to_string()
    };
    let name_label = gtk::Label::new(Some(&display_name));
    name_label.set_xalign(0.0);
    name_label.set_hexpand(true);
    name_label.set_hexpand_set(true);
    name_label.set_ellipsize(pango::EllipsizeMode::None);
    name_label.set_single_line_mode(true);
    name_label.set_width_chars(display_name.len() as i32);
    name_label.set_max_width_chars(80);
    name_label.set_tooltip_text(Some(&entry.path));
    if entry.is_dir {
        name_label.add_css_class("heading");
    }

    // Size.
    let size_str = if entry.is_dir {
        "—".to_string()
    } else {
        humansize::format_size(entry.size, humansize::BINARY)
    };
    let size_label = gtk::Label::new(Some(&size_str));
    size_label.set_xalign(1.0);
    size_label.set_width_request(90);
    size_label.add_css_class("dim-label");
    size_label.add_css_class("monospace");

    // Date.
    let date_str = entry.modified.map(|dt| dt.format("%d/%m/%Y").to_string()).unwrap_or_else(|| "—".into());
    let date_label = gtk::Label::new(Some(&date_str));
    date_label.set_xalign(0.5);
    date_label.set_width_request(100);
    date_label.add_css_class("dim-label");
    date_label.add_css_class("hide-narrow");

    // Method.
    let method_label = gtk::Label::new(Some(entry.method.as_deref().unwrap_or("—")));
    method_label.set_xalign(0.5);
    method_label.set_width_request(80);
    method_label.add_css_class("dim-label");
    method_label.add_css_class("hide-medium");
    method_label.add_css_class("hide-narrow");
    method_label.set_ellipsize(pango::EllipsizeMode::End);

    // Arrow for folders.
    let arrow = if entry.is_dir {
        let a = gtk::Image::from_icon_name("go-next-symbolic");
        a.set_pixel_size(12);
        a.add_css_class("dim-label");
        a
    } else {
        let a = gtk::Image::from_icon_name("");
        a.set_pixel_size(12);
        a
    };

    // Lock for encrypted entries.
    let lock_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    if entry.encrypted {
        let lock = gtk::Image::from_icon_name("system-lock-screen-symbolic");
        lock.set_pixel_size(12);
        lock.set_tooltip_text(Some("Encrypted"));
        lock_box.append(&lock);
    }
    lock_box.append(&arrow);

    bx.append(&icon);
    bx.append(&name_label);
    bx.append(&size_label);
    bx.append(&date_label);
    bx.append(&method_label);
    bx.append(&lock_box);

    row.set_child(Some(&bx));
    row.set_tooltip_text(Some(&entry.path));
    row
}

/// Live text filter over the current view (".." always stays visible).
pub(crate) fn filter_list(list_box: &gtk::ListBox, filter: &str) {
    let filter_lower = filter.to_lowercase();
    let mut child = list_box.first_child();
    while let Some(row) = child {
        let next = row.next_sibling();
        if let Some(lb_row) = row.downcast_ref::<gtk::ListBoxRow>() {
            // Never filter out "..".
            if lb_row.tooltip_text().as_deref() == Some("__UP__") {
                // Always visible.
            } else {
                let visible = if filter_lower.is_empty() {
                    true
                } else {
                    lb_row
                        .tooltip_text()
                        .map(|t| {
                            let s = t.to_lowercase();
                            s.contains(&filter_lower)
                                || lb_row
                                    .child()
                                    .as_ref()
                                    .map(|c| c.tooltip_text().unwrap_or_default().to_lowercase().contains(&filter_lower))
                                    .unwrap_or(false)
                        })
                        .unwrap_or(true)
                };
                lb_row.set_visible(visible);
            }
        }
        child = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::archive::ArchiveInfo;

    fn make_info(paths: Vec<(&str, bool)>) -> ArchiveInfo {
        let entries = paths
            .into_iter()
            .map(|(p, is_dir)| ArchiveEntry {
                path: p.to_string(),
                is_dir,
                size: if is_dir { 0 } else { 10 },
                packed_size: 10,
                modified: None,
                mode: None,
                crc32: None,
                method: None,
                encrypted: false,
            })
            .collect::<Vec<_>>();
        ArchiveInfo {
            path: "/tmp/test.zip".into(),
            format: "ZIP".into(),
            entries,
            total_size: 0,
            total_packed: 0,
            num_files: 0,
            num_dirs: 0,
            has_encrypted: false,
            comment: None,
        }
    }

    #[test]
    fn test_get_children_root() {
        let info = make_info(vec![
            ("another.txt", false),
            ("root.txt", false),
            ("sub1/", true),
            ("sub1/file1.txt", false),
            ("sub1/sub2/", true),
            ("sub1/sub2/file2.txt", false),
        ]);
        let children = get_children(&info, "");
        assert_eq!(children.len(), 3);
        assert!(children.iter().any(|e| e.path == "sub1/" && e.is_dir));
        assert!(children.iter().any(|e| e.path == "another.txt"));
        assert!(children.iter().any(|e| e.path == "root.txt"));
        // Nested files must not leak to the top level.
        assert!(!children.iter().any(|e| e.path == "sub1/file1.txt"));
    }

    #[test]
    fn test_get_children_sub1() {
        let info = make_info(vec![
            ("another.txt", false),
            ("sub1/", true),
            ("sub1/file1.txt", false),
            ("sub1/sub2/", true),
            ("sub1/sub2/file2.txt", false),
        ]);
        let children = get_children(&info, "sub1/");
        assert_eq!(children.len(), 2);
        assert!(children.iter().any(|e| e.path == "sub1/file1.txt" && !e.is_dir));
        assert!(children.iter().any(|e| e.path == "sub1/sub2/" && e.is_dir));
    }

    #[test]
    fn test_get_children_synthetic() {
        // Zips without explicit dir entries, files only.
        let info = make_info(vec![("a/b/c.txt", false), ("a/d.txt", false)]);
        let children = get_children(&info, "");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].path, "a/");
        assert!(children[0].is_dir);
        let children_a = get_children(&info, "a/");
        assert_eq!(children_a.len(), 2);
        assert!(children_a.iter().any(|e| e.path == "a/b/" && e.is_dir));
        assert!(children_a.iter().any(|e| e.path == "a/d.txt"));
    }

    #[test]
    fn test_get_all_descendants() {
        let info = make_info(vec![
            ("sub1/", true),
            ("sub1/file1.txt", false),
            ("sub1/sub2/file2.txt", false),
            ("root.txt", false),
        ]);
        let expanded = get_all_descendants(&info, &["sub1/".to_string()]);
        assert!(expanded.contains(&"sub1/file1.txt".to_string()));
        assert!(expanded.contains(&"sub1/sub2/file2.txt".to_string()));
        assert!(!expanded.contains(&"root.txt".to_string()));
    }
}
