//! The grid's tiles and the list's columns: how they are built, which columns show, and the
//! sort the header shows.

use super::*;

/// Columns the grid may have at most, whatever fits; the `max-columns` of the template.
pub(super) const GRID_MAX_COLUMNS: i32 = 20;

/// What the name column is left with before the columns beside it start giving way.
pub(super) const NAME_MIN_WIDTH: i32 = 220;

/// The star column: one flat button wide.
pub(super) const STAR_WIDTH: i32 = 40;

impl BrowserView {
    pub(super) fn setup_grid_factory(&self) {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let image = gtk::Image::builder()
                .pixel_size(view.icon_size())
                .css_classes(["spiral-image"])
                .build();
            view.bind_property("icon-size", &image, "pixel-size")
                .sync_create()
                .build();
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .lines(3)
                .max_width_chars(1)
                .justify(gtk::Justification::Center)
                .css_classes(["spiral-grid-name"])
                .build();
            name_tooltip(&label);
            view.bind_property("icon-size", &label, "width-request")
                .sync_create()
                .build();
            let captions = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .lines(3)
                .max_width_chars(1)
                .justify(gtk::Justification::Center)
                .visible(false)
                .css_classes(["caption", "dim-label"])
                .build();
            view.bind_property("icon-size", &captions, "width-request")
                .sync_create()
                .build();
            let labels = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            labels.append(&label);
            labels.append(&captions);
            // Icon between two emblem-wide margins, the lock stacked at the top of the
            // right one.
            image.set_margin_start(EMBLEM_MARGIN);
            image.set_hexpand(true);
            let emblem = emblem_image();
            emblem.set_width_request(EMBLEM_MARGIN);
            emblem.set_valign(gtk::Align::Start);
            let icon_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            icon_row.append(&image);
            icon_row.append(&emblem);
            let bx = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .css_classes(["spiral-view-cell"])
                .build();
            bx.append(&icon_row);
            bx.append(&labels);
            item.set_child(Some(&bx));
            remember_list_item(&bx, item);
            view.setup_cell_dnd(&bx);
        });
        let view = self.downgrade();
        factory.connect_bind(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let bx = item.child().unwrap();
            let icon_row = bx.first_child().unwrap();
            let image = icon_row.first_child().and_downcast::<gtk::Image>().unwrap();
            let emblem = icon_row.last_child().and_downcast::<gtk::Image>().unwrap();
            let labels = bx.last_child().unwrap();
            let label = labels.first_child().and_downcast::<gtk::Label>().unwrap();
            let captions = labels.last_child().and_downcast::<gtk::Label>().unwrap();
            view.bind_icon(&image, &emblem, &info, item.position());
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            item.set_accessible_label(&info.display_name());
            bind_tags(&bx, Some(&label), &info);
            view.bind_captions(&captions, &info);
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(bx) = item.child() else { return };
            if let Some(image) = bx
                .first_child()
                .and_then(|row| row.first_child())
                .and_downcast::<gtk::Image>()
            {
                unbind_icon(&image);
            }
            if let Some(captions) = bx
                .last_child()
                .and_then(|l| l.last_child())
                .and_downcast::<gtk::Label>()
            {
                unbind_captions(&captions);
            }
        });
        self.imp().grid_view.set_factory(Some(&factory));
    }

    /// Let the grid have as many columns as fit, and no more. It keeps thirty rows of
    /// cells bound for as many columns as it may ever have, so left with the twenty a
    /// wide window at the smallest zoom can hold, a window showing forty cells binds six
    /// hundred every time the folder changes.
    pub(super) fn fit_grid_columns(&self) {
        let imp = self.imp();
        let Some(width) = imp.grid_view.hadjustment().map(|a| a.page_size() as i32) else {
            return;
        };
        if width <= 0 {
            return;
        }
        // A cell is taken to be the icon between its emblem margins, a little less than
        // it is with its padding, so the count errs towards a column too many rather than
        // one too few.
        let cell = self.icon_size() + 2 * EMBLEM_MARGIN;
        imp.grid_view
            .set_max_columns((width / cell).clamp(1, GRID_MAX_COLUMNS) as u32);
    }

    pub(super) fn setup_columns(&self) {
        let cv = &self.imp().column_view;
        let name_factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        name_factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let bx = gtk::Box::builder()
                .spacing(6)
                .css_classes(["spiral-view-cell"])
                .build();
            let image = gtk::Image::builder()
                .pixel_size(view.list_icon_size())
                .css_classes(["spiral-image"])
                .build();
            view.bind_property("list-icon-size", &image, "pixel-size")
                .sync_create()
                .build();
            bx.append(&image);
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            name_tooltip(&label);
            bx.append(&label);
            bx.append(&emblem_image());
            // Folders unfold in place when the tree preference is on.
            let expander = gtk::TreeExpander::builder().child(&bx).build();
            item.set_child(Some(&expander));
            remember_list_item(&bx, item);
            view.setup_cell_dnd(&bx);
        });
        let view = self.downgrade();
        name_factory.connect_bind(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
            expander.set_list_row(item.item().and_downcast::<gtk::TreeListRow>().as_ref());
            expander.set_hide_expander(!crate::prefs::tree_view());
            let bx = expander.child().unwrap();
            let image = bx.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = image.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let emblem = bx.last_child().and_downcast::<gtk::Image>().unwrap();
            view.bind_icon(&image, &emblem, &info, item.position());
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            bind_tags(&bx, None, &info);
        });
        name_factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
                return;
            };
            expander.set_list_row(None);
            if let Some(image) = expander
                .child()
                .and_then(|b| b.first_child())
                .and_downcast::<gtk::Image>()
            {
                unbind_icon(&image);
            }
        });
        let name_col = gtk::ColumnViewColumn::new(Some(&gettext("Name")), Some(name_factory));
        name_col.set_expand(true);
        cv.append_column(&name_col);

        // `tip` fills in what an abbreviated column drops, on hover.
        let text_col = |title: String,
                        xalign: f32,
                        ellipsize: gtk::pango::EllipsizeMode,
                        f: fn(&gio::FileInfo) -> String,
                        tip: Option<fn(&gio::FileInfo) -> String>| {
            let factory = gtk::SignalListItemFactory::new();
            let view = self.downgrade();
            factory.connect_setup(move |_, item| {
                let Some(view) = view.upgrade() else { return };
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let label = gtk::Label::builder()
                    .xalign(xalign)
                    .ellipsize(ellipsize)
                    // Let the column width decide, not the longest value in it.
                    .max_width_chars(1)
                    .css_classes(["spiral-view-cell", "dim-label"])
                    .build();
                if xalign > 0.5 {
                    label.add_css_class("numeric");
                }
                if let Some(tip) = tip {
                    label.set_has_tooltip(true);
                    label.connect_query_tooltip(glib::clone!(
                        #[weak]
                        item,
                        #[upgrade_or]
                        false,
                        move |_, _, _, _, tooltip| {
                            let Some(info) =
                                item.item().and_then(|o| crate::folder_model::info_of(&o))
                            else {
                                return false;
                            };
                            tooltip.set_text(Some(&tip(&info)));
                            true
                        }
                    ));
                }
                item.set_child(Some(&label));
                remember_list_item(&label, item);
                view.setup_cell_dnd(&label);
            });
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                    return;
                };
                let label = item.child().and_downcast::<gtk::Label>().unwrap();
                set_cut(&label, &info);
                label.set_text(&f(&info));
            });
            gtk::ColumnViewColumn::new(Some(&title), Some(factory))
        };
        let text = |key: &str| -> fn(&gio::FileInfo) -> String {
            match key {
                "size" => file_utils::size_string,
                "type" => file_utils::short_type_string,
                "modified" => file_utils::modified_string,
                "accessed" => file_utils::accessed_string,
                "created" => file_utils::created_string,
                "owner" => |i| file_utils::caption(i, "owner").unwrap_or_default(),
                "group" => |i| file_utils::caption(i, "group").unwrap_or_default(),
                _ => |i| file_utils::permissions_string(i).unwrap_or_default(),
            }
        };
        // Widths that fit the usual values; the name column keeps the rest.
        let width = |key: &str| match key {
            "size" => 88,
            "owner" | "group" => 104,
            "permissions" => 112,
            "type" => 92,
            _ => 148,
        };
        let mut columns: Vec<(&'static str, gtk::ColumnViewColumn)> = Vec::new();
        for (key, title) in file_utils::optional_columns() {
            let col = text_col(
                title,
                if key == "size" { 1.0 } else { 0.0 },
                gtk::pango::EllipsizeMode::End,
                text(key),
                (key == "type").then_some(file_utils::type_string as fn(&gio::FileInfo) -> String),
            );
            col.set_fixed_width(width(key));
            col.set_resizable(true);
            cv.append_column(&col);
            columns.push((key, col));
        }
        let star_col = self.star_column();
        cv.append_column(&star_col);
        columns.push(("star", star_col));
        // Search results come from anywhere below the folder; say where. A chooser searches
        // the one folder, so every result is in it and the column would say the same thing
        // on every row.
        let searchable_below = !self.chooser_mode();
        let location_col = text_col(
            gettext("Location"),
            0.0,
            gtk::pango::EllipsizeMode::Middle,
            file_utils::location_of,
            Some(file_utils::location_of),
        );
        location_col.set_visible(false);
        location_col.set_expand(true);
        // The trash says where each item was and when it was trashed.
        let trashed_on_col = text_col(
            gettext("Trashed On"),
            0.0,
            gtk::pango::EllipsizeMode::End,
            file_utils::trashed_on_string,
            None,
        );
        let trashed_from_col = text_col(
            gettext("Original Location"),
            0.0,
            gtk::pango::EllipsizeMode::Middle,
            file_utils::trashed_from,
            Some(file_utils::trashed_from),
        );
        for (key, col, width) in [
            ("trashed-on", &trashed_on_col, 148),
            ("trashed-from", &trashed_from_col, 200),
        ] {
            col.set_fixed_width(width);
            col.set_resizable(true);
            cv.insert_column(1, col);
            // First, so the room they need is found before the others are given any.
            columns.insert(0, (key, col.clone()));
        }
        cv.insert_column(1, &location_col);
        self.imp().model.connect_searching_notify(glib::clone!(
            #[weak]
            location_col,
            move |m| location_col.set_visible(searchable_below && m.searching())
        ));
        let column = |key: &str| {
            columns
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, col)| col.clone())
                .unwrap()
        };
        let (size_col, type_col, mod_col) = (column("size"), column("type"), column("modified"));
        self.imp().columns.replace(columns);
        self.apply_visible_columns();
        self.imp().settings.connect_changed(
            Some("visible-columns"),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _| view.apply_visible_columns()
            ),
        );
        self.connect_location_notify(|view| view.queue_visible_columns());
        // Resizing the window redivides the width; the page size of the scroll is what
        // the list actually got.
        self.imp()
            .list_scroll
            .hadjustment()
            .connect_page_size_notify(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| view.queue_visible_columns()
            ));

        // Header clicks drive FolderModel sort props instead of the column view's own sorter.
        for (col, key) in [
            (&name_col, SortKey::Name),
            (&size_col, SortKey::Size),
            (&type_col, SortKey::Type),
            (&mod_col, SortKey::Modified),
            (&trashed_on_col, SortKey::Trashed),
        ] {
            let sorter = gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal);
            col.set_sorter(Some(&sorter));
            SORT_KEY.set(col, key);
        }
        let cv_sorter = cv.sorter().and_downcast::<gtk::ColumnViewSorter>().unwrap();
        cv_sorter.connect_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |s, _| {
                if view.imp().syncing_header.get() {
                    return;
                }
                let Some(col) = s.primary_sort_column() else {
                    return;
                };
                let Some(key) = SORT_KEY.get(&col) else {
                    return;
                };
                let reversed = s.primary_sort_order() == gtk::SortType::Descending;
                view.set_sort(key, reversed);
            }
        ));
        // The header arrow follows the model, whichever way the order was set.
        for prop in ["sort-key", "sort-reversed"] {
            self.imp().model.connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_, _| view.sync_sort_header()
                ),
            );
        }
        self.sync_sort_header();
    }

    /// The width is learnt while the list is being given it, and hiding a column then leaves
    /// the layout half done; the next idle is soon enough and outside the allocation.
    pub(super) fn queue_visible_columns(&self) {
        if self.imp().fit_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().fit_pending.set(false);
                view.apply_visible_columns();
            }
        ));
    }

    /// Show the columns the `visible-columns` key names, as many of them as there is room
    /// for. The name column takes what the others leave, so in a narrow window they are
    /// dropped from the right until the name is readable again, rather than the name
    /// shrinking to an ellipsis or the list running off the edge.
    pub(super) fn apply_visible_columns(&self) {
        let imp = self.imp();
        let on = imp.settings.strv("visible-columns");
        // The columns of the trash are there in the trash, whatever the key says.
        let in_trash = self
            .location()
            .is_some_and(|l| l.uri().starts_with("trash:"));
        let width = imp.list_scroll.width();
        // Before the first allocation there is no width to divide; the key decides alone.
        let mut room = if width > 0 {
            width - NAME_MIN_WIDTH
        } else {
            i32::MAX
        };
        for (key, col) in imp.columns.borrow().iter() {
            // The star is a button wide and worth its place at any size.
            let cost = if *key == "star" { 0 } else { col.fixed_width() };
            let wanted = match *key {
                "trashed-on" | "trashed-from" => in_trash,
                _ => on.iter().any(|k| k == key),
            };
            let show = wanted && cost <= room;
            if show {
                room -= cost;
            }
            col.set_visible(show);
        }
    }

    /// A star per row that toggles the favourite.
    pub(super) fn star_column(&self) -> gtk::ColumnViewColumn {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let button = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .css_classes(["flat", "circular", "spiral-star"])
                .build();
            button.connect_clicked(glib::clone!(
                #[weak]
                item,
                #[weak]
                view,
                move |button| {
                    let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o))
                    else {
                        return;
                    };
                    let file = file_utils::file_of(&info);
                    let starred = !crate::starred::is_starred(&file);
                    crate::starred::set_starred(&file, starred);
                    set_star(button, starred);
                    if !starred {
                        view.offer_to_star_again(&[info]);
                    }
                }
            ));
            star_tooltip(&button);
            item.set_child(Some(&button));
            remember_list_item(&button, item);
            view.setup_cell_dnd(&button);
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let button = item.child().and_downcast::<gtk::Button>().unwrap();
            set_cut(&button, &info);
            set_star(
                &button,
                crate::starred::is_starred(&file_utils::file_of(&info)),
            );
        });
        // No header title and no more room than the button: the star is an icon people
        // recognise, and "Visible Columns" is where it is named.
        let column = gtk::ColumnViewColumn::new(None, Some(factory));
        column.set_fixed_width(STAR_WIDTH);
        column
    }

    pub(super) fn sync_sort_header(&self) {
        let imp = self.imp();
        let cv = &imp.column_view;
        let key = imp.model.sort_key();
        let order = if imp.model.sort_reversed() {
            gtk::SortType::Descending
        } else {
            gtk::SortType::Ascending
        };
        let col = cv
            .columns()
            .iter::<gtk::ColumnViewColumn>()
            .flatten()
            .find(|c| SORT_KEY.get(c) == Some(key));
        imp.syncing_header.set(true);
        // Clear first: the column view would otherwise keep the old column as a secondary sort.
        cv.sort_by_column(None::<&gtk::ColumnViewColumn>, order);
        cv.sort_by_column(col.as_ref(), order);
        imp.syncing_header.set(false);
    }
}
