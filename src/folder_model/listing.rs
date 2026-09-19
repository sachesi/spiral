//! The files of one folder: read in batches, then kept up to date from a monitor of the
//! folder. `gtk::DirectoryList` did this until it turned out to stop for good at the first
//! file it could not read back -- one written and renamed at once, as saving does -- taking
//! every change the folder heard of after it along, in whatever folder the list moved on to.
//! Here a change only says which name to read again, and what the read finds is what the
//! list shows: a file that is gone by then is taken out, and nothing waits on anything.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::file_utils;
use crate::{gio, glib, gtk};

/// How many files one read of a folder brings: a local disk answers a lot at once, a
/// share on the network is better asked for a little at a time.
const BATCH_LOCAL: i32 = 5000;
const BATCH_REMOTE: i32 = 100;

/// How many changed files are read back at the same time.
const READ_AT_ONCE: usize = 32;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Listing)]
    pub struct Listing {
        pub file: RefCell<Option<gio::File>>,
        pub items: RefCell<Vec<gio::FileInfo>>,
        /// The name of each item, in step with `items`, to find one by without asking the
        /// info for a fresh copy of its name every time.
        pub names: RefCell<Vec<PathBuf>>,
        #[property(get)]
        pub loading: Cell<bool>,
        #[property(get, nullable)]
        pub error_message: RefCell<Option<String>>,
        pub error: RefCell<Option<glib::Error>>,
        /// Whether a monitor is watching the folder.
        #[property(get)]
        pub monitored: Cell<bool>,
        pub monitor: RefCell<Option<gio::FileMonitor>>,
        /// Bumped whenever the folder is listed anew, so what the old listing or its reads
        /// bring back is let go.
        pub generation: Cell<u64>,
        /// Bumped as well when a listing is stopped where it is.
        pub read_gen: Cell<u64>,
        /// Names changed since they were last read, each with the number of the change,
        /// so a read that another change overtook is not taken for the last word.
        pub dirty: RefCell<HashMap<PathBuf, u64>>,
        pub changes: Cell<u64>,
        pub reading: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Listing {
        const NAME: &'static str = "SpiralListing";
        type Type = super::Listing;
        type Interfaces = (gio::ListModel,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for Listing {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // The folder itself was deleted, moved away or unmounted.
                    glib::subclass::Signal::builder("gone").build(),
                    // `info` is about to be read again in place, and its row put in anew.
                    glib::subclass::Signal::builder("updating")
                        .param_types([gio::FileInfo::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            LIVE.with(|live| {
                let mut live = live.borrow_mut();
                live.retain(|w| w.upgrade().is_some());
                live.push(self.obj().downgrade());
            });
        }

        fn dispose(&self) {
            if let Some(monitor) = self.monitor.take() {
                monitor.cancel();
            }
            self.generation.set(self.generation.get() + 1);
        }
    }

    impl ListModelImpl for Listing {
        fn item_type(&self) -> glib::Type {
            gio::FileInfo::static_type()
        }

        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.items
                .borrow()
                .get(position as usize)
                .map(|info| info.clone().upcast())
        }
    }
}

thread_local! {
    /// Every listing alive, for what an operation changed to reach each that shows it.
    static LIVE: RefCell<Vec<glib::WeakRef<Listing>>> = const { RefCell::new(Vec::new()) };
}

glib::wrapper! {
    pub struct Listing(ObjectSubclass<imp::Listing>)
        @implements gio::ListModel;
}

impl Default for Listing {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// Have every listing of a folder that holds one of `files` read it again. An operation
/// that made, moved or took away files says so here once it is done, so a folder whose
/// monitor did not hear of it, or that has none, shows what the operation did.
pub fn touch(files: &[gio::File]) {
    let live: Vec<Listing> = LIVE.with(|live| {
        let mut live = live.borrow_mut();
        live.retain(|w| w.upgrade().is_some());
        live.iter().filter_map(|w| w.upgrade()).collect()
    });
    for listing in live {
        for file in files {
            listing.mark(file);
        }
    }
}

impl Listing {
    /// A listing of `dir` that starts once the main loop comes round again, and not at all
    /// if it has been let go by then. A tree asks for the model of every folder it shows,
    /// only to learn that it can unfold, and drops it at once.
    pub fn deferred(dir: &gio::File) -> Self {
        let listing = Self::default();
        let dir = dir.clone();
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            listing,
            move || listing.set_file(Some(&dir))
        ));
        listing
    }

    pub fn file(&self) -> Option<gio::File> {
        self.imp().file.borrow().clone()
    }

    /// Why the listing stopped where it did.
    pub fn error(&self) -> Option<glib::Error> {
        self.imp().error.borrow().clone()
    }

    pub fn connect_gone<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("gone", false, move |values| {
            f(&values[0].get::<Self>().unwrap());
            None
        })
    }

    pub fn connect_updating<F: Fn(&Self, &gio::FileInfo) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local("updating", false, move |values| {
            f(
                &values[0].get::<Self>().unwrap(),
                &values[1].get::<gio::FileInfo>().unwrap(),
            );
            None
        })
    }

    /// List `file` from the start, or nothing. A local folder is watched from the start;
    /// one on another machine once it has been listed, since watching it waits for it to
    /// be mounted, and listing it mounts it without blocking.
    pub fn set_file(&self, file: Option<&gio::File>) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        imp.read_gen.set(imp.read_gen.get() + 1);
        imp.reading.set(false);
        imp.dirty.borrow_mut().clear();
        if let Some(monitor) = imp.monitor.take() {
            monitor.cancel();
        }
        self.set_monitored(false);
        self.set_error(None);
        let n = imp.items.borrow().len() as u32;
        imp.items.borrow_mut().clear();
        imp.names.borrow_mut().clear();
        imp.file.replace(file.cloned());
        if n > 0 {
            self.items_changed(0, n, 0);
        }
        let Some(file) = file.cloned() else {
            self.set_loading(false);
            return;
        };
        self.set_loading(true);
        if file.is_native() {
            self.watch();
        }
        let generation = imp.generation.get();
        let read_gen = imp.read_gen.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = listing)]
            self,
            async move { listing.list(file, generation, read_gen).await }
        ));
    }

    /// Stop reading the folder, keeping what has arrived, and go on following what
    /// changes in it.
    pub fn stop(&self) {
        let imp = self.imp();
        if !imp.loading.get() {
            return;
        }
        imp.read_gen.set(imp.read_gen.get() + 1);
        self.set_loading(false);
        self.read_changes();
    }

    async fn list(&self, file: gio::File, generation: u64, read_gen: u64) {
        let imp = self.imp();
        let current = || imp.generation.get() == generation && imp.read_gen.get() == read_gen;
        let batch = if file.is_native() {
            BATCH_LOCAL
        } else {
            BATCH_REMOTE
        };
        let listed = file
            .enumerate_children_future(
                file_utils::ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await;
        let enumerator = match listed {
            Ok(enumerator) => enumerator,
            Err(e) => {
                if current() {
                    self.set_error(Some(e));
                    self.finish_listing();
                }
                return;
            }
        };
        loop {
            let next = enumerator
                .next_files_future(batch, glib::Priority::DEFAULT)
                .await;
            if !current() {
                break;
            }
            match next {
                Ok(infos) if infos.is_empty() => {
                    self.finish_listing();
                    break;
                }
                Ok(infos) => {
                    for info in &infos {
                        info.set_attribute_object("standard::file", &enumerator.child(info));
                    }
                    self.append(infos);
                }
                Err(e) => {
                    self.set_error(Some(e));
                    self.finish_listing();
                    break;
                }
            }
        }
        let _ = enumerator.close_future(glib::Priority::DEFAULT).await;
    }

    fn finish_listing(&self) {
        self.set_loading(false);
        if self.imp().error.borrow().is_none() {
            self.watch();
        }
        self.read_changes();
    }

    fn watch(&self) {
        let imp = self.imp();
        if imp.monitor.borrow().is_some() {
            return;
        }
        let Some(dir) = self.file() else { return };
        // Not every backend can watch a folder: one on a server that cannot is shown as
        // it was listed, and what Spiral itself does there is told through `touch`.
        let Ok(monitor) =
            dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        monitor.connect_changed(glib::clone!(
            #[weak(rename_to = listing)]
            self,
            move |_, file, other, event| listing.heard(file, other, event)
        ));
        imp.monitor.replace(Some(monitor));
        self.set_monitored(true);
    }

    fn heard(&self, file: &gio::File, other: Option<&gio::File>, event: gio::FileMonitorEvent) {
        use gio::FileMonitorEvent as E;
        let Some(dir) = self.file() else { return };
        match event {
            E::Deleted | E::MovedOut if file.equal(&dir) => {
                self.emit_by_name::<()>("gone", &[]);
            }
            E::Unmounted => self.emit_by_name::<()>("gone", &[]),
            E::Created
            | E::MovedIn
            | E::AttributeChanged
            | E::ChangesDoneHint
            | E::Deleted
            | E::MovedOut => self.mark(file),
            E::Renamed => {
                if let Some(other) = other {
                    self.rename(file, other);
                    self.mark(file);
                    self.mark(other);
                }
            }
            _ => {}
        }
    }

    /// A file renamed within the folder keeps its row, under the new name, rather than
    /// going out and coming back as a stranger: whatever was selected or unfolded stays
    /// so. The read that follows fills in what the name changes, like the type.
    fn rename(&self, from: &gio::File, to: &gio::File) {
        let imp = self.imp();
        if imp.loading.get() {
            return;
        }
        let (Some(old), Some(new)) = (from.basename(), to.basename()) else {
            return;
        };
        let Some(dir) = self.file() else { return };
        if !to.parent().is_some_and(|p| p.equal(&dir)) {
            return;
        }
        let positions = self.positions(&[old.as_path(), new.as_path()]);
        let (Some(&pos), None) = (positions.get(old.as_path()), positions.get(new.as_path()))
        else {
            return;
        };
        let info = imp.items.borrow()[pos].clone();
        self.emit_by_name::<()>("updating", &[&info]);
        info.set_attribute_object("standard::file", to);
        info.set_name(&new);
        info.set_display_name(&glib::filename_display_name(&new));
        info.set_edit_name(&glib::filename_display_name(&new));
        file_utils::forget_sort_keys(&info);
        imp.names.borrow_mut()[pos] = new;
        self.items_changed(pos as u32, 1, 1);
    }

    /// Read `file` again if it is one of this folder's, once the listing is complete.
    pub(crate) fn mark(&self, file: &gio::File) {
        let imp = self.imp();
        let Some(dir) = self.file() else { return };
        if !file.parent().is_some_and(|p| p.equal(&dir)) {
            return;
        }
        let Some(name) = file.basename() else { return };
        let change = imp.changes.get() + 1;
        imp.changes.set(change);
        imp.dirty.borrow_mut().insert(name, change);
        self.read_changes();
    }

    /// Read what changed, unless a read is on already or the listing is still coming in:
    /// then that picks it up when it ends.
    fn read_changes(&self) {
        let imp = self.imp();
        if imp.loading.get() || imp.reading.get() || imp.dirty.borrow().is_empty() {
            return;
        }
        imp.reading.set(true);
        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = listing)]
            self,
            async move {
                listing.read(generation).await;
                if listing.imp().generation.get() == generation {
                    listing.imp().reading.set(false);
                }
            }
        ));
    }

    async fn read(&self, generation: u64) {
        let imp = self.imp();
        let Some(dir) = self.file() else { return };
        loop {
            let batch: Vec<(PathBuf, u64)> = imp
                .dirty
                .borrow()
                .iter()
                .take(READ_AT_ONCE)
                .map(|(name, change)| (name.clone(), *change))
                .collect();
            if batch.is_empty() {
                return;
            }
            let reads = batch.iter().map(|(name, _)| {
                dir.child(name).query_info_future(
                    file_utils::ATTRIBUTES,
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
            });
            let results = futures_util::future::join_all(reads).await;
            if imp.generation.get() != generation {
                return;
            }
            // A name changed again while it was being read is read again; the rest are
            // what the folder holds now.
            let settled: Vec<(PathBuf, Result<gio::FileInfo, glib::Error>)> = batch
                .into_iter()
                .zip(results)
                .filter(|((name, change), _)| {
                    let mut dirty = imp.dirty.borrow_mut();
                    let current = dirty.get(name) == Some(change);
                    if current {
                        dirty.remove(name);
                    }
                    current
                })
                .map(|((name, _), result)| (name, result))
                .collect();
            self.apply(&dir, settled);
        }
    }

    /// Put what the reads found in the list: a file already there is read into its own
    /// info, which keeps its row; one that is gone goes; a new one goes on the end.
    fn apply(&self, dir: &gio::File, settled: Vec<(PathBuf, Result<gio::FileInfo, glib::Error>)>) {
        let imp = self.imp();
        let names: Vec<&Path> = settled.iter().map(|(name, _)| name.as_path()).collect();
        let positions = self.positions(&names);
        let mut gone = Vec::new();
        let mut new = Vec::new();
        for (name, result) in &settled {
            let pos = positions.get(name.as_path()).copied();
            match (result, pos) {
                (Ok(fresh), Some(pos)) => {
                    let info = imp.items.borrow()[pos].clone();
                    if same(&info, fresh) {
                        continue;
                    }
                    self.emit_by_name::<()>("updating", &[&info]);
                    fresh.copy_into(&info);
                    info.set_attribute_object("standard::file", &dir.child(name));
                    file_utils::forget_sort_keys(&info);
                    self.items_changed(pos as u32, 1, 1);
                }
                (Ok(fresh), None) => {
                    fresh.set_attribute_object("standard::file", &dir.child(name));
                    new.push(fresh.clone());
                }
                (Err(e), Some(pos)) if e.matches(gio::IOErrorEnum::NotFound) => gone.push(pos),
                // Something that cannot be read for another reason stays as it was.
                (Err(_), _) => {}
            }
        }
        gone.sort_unstable();
        for pos in gone.into_iter().rev() {
            imp.items.borrow_mut().remove(pos);
            imp.names.borrow_mut().remove(pos);
            self.items_changed(pos as u32, 1, 0);
        }
        self.append(new);
    }

    fn append(&self, infos: Vec<gio::FileInfo>) {
        if infos.is_empty() {
            return;
        }
        let imp = self.imp();
        let at = imp.items.borrow().len() as u32;
        let n = infos.len() as u32;
        imp.names
            .borrow_mut()
            .extend(infos.iter().map(|info| info.name()));
        imp.items.borrow_mut().extend(infos);
        self.items_changed(at, 0, n);
    }

    /// Where each of `names` is in the list, in one pass over it.
    fn positions(&self, names: &[&Path]) -> HashMap<PathBuf, usize> {
        let wanted: HashSet<&Path> = names.iter().copied().collect();
        self.imp()
            .names
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, name)| wanted.contains(name.as_path()))
            .map(|(pos, name)| (name.clone(), pos))
            .collect()
    }

    fn set_loading(&self, loading: bool) {
        if self.imp().loading.replace(loading) != loading {
            self.notify_loading();
        }
    }

    fn set_monitored(&self, monitored: bool) {
        if self.imp().monitored.replace(monitored) != monitored {
            self.notify_monitored();
        }
    }

    fn set_error(&self, error: Option<glib::Error>) {
        let imp = self.imp();
        let message = error.as_ref().map(|e| e.message().to_string());
        imp.error.replace(error);
        if imp.error_message.replace(message.clone()) != message {
            self.notify_error_message();
        }
    }
}

/// Whether two reads of a file say the same, leaving out the objects on them, which are
/// never the same object twice, and the keys Spiral keeps on an info for sorting.
fn same(a: &gio::FileInfo, b: &gio::FileInfo) -> bool {
    let attributes = |info: &gio::FileInfo| -> Vec<(glib::GString, Option<glib::GString>)> {
        let mut list: Vec<_> = info
            .list_attributes(None)
            .into_iter()
            .filter(|name| {
                !name.starts_with("spiral::")
                    && info.attribute_type(name) != gio::FileAttributeType::Object
            })
            .map(|name| {
                let value = info.attribute_as_string(&name);
                (name, value)
            })
            .collect();
        list.sort();
        list
    };
    attributes(a) == attributes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("spiral-listing-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(listing: &Listing) -> Vec<String> {
        let mut names: Vec<String> = listing
            .imp()
            .names
            .borrow()
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Turn the main loop until `done` holds, or fail after a few seconds.
    fn until(ctx: &glib::MainContext, what: &str, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(
                start.elapsed().as_secs() < 5,
                "timed out waiting for {what}"
            );
            ctx.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn with_listing(
        name: &str,
        files: &[&str],
        test: impl FnOnce(&Scratch, &Listing, &glib::MainContext),
    ) {
        let scratch = Scratch::new(name);
        for f in files {
            std::fs::write(scratch.path(f), f).unwrap();
        }
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let listing = Listing::default();
            listing.set_file(Some(&gio::File::for_path(&scratch.0)));
            until(&ctx, "the listing", || !listing.loading());
            test(&scratch, &listing, &ctx);
        })
        .unwrap();
    }

    #[test]
    fn a_file_saved_over_leaves_the_listing_following_the_folder() {
        with_listing("save", &["a"], |scratch, listing, ctx| {
            assert!(listing.monitored());
            // The way editors save: a hidden file written, then renamed over the old one.
            gio::File::for_path(scratch.path("a"))
                .replace_contents(
                    b"x",
                    None,
                    false,
                    gio::FileCreateFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .unwrap();
            std::fs::write(scratch.path("b"), "b").unwrap();
            until(ctx, "b to arrive", || names(listing) == ["a", "b"]);
            std::fs::remove_file(scratch.path("a")).unwrap();
            until(ctx, "a to go", || names(listing) == ["b"]);
        });
    }

    #[test]
    fn a_file_gone_before_it_is_read_does_not_hold_up_the_rest() {
        with_listing("fleeting", &["a"], |scratch, listing, ctx| {
            for i in 0..20 {
                let p = scratch.path(&format!("tmp{i}"));
                std::fs::write(&p, "").unwrap();
                std::fs::remove_file(&p).unwrap();
            }
            std::fs::write(scratch.path("b"), "b").unwrap();
            until(ctx, "b to arrive", || names(listing) == ["a", "b"]);
        });
    }

    #[test]
    fn a_rename_keeps_the_row() {
        with_listing("rename", &["a", "c"], |scratch, listing, ctx| {
            let before = listing.imp().items.borrow()[listing
                .imp()
                .names
                .borrow()
                .iter()
                .position(|n| n == Path::new("a"))
                .unwrap()]
            .clone();
            std::fs::rename(scratch.path("a"), scratch.path("b")).unwrap();
            until(ctx, "the new name", || names(listing) == ["b", "c"]);
            let pos = listing
                .imp()
                .names
                .borrow()
                .iter()
                .position(|n| n == Path::new("b"))
                .unwrap();
            assert_eq!(listing.imp().items.borrow()[pos], before);
        });
    }

    #[test]
    fn listing_another_folder_follows_that_one() {
        with_listing("move-on", &["a"], |scratch, listing, ctx| {
            gio::File::for_path(scratch.path("a"))
                .replace_contents(
                    b"x",
                    None,
                    false,
                    gio::FileCreateFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .unwrap();
            std::fs::create_dir(scratch.path("sub")).unwrap();
            listing.set_file(Some(&gio::File::for_path(scratch.path("sub"))));
            until(ctx, "the listing", || !listing.loading());
            std::fs::write(scratch.path("sub/new"), "").unwrap();
            until(ctx, "new to arrive", || names(listing) == ["new"]);
        });
    }

    #[test]
    fn the_folder_going_away_is_heard() {
        with_listing("gone", &["a"], |scratch, listing, ctx| {
            let gone = std::rc::Rc::new(Cell::new(false));
            let heard = gone.clone();
            listing.connect_gone(move |_| heard.set(true));
            std::fs::rename(&scratch.0, scratch.0.with_extension("moved")).unwrap();
            until(ctx, "the folder to be gone", || gone.get());
            std::fs::rename(scratch.0.with_extension("moved"), &scratch.0).unwrap();
        });
    }

    #[test]
    fn a_touched_file_is_read_again_without_a_monitor() {
        with_listing("touch", &["a"], |scratch, listing, ctx| {
            if let Some(monitor) = listing.imp().monitor.take() {
                monitor.cancel();
            }
            std::fs::write(scratch.path("b"), "b").unwrap();
            touch(&[gio::File::for_path(scratch.path("b"))]);
            until(ctx, "b to arrive", || names(listing) == ["a", "b"]);
        });
    }
}
