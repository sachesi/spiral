//! Quick preview: Space shows the selected file without launching an application.
//!
//! Images, video, sound and text come from GTK itself. PDF pages are drawn by `pdftoppm`
//! in the sandbox the thumbnailers run in, so PDFs are previewed where poppler-utils is
//! installed. Anything else falls back to the file's thumbnail, or to its icon and type.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::{adw, file_utils, gdk, gio, glib, gtk};

const PRIO: glib::Priority = glib::Priority::DEFAULT;
/// How much of a text file is read; a log of any size still opens at once.
const TEXT_LIMIT: usize = 256 * 1024;
/// Images past this size are left to their thumbnail rather than decoded whole.
const IMAGE_LIMIT: i64 = 128 * 1024 * 1024;
/// Resolution PDF pages are rendered at.
const PDF_DPI: u32 = 150;
/// Size of the icon shown for files nothing can preview.
const ICON_SIZE: i32 = 128;
/// The sound player has only its icon and its controls to show, so it stays small.
const SOUND_ICON_SIZE: i32 = 96;
/// How long a file has to stay selected before it is loaded, so that running through a
/// folder with the arrows does not start a decoder for every file passed over.
const LOAD_DELAY: Duration = Duration::from_millis(120);
/// What the header takes before it has been measured.
const HEADER_HEIGHT: i32 = 46;
/// Bounds the dialog shapes itself within, whatever proportions the content has.
const MAX_WIDTH: i32 = 900;
const MAX_HEIGHT: i32 = 620;
const MIN_SIDE: i32 = 180;
/// Shapes for content whose proportions are not known before it is loaded: text to read,
/// video before its stream says how big it is, an image whose header could not be read, a
/// page before it is rendered, the sound player, and the icon for everything else.
const TEXT_SHAPE: (i32, i32) = (760, 514);
/// Video is shown at the size of its proportions, not of its pixel count: a small clip is
/// worth a window one can watch, and the shape then matches the one guessed before the
/// stream reported anything, so nothing has to move.
const VIDEO_BOX: (i32, i32) = (720, 405);
const IMAGE_SHAPE: (i32, i32) = (720, 494);
/// A4 upright, which is what most PDFs turn out to be.
const PAGE_SHAPE: (i32, i32) = (438, 620);
const SOUND_SHAPE: (i32, i32) = (420, 234);
const INFO_SHAPE: (i32, i32) = (340, 214);
/// How long one page of the preview takes to fade into the next.
const CROSSFADE: Duration = Duration::from_millis(120);
/// Zoom: one step of the buttons or the wheel, and how far it goes either way.
const ZOOM_STEP: f64 = 1.25;
const ZOOM_MIN: f64 = 0.05;
const ZOOM_MAX: f64 = 8.0;

/// Keeps the working directories of two tools running at once apart.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// A slot for one of the dialog's callbacks: stepping through the folder, opening the
/// file, turning a page.
type Slot<T> = RefCell<Option<Box<dyn Fn(T)>>>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PreviewDialog {
        pub title: adw::WindowTitle,
        pub header: adw::HeaderBar,
        pub content: gtk::Stack,
        /// Bumped per file, so a slow load cannot land after a newer one.
        pub generation: Cell<u64>,
        /// What is playing, stopped when it is replaced and when the dialog closes.
        pub media: RefCell<Option<gtk::MediaStream>>,
        /// Moves the selection the preview follows, by -1 or 1.
        pub step: Slot<i32>,
        /// Opens the file being previewed in its application.
        pub open: Slot<()>,
        /// Turns the page of the PDF on screen, by -1 or 1.
        pub flip: Slot<i32>,
        /// Zooms the picture on screen: 1 in, -1 out, 0 back to fitting the dialog.
        pub zoom: Slot<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreviewDialog {
        const NAME: &'static str = "SpiralPreviewDialog";
        type Type = super::PreviewDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for PreviewDialog {}
    impl WidgetImpl for PreviewDialog {}
    impl AdwDialogImpl for PreviewDialog {
        fn closed(&self) {
            self.obj().stop_media();
            self.parent_closed();
        }
    }
}

glib::wrapper! {
    pub struct PreviewDialog(ObjectSubclass<imp::PreviewDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreviewDialog {
    pub fn new() -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Preview"))
            .property("content-width", 720)
            .property("content-height", 500)
            .build();
        let imp = dialog.imp();

        // No close button: Space closes the preview the way it opened it, Escape too.
        let header = &imp.header;
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        header.set_title_widget(Some(&imp.title));

        // One page fades into the next, and the stack takes the size of the page on screen
        // rather than the largest one it has held.
        imp.content.set_vexpand(true);
        imp.content
            .set_transition_type(gtk::StackTransitionType::Crossfade);
        imp.content
            .set_transition_duration(CROSSFADE.as_millis() as u32);
        imp.content.set_hhomogeneous(false);
        imp.content.set_vhomogeneous(false);
        imp.content.connect_transition_running_notify(|stack| {
            if stack.is_transition_running() {
                return;
            }
            let shown = stack.visible_child();
            let mut pages: Vec<gtk::Widget> = Vec::new();
            let mut child = stack.first_child();
            while let Some(page) = child {
                child = page.next_sibling();
                if Some(&page) != shown.as_ref() {
                    pages.push(page);
                }
            }
            for page in pages {
                stack.remove(&page);
            }
        });
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(header);
        toolbar.set_content(Some(&imp.content));
        dialog.set_child(Some(&toolbar));
        dialog.setup_keys();
        dialog
    }

    /// Space closes the preview the way it opened it, Return hands the file to its
    /// application, the arrows walk the folder, Page Up and Page Down turn the pages of a
    /// PDF and Ctrl with +, - or 0 zooms. Captured, because the text view and the media
    /// controls below would otherwise keep the keys to themselves.
    fn setup_keys(&self) {
        use gdk::{Key, ModifierType as M};
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                if state.intersects(M::ALT_MASK | M::SUPER_MASK) {
                    return glib::Propagation::Proceed;
                }
                let imp = dialog.imp();
                let call = |slot: &Slot<i32>, delta: i32| match slot.borrow().as_ref() {
                    Some(f) => {
                        f(delta);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                };
                if state.contains(M::CONTROL_MASK) {
                    return match key {
                        Key::plus | Key::equal | Key::KP_Add => call(&imp.zoom, 1),
                        Key::minus | Key::KP_Subtract => call(&imp.zoom, -1),
                        Key::_0 | Key::KP_0 => call(&imp.zoom, 0),
                        _ => glib::Propagation::Proceed,
                    };
                }
                if state.contains(M::SHIFT_MASK) {
                    return glib::Propagation::Proceed;
                }
                match key {
                    Key::space => {
                        dialog.close();
                        glib::Propagation::Stop
                    }
                    Key::Return | Key::KP_Enter => {
                        if let Some(open) = imp.open.borrow().as_ref() {
                            open(());
                        }
                        dialog.close();
                        glib::Propagation::Stop
                    }
                    Key::Left => call(&imp.step, -1),
                    Key::Right => call(&imp.step, 1),
                    Key::Page_Up => call(&imp.flip, -1),
                    Key::Page_Down => call(&imp.flip, 1),
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.add_controller(keys);
    }

    /// Called with -1 or 1 when the arrows ask for the file before or after this one.
    pub fn connect_step(&self, f: impl Fn(i32) + 'static) {
        self.imp().step.replace(Some(Box::new(f)));
    }

    /// Called when the preview is asked to hand the file to its application.
    pub fn connect_open(&self, f: impl Fn() + 'static) {
        self.imp().open.replace(Some(Box::new(move |()| f())));
    }

    /// Show `info`: the header at once, the content when it has loaded.
    pub fn show_info(&self, info: &gio::FileInfo) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        self.stop_media();
        imp.flip.take();
        imp.zoom.take();
        imp.title.set_title(&file_utils::display_name(info));
        imp.title.set_subtitle(&subtitle(info));
        self.shape_for_kind(info);
        self.show_child(&spinner());
        let info = info.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                // Nothing is decoded for a file the arrows only passed over.
                glib::timeout_future(LOAD_DELAY).await;
                if dialog.imp().generation.get() != generation {
                    return;
                }
                let child = dialog.build_content(&info).await;
                if dialog.imp().generation.get() == generation {
                    dialog.show_child(&child);
                }
            }
        ));
    }

    /// Fade `child` in over whatever is on screen. The page it replaces is dropped when
    /// the fade is over, not while it is still being drawn.
    fn show_child(&self, child: &impl IsA<gtk::Widget>) {
        let stack = &self.imp().content;
        stack.add_child(child);
        stack.set_visible_child(child);
    }

    async fn build_content(&self, info: &gio::FileInfo) -> gtk::Widget {
        let file = file_utils::file_of(info);
        let content_type = info.content_type().unwrap_or_default().to_string();
        if file_utils::is_dir(info) {
            return self.info_page(info);
        }
        if content_type.starts_with("image/")
            && info.size() <= IMAGE_LIMIT
            && let Some(texture) = load_texture(&file).await
        {
            self.shape_to(&texture);
            return self.zoomable(&picture(&texture), &[]);
        }
        if content_type.starts_with("video/") {
            return self.video(&file);
        }
        if content_type.starts_with("audio/") {
            return self.sound(info, &file);
        }
        if gio::content_type_is_a(&content_type, "text/plain")
            && let Some(text) = load_text(&file).await
        {
            return text_view(&text);
        }
        if content_type == "application/pdf"
            && let Some(path) = file.path()
            && let Some(widget) = self.pdf(path).await
        {
            return widget;
        }
        match crate::thumbnails::load(info).await {
            Some(texture) => {
                self.shape_to(&texture);
                picture(&texture).upcast()
            }
            None => {
                // The guessed shape was for content that turned out not to be drawable.
                self.shape(INFO_SHAPE.0, INFO_SHAPE.1);
                self.info_page(info)
            }
        }
    }

    /// Video keeps its own proportions once the stream knows them; until then the dialog
    /// holds the shape most video has.
    fn video(&self, file: &gio::File) -> gtk::Widget {
        let video = gtk::Video::for_file(Some(file));
        video.set_autoplay(true);
        if let Some(stream) = video.media_stream() {
            stream.connect_prepared_notify(glib::clone!(
                #[weak(rename_to = dialog)]
                self,
                move |stream| {
                    let (w, h) = (stream.intrinsic_width(), stream.intrinsic_height());
                    if stream.is_prepared() && w > 0 && h > 0 {
                        dialog.shape_boxed(w as f64, h as f64, VIDEO_BOX);
                    }
                }
            ));
            self.imp().media.replace(Some(stream));
        }
        video.upcast()
    }

    /// Sound has nothing to draw: the file's own icon over the transport controls.
    fn sound(&self, info: &gio::FileInfo, file: &gio::File) -> gtk::Widget {
        let stream = gtk::MediaFile::for_file(file);
        stream.play();
        let controls = gtk::MediaControls::builder()
            .media_stream(&stream)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        self.imp().media.replace(Some(stream.upcast()));
        let icon = gtk::Image::from_gicon(&file_utils::icon_of(info));
        icon.set_pixel_size(SOUND_ICON_SIZE);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(7)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        column.append(&icon);
        column.append(&controls);
        column.upcast()
    }

    /// One PDF page with the buttons that turn it, or nothing when `pdftoppm` is missing.
    async fn pdf(&self, path: PathBuf) -> Option<gtk::Widget> {
        let pages = pdf_pages(path.clone()).await?;
        let first = pdf_page(path.clone(), 1).await?;
        self.shape_to(&first);
        let page = Rc::new(Cell::new(1u32));
        let picture = picture(&first);
        let label = gtk::Label::new(Some(&page_text(1, pages)));
        let previous = flat_button("go-previous-symbolic", &gettext("Previous Page"));
        previous.set_sensitive(false);
        let next = flat_button("go-next-symbolic", &gettext("Next Page"));
        next.set_sensitive(pages > 1);

        let flip = glib::clone!(
            #[strong]
            page,
            #[strong]
            picture,
            #[strong]
            label,
            #[strong]
            previous,
            #[strong]
            next,
            move |delta: i32| {
                let target = (page.get() as i32 + delta).clamp(1, pages as i32) as u32;
                if target == page.get() {
                    return;
                }
                page.set(target);
                label.set_label(&page_text(target, pages));
                previous.set_sensitive(target > 1);
                next.set_sensitive(target < pages);
                glib::spawn_future_local(glib::clone!(
                    #[strong]
                    path,
                    #[weak]
                    picture,
                    #[strong]
                    page,
                    async move {
                        // A page rendered after the reader moved on is dropped.
                        if let Some(texture) = pdf_page(path, target).await
                            && page.get() == target
                        {
                            picture.set_paintable(Some(&texture));
                        }
                    }
                ));
            }
        );
        previous.connect_clicked(glib::clone!(
            #[strong]
            flip,
            move |_| flip(-1)
        ));
        next.connect_clicked(glib::clone!(
            #[strong]
            flip,
            move |_| flip(1)
        ));
        self.imp().flip.replace(Some(Box::new(flip)));
        let extras: Vec<gtk::Widget> = vec![
            previous.upcast(),
            label.upcast(),
            next.upcast(),
            gtk::Separator::new(gtk::Orientation::Vertical).upcast(),
        ];
        Some(self.zoomable(&picture, &extras))
    }

    /// A picture that fits the dialog until the buttons, Ctrl with + and -, or Ctrl and
    /// the wheel say otherwise. `extras` share the floating bar, for the PDF page buttons.
    fn zoomable(&self, picture: &gtk::Picture, extras: &[gtk::Widget]) -> gtk::Widget {
        let scroll = gtk::ScrolledWindow::builder()
            .child(picture)
            .hexpand(true)
            .vexpand(true)
            .build();
        // Scrolling off while the picture is fitted: the policy is what makes the viewport
        // hold the picture to its own size instead of the picture's natural one.
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        let level = Rc::new(Cell::new(0.0f64));
        let label = gtk::Label::new(Some(&gettext("Fit")));
        label.set_width_chars(5);
        let out = flat_button("zoom-out-symbolic", &gettext("Zoom Out"));
        let fit = flat_button("zoom-fit-best-symbolic", &gettext("Fit to Window"));
        let in_ = flat_button("zoom-in-symbolic", &gettext("Zoom In"));

        let zoom = glib::clone!(
            #[strong]
            level,
            #[strong]
            picture,
            #[strong]
            scroll,
            #[strong]
            label,
            move |delta: i32| {
                let fit = fit_scale(&picture, &scroll);
                let next = if delta == 0 {
                    0.0
                } else {
                    let from = if level.get() > 0.0 { level.get() } else { fit };
                    let step = if delta > 0 {
                        ZOOM_STEP
                    } else {
                        1.0 / ZOOM_STEP
                    };
                    let wanted = (from * step).clamp(ZOOM_MIN, ZOOM_MAX);
                    // Zooming out stops at the fit instead of counting below it, where the
                    // picture cannot follow the number any further.
                    if wanted <= fit { 0.0 } else { wanted }
                };
                level.set(next);
                let Some(paintable) = picture.paintable() else {
                    return;
                };
                scroll.set_cursor_from_name(pan_cursor(next));
                if next <= 0.0 {
                    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
                    // Fitting never blows a small picture up; zooming is what does that.
                    picture.set_content_fit(gtk::ContentFit::ScaleDown);
                    picture.set_size_request(-1, -1);
                    label.set_label(&gettext("Fit"));
                    return;
                }
                picture.set_content_fit(gtk::ContentFit::Contain);
                scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
                picture.set_size_request(
                    (paintable.intrinsic_width() as f64 * next) as i32,
                    (paintable.intrinsic_height() as f64 * next) as i32,
                );
                label.set_label(&format!("{}%", (next * 100.0).round()));
            }
        );
        for (button, delta) in [(&out, -1), (&fit, 0), (&in_, 1)] {
            button.connect_clicked(glib::clone!(
                #[strong]
                zoom,
                move |_| zoom(delta)
            ));
        }
        // Ctrl and the wheel, as everywhere else that zooms.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        wheel.connect_scroll(glib::clone!(
            #[strong]
            zoom,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                    || dy == 0.0
                {
                    return glib::Propagation::Proceed;
                }
                zoom(if dy < 0.0 { 1 } else { -1 });
                glib::Propagation::Stop
            }
        ));
        scroll.add_controller(wheel);
        // Zoomed in, the picture is moved by dragging it: there are no scrollbars to grab.
        let drag = gtk::GestureDrag::new();
        let from = Rc::new(Cell::new((0.0, 0.0)));
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            scroll,
            #[strong]
            from,
            move |_, _, _| {
                from.set((scroll.hadjustment().value(), scroll.vadjustment().value()));
                scroll.set_cursor_from_name(Some("grabbing"));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[strong]
            scroll,
            #[strong]
            from,
            move |_, x, y| {
                let (left, top) = from.get();
                scroll.hadjustment().set_value(left - x);
                scroll.vadjustment().set_value(top - y);
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[strong]
            scroll,
            #[strong]
            level,
            move |_, _, _| scroll.set_cursor_from_name(pan_cursor(level.get()))
        ));
        scroll.add_controller(drag);
        self.imp().zoom.replace(Some(Box::new(zoom)));

        let bar = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .css_classes(["floating-bar"])
            .build();
        for extra in extras {
            bar.append(extra);
        }
        bar.append(&out);
        bar.append(&label);
        bar.append(&in_);
        bar.append(&fit);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroll));
        overlay.add_overlay(&bar);
        overlay.upcast()
    }

    /// Files nothing can draw: their icon alone, since the header already carries the name
    /// and the type.
    fn info_page(&self, info: &gio::FileInfo) -> gtk::Widget {
        let icon = gtk::Image::from_gicon(&file_utils::icon_of(info));
        icon.set_pixel_size(ICON_SIZE);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        icon.set_vexpand(true);
        icon.upcast()
    }

    /// The shape the file will want, from what the listing already knows and, for a local
    /// image, from its header alone. Done before the content is loaded so that the dialog
    /// opens at its size instead of growing into it a moment later.
    fn shape_for_kind(&self, info: &gio::FileInfo) {
        if file_utils::is_dir(info) {
            return self.shape(INFO_SHAPE.0, INFO_SHAPE.1);
        }
        let content_type = info.content_type().unwrap_or_default().to_string();
        let (width, height) = if content_type.starts_with("image/") {
            // Reading the header of an image is a few bytes, not a decode.
            match file_utils::file_of(info)
                .path()
                .and_then(gtk::gdk_pixbuf::Pixbuf::file_info)
            {
                Some((_, width, height)) if width > 0 && height > 0 => {
                    return self.shape_fitted(width as f64, height as f64);
                }
                _ => IMAGE_SHAPE,
            }
        } else if content_type.starts_with("video/") {
            // The thumbnail, when there is one, has the proportions of the video itself.
            let (width, height) = thumbnail_size(info).unwrap_or((16.0, 9.0));
            return self.shape_boxed(width, height, VIDEO_BOX);
        } else if content_type.starts_with("audio/") {
            SOUND_SHAPE
        } else if gio::content_type_is_a(&content_type, "text/plain") {
            TEXT_SHAPE
        } else if content_type == "application/pdf" && can_render_pdf() {
            PAGE_SHAPE
        } else {
            INFO_SHAPE
        };
        self.shape(width, height);
    }

    /// Give the dialog the proportions of what it holds, within the bounds it may take.
    /// `width` and `height` are for the content itself; the header is added on top of them,
    /// so a picture asked for in its own proportions is drawn in them and not letterboxed.
    fn shape(&self, width: i32, height: i32) {
        let header = self
            .imp()
            .header
            .measure(gtk::Orientation::Vertical, -1)
            .0
            .max(HEADER_HEIGHT);
        self.set_content_width(width.clamp(MIN_SIDE, MAX_WIDTH));
        self.set_content_height(height.clamp(MIN_SIDE, MAX_HEIGHT) + header);
    }

    fn shape_to(&self, paintable: &impl IsA<gdk::Paintable>) {
        let paintable = paintable.as_ref();
        self.shape_fitted(
            paintable.intrinsic_width() as f64,
            paintable.intrinsic_height() as f64,
        );
    }

    /// `width` by `height` scaled to fill `box_`, up as well as down: what is shown at a
    /// size of its own choosing keeps its proportions but not its pixel count.
    fn shape_boxed(&self, width: f64, height: f64, box_: (i32, i32)) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let scale = (box_.0 as f64 / width).min(box_.1 as f64 / height);
        self.shape((width * scale) as i32, (height * scale) as i32);
    }

    /// A `width` by `height` picture scaled into the largest shape allowed, never blown up
    /// past its own size: a thumbnail should not open a window the size of a wall.
    fn shape_fitted(&self, width: f64, height: f64) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let scale = (MAX_WIDTH as f64 / width)
            .min(MAX_HEIGHT as f64 / height)
            .min(1.0);
        self.shape((width * scale) as i32, (height * scale) as i32);
    }

    fn stop_media(&self) {
        let Some(stream) = self.imp().media.take() else {
            return;
        };
        stream.pause();
        // Tearing the pipeline down through `clear` rather than leaving it to the last
        // reference: a decoder disposed of mid-start takes the process with it.
        if let Some(file) = stream.downcast_ref::<gtk::MediaFile>() {
            file.clear();
        }
    }
}

impl Default for PreviewDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// The type of the file and its size, for under the name.
fn subtitle(info: &gio::FileInfo) -> String {
    let kind = file_utils::type_string(info);
    let size = file_utils::size_string(info);
    if size.is_empty() {
        return kind;
    }
    // Translators: %t is a file type ("PDF document"), %s its size ("1.2 MB").
    gettext("%t, %s").replace("%t", &kind).replace("%s", &size)
}

fn page_text(page: u32, pages: u32) -> String {
    // Translators: %p is the page being shown, %n the number of pages in the document.
    gettext("%p of %n")
        .replace("%p", &page.to_string())
        .replace("%n", &pages.to_string())
}

fn spinner() -> gtk::Widget {
    adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build()
        .upcast()
}

fn picture(paintable: &impl IsA<gdk::Paintable>) -> gtk::Picture {
    gtk::Picture::builder()
        .paintable(paintable)
        .can_shrink(true)
        .content_fit(gtk::ContentFit::ScaleDown)
        .hexpand(true)
        .vexpand(true)
        .build()
}

/// The scale a fitted picture is drawn at: where zooming starts and where zooming out
/// ends. Never above 1, since fitting shows a small picture at its own size.
fn fit_scale(picture: &gtk::Picture, scroll: &gtk::ScrolledWindow) -> f64 {
    let Some(paintable) = picture.paintable() else {
        return 1.0;
    };
    let (width, height) = (
        paintable.intrinsic_width() as f64,
        paintable.intrinsic_height() as f64,
    );
    if width <= 0.0 || height <= 0.0 {
        return 1.0;
    }
    (scroll.width() as f64 / width)
        .min(scroll.height() as f64 / height)
        .min(1.0)
}

/// The size of the thumbnail the listing already holds, which has the proportions of the
/// file itself. Reading its header is a few bytes and no decode.
fn thumbnail_size(info: &gio::FileInfo) -> Option<(f64, f64)> {
    let path = info.attribute_byte_string("thumbnail::path")?;
    let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(path.as_str())?;
    (width > 0 && height > 0).then_some((width as f64, height as f64))
}

/// Whether a PDF page can be drawn at all: without the tool, a PDF is shaped like the icon
/// it will end up showing instead of shrinking into it once the attempt has failed.
fn can_render_pdf() -> bool {
    glib::find_program_in_path("pdftoppm").is_some()
}

/// The hand that says a zoomed picture can be dragged, and nothing while it fits.
fn pan_cursor(level: f64) -> Option<&'static str> {
    (level > 0.0).then_some("grab")
}

fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button
}

fn text_view(text: &str) -> gtk::Widget {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    view.buffer().set_text(text);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();
    // No scrollbars anywhere in the preview; the wheel and the keys still scroll.
    scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
    scroll.upcast()
}

/// Decoding happens off the main loop for local files, where a large photograph would
/// otherwise freeze the window; anything else is small enough to read whole.
async fn load_texture(file: &gio::File) -> Option<gdk::Texture> {
    match file.path() {
        Some(path) => gio::spawn_blocking(move || gdk::Texture::from_filename(path).ok())
            .await
            .ok()
            .flatten(),
        None => {
            let (data, _) = file.load_bytes_future().await.ok()?;
            gdk::Texture::from_bytes(&data).ok()
        }
    }
}

/// The first `TEXT_LIMIT` bytes of `file`. Bytes that are not UTF-8 are replaced rather
/// than refused, so a file that only claims to be text still shows what it holds.
async fn load_text(file: &gio::File) -> Option<String> {
    let stream = file.read_future(PRIO).await.ok()?;
    let mut data: Vec<u8> = Vec::new();
    while data.len() < TEXT_LIMIT {
        let read = stream
            .read_bytes_future(TEXT_LIMIT - data.len(), PRIO)
            .await
            .ok()?;
        if read.is_empty() {
            break;
        }
        data.extend_from_slice(&read);
    }
    let _ = stream.close_future(PRIO).await;
    Some(String::from_utf8_lossy(&data).into_owned())
}

async fn pdf_pages(path: PathBuf) -> Option<u32> {
    gio::spawn_blocking(move || {
        run_tool(
            "pdfinfo",
            &path,
            |input, _| vec![input.as_os_str().to_owned()],
            |out, _| {
                String::from_utf8_lossy(out)
                    .lines()
                    .find_map(|line| line.strip_prefix("Pages:"))
                    .and_then(|count| count.trim().parse().ok())
            },
        )
    })
    .await
    .ok()
    .flatten()
}

async fn pdf_page(path: PathBuf, page: u32) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || {
        run_tool(
            "pdftoppm",
            &path,
            |input, work| {
                let page = page.to_string();
                vec![
                    "-png".into(),
                    "-singlefile".into(),
                    "-r".into(),
                    PDF_DPI.to_string().into(),
                    "-f".into(),
                    page.clone().into(),
                    "-l".into(),
                    page.into(),
                    input.as_os_str().to_owned(),
                    work.join("page").into_os_string(),
                ]
            },
            |_, work| gdk::Texture::from_filename(work.join("page.png")).ok(),
        )
    })
    .await
    .ok()
    .flatten()
}

/// Run `program` over `input` the way a thumbnailer runs: inside bubblewrap where it is
/// installed, with the file bound read-only and one private directory to write into.
/// `args` is handed the paths as the child sees them, `result` what it printed and the
/// directory it wrote to, which is removed as soon as `result` returns.
fn run_tool<T>(
    program: &str,
    input: &Path,
    args: impl FnOnce(&Path, &Path) -> Vec<OsString>,
    result: impl FnOnce(&[u8], &Path) -> Option<T>,
) -> Option<T> {
    let program = glib::find_program_in_path(program)?;
    let work = std::env::temp_dir().join(format!(
        "spiral-preview-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // Not create_dir_all: a name another process got to first is refused, not adopted.
    std::fs::create_dir(&work).ok()?;
    let sandbox = crate::thumbnails::sandbox_base(&program.to_string_lossy());
    let (input_seen, work_seen) = match sandbox {
        Some(_) => (Path::new("/tmp/in"), Path::new("/tmp/out")),
        None => (input, work.as_path()),
    };
    let argv = args(input_seen, work_seen);
    let mut seccomp = None;
    let mut cmd = match sandbox {
        Some(sandbox) => {
            let mut cmd = std::process::Command::new(&sandbox.argv[0]);
            cmd.args(&sandbox.argv[1..]);
            // Drawing a page needs the fonts the document does not carry itself.
            let font_cache = glib::user_cache_dir().join("fontconfig");
            cmd.args(["--ro-bind-try", "/etc/fonts", "/etc/fonts"]);
            cmd.args([
                "--ro-bind-try",
                "/var/cache/fontconfig",
                "/var/cache/fontconfig",
            ]);
            cmd.arg("--ro-bind-try").arg(&font_cache).arg(&font_cache);
            cmd.arg("--ro-bind").arg(input).arg(input_seen);
            cmd.arg("--bind").arg(&work).arg(work_seen);
            cmd.arg("--").arg(&program);
            // The seccomp memfd must stay open until the child has started.
            seccomp = sandbox.seccomp;
            cmd
        }
        None => std::process::Command::new(&program),
    };
    let run = cmd
        .args(&argv)
        .stderr(std::process::Stdio::piped())
        .output();
    drop(seccomp);
    let out = match run {
        Ok(out) if out.status.success() => result(&out.stdout, &work),
        Ok(out) => {
            glib::g_debug!(
                "spiral",
                "preview {program:?} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            None
        }
        Err(e) => {
            glib::g_debug!("spiral", "preview {program:?} could not start: {e}");
            None
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    out
}
