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
/// Images past this size on disk, or past this many pixels, are left to their thumbnail
/// rather than decoded whole: a panorama decodes to four bytes a pixel.
const IMAGE_LIMIT: i64 = 128 * 1024 * 1024;
const IMAGE_PIXELS: i64 = 80_000_000;
/// How much of an image is read looking for the EXIF tag that says which way up it is.
const EXIF_SCAN: usize = 64 * 1024;
/// Resolution PDF pages are rendered at.
const PDF_DPI: u32 = 150;
/// How far the proportions of what arrives may differ from the ones the dialog opened
/// with before it is worth resizing the window under the reader.
const SHAPE_SLACK: f64 = 0.2;
/// How much of a PDF is read looking for the size of its first page.
const PDF_SCAN: usize = 256 * 1024;
/// Size of the icon shown for files nothing can preview.
const ICON_SIZE: i32 = 128;
/// The sound player has only its icon and its controls to show, so it stays small.
const SOUND_ICON_SIZE: i32 = 96;
/// How long a file has to stay selected before it is loaded, so that running through a
/// folder with the arrows does not start a decoder for every file passed over.
const LOAD_DELAY: Duration = Duration::from_millis(120);
/// What the header takes before it has been measured.
const HEADER_HEIGHT: i32 = 46;
/// How much of the window the preview may take, and the bounds it keeps until it is told
/// how large that window is.
const WINDOW_SHARE: f64 = 0.82;
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
/// A4 upright in points, which is what most PDFs turn out to be.
const PAGE_POINTS: (f64, f64) = (595.28, 841.89);
const SOUND_SHAPE: (i32, i32) = (420, 234);
const INFO_SHAPE: (i32, i32) = (340, 214);
/// How long one page of the preview takes to fade into the next.
const CROSSFADE: Duration = Duration::from_millis(120);

/// How long the PDF tool may take over one page before it is killed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(20);
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
        /// The content size last asked for, to tell a shape that has to change from one
        /// that would only flinch.
        pub shaped: Cell<(i32, i32)>,
        /// The largest content the dialog may take, from the window it opens over.
        pub bounds: Cell<(i32, i32)>,
        /// The page size of the PDF being shown, once anything has said what it is.
        pub page_size: Cell<Option<(f64, f64)>>,
        /// How many pages it has, so the tool is asked once and not twice.
        pub page_count: Cell<Option<u32>>,
        /// What this dialog listens to on the player, undone when the file changes and
        /// when the dialog closes: the player outlives both.
        pub player_handlers: RefCell<Vec<glib::SignalHandlerId>>,
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

    impl ObjectImpl for PreviewDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.bounds.set((MAX_WIDTH, MAX_HEIGHT));
        }
    }
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

    /// The room the preview has: a share of the window it opens over, so a page or a
    /// picture is shown as large as that window can hold rather than at a fixed size.
    pub fn set_bounds(&self, width: i32, height: i32) {
        if width > 0 && height > 0 {
            self.imp().bounds.set((
                ((width as f64 * WINDOW_SHARE) as i32).max(MIN_SIDE),
                ((height as f64 * WINDOW_SHARE) as i32).max(MIN_SIDE),
            ));
        }
    }

    /// Called with -1 or 1 when the arrows ask for the file before or after this one.
    pub fn connect_step(&self, f: impl Fn(i32) + 'static) {
        self.imp().step.replace(Some(Box::new(f)));
    }

    /// Called when the preview is asked to hand the file to its application.
    pub fn connect_open(&self, f: impl Fn() + 'static) {
        self.imp().open.replace(Some(Box::new(move |()| f())));
    }

    /// Ask the PDF tool for the page size before the dialog is presented, for the files
    /// that keep it inside a compressed object stream where nothing else can read it.
    /// Without this the dialog would open upright and turn itself over a moment later.
    pub async fn shape_ahead(&self, info: &gio::FileInfo) {
        let probe = Probe::of(info);
        if probe.content_type != "application/pdf" {
            return;
        }
        let generation = self.imp().generation.get();
        let facts = gio::spawn_blocking(move || {
            let path = probe.path.as_deref()?;
            let known = probe.thumbnail_size().is_some() || pdf_page_size(path).is_some();
            (can_render_pdf() && !known)
                .then(|| pdf_facts(path))
                .flatten()
        })
        .await
        .ok()
        .flatten();
        let Some((pages, size)) = facts else { return };
        // The selection may have moved on while the tool ran.
        if self.imp().generation.get() != generation {
            return;
        }
        self.imp().page_size.set(Some(size));
        self.imp().page_count.set(Some(pages));
        self.shape_boxed(size.0, size.1, self.imp().bounds.get());
    }

    /// Show `info`: the header at once, the content when it has loaded.
    pub async fn show_info(&self, info: &gio::FileInfo) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        self.stop_media();
        imp.flip.take();
        imp.zoom.take();
        imp.page_size.set(None);
        imp.page_count.set(None);
        imp.title.set_title(&file_utils::display_name(info));
        imp.title.set_subtitle(&subtitle(info));
        // The shape comes from headers on disk, read off the main thread. Until it is
        // known the page on screen stays, so nothing is drawn in a shape it will not keep.
        let probe = Probe::of(info);
        let shape = gio::spawn_blocking(move || probe.shape())
            .await
            .unwrap_or(Shape::Fixed(INFO_SHAPE));
        if imp.generation.get() != generation {
            return;
        }
        self.apply_shape(shape);
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
            return self.video(info, &file);
        }
        if content_type.starts_with("audio/") {
            // The cover takes the place of the icon and its size, so waiting for one to be
            // made costs nothing but the wait: the player is the same shape either way.
            return self.sound(info, &file);
        }
        if gio::content_type_is_a(&content_type, "text/plain")
            && let Some(text) = load_text(&file).await
        {
            return text_view(&text, info, &content_type);
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

    /// The one player, pointed at `file`, with this dialog listening to it until the next
    /// file or the close takes it away again. `None` where there is no playback to offer.
    fn player(&self, info: &gio::FileInfo, file: &gio::File) -> Option<crate::player::Player> {
        let player = crate::player::player()?;
        player.set_file(Some(file.clone()));
        let generation = self.imp().generation.get();
        let info = info.clone();
        let failed = player.connect_error_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |player| {
                // A file the pipeline cannot play shows what it would have shown anyway,
                // in the shape the dialog already has.
                if player.error().is_some() && dialog.imp().generation.get() == generation {
                    dialog.show_child(&dialog.info_page(&info));
                }
            }
        ));
        self.imp().player_handlers.borrow_mut().push(failed);
        Some(player)
    }

    /// Video keeps its own proportions once the stream knows them; until then the dialog
    /// holds the shape most video has.
    fn video(&self, info: &gio::FileInfo, file: &gio::File) -> gtk::Widget {
        let Some(player) = self.player(info, file) else {
            return self.info_page(info);
        };
        let video = gtk::Video::for_media_stream(Some(&player));
        video.set_autoplay(true);
        // The size comes with the first frame, which may be after the stream is prepared.
        let reshape = glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |player: &crate::player::Player| {
                let (w, h) = (player.intrinsic_width(), player.intrinsic_height());
                if w > 0 && h > 0 {
                    dialog.shape_boxed(w as f64, h as f64, VIDEO_BOX);
                }
            }
        );
        let handlers = [
            player.connect_prepared_notify(glib::clone!(
                #[strong]
                reshape,
                move |player| reshape(player)
            )),
            player.connect_invalidate_size(glib::clone!(
                #[strong]
                reshape,
                move |player| reshape(player)
            )),
        ];
        self.imp().player_handlers.borrow_mut().extend(handlers);
        video.upcast()
    }

    /// Sound has its icon to draw, and the cover in its place once the file has given one
    /// up; either way the transport controls sit under it.
    fn sound(&self, info: &gio::FileInfo, file: &gio::File) -> gtk::Widget {
        let Some(player) = self.player(info, file) else {
            return self.info_page(info);
        };
        player.play();
        let controls = gtk::MediaControls::builder()
            .media_stream(&player)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        let art = gtk::Image::from_gicon(&file_utils::icon_of(info));
        art.set_pixel_size(SOUND_ICON_SIZE);
        // The rounded corners of the cover come from clipping the widget, so the cover is
        // made to fill it exactly: square, at the size of the icon it replaces, in a widget
        // no wider than that rather than one stretched across the player.
        art.set_halign(gtk::Align::Center);
        art.set_overflow(gtk::Overflow::Hidden);
        let side = SOUND_ICON_SIZE * self.scale_factor();
        let cover = player.connect_cover(glib::clone!(
            #[weak]
            art,
            move |player| {
                let Some(bytes) = player.cover() else { return };
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    art,
                    async move {
                        if let Some(texture) = cover_texture(bytes, side).await {
                            art.set_paintable(Some(&texture));
                            art.add_css_class("spiral-preview-cover");
                        }
                    }
                ));
            }
        ));
        self.imp().player_handlers.borrow_mut().push(cover);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        column.append(&art);
        column.append(&controls);
        column.upcast()
    }

    /// One PDF page with the buttons that turn it, or nothing when `pdftoppm` is missing.
    async fn pdf(&self, path: PathBuf) -> Option<gtk::Widget> {
        let known = self.imp().page_count.get().zip(self.imp().page_size.get());
        let (pages, size) = match known {
            Some(facts) => facts,
            None => pdf_info(path.clone()).await?,
        };
        // The shape was decided before the dialog opened, from the thumbnail or from the
        // file itself; the page dictionary of a modern PDF is compressed and neither may
        // have found it. Rather than resize a window the reader is already looking at, the
        // page is drawn inside the shape there is, and only a page lying on its side —
        // which no margin can absorb — is worth moving the window for.
        let (shaped_width, shaped_height) = self.imp().shaped.get();
        let shaped = shaped_width as f64 / shaped_height as f64;
        if ((size.0 / size.1) / shaped - 1.0).abs() > SHAPE_SLACK {
            self.shape_boxed(size.0, size.1, self.imp().bounds.get());
        }
        let first = pdf_page(path.clone(), 1).await?;
        let page = Rc::new(Cell::new(1u32));
        let picture = picture(&first);
        let label = gtk::Label::new(Some(&page_text(1, pages)));
        let previous = flat_button("go-previous-symbolic", &gettext("Previous Page"));
        previous.set_sensitive(false);
        let next = flat_button("go-next-symbolic", &gettext("Next Page"));
        next.set_sensitive(pages > 1);

        // Weak, because the buttons hold this closure.
        let flip = glib::clone!(
            #[strong]
            page,
            #[weak]
            picture,
            #[weak]
            label,
            #[weak]
            previous,
            #[weak]
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

        // Where the pointer is, so that the wheel zooms around what is under it.
        let pointer = Rc::new(Cell::new(None::<(f64, f64)>));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            pointer,
            move |_, x, y| pointer.set(Some((x, y)))
        ));
        motion.connect_leave(glib::clone!(
            #[strong]
            pointer,
            move |_| pointer.set(None)
        ));
        scroll.add_controller(motion);

        // Weak, because the wheel and the drag on `scroll` hold this closure: a strong
        // reference from there would keep the widget alive after the page is gone.
        let zoom = glib::clone!(
            #[strong]
            level,
            #[weak]
            picture,
            #[weak]
            scroll,
            #[weak]
            label,
            move |step: f64, at: Option<(f64, f64)>| {
                let fit = fit_scale(&picture, &scroll);
                let before = if level.get() > 0.0 { level.get() } else { fit };
                let next = if step <= 0.0 {
                    0.0
                } else {
                    let wanted = (before * step).clamp(ZOOM_MIN, ZOOM_MAX);
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
                let (width, height) = (
                    paintable.intrinsic_width() as f64 * next,
                    paintable.intrinsic_height() as f64 * next,
                );
                picture.set_size_request(width as i32, height as i32);
                label.set_label(&format!("{}%", (next * 100.0).round()));
                // Hold the point the zoom happened around still. The scrolled window only
                // learns the new size in the next layout, so the room for it is made here
                // and the offsets land in the same frame as the picture that needs them.
                let (anchor_x, anchor_y) =
                    at.unwrap_or((scroll.width() as f64 / 2.0, scroll.height() as f64 / 2.0));
                let ratio = next / before;
                let (horizontal, vertical) = (scroll.hadjustment(), scroll.vadjustment());
                horizontal.set_upper(width.max(scroll.width() as f64));
                vertical.set_upper(height.max(scroll.height() as f64));
                horizontal.set_value((horizontal.value() + anchor_x) * ratio - anchor_x);
                vertical.set_value((vertical.value() + anchor_y) * ratio - anchor_y);
            }
        );
        for (button, step) in [(&out, 1.0 / ZOOM_STEP), (&fit, 0.0), (&in_, ZOOM_STEP)] {
            button.connect_clicked(glib::clone!(
                #[strong]
                zoom,
                move |_| zoom(step, None)
            ));
        }
        // Ctrl and the wheel, as everywhere else that zooms.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        wheel.connect_scroll(glib::clone!(
            #[strong]
            zoom,
            #[strong]
            pointer,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                    || dy == 0.0
                {
                    return glib::Propagation::Proceed;
                }
                // A wheel notch is a whole step; a touchpad sends fractions of one and
                // zooms by fractions of a step, which is what makes it feel continuous.
                zoom(ZOOM_STEP.powf(-dy), pointer.get());
                glib::Propagation::Stop
            }
        ));
        scroll.add_controller(wheel);
        // Zoomed in, the picture is moved by dragging it: there are no scrollbars to grab.
        let drag = gtk::GestureDrag::new();
        let from = Rc::new(Cell::new((0.0, 0.0)));
        drag.connect_drag_begin(glib::clone!(
            #[weak]
            scroll,
            #[strong]
            from,
            move |_, _, _| {
                from.set((scroll.hadjustment().value(), scroll.vadjustment().value()));
                scroll.set_cursor_from_name(Some("grabbing"));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak]
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
            #[weak]
            scroll,
            #[strong]
            level,
            move |_, _, _| scroll.set_cursor_from_name(pan_cursor(level.get()))
        ));
        scroll.add_controller(drag);
        self.imp().zoom.replace(Some(Box::new(move |delta| {
            let step = match delta {
                1 => ZOOM_STEP,
                -1 => 1.0 / ZOOM_STEP,
                _ => 0.0,
            };
            zoom(step, None)
        })));

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

    /// Give the dialog the shape a probe found for the file, before its content is loaded,
    /// so that it opens at its size instead of growing into it a moment later.
    fn apply_shape(&self, shape: Shape) {
        match shape {
            Shape::Fitted(width, height) => self.shape_fitted(width, height),
            Shape::Page(width, height) => self.shape_boxed(width, height, self.imp().bounds.get()),
            Shape::Video(width, height) => self.shape_boxed(width, height, VIDEO_BOX),
            Shape::Fixed((width, height)) => self.shape(width, height),
        }
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
        let (most_width, most_height) = self.imp().bounds.get();
        let (width, height) = (
            width.clamp(MIN_SIDE, most_width),
            height.clamp(MIN_SIDE, most_height),
        );
        self.imp().shaped.set((width, height));
        self.set_content_width(width);
        self.set_content_height(height + header);
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
        let bounds = self.imp().bounds.get();
        let box_ = (box_.0.min(bounds.0), box_.1.min(bounds.1));
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
        let (most_width, most_height) = self.imp().bounds.get();
        let scale = (most_width as f64 / width)
            .min(most_height as f64 / height)
            .min(1.0);
        self.shape((width * scale) as i32, (height * scale) as i32);
    }

    /// Stop listening to the player and take its file away; the player itself stays for
    /// the next preview.
    fn stop_media(&self) {
        let handlers = self.imp().player_handlers.take();
        let Some(player) = crate::player::current() else {
            return;
        };
        for id in handlers {
            player.disconnect(id);
        }
        player.set_file(None);
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

/// The first `most` bytes of the file at `path`.
fn head_of(path: &Path, most: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut head = vec![0u8; most];
    let read = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    head.truncate(read);
    Some(head)
}

/// Width and height of a local image from its header alone, turned the way its EXIF tag
/// says, which is the way it will be drawn.
fn image_size(path: &Path) -> Option<(i32, i32)> {
    let (format, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(path)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    let turned = format.name().as_deref() == Some("jpeg") && exif_turned(path);
    Some(if turned {
        (height, width)
    } else {
        (width, height)
    })
}

/// Whether the EXIF tag of the image at `path` turns it on its side.
fn exif_turned(path: &Path) -> bool {
    head_of(path, EXIF_SCAN).is_some_and(|head| matches!(exif_orientation(&head), 5..=8))
}

/// The EXIF orientation in `head`, 1 (upright) where there is none: the APP1 segment
/// holds a TIFF header, and the first directory of that holds the tag.
fn exif_orientation(head: &[u8]) -> u16 {
    fn read(head: &[u8], at: usize, big: bool, len: usize) -> Option<u32> {
        let bytes = head.get(at..at + len)?;
        let value = bytes.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b));
        Some(if big {
            value
        } else {
            value.swap_bytes() >> (32 - 8 * len as u32)
        })
    }
    let orientation = || {
        let tiff = head.windows(6).position(|w| w == b"Exif\0\0")? + 6;
        let big = match head.get(tiff..tiff + 2)? {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        let directory = tiff + read(head, tiff + 4, big, 4)? as usize;
        let entries = read(head, directory, big, 2)?;
        (0..entries as usize)
            .map(|n| directory + 2 + n * 12)
            .find(|&entry| read(head, entry, big, 2) == Some(0x0112))
            .and_then(|entry| read(head, entry + 8, big, 2))
            .map(|value| value as u16)
    };
    orientation().unwrap_or(1)
}

/// The size of the first page in points, read out of the file: no tool to start and no
/// page to render, so the dialog has the shape before it opens. `None` when the page
/// dictionary is compressed out of reach, which is what `pdfinfo` answers later.
fn pdf_page_size(path: &Path) -> Option<(f64, f64)> {
    media_box(&String::from_utf8_lossy(&head_of(path, PDF_SCAN)?))
}

/// The first `/MediaBox` in `text`, as a width and a height.
fn media_box(text: &str) -> Option<(f64, f64)> {
    let box_ = text.find("/MediaBox")?;
    let open = text[box_..].find('[')? + box_ + 1;
    let close = text[open..].find(']')? + open;
    let mut corners = text[open..close]
        .split_whitespace()
        .filter_map(|number| number.parse::<f64>().ok());
    let (left, bottom, right, top) = (
        corners.next()?,
        corners.next()?,
        corners.next()?,
        corners.next()?,
    );
    let (width, height) = ((right - left).abs(), (top - bottom).abs());
    (width > 1.0 && height > 1.0).then_some((width, height))
}

/// What is known about a file before its preview is shaped, gathered on the main thread
/// from the listing and read on another, since it means reading headers on disk and
/// looking for the PDF tool.
struct Probe {
    is_dir: bool,
    content_type: String,
    path: Option<PathBuf>,
    /// What names the thumbnail the cache may already hold, which has the proportions of
    /// the file. Looking for it is a read, so it waits for the worker with the rest.
    uri: String,
    mtime: u64,
}

/// The shape a file wants: at its own size, never enlarged; as proportions to fill the
/// room there is, or the video box, with; or one of the fixed shapes for content whose
/// proportions are not known until it is loaded.
enum Shape {
    Fitted(f64, f64),
    Page(f64, f64),
    Video(f64, f64),
    Fixed((i32, i32)),
}

impl Probe {
    fn of(info: &gio::FileInfo) -> Self {
        Self {
            is_dir: file_utils::is_dir(info),
            content_type: info.content_type().unwrap_or_default().to_string(),
            path: file_utils::file_of(info).path(),
            uri: file_utils::file_of(info).uri().to_string(),
            mtime: info
                .modification_date_time()
                .map(|d| d.to_unix() as u64)
                .unwrap_or(0),
        }
    }

    fn shape(&self) -> Shape {
        let content_type = self.content_type.as_str();
        if self.is_dir {
            return Shape::Fixed(INFO_SHAPE);
        }
        if content_type.starts_with("image/") {
            // Reading the header of an image is a few bytes, not a decode.
            return match self.path.as_deref().and_then(image_size) {
                Some((width, height)) => Shape::Fitted(width as f64, height as f64),
                None => Shape::Fixed(IMAGE_SHAPE),
            };
        }
        if content_type.starts_with("video/") {
            let (width, height) = self.thumbnail_size().unwrap_or((16.0, 9.0));
            return Shape::Video(width, height);
        }
        if content_type.starts_with("audio/") {
            return Shape::Fixed(SOUND_SHAPE);
        }
        if gio::content_type_is_a(content_type, "text/plain") {
            return Shape::Fixed(TEXT_SHAPE);
        }
        if content_type == "application/pdf" && can_render_pdf() {
            // The proportions come from the thumbnail, from the page dictionary, or from
            // A4, which is what most pages are; the size comes from the room there is. A
            // thumbnail is 256 pixels tall and a page dictionary is in points, so neither
            // is a size to show a page at.
            let (width, height) = self
                .thumbnail_size()
                .or_else(|| pdf_page_size(self.path.as_deref()?))
                .unwrap_or(PAGE_POINTS);
            return Shape::Page(width, height);
        }
        match self.thumbnail_size() {
            // What will be shown is the thumbnail, at its own size.
            Some((width, height)) => Shape::Fitted(width, height),
            None => Shape::Fixed(INFO_SHAPE),
        }
    }

    /// Reading the header of the thumbnail is a few bytes and no decode.
    fn thumbnail_size(&self) -> Option<(f64, f64)> {
        let png = crate::thumbnails::cached_png(&self.uri, self.mtime)?;
        let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(png)?;
        (width > 0 && height > 0).then_some((width as f64, height as f64))
    }
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

/// Text is drawn by GtkSourceView, which colours whatever language it recognises the file
/// as and leaves anything else plain. Lines are not wrapped: source is read as it is
/// written, and the preview scrolls sideways for the long ones.
fn text_view(text: &str, info: &gio::FileInfo, content_type: &str) -> gtk::Widget {
    use sourceview5::prelude::*;

    static SOURCE_INIT: std::sync::Once = std::sync::Once::new();
    SOURCE_INIT.call_once(sourceview5::init);

    let buffer = sourceview5::Buffer::new(None);
    buffer.set_language(
        sourceview5::LanguageManager::default()
            .guess_language(Some(info.display_name().as_str()), Some(content_type))
            .as_ref(),
    );
    // One of the schemes GtkSourceView ships for the GNOME palette; with none set the
    // language is recognised but nothing is coloured.
    let scheme = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    buffer.set_style_scheme(
        sourceview5::StyleSchemeManager::default()
            .scheme(scheme)
            .as_ref(),
    );
    buffer.set_text(text);
    let view = sourceview5::View::with_buffer(&buffer);
    view.add_css_class("spiral-source-view");
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    view.set_left_margin(12);
    view.set_right_margin(12);
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
        Some(path) => gio::spawn_blocking(move || {
            if image_size(&path).is_some_and(|(w, h)| w as i64 * h as i64 > IMAGE_PIXELS) {
                return None;
            }
            // GTK's own loaders leave a photograph the way the camera held it; the EXIF
            // tag that says to turn it is honoured by the pixbuf loader alone.
            if exif_turned(&path) {
                let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file(&path)
                    .ok()?
                    .apply_embedded_orientation()?;
                return Some(texture_of(&pixbuf));
            }
            gdk::Texture::from_filename(path).ok()
        })
        .await
        .ok()
        .flatten(),
        None => {
            let (data, _) = file.load_bytes_future().await.ok()?;
            gdk::Texture::from_bytes(&data).ok()
        }
    }
}

fn texture_of(pixbuf: &gtk::gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    )
    .upcast()
}

/// The picture a sound file carries, cut to a square `side` pixels across: the middle of
/// it, since a cover is nearly square and the corners are what gets rounded off.
async fn cover_texture(bytes: glib::Bytes, side: i32) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || {
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&bytes).ok()?;
        loader.close().ok()?;
        let pixbuf = loader.pixbuf()?;
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
        let (width, height) = (pixbuf.width(), pixbuf.height());
        let square = width.min(height);
        if square <= 0 {
            return None;
        }
        let middle =
            pixbuf.new_subpixbuf((width - square) / 2, (height - square) / 2, square, square);
        let scaled = middle.scale_simple(side, side, gtk::gdk_pixbuf::InterpType::Bilinear)?;
        Some(texture_of(&scaled))
    })
    .await
    .ok()
    .flatten()
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

/// How many pages the document has and how large one is, in points.
async fn pdf_info(path: PathBuf) -> Option<(u32, (f64, f64))> {
    gio::spawn_blocking(move || pdf_facts(&path))
        .await
        .ok()
        .flatten()
}

fn pdf_facts(path: &Path) -> Option<(u32, (f64, f64))> {
    run_tool(
        "pdfinfo",
        path,
        |input, _| vec![input.as_os_str().to_owned()],
        |out, _| {
            let text = String::from_utf8_lossy(out);
            let pages: u32 = text
                .lines()
                .find_map(|line| line.strip_prefix("Pages:"))
                .and_then(|count| count.trim().parse().ok())?;
            // "Page size:       595.2 x 841.92 pts (A4)"
            let (width, height) = text
                .lines()
                .find_map(|line| line.strip_prefix("Page size:"))
                .and_then(|size| size.trim().split_once(" x "))
                .and_then(|(width, height)| {
                    let height = height.split_whitespace().next()?;
                    Some((
                        width.trim().parse::<f64>().ok()?,
                        height.parse::<f64>().ok()?,
                    ))
                })
                .unwrap_or(PAGE_POINTS);
            // "Page rot:        90": the page is drawn turned, so it is shown turned.
            let turned = text
                .lines()
                .find_map(|line| line.strip_prefix("Page rot:"))
                .and_then(|rot| rot.trim().parse::<i32>().ok())
                .is_some_and(|rot| rot.rem_euclid(180) == 90);
            let size = if turned {
                (height, width)
            } else {
                (width, height)
            };
            Some((pages, size))
        },
    )
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

/// Run `program` over `input` the way a thumbnailer runs: inside bubblewrap, with the file
/// bound read-only and one private directory to write into. `args` is handed the paths as
/// the child sees them, `result` what it printed and the directory it wrote to, which is
/// removed as soon as `result` returns. Without bubblewrap the tool is not run: it is fed
/// a document from wherever the reader got it.
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
    let sandbox = match crate::thumbnails::sandbox_base(&program.to_string_lossy()) {
        Some(sandbox) => sandbox,
        None => {
            let _ = std::fs::remove_dir(&work);
            return None;
        }
    };
    let (input_seen, work_seen) = (Path::new("/tmp/in"), Path::new("/tmp/out"));
    let argv = args(input_seen, work_seen);
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
    cmd.args(&argv);
    // Bounded like a thumbnailer: a document that stops the tool would otherwise leave
    // the preview on its spinner and the worker thread on the tool, for good.
    let run = crate::thumbnails::run_bounded(&mut cmd, TOOL_TIMEOUT);
    // The seccomp memfd must stay open until the child has started.
    drop(sandbox.seccomp);
    let out = match run {
        Ok(ran) if ran.ok => result(&ran.stdout, &work),
        Ok(ran) => {
            glib::g_debug!("spiral", "preview {program:?} failed: {}", ran.trouble);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG header with one EXIF directory holding the orientation tag.
    fn jpeg_with_orientation(big: bool, orientation: u16) -> Vec<u8> {
        let word = |n: u16| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let long = |n: u32| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let mut tiff = Vec::new();
        tiff.extend(if big { b"MM" } else { b"II" });
        tiff.extend(word(42));
        tiff.extend(long(8));
        tiff.extend(word(1));
        tiff.extend(word(0x0112));
        tiff.extend(word(3));
        tiff.extend(long(1));
        tiff.extend(word(orientation));
        tiff.extend(word(0));
        tiff.extend(long(0));
        let mut head = vec![0xff, 0xd8, 0xff, 0xe1];
        head.extend(((tiff.len() + 8) as u16).to_be_bytes());
        head.extend(b"Exif\0\0");
        head.extend(tiff);
        head
    }

    #[test]
    fn exif_orientation_is_read_either_way_round() {
        assert_eq!(exif_orientation(&jpeg_with_orientation(false, 6)), 6);
        assert_eq!(exif_orientation(&jpeg_with_orientation(true, 8)), 8);
        assert_eq!(exif_orientation(&jpeg_with_orientation(true, 1)), 1);
        assert_eq!(exif_orientation(b"\xff\xd8no exif here"), 1);
        assert_eq!(exif_orientation(b"Exif\0\0MM"), 1);
    }

    #[test]
    fn media_box_is_the_size_of_the_page() {
        assert_eq!(
            media_box("<< /Type /Page /MediaBox [0 0 612 792] >>"),
            Some((612.0, 792.0))
        );
        assert_eq!(
            media_box("/MediaBox [ 0.0 0.0 595.28 841.89 ]"),
            Some((595.28, 841.89))
        );
        assert_eq!(media_box("/MediaBox [0 0 0 0]"), None);
        assert_eq!(media_box("%PDF-1.5 nothing readable"), None);
    }
}
