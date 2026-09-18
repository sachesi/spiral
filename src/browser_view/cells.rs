//! The cells of the grid and the list: icons, captions, emblems, tags and stars, and finding
//! a cell again from a point or a widget inside it.

use super::*;

pub(super) fn unbind_captions(label: &gtk::Label) {
    if let Some(handle) = COUNT_ABORT.take(label) {
        handle.abort();
    }
}

// Item counts already worked out, oldest first, keyed by the folder and the time it last
// changed. Counting means enumerating the folder in full, and a caption is bound afresh
// every time its row is scrolled back into view.
thread_local! {
    static COUNTS: RefCell<CountCache> = RefCell::new(CountCache::default());
}

#[derive(Default)]
pub(super) struct CountCache {
    seen: std::collections::HashMap<(String, i64), u64>,
    order: std::collections::VecDeque<(String, i64)>,
}

pub(super) const COUNT_CACHE_ENTRIES: usize = 4096;

/// Number of direct children of `dir` as of `stamp`, the time it last changed.
pub(crate) async fn count_children_at(dir: &gio::File, stamp: i64) -> Option<u64> {
    let key = (dir.uri().to_string(), stamp);
    if let Some(n) = COUNTS.with(|c| c.borrow().seen.get(&key).copied()) {
        return Some(n);
    }
    let n = count_children(dir).await?;
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.seen.insert(key.clone(), n).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > COUNT_CACHE_ENTRIES {
            let Some(oldest) = c.order.pop_front() else {
                break;
            };
            c.seen.remove(&oldest);
        }
    });
    Some(n)
}

/// Number of direct children of `dir`, or None if it cannot be read.
pub(super) async fn count_children(dir: &gio::File) -> Option<u64> {
    let en = dir
        .enumerate_children_future(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::LOW,
        )
        .await
        .ok()?;
    let mut n = 0u64;
    loop {
        let batch = en.next_files_future(256, glib::Priority::LOW).await.ok()?;
        if batch.is_empty() {
            return Some(n);
        }
        n += batch.len() as u64;
    }
}

/// Width of the emblem margin beside a grid icon.
pub(super) const EMBLEM_MARGIN: i32 = 18;

/// Lock shown on files the user cannot read or change, dimmed like the other emblems. It
/// keeps its place when empty so icons line up across cells.
pub(crate) fn emblem_image() -> gtk::Image {
    gtk::Image::builder()
        .pixel_size(16)
        .css_classes(["dim-label"])
        .build()
}

pub(super) fn set_emblem(emblem: &gtk::Image, locked: bool) {
    emblem.set_icon_name(locked.then_some("changes-prevent-symbolic"));
}

/// A tag's colour as a dot; CSS paints it.
pub(crate) fn tag_dot(color: &str) -> gtk::Image {
    gtk::Image::builder()
        .css_classes(["spiral-tag-dot", &crate::tags::dot_class(color)])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build()
}

/// Show the file's tags on its cell as a wash of the first colour: across the row in
/// the list and column views, behind the name in the grid, where `name` is the label.
/// A file that carries tags is made known to the index while it is here, in case it was
/// tagged elsewhere or moved by another program.
pub(crate) fn bind_tags(
    cell: &impl IsA<gtk::Widget>,
    name: Option<&gtk::Label>,
    info: &gio::FileInfo,
) {
    let names = if crate::tags::enabled() {
        crate::tags::of_info(info)
    } else {
        Vec::new()
    };
    let color = names
        .iter()
        .find_map(|n| crate::tags::color_of(n).filter(|c| !c.is_empty()));
    match name {
        Some(label) => wash(label.upcast_ref(), color.as_deref()),
        None => wash_row(cell.upcast_ref(), color),
    }
    if !names.is_empty() {
        crate::tags::note(&file_utils::file_of(info), &names);
    }
}

/// Give `widget` the wash class of `color`, or none.
pub(super) fn wash(widget: &gtk::Widget, color: Option<&str>) {
    for class in widget.css_classes() {
        if class.starts_with("spiral-tag-wash") {
            widget.remove_css_class(&class);
        }
    }
    if let Some(c) = color {
        widget.add_css_class("spiral-tag-wash");
        widget.add_css_class(&crate::tags::wash_class(c));
    }
}

/// Wash the row the cell sits in. A cell bound before it is in the window belongs to a
/// row widget GTK is still holding as a floating reference, and walking up to it from
/// here would sink that reference and drop the row; such a cell is done once the main
/// loop comes round, by which time the row is in the list.
pub(super) fn wash_row(cell: &gtk::Widget, color: Option<String>) {
    let apply = move |cell: &gtk::Widget| {
        if let Some(row) = row_widget(cell) {
            wash(&row, color.as_deref());
        }
    };
    if cell.root().is_some() {
        apply(cell);
    } else {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            cell,
            move || apply(&cell)
        ));
    }
}

/// The name label of a grid cell, which carries its wash; a list cell has none.
pub(super) fn cell_name(cell: &gtk::Widget) -> Option<gtk::Label> {
    let mut child = cell.first_child();
    while let Some(c) = child {
        if c.has_css_class("spiral-grid-name") {
            return c.downcast().ok();
        }
        if let Some(found) = cell_name(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

pub(crate) fn unbind_icon(image: &gtk::Image) {
    if let Some(handle) = THUMB_ABORT.take(image) {
        handle.abort();
    }
    if let Some(id) = THUMB_MAP.take(image) {
        image.disconnect(id);
    }
    image.remove_css_class("file-thumbnail");
}

/// Wait until the folder has stopped listing. A big folder is put in order once its
/// listing ends, so a thumbnail asked for before that is for a file about to move somewhere
/// else: opening a folder of fifty thousand files spent every one of its first requests
/// that way, on files that were nowhere near the screen by the time they were made. A
/// search is not waited for, since its results arrive for as long as it runs.
pub(super) async fn folder_listed(model: &FolderModel) {
    if !model.loading() || model.searching() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = model.connect_loading_notify(move |model| {
        if !model.loading()
            && let Some(tx) = tx.borrow_mut().take()
        {
            let _ = tx.send(());
        }
    });
    let _ = rx.await;
    model.disconnect(id);
}

/// Wait until `image` is on screen. The list widgets bind far more cells than they show:
/// they keep a pool of them, and the views on the stack pages that are not showing bind as
/// well, so binding a cell says nothing about anyone ever looking at it. A folder of a few
/// thousand pictures was handing twenty times as many files to a decoder as it displayed.
pub(super) async fn on_screen(image: &gtk::Image) {
    if image.is_mapped() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = image.connect_map(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
    THUMB_MAP.set(image, id);
    let _ = rx.await;
    if let Some(id) = THUMB_MAP.take(image) {
        image.disconnect(id);
    }
}

/// Cells keep a weak link to their `ListItem`: its position is live, unlike a cached
/// number, when items are inserted above it.
/// A filled star for a favourite, a hollow one otherwise.
pub(super) fn set_star(button: &gtk::Button, starred: bool) {
    button.set_icon_name(if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    // Some icon themes draw the empty star as solidly as the full one, which leaves the
    // column saying nothing; faded, the two are told apart whatever the theme draws.
    button.set_opacity(if starred { 1.0 } else { 0.45 });
}

/// What the star button offers, read off the star it shows when the pointer asks.
pub(super) fn star_tooltip(button: &gtk::Button) {
    button.set_has_tooltip(true);
    button.connect_query_tooltip(|button, _, _, _, tooltip| {
        let starred = button.icon_name().as_deref() == Some("starred-symbolic");
        tooltip.set_text(Some(&if starred {
            gettext("Remove from Starred")
        } else {
            gettext("Add to Starred")
        }));
        true
    });
}

/// The whole name on hover, for a label that had to cut it. Decided when the pointer
/// asks: setting a tooltip on a visible widget makes GTK look for the pointer through
/// the widget tree, a style lookup per cell per bind, which a scrolling grid paid for
/// every cell that came into view.
pub(crate) fn name_tooltip(label: &gtk::Label) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        let cut = label.layout().is_ellipsized();
        if cut {
            tooltip.set_text(Some(&label.text()));
        }
        cut
    });
}

/// Files waiting on the clipboard as a cut are dimmed.
pub(crate) fn set_cut(cell: &impl IsA<gtk::Widget>, info: &gio::FileInfo) {
    if crate::clipboard::is_cut(&file_utils::file_of(info)) {
        cell.add_css_class("spiral-cut");
    } else {
        cell.remove_css_class("spiral-cut");
    }
}

pub(crate) fn remember_list_item(cell: &impl IsA<gtk::Widget>, item: &gtk::ListItem) {
    LIST_ITEM.set(cell.upcast_ref::<gtk::Widget>(), item.downgrade());
}

/// A bound cell of the row under a point, whichever part of the row the point hits.
pub(crate) fn cell_at(root: &gtk::Widget, x: f64, y: f64) -> Option<gtk::Widget> {
    let row = row_widget(&root.pick(x, y, gtk::PickFlags::DEFAULT)?)?;
    let mut found: Option<gtk::Widget> = None;
    each_cell(&row, &mut |cell| {
        found.get_or_insert_with(|| cell.clone());
    });
    found
}

/// The list or grid item a widget sits in: the child of the list itself.
pub(crate) fn row_widget(inner: &gtk::Widget) -> Option<gtk::Widget> {
    let mut w = inner.clone();
    while let Some(parent) = w.parent() {
        if parent.is::<gtk::ListView>() || parent.is::<gtk::GridView>() {
            return Some(w);
        }
        w = parent;
    }
    None
}

/// Visit every bound cell under `root`. Cells do not nest, so a match ends that branch.
pub(super) fn each_cell(root: &gtk::Widget, f: &mut impl FnMut(&gtk::Widget)) {
    let mut child = root.first_child();
    while let Some(c) = child {
        if LIST_ITEM.has(&c) {
            f(&c);
        } else {
            each_cell(&c, f);
        }
        child = c.next_sibling();
    }
}

/// The lock emblem of a grid or list name cell: its last child, or the icon row's.
pub(super) fn cell_emblem(cell: &gtk::Widget) -> Option<gtk::Image> {
    cell.last_child().and_downcast::<gtk::Image>().or_else(|| {
        cell.first_child()?
            .last_child()
            .and_downcast::<gtk::Image>()
    })
}

pub(crate) fn cell_position(cell: &impl IsA<gtk::Widget>) -> Option<u32> {
    let pos = LIST_ITEM
        .get(cell.upcast_ref::<gtk::Widget>())?
        .upgrade()?
        .position();
    (pos != gtk::INVALID_LIST_POSITION).then_some(pos)
}

impl BrowserView {
    /// Caption lines under the name, per the `captions` setting. Folder item counts arrive async.
    pub(super) fn bind_captions(&self, label: &gtk::Label, info: &gio::FileInfo) {
        unbind_captions(label);
        let kinds: Vec<String> = self
            .imp()
            .settings
            .strv("captions")
            .iter()
            .map(|s| s.to_string())
            .filter(|s| s != "none")
            .collect();
        if kinds.is_empty() {
            label.set_visible(false);
            return;
        }
        let lines: Vec<Option<String>> =
            kinds.iter().map(|k| file_utils::caption(info, k)).collect();
        let render = |lines: &[Option<String>]| {
            lines
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        label.set_text(&render(&lines));
        label.set_visible(true);
        if let Some(idx) = kinds.iter().position(|k| k == "size")
            && file_utils::is_dir(info)
            && crate::prefs::counts_for(&file_utils::file_of(info))
        {
            let dir = file_utils::file_of(info);
            let stamp = info
                .modification_date_time()
                .map(|d| d.to_unix())
                .unwrap_or(0);
            let (fut, handle) = futures_util::future::abortable(glib::clone!(
                #[weak]
                label,
                async move {
                    let count = count_children_at(&dir, stamp).await;
                    let mut lines = lines;
                    lines[idx] = count.map(file_utils::items_string);
                    label.set_text(
                        &lines
                            .iter()
                            .flatten()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            ));
            COUNT_ABORT.set(label, handle);
            glib::spawn_future_local(async move {
                let _ = fut.await;
            });
        }
    }

    /// Icon now, thumbnail later (cancelled on unbind); the lock emblem for files that
    /// cannot be read or changed.
    pub(crate) fn bind_icon(
        &self,
        image: &gtk::Image,
        emblem: &gtk::Image,
        info: &gio::FileInfo,
        at: u32,
    ) {
        unbind_icon(image);
        set_emblem(
            emblem,
            file_utils::is_locked(info, self.imp().can_write.get()),
        );
        image.set_from_gicon(&crate::file_utils::icon_of(info));
        if file_utils::is_hidden(info) {
            image.add_css_class("hidden-file");
        } else {
            image.remove_css_class("hidden-file");
        }
        let info = info.clone();
        let model = self.model();
        let (fut, handle) = futures_util::future::abortable(glib::clone!(
            #[weak]
            image,
            async move {
                on_screen(&image).await;
                folder_listed(&model).await;
                if let Some(file) = file_utils::custom_icon_file(&info)
                    && let Some(icon) = file_utils::custom_icon(&file).await
                {
                    image.set_from_gicon(&icon);
                }
                if let Some(texture) = crate::thumbnails::load(&info, at).await {
                    image.set_paintable(Some(&texture));
                    image.add_css_class("file-thumbnail");
                }
            }
        ));
        THUMB_ABORT.set(image, handle);
        // Low priority, as the folder appearing matters more than the pictures in it.
        glib::MainContext::default().spawn_local_with_priority(glib::Priority::LOW, async move {
            let _ = fut.await;
        });
    }

    /// Re-apply the cell state that lives outside the file info: lock emblems, cut
    /// dimming, stars. The rows keep their objects, so the factories never rebind them.
    pub(crate) fn refresh_cells(&self) {
        let imp = self.imp();
        let writable = imp.can_write.get();
        let roots: [gtk::Widget; 3] = [
            imp.grid_view.clone().upcast(),
            imp.column_view.clone().upcast(),
            imp.miller_list.clone().upcast(),
        ];
        for root in roots {
            each_cell(&root, &mut |cell| {
                let Some(info) = cell_position(cell).and_then(|p| self.model().info_at(p)) else {
                    return;
                };
                set_cut(cell, &info);
                if let Some(button) = cell.downcast_ref::<gtk::Button>() {
                    set_star(
                        button,
                        crate::starred::is_starred(&file_utils::file_of(&info)),
                    );
                } else if let Some(emblem) = cell_emblem(cell) {
                    set_emblem(&emblem, file_utils::is_locked(&info, writable));
                    bind_tags(cell, cell_name(cell).as_ref(), &info);
                }
            });
        }
        self.refresh_side_cells();
    }
}
