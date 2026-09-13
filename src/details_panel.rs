//! The details panel beside the panes: what is selected in the pane in charge, or the
//! folder it shows when nothing is. It only reads; Properties is where things change.
//!
//! Nearly everything comes from the listing the pane already holds. Reading more is kept
//! to what is cheap and safe to do for every file the selection stops on: the thumbnail,
//! made by the sandboxed thumbnailers as for the views; what a photo, a recording, a video
//! or a document says about itself, read in the same sandbox; the number of items in a
//! folder; the folder's own record and free space.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::browser_view::BrowserView;
use crate::dialogs::property_row as row;
use crate::{adw, file_utils, gio, glib, gtk, prefs};

/// How long the selection stays put before the panel follows it. An arrow key held down
/// walks through files faster than their thumbnails can be read, and only the file it
/// stops on is worth reading.
const SETTLE: Duration = Duration::from_millis(80);
/// Changes that keep coming, a large folder being listed, hold the panel back no longer
/// than this: it would go on showing the folder that was left.
const MAX_WAIT: Duration = Duration::from_secs(1);
const ICON_SIZE: i32 = 128;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct DetailsPanel {
        pub scroll: gtk::ScrolledWindow,
        pub view: RefCell<Option<glib::WeakRef<BrowserView>>>,
        pub pending: RefCell<Option<glib::SourceId>>,
        /// Since when the update that is pending has been put off.
        pub waiting_since: Cell<Option<Instant>>,
        /// What the page on show was made for, see `shown_for`.
        pub shown: Cell<Option<u64>>,
        /// Counts the updates, so what an earlier one is still reading is not shown.
        pub generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailsPanel {
        const NAME: &'static str = "SpiralDetailsPanel";
        type Type = super::DetailsPanel;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for DetailsPanel {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
            self.scroll.set_vexpand(true);
            obj.set_child(Some(&self.scroll));
            // Hidden, the panel reads nothing; shown again, it catches up.
            obj.connect_map(|panel| panel.queue_update());
        }
    }

    impl WidgetImpl for DetailsPanel {}
    impl BinImpl for DetailsPanel {}
}

glib::wrapper! {
    pub struct DetailsPanel(ObjectSubclass<imp::DetailsPanel>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DetailsPanel {
    /// Follow `view`, the pane now in charge.
    pub fn set_view(&self, view: &BrowserView) {
        self.imp().view.replace(Some(view.downgrade()));
        self.queue_update();
    }

    /// The selection or the folder of the pane followed changed.
    pub fn queue_update(&self) {
        let imp = self.imp();
        if !self.is_mapped() {
            if let Some(id) = imp.pending.take() {
                id.remove();
            }
            imp.waiting_since.set(None);
            return;
        }
        let since = imp.waiting_since.get().unwrap_or_else(Instant::now);
        if imp.pending.borrow().is_some() && since.elapsed() >= MAX_WAIT {
            return;
        }
        if let Some(id) = imp.pending.take() {
            id.remove();
        }
        imp.waiting_since.set(Some(since));
        let id = glib::timeout_add_local_once(
            SETTLE,
            glib::clone!(
                #[weak(rename_to = panel)]
                self,
                move || {
                    panel.imp().pending.take();
                    panel.imp().waiting_since.set(None);
                    panel.update();
                }
            ),
        );
        imp.pending.replace(Some(id));
    }

    fn update(&self) {
        let imp = self.imp();
        let view = imp.view.borrow().as_ref().and_then(|w| w.upgrade());
        let Some(view) = view else {
            imp.shown.set(None);
            imp.scroll.set_child(gtk::Widget::NONE);
            return;
        };
        let infos = view.model().selected_infos();
        // Another file of the folder changing is no reason to build the page again, which
        // would take it back to the top and the keyboard out of it.
        let key = shown_for(&view, &infos);
        if imp.shown.get() == Some(key) {
            return;
        }
        imp.shown.set(Some(key));
        imp.generation.set(imp.generation.get() + 1);
        let focused = self
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(self));
        let page = match infos.as_slice() {
            [] => self.folder_page(&view),
            [info] => self.file_page(&view, info),
            _ => many_page(&infos),
        };
        imp.scroll.set_child(Some(&page));
        // The keyboard was in the page that went: it goes on in the new one, at its button.
        if focused {
            page.child_focus(gtk::DirectionType::TabBackward);
        }
    }

    /// Hand what `fut` answers to `show`, unless the panel has moved on by then.
    fn when<T: 'static>(
        &self,
        fut: impl Future<Output = T> + 'static,
        show: impl FnOnce(T) + 'static,
    ) {
        let generation = self.imp().generation.get();
        let panel = self.downgrade();
        glib::spawn_future_local(async move {
            let answer = fut.await;
            if panel
                .upgrade()
                .is_some_and(|p| p.imp().generation.get() == generation)
            {
                show(answer);
            }
        });
    }

    fn file_page(&self, view: &BrowserView, info: &gio::FileInfo) -> gtk::Widget {
        let file = file_utils::file_of(info);
        let is_dir = file_utils::is_dir(info);
        let content_type = file_utils::content_type_of(info).unwrap_or_default();
        let icon = gtk::Image::builder()
            .gicon(&file_utils::icon_of(info))
            .pixel_size(ICON_SIZE)
            .build();
        let group = adw::PreferencesGroup::new();
        if !is_dir && !content_type.is_empty() {
            group.add(&row(&gettext("MIME Type"), &content_type));
        }
        if is_dir {
            // What the Size column says of a folder: how many items, where counting is on.
            if prefs::counts_for(&file) {
                let size = hidden_row(&group, &gettext("Size"));
                let stamp = info
                    .modification_date_time()
                    .map(|d| d.to_unix())
                    .unwrap_or(0);
                let dir = file.clone();
                self.when(
                    async move { crate::browser_view::count_children_at(&dir, stamp).await },
                    move |count| {
                        if let Some(count) = count {
                            size.set_subtitle(&file_utils::items_string(count));
                            size.set_visible(true);
                        }
                    },
                );
            }
        } else {
            let size = file_utils::size_string(info);
            if !size.is_empty() {
                group.add(&row(&gettext("Size"), &size));
            }
        }
        // Where the file is, when that is not the folder on screen: a search result, a
        // file in Favorites or under a folder unfolded in the list.
        if let Some(parent) = file.parent()
            && !view.location().is_some_and(|l| l.equal(&parent))
        {
            group.add(&row(&gettext("Location"), &file_utils::location_of(info)));
        }
        if info.is_symlink()
            && let Some(target) = info.symlink_target()
        {
            group.add(&row(&gettext("Link Target"), &target.to_string_lossy()));
        }
        let facts = [
            (gettext("Original Folder"), file_utils::trashed_from(info)),
            (gettext("Trashed On"), file_utils::trashed_on_string(info)),
            (gettext("Modified"), file_utils::modified_string(info)),
            (gettext("Accessed"), file_utils::accessed_string(info)),
            (gettext("Created"), file_utils::created_string(info)),
            (
                gettext("Owner"),
                file_utils::caption(info, "owner").unwrap_or_default(),
            ),
            (
                gettext("Group"),
                file_utils::caption(info, "group").unwrap_or_default(),
            ),
            (
                gettext("Permissions"),
                file_utils::permissions_string(info).unwrap_or_default(),
            ),
        ];
        for (title, value) in facts {
            if !value.is_empty() {
                group.add(&row(&title, &value));
            }
        }
        // What the file says of itself, above what the folder says of it: the size of a
        // picture and the camera, the length of a song and who sings it.
        let about = adw::PreferencesGroup::builder().visible(false).build();
        if !is_dir && crate::metadata::kind_of(&content_type).is_some() {
            let info = info.clone();
            let about = about.clone();
            self.when(
                async move { crate::metadata::read(&info).await },
                move |facts| {
                    for (title, value) in facts.map(|f| f.rows()).unwrap_or_default() {
                        about.add(&row(&title, &value));
                        about.set_visible(true);
                    }
                },
            );
        }
        if !is_dir {
            let info = info.clone();
            let icon = icon.clone();
            self.when(
                async move { crate::thumbnails::load(&info, 0).await },
                move |texture| {
                    if let Some(texture) = texture {
                        icon.set_paintable(Some(&texture));
                    }
                },
            );
        }
        page(
            &icon,
            &info.display_name(),
            &file_utils::type_string(info),
            &[&about, &group],
        )
    }

    fn folder_page(&self, view: &BrowserView) -> gtk::Widget {
        let model = view.model();
        let icon = gtk::Image::builder()
            .icon_name("folder")
            .pixel_size(ICON_SIZE)
            .build();
        let group = adw::PreferencesGroup::new();
        // A search is about what it found, not the folder it looks in.
        if let Some(folder) = view.location().filter(|_| !model.searching()) {
            let titles = [
                gettext("Modified"),
                gettext("Owner"),
                gettext("Group"),
                gettext("Permissions"),
            ];
            let rows: Vec<adw::ActionRow> = titles.iter().map(|t| hidden_row(&group, t)).collect();
            let free = hidden_row(&group, &gettext("Free"));
            let dir = folder.clone();
            let head = icon.clone();
            self.when(
                async move {
                    dir.query_info_future(
                        file_utils::ATTRIBUTES,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .ok()
                },
                move |info| {
                    let Some(info) = info else { return };
                    head.set_from_gicon(&file_utils::icon_of(&info));
                    let values = [
                        file_utils::modified_string(&info),
                        file_utils::caption(&info, "owner").unwrap_or_default(),
                        file_utils::caption(&info, "group").unwrap_or_default(),
                        file_utils::permissions_string(&info).unwrap_or_default(),
                    ];
                    for (row, value) in rows.iter().zip(values) {
                        if !value.is_empty() {
                            row.set_subtitle(&value);
                            row.set_visible(true);
                        }
                    }
                },
            );
            // A share that is slow to answer goes without, as in Properties.
            self.when(
                async move {
                    glib::future_with_timeout(
                        Duration::from_secs(1),
                        folder.query_filesystem_info_future(
                            "filesystem::free,filesystem::size",
                            glib::Priority::DEFAULT,
                        ),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                },
                move |fs| {
                    if let Some(fs) = fs.filter(|fs| fs.attribute_uint64("filesystem::size") > 0) {
                        free.set_subtitle(&prefs::size(fs.attribute_uint64("filesystem::free")));
                        free.set_visible(true);
                    }
                },
            );
        }
        let count = file_utils::items_string(u64::from(model.n_top_items()));
        page(&icon, &view.location_title(), &count, &[&group])
    }
}

/// What a page shows is made from: the pane, and the files selected in it as the listing has
/// them, a changed file being a new record there; with none selected, the folder. The record
/// of the first item stands for a listing read again, as it is after a change of preference.
fn shown_for(view: &BrowserView, infos: &[gio::FileInfo]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (view.as_ptr() as usize).hash(&mut hasher);
    let model = view.model();
    if infos.is_empty() {
        view.location()
            .map(|l| l.uri().to_string())
            .hash(&mut hasher);
        model.searching().hash(&mut hasher);
        model.n_top_items().hash(&mut hasher);
        model
            .info_at(0)
            .map(|i| i.as_ptr() as usize)
            .hash(&mut hasher);
    }
    for info in infos {
        (info.as_ptr() as usize).hash(&mut hasher);
        file_utils::size_of(info).hash(&mut hasher);
        info.modification_date_time()
            .map(|d| d.to_unix())
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Several files: how many, their type where they share one, and what the files among
/// them weigh. A folder's size is its whole content, which Properties adds up.
fn many_page(infos: &[gio::FileInfo]) -> gtk::Widget {
    let icon = gtk::Image::builder()
        .icon_name("folder-documents-symbolic")
        .pixel_size(ICON_SIZE / 2)
        .css_classes(["dim-label"])
        .build();
    let n = infos.len();
    let title = ngettext("%d Item", "%d Items", n as u32).replace("%d", &n.to_string());
    let kind = |i: &gio::FileInfo| (file_utils::is_dir(i), file_utils::content_type_of(i));
    let first = kind(&infos[0]);
    let subtitle = if infos.iter().all(|i| kind(i) == first) {
        file_utils::type_string(&infos[0])
    } else {
        String::new()
    };
    let group = adw::PreferencesGroup::new();
    let files: Vec<&gio::FileInfo> = infos.iter().filter(|i| !file_utils::is_dir(i)).collect();
    if !files.is_empty() {
        let size = prefs::size(files.iter().map(|i| file_utils::size_of(i)).sum());
        let size = if files.len() == n {
            size
        } else {
            gettext("%s, folders not counted").replace("%s", &size)
        };
        group.add(&row(&gettext("Size"), &size));
    }
    page(&icon, &title, &subtitle, &[&group])
}

/// A row for `group` that stays hidden until what it says has been read.
fn hidden_row(group: &adw::PreferencesGroup, title: &str) -> adw::ActionRow {
    let row = row(title, "");
    row.set_visible(false);
    group.add(&row);
    row
}

/// The panel's column: a picture, a name and what it is, the facts, and the way into
/// Properties for the same files.
fn page(
    icon: &gtk::Image,
    title: &str,
    subtitle: &str,
    groups: &[&adw::PreferencesGroup],
) -> gtk::Widget {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    icon.set_margin_bottom(6);
    head.append(icon);
    let label = |text: &str, class: &str| {
        gtk::Label::builder()
            .label(text)
            .css_classes([class])
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .selectable(true)
            .build()
    };
    head.append(&label(title, "title-4"));
    if !subtitle.is_empty() {
        head.append(&label(subtitle, "dim-label"));
    }
    page.append(&head);
    for group in groups {
        page.append(*group);
    }
    let actions = adw::PreferencesGroup::new();
    actions.add(
        &adw::ButtonRow::builder()
            .title(gettext("Properties"))
            .action_name("view.properties")
            .build(),
    );
    page.append(&actions);
    page.upcast()
}
