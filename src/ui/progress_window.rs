use gtk4 as gtk;
use gtk::glib;
use adw::prelude::*;
use pango;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use crate::core::archive::ProgressInfo;
use crate::core::util::truncate_middle;

/// Separate progress window: live bar during the job, permanent outcome
/// afterwards. The user dismisses it with Close, Esc or the window X.
/// A collapsed "Details" pane (Windows copy-dialog style) shows the current
/// file, written bytes, speed and remaining time.
pub struct ProgressWindow {
    window: gtk::Window,
    bar: gtk::ProgressBar,
    file_label: gtk::Label,
    title_label: gtk::Label,
    action_btn: gtk::Button,
    detail_file: gtk::Label,
    detail_bytes: gtk::Label,
    detail_speed: gtk::Label,
    detail_eta: gtk::Label,
    finished: Rc<Cell<bool>>,
    last_sample: RefCell<(u64, Instant)>,
    speed_ema: Cell<f64>,
    /// Operation verb shown in the title ("Extracting" / "Compressing").
    verb: RefCell<String>,
}

impl ProgressWindow {
    pub fn new(parent: &impl IsA<gtk::Window>, title: &str, subtitle: &str) -> Rc<RefCell<Self>> {
        Self::build_with_verb(Some(parent), title, subtitle, "Extracting")
    }

    /// Top-level window without a parent: used by the standalone
    /// file-manager progress app (`arkx compress --progress` from Dolphin).
    pub fn new_standalone(title: &str, subtitle: &str) -> Rc<RefCell<Self>> {
        Self::build_with_verb::<gtk::Window>(None, title, subtitle, "Compressing")
    }

    /// Switch the operation verb after creation ("Extracting" / "Compressing").
    pub fn set_operation(&self, verb: &str) {
        *self.verb.borrow_mut() = verb.to_string();
    }

    /// Register the window with an application so `GApplication::run` stays
    /// alive while it is open (standalone file-manager mode: the window
    /// would otherwise hold nothing and the app would quit instantly).
    pub fn register_with_app(&self, app: &impl IsA<gtk::Application>) {
        app.upcast_ref().add_window(&self.window);
    }

    fn build_with_verb<W: IsA<gtk::Window>>(
        parent: Option<&W>,
        title: &str,
        subtitle: &str,
        verb: &str,
    ) -> Rc<RefCell<Self>> {
        let mut builder = gtk::Window::builder()
            .modal(false)
            .resizable(false)
            .decorated(true)
            .title(title)
            .default_width(480)
            .default_height(160);
        if let Some(p) = parent {
            builder = builder.transient_for(p);
        }
        let window: gtk::Window = builder.build();

        // Header
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&{
            let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let t = gtk::Label::new(Some(title));
            t.add_css_class("title-4");
            let s = gtk::Label::new(Some(subtitle));
            s.add_css_class("dim-label");
            s.add_css_class("caption");
            b.append(&t);
            b.append(&s);
            b
        }));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(16);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let title_label = gtk::Label::new(Some("Preparing…"));
        title_label.set_xalign(0.0);
        title_label.add_css_class("heading");
        title_label.set_wrap(true);
        title_label.set_ellipsize(pango::EllipsizeMode::Middle);

        let bar = gtk::ProgressBar::new();
        bar.set_hexpand(true);
        bar.set_show_text(true);
        bar.set_text(Some("0%"));
        bar.add_css_class("progress-bar");
        bar.set_fraction(0.0);

        let file_label = gtk::Label::new(Some("Waiting…"));
        file_label.set_xalign(0.0);
        file_label.add_css_class("dim-label");
        file_label.add_css_class("caption");
        file_label.set_ellipsize(pango::EllipsizeMode::Middle);
        file_label.set_single_line_mode(true);

        // Details pane (collapsed by default): current file, bytes, speed, ETA.
        let details_grid = gtk::Grid::new();
        details_grid.set_row_spacing(4);
        details_grid.set_column_spacing(12);
        details_grid.set_margin_top(4);

        let detail_file = gtk::Label::new(Some("—"));
        detail_file.set_xalign(0.0);
        detail_file.set_hexpand(true);
        detail_file.set_wrap(true);
        detail_file.set_wrap_mode(pango::WrapMode::WordChar);
        detail_file.set_selectable(true);
        detail_file.add_css_class("dim-label");

        let detail_bytes = gtk::Label::new(Some("—"));
        detail_bytes.set_xalign(0.0);
        detail_bytes.add_css_class("dim-label");
        detail_bytes.add_css_class("monospace");

        let detail_speed = gtk::Label::new(Some("—"));
        detail_speed.set_xalign(0.0);
        detail_speed.add_css_class("dim-label");
        detail_speed.add_css_class("monospace");

        let detail_eta = gtk::Label::new(Some("—"));
        detail_eta.set_xalign(0.0);
        detail_eta.add_css_class("dim-label");
        detail_eta.add_css_class("monospace");

        for (row, name, value) in [
            (0, "Current file", &detail_file),
            (1, "Written", &detail_bytes),
            (2, "Speed", &detail_speed),
            (3, "Remaining", &detail_eta),
        ] {
            let name_label = gtk::Label::new(Some(name));
            name_label.set_xalign(0.0);
            name_label.add_css_class("dim-label");
            name_label.add_css_class("caption");
            details_grid.attach(&name_label, 0, row, 1, 1);
            details_grid.attach(value, 1, row, 1, 1);
        }

        let details_revealer = gtk::Revealer::new();
        details_revealer.set_child(Some(&details_grid));
        details_revealer.set_reveal_child(false);

        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let details_btn = gtk::ToggleButton::with_label("Details");
        details_btn.set_icon_name("pan-down-symbolic");
        details_btn.set_tooltip_text(Some("Show details"));
        btn_box.append(&details_btn);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        btn_box.append(&spacer);
        // During the run this is Cancel; after the outcome it becomes Close.
        let action_btn = gtk::Button::with_label("Cancel");
        action_btn.set_icon_name("process-stop-symbolic");
        action_btn.add_css_class("destructive-action");
        action_btn.set_halign(gtk::Align::End);
        btn_box.append(&action_btn);

        content.append(&title_label);
        content.append(&bar);
        content.append(&file_label);
        content.append(&details_revealer);
        content.append(&btn_box);

        toolbar.set_content(Some(&content));
        window.set_child(Some(&toolbar));

        let finished = Rc::new(Cell::new(false));

        // Details toggle.
        let revealer_toggle = details_revealer.clone();
        let btn_toggle = details_btn.clone();
        details_btn.connect_toggled(move |b| {
            let open = b.is_active();
            revealer_toggle.set_reveal_child(open);
            b.set_icon_name(if open { "pan-up-symbolic" } else { "pan-down-symbolic" });
            btn_toggle.set_tooltip_text(Some(if open { "Hide details" } else { "Show details" }));
        });

        window.present();

        Rc::new(RefCell::new(Self {
            window,
            bar,
            file_label,
            title_label,
            action_btn,
            detail_file,
            detail_bytes,
            detail_speed,
            detail_eta,
            finished,
            last_sample: RefCell::new((0, Instant::now())),
            speed_ema: Cell::new(0.0),
            verb: RefCell::new(verb.to_string()),
        }))
    }

    /// Reset to the initial 0% state (used right after creation).
    pub fn reset(&self) {
        self.bar.set_fraction(0.0);
        self.bar.set_text(Some("0%"));
        self.file_label.set_text("Preparing…");
        self.title_label.set_text("Preparing…");
        self.detail_file.set_text("—");
        self.detail_bytes.set_text("—");
        self.detail_speed.set_text("—");
        self.detail_eta.set_text("—");
        *self.last_sample.borrow_mut() = (0, Instant::now());
        self.speed_ema.set(0.0);
    }

    /// Live update from a backend event. Drives the bar, the compact labels
    /// and the details pane (speed is an exponential moving average over the
    /// throttled event stream, so no extra backend traffic is needed).
    pub fn set_progress(&self, info: &ProgressInfo) {
        let pct = info.percent.clamp(0.0, 100.0);
        self.bar.set_fraction((pct / 100.0) as f64);
        let pct_s = crate::core::util::format_percent(pct, info.current, info.total);
        self.bar.set_text(Some(&pct_s));
        self.file_label.set_text(&truncate_middle(&info.file, 60));
        self.title_label.set_text(&format!("{}… {}", self.verb.borrow(), pct_s));

        let now = Instant::now();
        let (prev_bytes, prev_time) = *self.last_sample.borrow();
        let dt = now.duration_since(prev_time).as_secs_f64();
        if dt >= 0.05 {
            let instant = info.current.saturating_sub(prev_bytes) as f64 / dt;
            let ema = if self.speed_ema.get() <= 0.0 {
                instant
            } else {
                0.3 * instant + 0.7 * self.speed_ema.get()
            };
            self.speed_ema.set(ema.max(0.0));
            *self.last_sample.borrow_mut() = (info.current, now);
        }

        self.detail_file.set_text(&info.file);
        if info.total > 0 {
            self.detail_bytes.set_text(&format!(
                "{} of {}",
                humansize::format_size(info.current, humansize::BINARY),
                humansize::format_size(info.total, humansize::BINARY)
            ));
            let speed = self.speed_ema.get();
            if speed > 0.0 {
                self.detail_speed.set_text(&format!(
                    "{}/s",
                    humansize::format_size(speed as u64, humansize::BINARY)
                ));
                let remaining = info.total.saturating_sub(info.current) as f64 / speed;
                self.detail_eta.set_text(&format_duration(remaining));
            } else {
                self.detail_eta.set_text("…");
            }
        } else {
            self.detail_bytes.set_text("…");
            let speed = self.speed_ema.get();
            if speed > 0.0 {
                self.detail_speed.set_text(&format!(
                    "{}/s",
                    humansize::format_size(speed as u64, humansize::BINARY)
                ));
            } else {
                self.detail_speed.set_text("—");
            }
            self.detail_eta.set_text("…");
        }
    }

    /// Indeterminate mode (unknown total, e.g. encrypted headers): steps the
    /// pulsing bar instead of showing a fake %. Called on every event.
    pub fn pulse(&self, file: &str) {
        self.bar.pulse();
        self.bar.set_text(Some("…"));
        self.file_label.set_text(&truncate_middle(file, 60));
        self.title_label.set_text(&format!("{}…", self.verb.borrow()));
        self.detail_file.set_text(file);
    }

    /// Permanent success outcome. The window stays until the user dismisses it.
    pub fn set_complete(&self) {
        self.finished.set(true);
        self.bar.set_fraction(1.0);
        self.bar.set_text(Some("100%"));
        self.file_label.set_text("Completed ✓");
        self.title_label.set_text("Completed ✓");
    }

    /// Permanent error outcome. The message wraps instead of truncating.
    pub fn set_error(&self, msg: &str) {
        self.finished.set(true);
        self.bar.set_fraction(0.0);
        self.bar.set_text(Some("Error"));
        self.file_label.set_text(msg);
        self.file_label.set_wrap(true);
        self.file_label.set_single_line_mode(false);
        self.title_label.set_text("Error");
    }

    /// Turn the window into its dismissable outcome state: the action button
    /// becomes Close and Esc/X only close (the cancel callback goes quiet).
    /// `on_close` runs after the window is closed (holder cleanup, …).
    pub fn finish_dismiss<F: Fn() + 'static>(&self, on_close: F) {
        self.finished.set(true);
        self.action_btn.set_label("Close");
        self.action_btn.set_icon_name("window-close-symbolic");
        self.action_btn.remove_css_class("destructive-action");
        self.action_btn.add_css_class("suggested-action");
        self.action_btn.grab_focus();
        let window = self.window.clone();
        self.action_btn.connect_clicked(move |_| {
            window.close();
            on_close();
        });
    }

    /// Wire Cancel (button), Esc and the window X through one guarded path:
    /// while the job runs they invoke `f`, after the outcome they only close.
    pub fn on_cancel<F: Fn() + 'static>(&self, f: F) {
        let f = Rc::new(f);
        let finished = self.finished.clone();
        let run = move || {
            if !finished.get() {
                f.as_ref()();
            }
        };
        let run_btn = run.clone();
        self.action_btn.connect_clicked(move |_| run_btn());
        let run_esc = run.clone();
        let window_esc = self.window.clone();
        let esc = gtk::ShortcutController::new();
        esc.add_shortcut(gtk::Shortcut::new(
            Some(gtk::ShortcutTrigger::parse_string("Escape").unwrap()),
            Some(gtk::CallbackAction::new(move |_, _| {
                run_esc();
                window_esc.close();
                glib::Propagation::Stop
            })),
        ));
        self.window.add_controller(esc);
        let run_close = run.clone();
        self.window.connect_close_request(move |_| {
            run_close();
            glib::Propagation::Proceed
        });
    }

    pub fn close(&self) {
        self.window.close();
    }
}

fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "…".to_string();
    }
    let secs = secs as u64;
    if secs < 60 {
        format!("~{}s", secs.max(1))
    } else if secs < 3600 {
        format!("~{}m {}s", secs / 60, secs % 60)
    } else {
        format!("~{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;

    #[test]
    fn eta_formats() {
        assert_eq!(format_duration(3.2), "~3s");
        assert_eq!(format_duration(0.0), "~1s");
        assert_eq!(format_duration(125.0), "~2m 5s");
        assert_eq!(format_duration(3700.0), "~1h 1m");
        assert_eq!(format_duration(f64::INFINITY), "…");
    }
}
