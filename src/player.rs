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
//!
//! The file itself is read and decoded by `spiral-thumbnailer --play` in the sandbox, one
//! helper per file (see [`crate::media`]). The pipeline here starts at two `appsrc`s that
//! take the plain pictures and sound it hands over, and does no more than queue them, set
//! the volume and draw.

use std::cell::{Cell, OnceCell, RefCell};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use gstreamer_allocators as gst_allocators;
use gstreamer_allocators::prelude::*;
use gstreamer_video as gst_video;

use crate::gtk::prelude::*;
use crate::gtk::subclass::prelude::*;
use crate::media::{self, Note, Packet, Picture};
use crate::{gdk, gio, glib, gst, gtk};

/// How often the position is reported while something plays.
const TICK: Duration = Duration::from_millis(100);
/// How long a file may take to start before it is given up on: a pipeline that is still
/// starting is not switched away from, since that is the race being avoided, so a file
/// that never starts would otherwise hold the player for good.
const START_LIMIT: Duration = Duration::from_secs(5);
/// Notes from a helper waiting for the main loop. A helper sending faster than they are
/// taken waits, rather than piling them up in this process.
const NOTES_QUEUED: usize = 8;
/// Pictures in GPU memory the queue in front of the sink holds. Counted by the picture:
/// one says little of its size, and each keeps descriptors open and a surface of the
/// decoder's. Plain rows are held back by their bytes, and keep GStreamer's default.
const FRAMES_QUEUED: u32 = 4;
const ROWS_QUEUED: u32 = 200;

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
    /// Set when the pipeline could not be built at all, so that a missing plugin is
    /// looked for once and complained about once rather than at every media file.
    static UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
}

/// The one player, made on first use. `None` where GStreamer or the paintable sink is
/// missing, in which case there is no playback to offer. A `gtk::MediaStream` that has
/// failed once stays failed, so a player that has is replaced.
pub fn player() -> Option<Player> {
    if UNAVAILABLE.get() {
        return None;
    }
    PLAYER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|player| player.error().is_some()) {
            slot.take().expect("checked").set_file(None);
        }
        if slot.is_none() {
            *slot = Player::new();
            UNAVAILABLE.set(slot.is_none());
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
        pub pipeline: OnceCell<gst::Pipeline>,
        /// Where the pictures and the sound come in.
        pub sources: OnceCell<[gst::Element; 2]>,
        pub video_queue: OnceCell<gst::Element>,
        pub volume: OnceCell<gst::Element>,
        pub paintable: OnceCell<gdk::Paintable>,
        /// The helper decoding the file.
        pub(super) session: RefCell<Option<super::Session>>,
        pub sessions: Cell<u32>,
        /// Whether the pipeline has been set up for the file: until the helper says what
        /// the file holds, playing or pausing is only remembered, for `ready` to act on.
        pub configured: Cell<bool>,
        /// What packets are taken: the session in the upper half, the seek in the lower.
        pub current: Arc<AtomicU64>,
        pub(super) notes: OnceCell<async_channel::Sender<(u32, Option<Note>)>>,
        /// The DMA-BUF formats the sink takes, as the helper is told them.
        pub drm_formats: OnceCell<Vec<String>>,
        pub watch: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub tick: RefCell<Option<glib::SourceId>>,
        pub limit: RefCell<Option<glib::SourceId>>,
        pub file: RefCell<Option<gio::File>>,
        /// The file asked for while the pipeline was still starting on the one before.
        pub pending: RefCell<Option<Option<gio::File>>>,
        /// Between the file being set and the pipeline reporting itself ready or failed.
        pub starting: Cell<bool>,
        /// Set while the stream is being prepared again with a length it has just learnt,
        /// so that the pause that comes with that is not passed on to the pipeline.
        pub redating: Cell<bool>,
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
            !self.configured.get() || self.pipeline().set_state(gst::State::Playing).is_ok()
        }

        fn pause(&self) {
            if self.redating.get() || !self.configured.get() {
                return;
            }
            if self.file.borrow().is_some() {
                let _ = self.pipeline().set_state(gst::State::Paused);
            }
        }

        fn seek(&self, timestamp: i64) {
            let obj = self.obj();
            let position = gst::ClockTime::from_useconds(timestamp.max(0) as u64);
            // The helper seeks first, and what it sends from then on is marked so that
            // what it had sent before can be told apart and dropped.
            if !obj.is_seekable() || !obj.seek_helper(position) {
                obj.seek_failed();
                return;
            }
            self.seeking.set(true);
            let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
            if self.pipeline().seek_simple(flags, position).is_err() {
                self.seeking.set(false);
                obj.seek_failed();
            }
        }

        fn update_audio(&self, muted: bool, volume: f64) {
            let element = self.volume.get().expect("player built");
            element.set_property("mute", muted);
            element.set_property("volume", volume);
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
        pub fn pipeline(&self) -> &gst::Pipeline {
            self.pipeline.get().expect("player built")
        }

        pub fn sources(&self) -> &[gst::Element; 2] {
            self.sources.get().expect("player built")
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

/// What a reader of the helper's packets takes.
#[derive(Clone, Copy)]
enum Take {
    /// Samples of at most `max` bytes, of exactly `exact` where given.
    Samples { max: usize, exact: Option<usize> },
    /// Pictures in GPU memory, of this size.
    Frames { width: u32, height: u32 },
}

/// What a reader is let through with, or `None` for a stream the file does not have.
type Gate = Option<Take>;

/// The helper decoding one file, and this side's ends of its sockets.
struct Session {
    id: u32,
    child: gio::Subprocess,
    control: UnixStream,
    epoch: u32,
    /// Opened for each reader once the pipeline can take what it reads.
    gates: [mpsc::Sender<Gate>; 2],
}

impl Drop for Session {
    fn drop(&mut self) {
        self.child.force_exit();
    }
}

impl Player {
    fn new() -> Option<Self> {
        if let Err(e) = gst::init() {
            glib::g_warning!("spiral", "GStreamer will not start, no media preview: {e}");
            return None;
        }
        let make = |name: &str| gst::ElementFactory::make(name).build().ok();
        let Ok(sink) = gst::ElementFactory::make("gtk4paintablesink").build() else {
            glib::g_warning!(
                "spiral",
                "no gtk4paintablesink (gst-plugins-rs), no media preview"
            );
            return None;
        };
        // The sink takes both what the helper sends, BGRA rows and DMA-BUFs, as they are.
        let video: Option<Vec<gst::Element>> = ["appsrc", "queue"].map(make).into_iter().collect();
        let audio: Option<Vec<gst::Element>> = [
            "appsrc",
            "queue",
            "volume",
            "audioconvert",
            "audioresample",
            "autoaudiosink",
        ]
        .map(make)
        .into_iter()
        .collect();
        let (Some(mut video), Some(audio)) = (video, audio) else {
            glib::g_warning!(
                "spiral",
                "GStreamer's base plugins are missing, no media preview"
            );
            return None;
        };
        video.push(sink.clone());
        let pipeline = gst::Pipeline::new();
        pipeline.add_many(video.iter().chain(&audio)).ok()?;
        gst::Element::link_many(&video).ok()?;
        gst::Element::link_many(&audio).ok()?;
        let sources = [video[0].clone(), audio[0].clone()];
        for source in &sources {
            source.set_property_from_str("format", "time");
            source.set_property("block", true);
            // A seek is passed to the helper before the pipeline is told of it.
            source.connect("seek-data", false, |_| Some(true.to_value()));
        }
        sources[1].set_property(
            "caps",
            gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("layout", "interleaved")
                .field("rate", media::AUDIO_RATE)
                .field("channels", media::AUDIO_CHANNELS)
                .build(),
        );
        let paintable = sink.property::<gdk::Paintable>("paintable");

        let player: Self = glib::Object::new();
        let imp = player.imp();
        imp.pipeline.set(pipeline.clone()).ok();
        imp.sources.set(sources).ok();
        imp.video_queue.set(video[1].clone()).ok();
        imp.volume.set(audio[2].clone()).ok();
        imp.paintable.set(paintable.clone()).ok();
        imp.drm_formats.set(dmabuf_formats()).ok();
        let (tx, rx) = async_channel::bounded(NOTES_QUEUED);
        imp.notes.set(tx).ok();
        // Weak between notes: a player replaced after failing is let go, and its helper
        // with it, which is what closes the channel.
        let weak = player.downgrade();
        glib::spawn_future_local(async move {
            while let Ok((id, note)) = rx.recv().await {
                let Some(player) = weak.upgrade() else { break };
                player.on_note(id, note);
            }
        });
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
        let watch = pipeline.bus()?.add_watch_local(glib::clone!(
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
        let _ = imp.pipeline().set_state(gst::State::Null);
        imp.configured.set(false);
        // The helper of the file before goes, and whatever it sent with it.
        imp.session.take();
        imp.seeking.set(false);
        imp.has_audio.set(false);
        imp.has_video.set(false);
        imp.cover.take();
        imp.file.replace(file.clone());
        let Some(file) = file else { return };
        let id = imp.sessions.get().wrapping_add(1);
        imp.sessions.set(id);
        imp.current.store(u64::from(id) << 32, Ordering::SeqCst);
        imp.starting.set(true);
        // The command line is worked out on a worker: building the seccomp filter and
        // asking where a file on a share lives both take time the window should not wait.
        let formats = imp
            .drm_formats
            .get()
            .map(|f| f.join(","))
            .unwrap_or_default();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = player)]
            self,
            async move {
                let prepared = gio::spawn_blocking(move || prepare(file, formats))
                    .await
                    .unwrap_or_else(|_| Err("the decoder could not be prepared".into()));
                player.prepared(id, prepared);
            }
        ));
        // The pipeline starts once the helper has said what the file holds.
        imp.limit.replace(Some(glib::timeout_add_local_once(
            START_LIMIT,
            glib::clone!(
                #[weak(rename_to = player)]
                self,
                move || player.give_up()
            ),
        )));
    }

    /// The helper for session `id` is ready to start. Unless the file has been given up on
    /// or switched away from meanwhile, start it, or fail the file where it cannot be.
    fn prepared(&self, id: u32, prepared: Result<Prepared, String>) {
        let imp = self.imp();
        if !imp.starting.get() || imp.sessions.get() != id {
            return;
        }
        match prepared.and_then(|prepared| self.launch(id, prepared)) {
            Ok(session) => {
                imp.session.replace(Some(session));
            }
            Err(e) => {
                if let Some(limit) = imp.limit.take() {
                    limit.remove();
                }
                imp.starting.set(false);
                self.fail(true, glib::Error::new(gio::IOErrorEnum::Failed, &e));
            }
        }
    }

    /// Start the helper, with a thread reading each of its sockets. Here, on the main
    /// loop, so that its notes cannot arrive before the session they belong to is in place.
    fn launch(&self, id: u32, prepared: Prepared) -> Result<Session, String> {
        let imp = self.imp();
        let Prepared {
            argv,
            inherited,
            control,
            video,
            audio,
            remote,
        } = prepared;
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
        for fd in &inherited {
            launcher.take_fd(fd.try_clone().map_err(|e| e.to_string())?, fd);
        }
        // What the helper says goes to the log, not to the terminal as it is: once taken
        // over by a file, it could say anything there.
        let (stderr, stderr_end) = std::io::pipe().map_err(|e| e.to_string())?;
        launcher.take_stderr_fd(Some(OwnedFd::from(stderr_end)));
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
        let child = launcher.spawn(&argv).map_err(|e| e.message().to_string())?;
        drop((inherited, launcher));
        std::thread::spawn(move || log_helper(stderr));
        if let Some((data, file)) = remote {
            std::thread::spawn(move || serve_bytes(file, data));
        }

        let notes = imp.notes.get().expect("player built").clone();
        let mut control_in = control.try_clone().map_err(|e| e.to_string())?;
        std::thread::spawn({
            let notes = notes.clone();
            move || {
                while let Ok(note) = media::read_note(&mut control_in) {
                    if notes.send_blocking((id, Some(note))).is_err() {
                        return;
                    }
                }
                let _ = notes.send_blocking((id, None));
            }
        });
        // Pictures in GPU memory go back to the helper on the socket they came on.
        let returns = Arc::new(video.try_clone().map_err(|e| e.to_string())?);
        let gates = [(video, 0), (audio, 1)].map(|(stream, kind)| {
            let (gate, opened) = mpsc::channel();
            let reader = Reader {
                source: imp.sources()[kind].clone(),
                current: imp.current.clone(),
                id,
                notes: notes.clone(),
                returns: returns.clone(),
            };
            std::thread::spawn(move || reader.run(media::FdStream::new(stream), opened));
            gate
        });
        Ok(Session {
            id,
            child,
            control,
            epoch: 0,
            gates,
        })
    }

    /// Tell the helper to seek to `position`. What it sends from then on carries a new
    /// epoch, and only that is taken.
    fn seek_helper(&self, position: gst::ClockTime) -> bool {
        let imp = self.imp();
        let mut session = imp.session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return false;
        };
        session.epoch = session.epoch.wrapping_add(1);
        imp.current.store(
            u64::from(session.id) << 32 | u64::from(session.epoch),
            Ordering::SeqCst,
        );
        media::write_seek(&session.control, session.epoch, position.nseconds())
    }

    /// What the helper of session `id` has to say; `None` when it has stopped or said
    /// something that does not parse.
    fn on_note(&self, id: u32, note: Option<Note>) {
        let imp = self.imp();
        if imp.session.borrow().as_ref().is_none_or(|s| s.id != id) || self.error().is_some() {
            return;
        }
        match note {
            Some(Note::Ready {
                audio,
                video,
                drm,
                colorimetry,
                seekable,
                duration,
            }) => self.ready(audio, video, drm, colorimetry, seekable, duration),
            Some(Note::Duration(duration)) => {
                self.set_length(duration);
                self.redate();
            }
            Some(Note::Cover(bytes)) => {
                if imp.cover.borrow().is_none() {
                    imp.cover.replace(Some(glib::Bytes::from_owned(bytes)));
                    self.emit_by_name::<()>("cover", &[]);
                }
            }
            Some(Note::NoVideoDecoder) => self.no_decoder(),
            Some(Note::Decoder(name)) => glib::g_debug!("spiral", "player: decoding with {name}"),
            Some(Note::Error(text)) => {
                self.failed(glib::Error::new(gst::StreamError::Decode, &text))
            }
            None => self.failed(glib::Error::new(
                gst::StreamError::Decode,
                "the decoder stopped",
            )),
        }
    }

    /// The helper has started on the file: set the pipeline up for what it holds, start it,
    /// and let the readers through.
    fn ready(
        &self,
        audio: bool,
        video: Option<Picture>,
        drm: Option<u32>,
        colorimetry: Option<String>,
        seekable: bool,
        duration: Option<u64>,
    ) {
        let imp = self.imp();
        if !imp.starting.get() {
            return;
        }
        imp.has_audio.set(audio);
        imp.has_video.set(video.is_some());
        let [video_src, audio_src] = imp.sources();
        let kind = if seekable { "seekable" } else { "stream" };
        for source in imp.sources() {
            source.set_property_from_str("stream-type", kind);
        }
        self.set_length(duration);
        // Pictures in GPU memory come in the one of the sink's formats the helper names.
        let format = drm
            .and_then(|i| imp.drm_formats.get()?.get(i as usize))
            .cloned();
        if let Some(picture) = video {
            video_src.set_property(
                "caps",
                picture.caps(format.as_deref(), colorimetry.as_deref()),
            );
            // Two pictures waiting, and the queue behind holds what it holds. One in GPU
            // memory counts few bytes and holds a surface of the decoder's, so it is
            // counted by the picture.
            let (bytes, buffers) = match format {
                Some(_) => (0, 2),
                None => (picture.len() as u64 * 2, 0),
            };
            video_src.set_property("max-bytes", bytes);
            video_src.set_property("max-buffers", buffers as u64);
            let queued = if format.is_some() {
                FRAMES_QUEUED
            } else {
                ROWS_QUEUED
            };
            imp.video_queue
                .get()
                .expect("player built")
                .set_property("max-size-buffers", queued);
        }
        imp.configured.set(true);
        let state = if self.is_playing() {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let _ = imp.pipeline().set_state(state);
        if let Some(session) = imp.session.borrow().as_ref() {
            let _ = session.gates[0].send(video.map(|p| match format {
                Some(_) => Take::Frames {
                    width: p.width,
                    height: p.height,
                },
                None => Take::Samples {
                    max: p.len(),
                    exact: Some(p.len()),
                },
            }));
            let _ = session.gates[1].send(audio.then_some(Take::Samples {
                max: media::AUDIO_PACKET_MAX,
                exact: None,
            }));
        }
        // A stream the file does not have is over before it starts, so the pipeline does
        // not wait on it.
        if video.is_none() {
            let _ = video_src.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        }
        if !audio {
            let _ = audio_src.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        }
    }

    /// How long the file is, for the pipeline to answer with.
    fn set_length(&self, duration: Option<u64>) {
        for source in self.imp().sources() {
            source.set_property("duration", duration.unwrap_or(u64::MAX));
        }
    }

    /// The file failed after it was handed to the pipeline, or its helper did.
    fn failed(&self, error: glib::Error) {
        glib::g_debug!("spiral", "player: failed: {}", error.message());
        let imp = self.imp();
        let starting = imp.starting.replace(false);
        imp.seeking.set(false);
        if let Some(limit) = imp.limit.take() {
            limit.remove();
        }
        imp.session.take();
        self.fail(starting, error);
    }

    /// The file has taken too long to start: it is failed, and the next one gets its turn.
    fn give_up(&self) {
        let imp = self.imp();
        imp.limit.take();
        if !imp.starting.get() {
            return;
        }
        imp.starting.set(false);
        imp.session.take();
        self.fail(
            true,
            glib::Error::new(gio::IOErrorEnum::TimedOut, "the file did not start playing"),
        );
    }

    /// Nothing can decode the picture. playbin3 does not call that an error: the file
    /// goes on playing without the stream, which leaves a video sitting on a black frame
    /// with its sound running. Stop there and fail, so the preview shows the file instead.
    fn no_decoder(&self) {
        let imp = self.imp();
        if let Some(limit) = imp.limit.take() {
            limit.remove();
        }
        let starting = imp.starting.replace(false);
        let _ = imp.pipeline().set_state(gst::State::Null);
        imp.session.take();
        self.fail(
            starting,
            glib::Error::new(
                gst::CoreError::MissingPlugin,
                "no decoder for the picture in this file",
            ),
        );
    }

    /// The file failed, while it was `starting` or later. One already switched away from
    /// fails unannounced: whoever listens is waiting for the next one and would take the
    /// failure for that one's, and a stream that has failed stays failed.
    fn fail(&self, starting: bool, error: glib::Error) {
        if starting && self.imp().pending.borrow().is_some() {
            glib::g_debug!(
                "spiral",
                "player: a file switched away from failed: {error}"
            );
        } else {
            self.set_error(error);
        }
        if starting {
            self.settle();
        }
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
            MessageView::Error(e) => self.failed(e.error()),
            MessageView::AsyncDone(_) => {
                if imp.seeking.replace(false) {
                    self.seek_success();
                }
                if imp.starting.replace(false) {
                    if let Some(limit) = imp.limit.take() {
                        limit.remove();
                    }
                    // A file already switched away from is not announced: whoever listens
                    // is waiting for the next one, and would take its size for that one's.
                    if imp.pending.borrow().is_none() {
                        self.prepare();
                    }
                    self.settle();
                }
            }
            MessageView::Eos(_) => self.stream_ended(),
            _ => {}
        }
    }

    /// The pipeline has prerolled: tell the stream what it is playing and start reporting
    /// where it is.
    fn prepare(&self) {
        let imp = self.imp();
        let (seekable, duration) = self.facts();
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
                        player.imp().pipeline().query_position::<gst::ClockTime>()
                    {
                        player.update(position.useconds() as i64);
                    }
                    glib::ControlFlow::Continue
                }
            ),
        )));
    }

    /// Whether the file can be seeked in and how long it is, as the pipeline has it now.
    fn facts(&self) -> (bool, i64) {
        let pipeline = self.imp().pipeline();
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|d| d.useconds() as i64)
            .unwrap_or(0);
        let mut seeking = gst::query::Seeking::new(gst::Format::Time);
        let seekable = pipeline.query(&mut seeking) && seeking.result().0;
        (seekable, duration)
    }

    /// How long some files are is not known until the pipeline has read into them, and it
    /// says so when it finds out. A stream is only told its length as it is prepared, so
    /// it is prepared again with the length in it: without one the controls have no
    /// timeline to show.
    fn redate(&self) {
        let imp = self.imp();
        // Not while a seek is waiting on the pipeline: preparing the stream again takes
        // the seek out of it, and there is nothing left to answer when it lands.
        if !self.is_prepared() || imp.starting.get() || imp.seeking.get() {
            return;
        }
        let (seekable, duration) = self.facts();
        if duration == self.duration() {
            return;
        }
        let playing = self.is_playing();
        imp.redating.set(true);
        self.stream_unprepared();
        self.stream_prepared(imp.has_audio.get(), imp.has_video.get(), seekable, duration);
        imp.redating.set(false);
        if let Some(position) = imp.pipeline().query_position::<gst::ClockTime>() {
            // Preparing puts the stream back at the start; say where the file really is,
            // so the timeline does not fall back until the next tick picks it up.
            self.update(position.useconds() as i64);
        }
        if playing {
            self.play();
        } else {
            // A video with autoplay starts its stream the moment it is prepared, and this
            // prepares it again: a file the viewer had paused would start up by itself.
            self.pause();
        }
    }
}

/// A helper ready to be started, worked out away from the main loop: its command line and
/// this side's ends of its sockets.
struct Prepared {
    argv: Vec<std::ffi::OsString>,
    /// What the helper inherits under the same numbers: the seccomp program, and its ends
    /// of the sockets.
    inherited: Vec<OwnedFd>,
    control: UnixStream,
    video: UnixStream,
    audio: UnixStream,
    /// A file the sandbox cannot reach, and the socket the helper asks for its bytes on.
    remote: Option<(UnixStream, gio::File)>,
}

/// On a worker: the helper that decodes `file`; `formats` are the DMA-BUF formats the
/// sink takes, separated by commas.
fn prepare(file: gio::File, formats: String) -> Result<Prepared, String> {
    let helper =
        crate::thumbnails::own_thumbnailer().ok_or("spiral-thumbnailer is not installed")?;
    let helper_arg = helper.to_string_lossy().into_owned();
    // Media files come from anywhere and their decoders are parsers: no sandbox, no run.
    let sandbox = crate::sandbox::command(&helper_arg)
        .ok_or("bubblewrap is missing or does not work here")?;
    let pair = || UnixStream::pair().map_err(|e| e.to_string());
    let (control, control_end) = pair()?;
    let (video, video_end) = pair()?;
    let (audio, audio_end) = pair()?;
    let mut inherited: Vec<OwnedFd> = vec![
        sandbox.seccomp.into(),
        control_end.into(),
        video_end.into(),
        audio_end.into(),
    ];
    let mut argv: Vec<std::ffi::OsString> = sandbox.argv.iter().map(Into::into).collect();
    argv.extend(crate::metadata::media_args());
    // Sound alone is decoded without the GPU. Told by the name: a video named as sound
    // is decoded in software.
    if !sound_only(&file) {
        argv.extend(gpu_args());
    }
    // GStreamer's debug output, where it was asked for, from the helper as well; without
    // colours, which would reach the log as the escapes it drops.
    if let Some(level) = std::env::var_os("GST_DEBUG") {
        argv.extend(["--setenv".into(), "GST_DEBUG".into(), level]);
        argv.extend(["--setenv", "GST_DEBUG_NO_COLOR", "1"].map(Into::into));
    }
    let mut remote = None;
    let source = match file.path().filter(|_| file.is_native()) {
        Some(path) => {
            // The name keeps its extension, which some formats are recognised by.
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let inside = format!("/tmp/in{ext}");
            argv.extend([
                "--ro-bind".into(),
                path.into_os_string(),
                inside.clone().into(),
            ]);
            inside
        }
        // Somewhere the sandbox cannot reach: the helper asks for the bytes it needs.
        None => {
            let (data, data_end) = pair()?;
            let source = format!("fd:{}", data_end.as_raw_fd());
            inherited.push(data_end.into());
            remote = Some((data, file));
            source
        }
    };
    argv.push("--".into());
    argv.push(helper.into_os_string());
    argv.push("--play".into());
    argv.push(source.into());
    argv.extend(
        inherited[1..4]
            .iter()
            .map(|fd| fd.as_raw_fd().to_string().into()),
    );
    argv.push(if formats.is_empty() {
        "-".into()
    } else {
        formats.into()
    });
    Ok(Prepared {
        argv,
        inherited,
        control,
        video,
        audio,
        remote,
    })
}

/// Whether the name of `file` says it holds sound alone.
fn sound_only(file: &gio::File) -> bool {
    file.basename().is_some_and(|name| {
        gio::content_type_guess(Some(&name), None)
            .0
            .starts_with("audio/")
    })
}

/// What the helper says on its standard error, into the log a line at a time, each cut
/// short and without control characters.
fn log_helper(stderr: std::io::PipeReader) {
    use std::io::BufRead;
    let mut stderr = std::io::BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut stderr).take(1024).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text: String = String::from_utf8_lossy(&line)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if !text.is_empty() {
            glib::g_debug!("spiral", "player: helper: {text}");
        }
    }
}

/// The GPU, for the helper to decode video on it as the player did before its decoding
/// moved into the sandbox: the render nodes VA-API decodes through, NVIDIA's device files
/// for its own decoder, and the parts of sysfs the drivers read to learn which GPU they
/// have, read-only, as Flatpak gives an application with `--device=dri`. A render node
/// can neither show anything on screen nor see another program's work on the GPU; the
/// primary nodes, which drive the display, stay out. With none of this there, or none of
/// it usable, decodebin3 falls back to a decoder in software.
fn gpu_args() -> Vec<std::ffi::OsString> {
    let nodes = |dir: &str, prefix: &str| {
        let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
            })
            .collect();
        found.sort();
        found
    };
    let mut devices = nodes("/dev/dri", "renderD");
    devices.extend(nodes("/dev", "nvidia"));
    if devices.is_empty() {
        return Vec::new();
    }
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    for device in devices {
        args.extend([
            "--dev-bind-try".into(),
            device.clone().into(),
            device.into(),
        ]);
    }
    for sys in ["/sys/bus", "/sys/class", "/sys/dev", "/sys/devices"] {
        args.extend(["--ro-bind-try".into(), sys.into(), sys.into()]);
    }
    // The environment is cleared in the sandbox; a driver chosen by hand stays chosen.
    if let Some(name) = std::env::var_os("LIBVA_DRIVER_NAME") {
        args.extend(["--setenv".into(), "LIBVA_DRIVER_NAME".into(), name]);
    }
    args
}

/// The DMA-BUF formats GTK can show, `FOURCC:0xMODIFIER` as GStreamer writes them: the
/// ones the sink takes.
fn dmabuf_formats() -> Vec<String> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let formats = display.dmabuf_formats();
    let mut out: Vec<String> = (0..formats.n_formats())
        .map(|i| formats.format(i))
        // DRM_FORMAT_MOD_INVALID, a layout only the driver knows, is nothing GStreamer can
        // name in caps.
        .filter(|&(fourcc, modifier)| fourcc != 0 && modifier != 0x00ff_ffff_ffff_ffff)
        .map(|(fourcc, modifier)| gst_video::dma_drm_fourcc_to_string(fourcc, modifier).into())
        .collect();
    out.dedup();
    out
}

/// Reads the packets of one stream from the helper and hands them to `source`.
struct Reader {
    source: gst::Element,
    current: Arc<AtomicU64>,
    id: u32,
    notes: async_channel::Sender<(u32, Option<Note>)>,
    returns: Arc<UnixStream>,
}

impl Reader {
    /// Once `gate` says the pipeline takes them and what they are, read and pass on. Packets
    /// of a seek or a session that is no longer the current one are dropped, and pictures
    /// among them handed back; one that is malformed ends the session.
    fn run(self, mut stream: media::FdStream, gate: mpsc::Receiver<Gate>) {
        let Ok(Some(take)) = gate.recv() else {
            return;
        };
        let (max, exact) = match take {
            Take::Samples { max, exact } => (max, exact),
            Take::Frames { .. } => (0, None),
        };
        let allocator = gst_allocators::DmaBufAllocator::new();
        loop {
            let packet = match media::read_packet(&mut stream, max, exact) {
                Ok(packet) => packet,
                Err(e) => return self.broken(e),
            };
            let epoch = match &packet {
                Packet::Data { epoch, .. }
                | Packet::End { epoch }
                | Packet::Frame { epoch, .. } => *epoch,
            };
            let current =
                self.current.load(Ordering::SeqCst) == u64::from(self.id) << 32 | u64::from(epoch);
            match packet {
                Packet::Data {
                    pts,
                    duration,
                    bytes,
                    ..
                } if current => {
                    let mut buffer = gst::Buffer::from_mut_slice(bytes);
                    if let Some(buffer) = buffer.get_mut() {
                        buffer.set_pts(pts.map(gst::ClockTime::from_nseconds));
                        buffer.set_duration(duration.map(gst::ClockTime::from_nseconds));
                    }
                    self.push(buffer);
                }
                Packet::End { .. } if current => {
                    let _ = self
                        .source
                        .emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
                }
                Packet::Frame {
                    pts,
                    duration,
                    id,
                    memories,
                    planes,
                    ..
                } => {
                    let fds: Option<Vec<OwnedFd>> =
                        memories.iter().map(|_| stream.take_fd()).collect();
                    let Take::Frames { width, height } = take else {
                        return self.broken(std::io::ErrorKind::InvalidData.into());
                    };
                    let Some(fds) = fds else {
                        return self.broken(std::io::ErrorKind::InvalidData.into());
                    };
                    if !current {
                        hand_back(&self.returns, id);
                        continue;
                    }
                    let Some(mut buffer) =
                        frame(&allocator, fds, &memories, &planes, width, height)
                    else {
                        hand_back(&self.returns, id);
                        return self.broken(std::io::ErrorKind::InvalidData.into());
                    };
                    if let Some(buffer) = buffer.get_mut() {
                        buffer.set_pts(pts.map(gst::ClockTime::from_nseconds));
                        buffer.set_duration(duration.map(gst::ClockTime::from_nseconds));
                        when_released(buffer.peek_memory(0), self.returns.clone(), id);
                    }
                    self.push(buffer);
                }
                _ => {}
            }
        }
    }

    fn push(&self, buffer: gst::Buffer) {
        let _ = self
            .source
            .emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]);
    }

    /// The stream has ended: the helper has gone, or it sent what does not parse.
    fn broken(&self, e: std::io::Error) {
        if e.kind() == std::io::ErrorKind::InvalidData {
            let _ = self.notes.send_blocking((self.id, None));
        }
    }
}

/// Tell the helper picture `id` is no longer in use.
fn hand_back(returns: &UnixStream, id: u64) {
    media::send_now(returns, &id.to_le_bytes());
}

/// Hand picture `id` back once `memory`, the first of it, is freed: after the sink has
/// shown it and let it go, or the pipeline has dropped it unshown.
fn when_released(memory: &gst::MemoryRef, returns: Arc<UnixStream>, id: u64) {
    struct Release {
        returns: Arc<UnixStream>,
        id: u64,
    }
    unsafe extern "C" fn released(data: glib::ffi::gpointer, _: *mut gst::ffi::GstMiniObject) {
        // SAFETY: `data` is the box handed over below, and this runs once.
        let release = unsafe { Box::from_raw(data as *mut Release) };
        hand_back(&release.returns, release.id);
    }
    let release = Box::into_raw(Box::new(Release { returns, id }));
    // SAFETY: a memory is a mini object; the weak reference calls `released` once, when
    // the memory is finalized, with the box it owns from then on.
    unsafe {
        gst::ffi::gst_mini_object_weak_ref(
            memory.as_mut_ptr() as *mut gst::ffi::GstMiniObject,
            Some(released),
            release as glib::ffi::gpointer,
        );
    }
}

/// A buffer of the DMA-BUFs `fds`, with the layout the helper gave for them checked
/// against what the descriptors hold.
fn frame(
    allocator: &gst_allocators::DmaBufAllocator,
    fds: Vec<OwnedFd>,
    memories: &[(u64, u64)],
    planes: &[(u64, i32)],
    width: u32,
    height: u32,
) -> Option<gst::Buffer> {
    let mut buffer = gst::Buffer::new();
    let buffer_mut = buffer.get_mut()?;
    let mut total = 0u64;
    for (fd, &(offset, size)) in fds.into_iter().zip(memories) {
        // SAFETY: lseek on a descriptor owned here; a DMA-BUF answers with its size.
        let len = unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_END) };
        let len = u64::try_from(len).ok()?;
        if size == 0 || offset.checked_add(size)? > len {
            return None;
        }
        // SAFETY: the descriptor is handed to the memory, which closes it when freed.
        let mut memory = unsafe { allocator.alloc_dmabuf(fd, len as usize) }.ok()?;
        memory
            .get_mut()?
            .resize(offset as usize..(offset + size) as usize);
        buffer_mut.append_memory(memory);
        total = total.checked_add(size)?;
    }
    if planes
        .iter()
        .any(|&(offset, stride)| offset >= total || !(1..=1 << 20).contains(&stride))
    {
        return None;
    }
    // The first plane holds a row of `stride` bytes for each row of the picture, whatever
    // the tiling; the planes after it are smaller in ways each format has its own rule for.
    let (offset, stride) = planes[0];
    if offset.checked_add(stride as u64 * u64::from(height))? > total {
        return None;
    }
    let offsets: Vec<usize> = planes.iter().map(|&(offset, _)| offset as usize).collect();
    let strides: Vec<i32> = planes.iter().map(|&(_, stride)| stride).collect();
    gst_video::VideoMeta::add_full(
        buffer_mut,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::DmaDrm,
        width,
        height,
        &offsets,
        &strides,
    )
    .ok()?;
    Some(buffer)
}

/// Answer the helper's reads of `file`, which is somewhere the sandbox cannot reach: its
/// size first, then the bytes at each offset asked for. Closing the socket unanswered
/// tells the helper the file cannot be read.
fn serve_bytes(file: gio::File, mut socket: UnixStream) {
    let opened = file.read(gio::Cancellable::NONE).ok().and_then(|stream| {
        let size = stream
            .query_info("standard::size", gio::Cancellable::NONE)
            .ok()?
            .size();
        Some((stream, u64::try_from(size).ok()?))
    });
    let Some((stream, size)) = opened else {
        return;
    };
    if socket.write_all(&size.to_le_bytes()).is_err() {
        return;
    }
    let mut request = [0u8; 12];
    let mut buffer = vec![0u8; media::READ_MAX as usize];
    while socket.read_exact(&mut request).is_ok() {
        let offset = u64::from_le_bytes(request[..8].try_into().unwrap());
        let len = u32::from_le_bytes(request[8..].try_into().unwrap()).min(media::READ_MAX);
        let read = i64::try_from(offset)
            .ok()
            .filter(|&at| {
                stream
                    .seek(at, glib::SeekType::Set, gio::Cancellable::NONE)
                    .is_ok()
            })
            .and_then(|_| {
                stream
                    .read_all(&mut buffer[..len as usize], gio::Cancellable::NONE)
                    .ok()
            })
            .map_or(0, |(n, _)| n);
        let mut reply = (read as u32).to_le_bytes().to_vec();
        reply.extend_from_slice(&buffer[..read]);
        if socket.write_all(&reply).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memfd(size: usize) -> OwnedFd {
        use std::os::fd::FromRawFd;
        // SAFETY: a fresh descriptor, owned from here on.
        let fd = unsafe {
            OwnedFd::from_raw_fd(libc::memfd_create(
                c"spiral-test".as_ptr(),
                libc::MFD_CLOEXEC,
            ))
        };
        let file = std::fs::File::from(fd);
        file.set_len(size as u64).unwrap();
        file.into()
    }

    /// A picture's descriptors cross the socket with its packet, and the buffer made of
    /// them has the memories and the planes the helper described; a layout reaching past
    /// what a descriptor holds is refused.
    #[test]
    fn pictures_in_gpu_memory_cross_and_are_checked() {
        gst::init().unwrap();
        let (helper, player) = UnixStream::pair().unwrap();
        let (y, uv) = (memfd(4096), memfd(4096));
        let packet = Packet::Frame {
            epoch: 1,
            pts: Some(5),
            duration: None,
            id: 7,
            memories: vec![(0, 2048), (1024, 1024)],
            planes: vec![(0, 64), (2048, 64)],
        };
        media::send_frame_for_test(&helper, &packet, &[y.as_raw_fd(), uv.as_raw_fd()]);
        let mut stream = media::FdStream::new(player);
        let read = media::read_packet(&mut stream, 0, None).unwrap();
        assert_eq!(read, packet);
        let fds: Vec<OwnedFd> = (0..2).map(|_| stream.take_fd().unwrap()).collect();
        assert!(stream.take_fd().is_none());

        let allocator = gst_allocators::DmaBufAllocator::new();
        let Packet::Frame {
            memories, planes, ..
        } = read
        else {
            unreachable!()
        };
        let buffer = frame(&allocator, fds, &memories, &planes, 32, 32).unwrap();
        assert_eq!(buffer.n_memory(), 2);
        assert_eq!(buffer.size(), 3072);
        let meta = buffer.meta::<gst_video::VideoMeta>().unwrap();
        assert_eq!(meta.offset(), &[0, 2048]);
        assert_eq!(meta.stride(), &[64, 64]);

        let past = [(0, 8192)];
        assert!(frame(&allocator, vec![memfd(4096)], &past, &[(0, 64)], 32, 32).is_none());
        let plane_past = [(0, 1024)];
        assert!(
            frame(
                &allocator,
                vec![memfd(4096)],
                &plane_past,
                &[(1024, 64)],
                32,
                32
            )
            .is_none()
        );
        // Rows of 64 bytes for 32 rows do not fit in 1024 bytes.
        let short = [(0, 1024)];
        assert!(frame(&allocator, vec![memfd(4096)], &short, &[(0, 64)], 32, 32).is_none());
    }
}
