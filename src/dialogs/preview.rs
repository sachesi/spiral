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

mod load;
mod shape;
mod viewers;

use load::*;
use shape::*;
use viewers::*;

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
/// The area a picture smaller than this opens with, in its own proportions and enlarged to
/// fill it: a dialog the size of a stamp has no room for its name or the zoom buttons.
const FLOOR_AREA: f64 = 560.0 * 420.0;
/// Shapes for content whose proportions are not known before it is loaded: text to read,
/// an image whose header could not be read, the sound player, and the icon for everything
/// else.
const TEXT_SHAPE: (i32, i32) = (760, 514);
const IMAGE_SHAPE: (i32, i32) = (720, 494);
/// A4 upright in points, which is what most PDFs turn out to be.
const PAGE_POINTS: (f64, f64) = (595.28, 841.89);
const SOUND_SHAPE: (i32, i32) = (420, 234);
const INFO_SHAPE: (i32, i32) = (340, 214);
/// How long one page of the preview takes to fade into the next.
const CROSSFADE: Duration = Duration::from_millis(120);
/// How long a file may take to load before a spinner says it is loading.
const SPINNER_DELAY: Duration = Duration::from_millis(400);
/// How far, in pixels, a new shape may differ from the one the dialog has and be ignored.
const SHAPE_JITTER: i32 = 2;

/// How long the PDF tool may take over one page before it is killed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(20);
/// Zoom: one step of the buttons or the wheel, and how far it goes either way; in, that is
/// past the fit or the picture's own size, whichever is larger.
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
            .set_transition_duration(CROSSFADE.as_millis() as u32);
        imp.content.set_hhomogeneous(false);
        imp.content.set_vhomogeneous(false);
        imp.content.connect_transition_running_notify(|stack| {
            if !stack.is_transition_running() {
                drop_hidden(stack);
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
        let probe = Probe::of(info, &crate::thumbnails::on_disk(info).await);
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
        self.shape_filled(size.0, size.1);
    }

    /// Show `info`: the header at once, the content when it has loaded.
    pub async fn show_info(&self, info: &gio::FileInfo) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        imp.flip.take();
        imp.zoom.take();
        imp.page_size.set(None);
        imp.page_count.set(None);
        // The shape comes from headers on disk, read off the main thread, and so does the
        // thumbnail that holds the place of the content. Until they are known the page on
        // screen stays, so nothing is drawn in a shape it will not keep.
        let probe = Probe::of(info, &crate::thumbnails::on_disk(info).await);
        let (shape, placeholder) =
            gio::spawn_blocking(move || (probe.shape(), probe.placeholder()))
                .await
                .unwrap_or((Shape::Fixed(INFO_SHAPE), None));
        if imp.generation.get() != generation {
            return;
        }
        // The name, the shape and what stands in for the content change in one frame, and
        // at once: the page on its way out would otherwise fade in a shape not its own.
        self.stop_media();
        imp.title.set_title(&file_utils::display_name(info));
        imp.title.set_subtitle(&subtitle(info));
        let reshaped = self.apply_shape(shape);
        let showing_video = imp
            .content
            .visible_child()
            .is_some_and(|page| page.is::<gtk::Video>());
        match placeholder {
            Some(texture) => self.show_child(&picture(&texture), false),
            // A stopped video is a black box.
            None if reshaped || showing_video => self.show_child(&spinner(SPINNER_DELAY), false),
            // Same shape and nothing to stand in: the page on screen stays until the next
            // one fades in over it, rather than blinking out. It is not this file's, though,
            // so a file slow to load has it give way to the spinner.
            None => {
                let shown = imp.content.visible_child();
                glib::timeout_add_local_once(
                    SPINNER_DELAY,
                    glib::clone!(
                        #[weak(rename_to = dialog)]
                        self,
                        move || {
                            let imp = dialog.imp();
                            if imp.generation.get() == generation
                                && imp.content.visible_child() == shown
                            {
                                dialog.show_child(&spinner(Duration::ZERO), false);
                            }
                        }
                    ),
                );
            }
        }
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
                if let Some(child) = child
                    && dialog.imp().generation.get() == generation
                {
                    dialog.show_child(&child, true);
                }
            }
        ));
    }

    /// Put `child` on screen, fading it in over whatever is there when `fade` is set. The
    /// page it replaces is dropped when the fade is over, not while it is still being drawn.
    fn show_child(&self, child: &impl IsA<gtk::Widget>, fade: bool) {
        let stack = &self.imp().content;
        // The video on its way out holds the one player, and a video with autoplay pauses
        // its stream when it goes. It goes after the fade, by which time the next file is
        // playing, so the page being replaced gives its autoplay up first.
        let mut old = stack.first_child();
        while let Some(widget) = old {
            old = widget.next_sibling();
            if let Some(video) = widget.downcast_ref::<gtk::Video>() {
                video.set_autoplay(false);
            }
        }
        stack.add_child(child);
        stack.set_transition_type(if fade {
            gtk::StackTransitionType::Crossfade
        } else {
            gtk::StackTransitionType::None
        });
        stack.set_visible_child(child);
        // No fade, or none while the dialog is not on screen yet: nothing waits for the
        // pages replaced.
        if !stack.is_transition_running() {
            drop_hidden(stack);
        }
    }

    /// The page for `info`, or `None` for a video, which puts itself on screen once it has
    /// a frame to show.
    async fn build_content(&self, info: &gio::FileInfo) -> Option<gtk::Widget> {
        // An item in the trash is read from the file on the disk it is.
        let file = crate::thumbnails::on_disk(info).await;
        let content_type = crate::file_utils::content_type_of(info)
            .unwrap_or_default()
            .to_string();
        if file_utils::is_dir(info) {
            return Some(self.info_page(info));
        }
        if content_type.starts_with("image/")
            && crate::file_utils::size_of(info) <= IMAGE_LIMIT as u64
            && let Some(texture) = load_texture(&file).await
        {
            self.shape_to(&texture);
            return Some(self.zoomable(&picture(&texture), &[]));
        }
        if content_type.starts_with("video/") {
            return self.video(info, &file);
        }
        if content_type.starts_with("audio/") {
            // The cover takes the place of the icon and its size, so waiting for one to be
            // made costs nothing but the wait: the player is the same shape either way.
            return Some(self.sound(info, &file));
        }
        if gio::content_type_is_a(&content_type, "text/plain")
            && let Some(text) = load_text(&file).await
        {
            return Some(text_view(&text, info, &content_type));
        }
        if content_type == "application/pdf"
            && let Some(path) = file.path()
            && let Some(widget) = self.pdf(path).await
        {
            return Some(widget);
        }
        // The file being looked at goes before any row waiting behind it.
        Some(match crate::thumbnails::load(info, 0).await {
            Some(texture) => {
                self.shape_to(&texture);
                picture(&texture).upcast()
            }
            None => {
                // The guessed shape was for content that turned out not to be drawable.
                self.shape(INFO_SHAPE.0, INFO_SHAPE.1);
                self.info_page(info)
            }
        })
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

/// A spinner that shows itself only once the wait has gone on for `delay`: one flashed up
/// for a moment by every file that loads at once is a blink.
fn spinner(delay: Duration) -> gtk::Widget {
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();
    if !delay.is_zero() {
        spinner.set_opacity(0.0);
        glib::timeout_add_local_once(
            delay,
            glib::clone!(
                #[weak]
                spinner,
                move || spinner.set_opacity(1.0)
            ),
        );
    }
    spinner.upcast()
}

/// Drop every page of `stack` but the one on screen.
fn drop_hidden(stack: &gtk::Stack) {
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
}
