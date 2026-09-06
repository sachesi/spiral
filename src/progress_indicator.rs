//! Sidebar-bottom button listing running file operations, with a pie progress icon per job.

use std::cell::Cell;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::subclass::prelude::*;

use crate::ops::{Job, JobManager, JobStatus};
use crate::{adw, gdk, glib, gtk};

mod paintable_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ProgressPaintable)]
    pub struct ProgressPaintable {
        #[property(get, set = Self::set_progress, minimum = 0.0, maximum = 1.0)]
        pub progress: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProgressPaintable {
        const NAME: &'static str = "SpiralProgressPaintable";
        type Type = super::ProgressPaintable;
        type Interfaces = (gdk::Paintable, gtk::SymbolicPaintable);
    }

    #[glib::derived_properties]
    impl ObjectImpl for ProgressPaintable {}

    impl gdk::subclass::prelude::PaintableImpl for ProgressPaintable {
        fn intrinsic_width(&self) -> i32 {
            16
        }
        fn intrinsic_height(&self) -> i32 {
            16
        }
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            self.draw(snapshot, width, height, &gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
        }
    }

    impl gtk::subclass::prelude::SymbolicPaintableImpl for ProgressPaintable {
        fn snapshot_symbolic(
            &self,
            snapshot: &gdk::Snapshot,
            width: f64,
            height: f64,
            colors: &[gdk::RGBA],
        ) {
            let color = colors
                .first()
                .copied()
                .unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
            self.draw(snapshot, width, height, &color);
        }

        // GTK 4.22 prefers this vfunc; the gtk4-rs default chains to a parent that does not
        // exist for interface defaults and panics.
        fn snapshot_with_weight(
            &self,
            snapshot: &gdk::Snapshot,
            width: f64,
            height: f64,
            colors: &[gdk::RGBA],
            _weight: f64,
        ) {
            self.snapshot_symbolic(snapshot, width, height, colors);
        }
    }

    impl ProgressPaintable {
        fn set_progress(&self, p: f64) {
            self.progress.set(p);
            self.obj().invalidate_contents();
        }

        fn draw(&self, snapshot: &gdk::Snapshot, width: f64, height: f64, color: &gdk::RGBA) {
            let snapshot = snapshot.downcast_ref::<gtk::Snapshot>().unwrap();
            let bounds =
                gtk::graphene::Rect::new(-2.0, -2.0, width as f32 + 4.0, height as f32 + 4.0);
            let cr = snapshot.append_cairo(&bounds);
            let end = self.progress.get() * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
            let r = width / 2.0 + 1.0;
            cr.translate(width / 2.0, height / 2.0);
            // Faint full disc, then the completed wedge on top.
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                color.alpha() as f64 * 0.3,
            );
            cr.arc(0.0, 0.0, r, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                color.alpha() as f64,
            );
            cr.move_to(0.0, 0.0);
            cr.arc(0.0, 0.0, r, -std::f64::consts::FRAC_PI_2, end);
            cr.close_path();
            let _ = cr.fill();
        }
    }
}

glib::wrapper! {
    pub struct ProgressPaintable(ObjectSubclass<paintable_imp::ProgressPaintable>)
        @implements gdk::Paintable, gtk::SymbolicPaintable;
}

impl ProgressPaintable {
    pub fn for_job(job: &Job) -> Self {
        let p: Self = glib::Object::new();
        job.bind_property("fraction", &p, "progress")
            .sync_create()
            .build();
        p
    }
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ProgressIndicator)]
    pub struct ProgressIndicator {
        #[property(get)]
        pub has_jobs: Cell<bool>,
        pub button: gtk::MenuButton,
        pub summary: gtk::ListView,
        pub operations_list: gtk::ListBox,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProgressIndicator {
        const NAME: &'static str = "SpiralProgressIndicator";
        type Type = super::ProgressIndicator;
        type ParentType = adw::Bin;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ProgressIndicator {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("spiral-progress-indicator");
            obj.set_hexpand(true);

            // Compact summary rows inside the button itself.
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                item.set_activatable(false);
                item.set_focusable(false);
                item.set_selectable(false);
                let bx = gtk::Box::builder().spacing(9).build();
                bx.append(&gtk::Image::builder().pixel_size(14).margin_start(3).build());
                bx.append(
                    &gtk::Label::builder()
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .xalign(0.0)
                        .build(),
                );
                item.set_child(Some(&bx));
            });
            factory.connect_bind(|_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(job) = item.item().and_downcast::<Job>() else {
                    return;
                };
                let bx = item.child().unwrap();
                let image = bx.first_child().and_downcast::<gtk::Image>().unwrap();
                let label = bx.last_child().and_downcast::<gtk::Label>().unwrap();
                image.set_paintable(Some(&ProgressPaintable::for_job(&job)));
                job.bind_property("description", &label, "label")
                    .sync_create()
                    .build();
            });
            self.summary.set_factory(Some(&factory));
            self.summary.set_valign(gtk::Align::Center);
            self.summary.set_can_focus(false);

            self.button.add_css_class("flat");
            self.button
                .set_tooltip_text(Some(&gettext("Show File Operations")));
            self.button.set_direction(gtk::ArrowType::Up);
            self.button.set_child(Some(&self.summary));

            self.operations_list
                .set_selection_mode(gtk::SelectionMode::None);
            self.operations_list.set_margin_top(6);
            self.operations_list.set_margin_bottom(6);
            self.operations_list.set_margin_start(6);
            self.operations_list.set_margin_end(6);
            self.operations_list.add_css_class("operations-list");
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .max_content_height(270)
                .propagate_natural_height(true)
                .propagate_natural_width(true)
                .child(&self.operations_list)
                .build();
            let popover = gtk::Popover::builder().child(&scroller).build();
            self.button.set_popover(Some(&popover));
            obj.set_child(Some(&self.button));
        }
    }

    impl WidgetImpl for ProgressIndicator {}
    impl adw::subclass::prelude::BinImpl for ProgressIndicator {}
}

glib::wrapper! {
    pub struct ProgressIndicator(ObjectSubclass<imp::ProgressIndicator>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ProgressIndicator {
    pub fn set_manager(&self, manager: &JobManager) {
        let imp = self.imp();
        let jobs = manager.jobs();
        imp.summary
            .set_model(Some(&gtk::NoSelection::new(Some(jobs.clone()))));
        imp.operations_list
            .bind_model(Some(&jobs), |obj| job_row(obj.downcast_ref().unwrap()));
        jobs.connect_items_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |m, _, _, added| {
                this.set_has_jobs(m.n_items() > 0);
                if added > 0 {
                    this.remove_css_class("needs-attention");
                    this.add_css_class("needs-attention");
                }
            }
        ));
        // Jobs may already be running when a window is opened later.
        self.set_has_jobs(jobs.n_items() > 0);
    }

    fn set_has_jobs(&self, has: bool) {
        if self.imp().has_jobs.replace(has) != has {
            self.notify_has_jobs();
        }
    }

    pub fn popup(&self) {
        self.imp().button.popup();
    }
}

/// Detailed row for the popover: status, progress bar, cancel, details.
fn job_row(job: &Job) -> gtk::Widget {
    let grid = gtk::Grid::builder()
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .row_spacing(4)
        .build();
    let status = gtk::Label::builder()
        .width_request(300)
        .hexpand(true)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(40)
        .build();
    job.bind_property("description", &status, "label")
        .sync_create()
        .build();
    let bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .margin_start(2)
        .build();
    job.bind_property("fraction", &bar, "fraction")
        .sync_create()
        .build();
    let details = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label", "numeric"])
        .build();
    job.bind_property("detail", &details, "label")
        .sync_create()
        .build();
    let cancel = gtk::Button::builder()
        .icon_name("process-stop-symbolic")
        .valign(gtk::Align::Center)
        .margin_start(20)
        .css_classes(["circular"])
        .tooltip_text(gettext("Cancel"))
        .build();
    cancel.connect_clicked(glib::clone!(
        #[weak]
        job,
        move |_| job.cancel()
    ));
    job.connect_status_notify(glib::clone!(
        #[weak]
        cancel,
        #[weak]
        bar,
        move |job| {
            cancel.set_visible(!job.is_finished());
            if job.status() == JobStatus::Done {
                bar.set_fraction(1.0);
            }
        }
    ));
    grid.attach(&status, 0, 0, 1, 1);
    grid.attach(&bar, 0, 1, 1, 1);
    grid.attach(&details, 0, 2, 1, 1);
    grid.attach(&cancel, 1, 0, 1, 3);
    gtk::ListBoxRow::builder()
        .child(&grid)
        .activatable(false)
        .build()
        .upcast()
}
