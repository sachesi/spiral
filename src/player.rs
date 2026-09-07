//! One GStreamer pipeline for the preview, kept for the life of the process and pointed
//! at one file after another.
//!
//! GTK's own media backend builds a pipeline per file on a thread of its own and tears it
//! down again, and both ends of that trip races in GStreamer that take the process with
//! them; its video sink also leaks a GL context per file. Here the pipeline is built once,
//! the file it plays is switched only when it is not in the middle of starting, and the
//! sink is the GTK 4 paintable sink from gst-plugins-rs, so the pipeline draws straight
//! into a `gdk::Paintable` that `gtk::Video` can show. The player is a `gtk::MediaStream`,
//! so `gtk::MediaControls` and `gtk::Video` drive it like any other.

use std::cell::{Cell, OnceCell, RefCell};
use std::time::Duration;

use gst::prelude::*;

use crate::gtk::prelude::*;
use crate::gtk::subclass::prelude::*;
use crate::{gdk, gio, glib, gst, gtk};

/// How often the position is reported while something plays.
const TICK: Duration = Duration::from_millis(100);
/// How long a file may take to start before it is given up on: a pipeline that is still
/// starting is not switched away from, since that is the race being avoided, so a file
/// that never starts would otherwise hold the player for good.
const START_LIMIT: Duration = Duration::from_secs(5);

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

/// The one player, made on first use. `None` where GStreamer or the paintable sink is
/// missing, in which case there is no playback to offer. A `gtk::MediaStream` that has
/// failed once stays failed, so a player that has is replaced.
pub fn player() -> Option<Player> {
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|player| player.error().is_some()) {
            slot.take().expect("checked").set_file(None);
        }
        if slot.is_none() {
            *slot = Player::new();
        }
        slot.clone()
    })
}

/// The player, if one has been made.
pub fn current() -> Option<Player> {
    PLAYER.with(|slot| slot.borrow().clone())
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Player {
        pub playbin: OnceCell<gst::Element>,
        pub paintable: OnceCell<gdk::Paintable>,
        pub watch: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub tick: RefCell<Option<glib::SourceId>>,
        pub limit: RefCell<Option<glib::SourceId>>,
        pub file: RefCell<Option<gio::File>>,
        /// The file asked for while the pipeline was still starting on the one before.
        pub pending: RefCell<Option<Option<gio::File>>>,
        /// Between the file being set and the pipeline reporting itself ready or failed.
        pub starting: Cell<bool>,
        pub seeking: Cell<bool>,
        pub has_audio: Cell<bool>,
        pub has_video: Cell<bool>,
        /// The picture embedded in the file, as it came: the preview decides its size.
        pub cover: RefCell<Option<glib::Bytes>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Player {
        const NAME: &'static str = "SpiralPlayer";
        type Type = super::Player;
        type ParentType = gtk::MediaStream;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Player {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("cover").build()])
        }
    }

    impl MediaStreamImpl for Player {
        fn play(&self) -> bool {
            if self.file.borrow().is_none() {
                return false;
            }
            self.playbin().set_state(gst::State::Playing).is_ok()
        }

        fn pause(&self) {
            if self.file.borrow().is_some() {
                let _ = self.playbin().set_state(gst::State::Paused);
            }
        }

        fn seek(&self, timestamp: i64) {
            let position = gst::ClockTime::from_useconds(timestamp.max(0) as u64);
            self.seeking.set(true);
            let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT;
            if self.playbin().seek_simple(flags, position).is_err() {
                self.seeking.set(false);
                self.obj().seek_failed();
            }
        }

        fn update_audio(&self, muted: bool, volume: f64) {
            self.playbin().set_property("mute", muted);
            self.playbin().set_property("volume", volume);
        }

        // The sink draws through its paintable wherever that is shown; there is nothing to
        // tie to a surface.
        fn realize(&self, _surface: gdk::Surface) {}
        fn unrealize(&self, _surface: gdk::Surface) {}
    }

    impl PaintableImpl for Player {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            self.paintable().snapshot(snapshot, width, height);
        }
        fn current_image(&self) -> gdk::Paintable {
            self.paintable().current_image()
        }
        fn intrinsic_width(&self) -> i32 {
            self.paintable().intrinsic_width()
        }
        fn intrinsic_height(&self) -> i32 {
            self.paintable().intrinsic_height()
        }
        fn intrinsic_aspect_ratio(&self) -> f64 {
            self.paintable().intrinsic_aspect_ratio()
        }
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::empty()
        }
    }

    impl Player {
        pub fn playbin(&self) -> &gst::Element {
            self.playbin.get().expect("player built")
        }

        pub fn paintable(&self) -> &gdk::Paintable {
            self.paintable.get().expect("player built")
        }
    }
}

glib::wrapper! {
    pub struct Player(ObjectSubclass<imp::Player>)
        @extends gtk::MediaStream,
        @implements gdk::Paintable;
}

impl Player {
    fn new() -> Option<Self> {
        if let Err(e) = gst::init() {
            glib::g_warning!("spiral", "GStreamer will not start, no media preview: {e}");
            return None;
        }
        let Ok(playbin) = gst::ElementFactory::make("playbin3").build() else {
            glib::g_warning!("spiral", "no playbin3, no media preview");
            return None;
        };
        let Ok(sink) = gst::ElementFactory::make("gtk4paintablesink").build() else {
            glib::g_warning!(
                "spiral",
                "no gtk4paintablesink (gst-plugins-rs), no media preview"
            );
            return None;
        };
        playbin.set_property("video-sink", &sink);
        let paintable = sink.property::<gdk::Paintable>("paintable");

        let player: Self = glib::Object::new();
        let imp = player.imp();
        imp.playbin.set(playbin.clone()).ok();
        imp.paintable.set(paintable.clone()).ok();
        paintable.connect_invalidate_contents(glib::clone!(
            #[weak]
            player,
            move |_| player.invalidate_contents()
        ));
        paintable.connect_invalidate_size(glib::clone!(
            #[weak]
            player,
            move |_| player.invalidate_size()
        ));
        let watch = playbin.bus()?.add_watch_local(glib::clone!(
            #[weak]
            player,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |_, message| {
                player.on_message(message);
                glib::ControlFlow::Continue
            }
        ));
        imp.watch.replace(watch.ok());
        Some(player)
    }

    /// The picture embedded in the file being played, once a `cover` signal has said
    /// there is one.
    pub fn cover(&self) -> Option<glib::Bytes> {
        self.imp().cover.borrow().clone()
    }

    pub fn connect_cover(&self, f: impl Fn(&Self) + 'static) -> glib::SignalHandlerId {
        self.connect_local("cover", false, move |values| {
            f(&values[0].get::<Self>().expect("player"));
            None
        })
    }

    /// Point the pipeline at `file`, or at nothing. A pipeline still starting on the last
    /// file is left to finish first; the change is made when it has.
    pub fn set_file(&self, file: Option<gio::File>) {
        let imp = self.imp();
        if imp.starting.get() {
            imp.pending.replace(Some(file));
            return;
        }
        self.switch(file);
    }

    fn switch(&self, file: Option<gio::File>) {
        let imp = self.imp();
        imp.pending.take();
        if let Some(tick) = imp.tick.take() {
            tick.remove();
        }
        if let Some(limit) = imp.limit.take() {
            limit.remove();
        }
        if self.is_prepared() {
            self.stream_unprepared();
        }
        let _ = imp.playbin().set_state(gst::State::Null);
        imp.seeking.set(false);
        imp.has_audio.set(false);
        imp.has_video.set(false);
        imp.cover.take();
        imp.file.replace(file.clone());
        let Some(file) = file else { return };
        imp.playbin().set_property("uri", file.uri());
        imp.starting.set(true);
        let _ = imp.playbin().set_state(gst::State::Paused);
        imp.limit.replace(Some(glib::timeout_add_local_once(
            START_LIMIT,
            glib::clone!(
                #[weak(rename_to = player)]
                self,
                move || player.give_up()
            ),
        )));
    }

    /// The file has taken too long to start: it is failed, and the next one gets its turn.
    fn give_up(&self) {
        let imp = self.imp();
        imp.limit.take();
        if !imp.starting.get() {
            return;
        }
        imp.starting.set(false);
        self.set_error(glib::Error::new(
            gio::IOErrorEnum::TimedOut,
            "the file did not start playing",
        ));
        self.settle();
    }

    /// Starting is over, one way or the other: make the switch that waited on it.
    fn settle(&self) {
        if let Some(file) = self.imp().pending.take() {
            self.switch(file);
        }
    }

    fn on_message(&self, message: &gst::Message) {
        use gst::MessageView;
        let imp = self.imp();
        match message.view() {
            MessageView::Error(e) => {
                let starting = imp.starting.replace(false);
                imp.seeking.set(false);
                if let Some(limit) = imp.limit.take() {
                    limit.remove();
                }
                self.set_error(e.error());
                if starting {
                    self.settle();
                }
            }
            MessageView::AsyncDone(_) => {
                if imp.seeking.replace(false) {
                    self.seek_success();
                }
                if imp.starting.replace(false) {
                    if let Some(limit) = imp.limit.take() {
                        limit.remove();
                    }
                    self.prepare();
                    self.settle();
                }
            }
            MessageView::Eos(_) => self.stream_ended(),
            MessageView::StreamCollection(c) => {
                for stream in c.stream_collection().iter() {
                    let kind = stream.stream_type();
                    if kind.contains(gst::StreamType::AUDIO) {
                        imp.has_audio.set(true);
                    }
                    if kind.contains(gst::StreamType::VIDEO) {
                        imp.has_video.set(true);
                    }
                }
            }
            MessageView::Tag(t) => {
                if imp.cover.borrow().is_some() {
                    return;
                }
                let tags = t.tags();
                let image = tags
                    .get::<gst::tags::Image>()
                    .or_else(|| tags.get::<gst::tags::PreviewImage>());
                let Some(image) = image else { return };
                let Some(buffer) = image.get().buffer_owned() else {
                    return;
                };
                let Ok(map) = buffer.map_readable() else {
                    return;
                };
                imp.cover.replace(Some(glib::Bytes::from(map.as_slice())));
                self.emit_by_name::<()>("cover", &[]);
            }
            _ => {}
        }
    }

    /// The pipeline has prerolled: tell the stream what it is playing and start reporting
    /// where it is.
    fn prepare(&self) {
        let imp = self.imp();
        let playbin = imp.playbin();
        let duration = playbin
            .query_duration::<gst::ClockTime>()
            .map(|d| d.useconds() as i64)
            .unwrap_or(0);
        let mut seeking = gst::query::Seeking::new(gst::Format::Time);
        let seekable = playbin.query(&mut seeking) && seeking.result().0;
        self.stream_prepared(imp.has_audio.get(), imp.has_video.get(), seekable, duration);
        self.invalidate_size();
        imp.tick.replace(Some(glib::timeout_add_local(
            TICK,
            glib::clone!(
                #[weak(rename_to = player)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    if let Some(position) =
                        player.imp().playbin().query_position::<gst::ClockTime>()
                    {
                        player.update(position.useconds() as i64);
                    }
                    glib::ControlFlow::Continue
                }
            ),
        )));
    }
}
