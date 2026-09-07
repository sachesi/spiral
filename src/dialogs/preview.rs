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
        pub content: adw::Bin,
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
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.set_title_widget(Some(&imp.title));
        let open = gtk::Button::builder()
            .label(gettext("_Open"))
            .use_underline(true)
            .css_classes(["suggested-action"])
            .build();
        open.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                if let Some(open) = dialog.imp().open.borrow().as_ref() {
                    open(());
                }
                dialog.close();
            }
        ));
        header.pack_end(&open);
        dialog.set_default_widget(Some(&open));

        imp.content.set_vexpand(true);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&imp.content));
        dialog.set_child(Some(&toolbar));
        dialog.setup_keys();
        dialog
    }

    /// Space closes the preview the way it opened it, the arrows walk the folder, and
    /// Page Up and Page Down turn the pages of a PDF. Captured, because the text view and
    /// the media controls below would otherwise keep the keys to themselves.
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
                if state.intersects(M::CONTROL_MASK | M::ALT_MASK | M::SHIFT_MASK | M::SUPER_MASK) {
                    return glib::Propagation::Proceed;
                }
                let call = |slot: &Slot<i32>, delta: i32| match slot.borrow().as_ref() {
                    Some(f) => {
                        f(delta);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                };
                let imp = dialog.imp();
                match key {
                    Key::space => {
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
        imp.title.set_title(&file_utils::display_name(info));
        imp.title.set_subtitle(&subtitle(info));
        imp.content.set_child(Some(&spinner()));
        let info = info.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                let child = dialog.build_content(&info).await;
                if dialog.imp().generation.get() == generation {
                    dialog.imp().content.set_child(Some(&child));
                }
            }
        ));
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
            return picture(&texture);
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
            Some(texture) => picture(&texture),
            None => self.info_page(info),
        }
    }

    fn video(&self, file: &gio::File) -> gtk::Widget {
        let video = gtk::Video::for_file(Some(file));
        video.set_autoplay(true);
        self.imp().media.replace(video.media_stream());
        video.upcast()
    }

    /// Sound has nothing to draw: the file's own icon over the transport controls.
    fn sound(&self, info: &gio::FileInfo, file: &gio::File) -> gtk::Widget {
        let stream = gtk::MediaFile::for_file(file);
        stream.play();
        let controls = gtk::MediaControls::builder()
            .media_stream(&stream)
            .halign(gtk::Align::Center)
            .width_request(400)
            .build();
        self.imp().media.replace(Some(stream.upcast()));
        let icon = gtk::Image::from_gicon(&file_utils::icon_of(info));
        icon.set_pixel_size(ICON_SIZE);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
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
        let page = Rc::new(Cell::new(1u32));
        let picture = gtk::Picture::builder()
            .paintable(&pdf_page(path.clone(), 1).await?)
            .hexpand(true)
            .vexpand(true)
            .build();
        let label = gtk::Label::new(Some(&page_text(1, pages)));
        let previous = gtk::Button::from_icon_name("go-previous-symbolic");
        previous.set_tooltip_text(Some(&gettext("Previous Page")));
        previous.set_sensitive(false);
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some(&gettext("Next Page")));
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

        let bar = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        bar.append(&previous);
        bar.append(&label);
        bar.append(&next);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&picture);
        column.append(&bar);
        Some(column.upcast())
    }

    /// Files nothing can draw: their icon, their name and what little is known about them.
    fn info_page(&self, info: &gio::FileInfo) -> gtk::Widget {
        let paintable = gtk::IconTheme::for_display(&self.display()).lookup_by_gicon(
            &file_utils::icon_of(info),
            ICON_SIZE,
            self.scale_factor(),
            self.direction(),
            gtk::IconLookupFlags::empty(),
        );
        adw::StatusPage::builder()
            .paintable(&paintable)
            .title(file_utils::display_name(info))
            .description(subtitle(info))
            .build()
            .upcast()
    }

    fn stop_media(&self) {
        if let Some(stream) = self.imp().media.take() {
            stream.pause();
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

fn picture(paintable: &impl IsA<gdk::Paintable>) -> gtk::Widget {
    gtk::Picture::builder()
        .paintable(paintable)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
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
    gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
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
