//! The file chooser window shown for portal requests, built from Spiral's own browser widgets.

use std::cell::RefCell;
use std::rc::Rc;

use ashpd::PortalError;
use ashpd::desktop::file_chooser::{Choice, FileFilter, SelectedFiles};
use ashpd::{Uri, WindowIdentifierType};
use futures_util::FutureExt;
use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::browser_view::BrowserView;
use crate::object_data::Key;
use crate::places_sidebar::PlacesSidebar;
use crate::{adw, file_utils, gio, glib, gtk};

use super::{Kind, Request};

static CHOICE_KEYS: Key<Vec<String>> = Key::new("choice-keys");

enum Mode {
    Open { multiple: bool, directory: bool },
    Save,
    SaveFiles(Vec<std::path::PathBuf>),
}

/// Show a chooser for `req` and reply through its channel.
pub async fn handle(req: Request) {
    let Request {
        kind,
        title,
        parent,
        reply,
        closed,
        ..
    } = req;
    let result = run(kind, &title, parent, closed).await;
    let _ = reply.send(result);
}

async fn run(
    kind: Kind,
    title: &str,
    parent: Option<WindowIdentifierType>,
    closed: futures_channel::oneshot::Receiver<()>,
) -> Result<SelectedFiles, PortalError> {
    let settings = gio::Settings::new(crate::config::APP_ID);

    let (mode, accept_label, modal, filters, current_filter, choices, start, current_name) =
        match &kind {
            Kind::Open(o) => (
                Mode::Open {
                    multiple: o.multiple().unwrap_or(false),
                    directory: o.directory().unwrap_or(false),
                },
                o.accept_label().map(str::to_string),
                o.modal().unwrap_or(true),
                o.filters().to_vec(),
                o.current_filter().cloned(),
                o.choices().to_vec(),
                o.current_folder().map(gio::File::for_path),
                None,
            ),
            Kind::Save(o) => {
                let start = o
                    .current_file()
                    .and_then(|f| gio::File::for_path(f).parent())
                    .or_else(|| o.current_folder().map(gio::File::for_path));
                // Only the last part of a proposed name: a path in it would be saved to
                // somewhere other than the folder on screen, with nothing but the text of
                // the entry to say so.
                let name = o
                    .current_name()
                    .and_then(|n| std::path::Path::new(n).file_name())
                    .or_else(|| o.current_file().and_then(|f| f.as_ref().file_name()))
                    .map(|n| n.to_string_lossy().into_owned());
                (
                    Mode::Save,
                    o.accept_label().map(str::to_string),
                    o.modal().unwrap_or(true),
                    o.filters().to_vec(),
                    o.current_filter().cloned(),
                    o.choices().to_vec(),
                    start,
                    name,
                )
            }
            Kind::SaveFiles(o) => (
                Mode::SaveFiles(o.files().iter().map(|f| f.as_ref().to_path_buf()).collect()),
                o.accept_label().map(str::to_string),
                o.modal().unwrap_or(true),
                Vec::new(),
                None,
                o.choices().to_vec(),
                o.current_folder().map(gio::File::for_path),
                None,
            ),
        };
    // The filter asked for first need not be one of the list; it is then offered after
    // them, as GTK's own chooser does, and on its own where there is no list.
    let mut filters = filters;
    if let Some(cur) = &current_filter
        && !filters.contains(cur)
    {
        filters.push(cur.clone());
    }
    let mut start = start.unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
    let is_dir = start
        .query_info_future(
            "standard::type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .is_ok_and(|i| i.file_type() == gio::FileType::Directory);
    if !is_dir {
        start = gio::File::for_path(glib::home_dir());
    }

    // ---- widgets --------------------------------------------------------------------------
    let view = BrowserView::new_chooser(&start);
    // Picking items out by pattern belongs to a dialog that opens them. Where a name is
    // being saved, Ctrl+S is the key of the application the dialog was opened for, so the
    // action goes with it -- the view carries the key itself, so turning the action off is
    // what silences it.
    if !matches!(mode, Mode::Open { .. })
        && let Some(pattern) = view
            .action_group()
            .lookup_action("select-pattern")
            .and_downcast::<gio::SimpleAction>()
    {
        pattern.set_enabled(false);
    }
    let model = view.model();
    // One file asked for is one selected: a wider selection would be cut down to its first
    // file on the way out, which need not be the one meant.
    if let Mode::Open {
        multiple: false, ..
    } = mode
    {
        for name in ["select-all", "invert-selection", "select-pattern"] {
            view.withhold_action(name);
        }
        model
            .selection()
            .connect_selection_changed(|sel, position, n| {
                let selected = sel.selection();
                if selected.size() > 1 {
                    // The newest of it: the end of the range just changed that is selected.
                    let keep = (position..position + n)
                        .rev()
                        .find(|&p| selected.contains(p))
                        .unwrap_or_else(|| selected.minimum());
                    sel.select_item(keep, true);
                }
            });
    }
    // Making a folder is for choosing where things go, not for picking a file to open.
    if matches!(
        mode,
        Mode::Open {
            directory: false,
            ..
        }
    ) {
        view.withhold_action("new-folder");
    }
    let sidebar: PlacesSidebar = glib::Object::new();
    sidebar.set_in_dialog(true);
    if !matches!(mode, Mode::Open { .. }) {
        sidebar.set_hide_tags(true);
    }
    let location_bar = crate::location_entry::LocationBar::new(&view);

    let back = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text(gettext("Back"))
        .valign(gtk::Align::Center)
        .build();
    let forward = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text(gettext("Forward"))
        .valign(gtk::Align::Center)
        .build();
    view.bind_property("can-go-back", &back, "sensitive")
        .sync_create()
        .build();
    view.bind_property("can-go-forward", &forward, "sensitive")
        .sync_create()
        .build();
    back.connect_clicked(glib::clone!(
        #[weak]
        view,
        move |_| view.go_back()
    ));
    forward.connect_clicked(glib::clone!(
        #[weak]
        view,
        move |_| view.go_forward()
    ));
    let nav = gtk::Box::builder().spacing(6).build();
    nav.append(&back);
    nav.append(&forward);

    let view_button = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("List View"))
        .icon_name("view-list-symbolic")
        .build();
    {
        let sync = glib::clone!(
            #[weak]
            view_button,
            move |v: &BrowserView| {
                let next = v.view_mode().next();
                view_button.set_icon_name(next.icon());
                view_button.set_tooltip_text(Some(&next.label()));
            }
        );
        sync(&view);
        view.connect_view_mode_notify(sync);
        view_button.connect_clicked(glib::clone!(
            #[weak]
            view,
            move |_| view.toggle_view_mode()
        ));
    }
    // Sorting, in the dialog's own order: the file manager's windows keep theirs.
    let sort_action = gio::SimpleAction::new_stateful(
        "sort",
        Some(glib::VariantTy::STRING),
        &sort_state(&settings).to_variant(),
    );
    sort_action.connect_activate(glib::clone!(
        #[weak]
        view,
        move |action, value| {
            let Some((key, dir)) = value.and_then(|v| v.str()).and_then(|v| v.split_once('-'))
            else {
                return;
            };
            if let Some(key) = crate::enums::SortKey::from_nick(key) {
                view.set_sort(key, dir == "desc");
                action.set_state(&format!("{}-{dir}", key.nick()).to_variant());
            }
        }
    ));
    let sort_menu = gio::Menu::new();
    for (label, target) in [
        (gettext("_A-Z"), "name-asc"),
        (gettext("_Z-A"), "name-desc"),
        (gettext("Last _Modified"), "modified-desc"),
        (gettext("_First Modified"), "modified-asc"),
        (gettext("_Size"), "size-desc"),
        (gettext("_Type"), "type-asc"),
    ] {
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(Some("chooser.sort"), Some(&target.to_variant()));
        sort_menu.append_item(&item);
    }
    let hidden_menu = gio::Menu::new();
    hidden_menu.append(
        Some(&gettext("Show _Hidden Files")),
        Some("chooser.show-hidden"),
    );
    let view_menu = gio::Menu::new();
    view_menu.append_section(None, &sort_menu);
    view_menu.append_section(None, &hidden_menu);
    let sort_button = gtk::MenuButton::builder()
        .icon_name("view-sort-descending-symbolic")
        .tooltip_text(gettext("Sort"))
        .valign(gtk::Align::Center)
        .menu_model(&view_menu)
        .build();

    // Search: the folder being shown, nothing under it.
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search this folder"))
        .hexpand(true)
        .build();
    let search_bar = gtk::SearchBar::builder()
        .child(&search_entry)
        .show_close_button(false)
        .build();
    let search_button = gtk::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text(gettext("Search"))
        .valign(gtk::Align::Center)
        .build();
    search_button
        .bind_property("active", &search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();
    search_entry.connect_search_changed(glib::clone!(
        #[weak]
        model,
        move |entry| model.set_search_text(entry.text().as_str())
    ));
    search_bar.connect_search_mode_enabled_notify(glib::clone!(
        #[weak]
        search_entry,
        #[weak]
        model,
        #[weak]
        view,
        move |bar| {
            if bar.is_search_mode() {
                search_entry.grab_focus();
            } else {
                search_entry.set_text("");
                model.set_search_text("");
                view.grab_view_focus();
            }
        }
    ));
    // A search belongs to the folder it was typed in; leaving ends it.
    view.connect_location_notify(glib::clone!(
        #[weak]
        search_button,
        move |_| search_button.set_active(false)
    ));

    let new_folder = gtk::Button::builder()
        .icon_name("folder-new-symbolic")
        .tooltip_text(gettext("New Folder"))
        .action_name("view.new-folder")
        .valign(gtk::Align::Center)
        .visible(
            matches!(mode, Mode::Save | Mode::SaveFiles(_))
                || matches!(
                    mode,
                    Mode::Open {
                        directory: true,
                        ..
                    }
                ),
        )
        .build();

    // Narrow enough and the sidebar goes over the folders instead of beside them; this
    // is how it is asked back.
    let show_sidebar = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(gettext("Show Sidebar"))
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(location_bar.widget())
        .build();
    header.pack_start(&show_sidebar);
    header.pack_start(&nav);
    header.pack_end(&view_button);
    header.pack_end(&new_folder);
    header.pack_end(&sort_button);
    header.pack_end(&search_button);

    // Bottom bar: filters | file name | choices + accept.
    let accept = gtk::Button::builder()
        .label(accept_label.unwrap_or_else(|| match mode {
            Mode::Open { .. } => gettext("_Open"),
            Mode::Save | Mode::SaveFiles(_) => gettext("_Save"),
        }))
        .use_underline(true)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .css_classes(["pill", "suggested-action"])
        .build();

    let filter_names = gtk::StringList::new(&filters.iter().map(|f| f.label()).collect::<Vec<_>>());
    let filter_dropdown = gtk::DropDown::builder()
        .model(&filter_names)
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Visible Files Filter"))
        .visible(!filters.is_empty())
        .css_classes(["flat-dropdown"])
        .build();
    if let Some(cur) = &current_filter
        && let Some(i) = filters.iter().position(|f| f == cur)
    {
        filter_dropdown.set_selected(i as u32);
    }
    let directory = matches!(
        mode,
        Mode::Open {
            directory: true,
            ..
        }
    );
    let apply_filter = {
        let filters = filters.clone();
        glib::clone!(
            #[weak]
            model,
            move |dd: &gtk::DropDown| {
                let selected = filters.get(dd.selected() as usize).cloned();
                model.set_extra_filter(Some(make_filter(selected, directory)));
            }
        )
    };
    apply_filter(&filter_dropdown);
    filter_dropdown.connect_selected_notify(apply_filter);

    let name_entry = gtk::Entry::builder()
        .valign(gtk::Align::Center)
        .max_width_chars(30)
        .activates_default(true)
        .tooltip_text(gettext("File Name"))
        .visible(matches!(mode, Mode::Save))
        .text(current_name.clone().unwrap_or_default())
        .build();

    // Choices: combos as dropdowns, booleans as check buttons, inside a menu button.
    let choice_widgets: Vec<(String, gtk::Widget)> = choices
        .iter()
        .map(|c| (c.id().to_string(), choice_widget(c)))
        .collect();
    let choices_button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(gettext("Show Options"))
        .valign(gtk::Align::Center)
        .visible(!choices.is_empty())
        .build();
    if !choice_widgets.is_empty() {
        let bx = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        for (_, w) in &choice_widgets {
            bx.append(w);
        }
        choices_button.set_popover(Some(&gtk::Popover::builder().child(&bx).build()));
    }

    let start_box = gtk::Box::builder().css_classes(["toolbar"]).build();
    start_box.append(&filter_dropdown);
    let end_box = gtk::Box::builder().css_classes(["toolbar"]).build();
    end_box.append(&choices_button);
    end_box.append(&accept);
    let bottom = gtk::CenterBox::builder()
        .start_widget(&start_box)
        .center_widget(&name_entry)
        .end_widget(&end_box)
        .build();

    let content_toolbar = adw::ToolbarView::new();
    content_toolbar.add_top_bar(&header);
    content_toolbar.add_top_bar(&search_bar);
    content_toolbar.set_content(Some(&view));
    content_toolbar.add_bottom_bar(&bottom);

    let sidebar_header = adw::HeaderBar::builder()
        .title_widget(
            &gtk::Label::builder()
                .label(title)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .single_line_mode(true)
                .css_classes(["title"])
                .build(),
        )
        .build();
    let sidebar_toolbar = adw::ToolbarView::new();
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.set_content(Some(&sidebar));

    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_toolbar)
        .content(&content_toolbar)
        .sidebar_width_fraction(0.2)
        .min_sidebar_width(180.0)
        .max_sidebar_width(300.0)
        .css_classes(["view"])
        .build();
    let (w, h) = settings.get::<(i32, i32)>("window-size");
    let window = adw::Window::builder()
        .title(title)
        .default_width(w.min(1000))
        .default_height(h.min(700))
        .width_request(360)
        .height_request(348)
        .content(&split)
        .default_widget(&accept)
        .modal(modal)
        .css_classes(["spiral-file-chooser"])
        .build();
    window.insert_action_group("view", Some(view.action_group()));
    let chooser_actions = gio::SimpleActionGroup::new();
    chooser_actions.add_action(&sort_action);
    // Hidden files are the file manager's own setting, the one the view is already
    // reading: a dialog that shows them is a dialog of a file manager that does.
    chooser_actions.add_action(&settings.create_action("show-hidden"));
    window.insert_action_group("chooser", Some(&chooser_actions));
    // Typing in the view starts a search, as it does in the file manager; the search bar
    // leaves the keys alone while they are going into the name entry.
    search_bar.set_key_capture_widget(Some(&window));
    split
        .bind_property("show-sidebar", &show_sidebar, "active")
        .bidirectional()
        .sync_create()
        .build();
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        682.0,
        adw::LengthUnit::Sp,
    ));
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    bp.add_setter(
        &split,
        "sidebar-width-unit",
        Some(&adw::LengthUnit::Px.to_value()),
    );
    bp.add_setter(&show_sidebar, "visible", Some(&true.to_value()));
    window.add_breakpoint(bp);

    // ---- wiring ---------------------------------------------------------------------------
    sidebar.connect_open_location(glib::clone!(
        #[weak]
        view,
        #[weak]
        split,
        move |_, f, _| {
            view.go_to(f);
            // Over the folders, the sidebar is in the way once it has been used.
            if split.is_collapsed() {
                split.set_show_sidebar(false);
            }
        }
    ));
    view.connect_location_notify(glib::clone!(
        #[weak]
        sidebar,
        move |v| sidebar.set_selected_location(v.location().as_ref())
    ));
    sidebar.set_selected_location(Some(&start));
    if let Mode::Save = mode {
        // Clicking a file proposes its name.
        model.selection().connect_selection_changed(glib::clone!(
            #[weak]
            model,
            #[weak]
            name_entry,
            move |_, _, _| {
                if let [info] = model.selected_infos().as_slice()
                    && !file_utils::is_dir(info)
                {
                    name_entry.set_text(&info.display_name());
                }
            }
        ));
    }

    let result: Rc<RefCell<Option<Result<SelectedFiles, PortalError>>>> =
        Rc::new(RefCell::new(None));
    let (done_tx, done_rx) = futures_channel::oneshot::channel::<()>();
    let done_tx = Rc::new(RefCell::new(Some(done_tx)));
    // Weak, because the window's own close request and buttons hold these closures: a
    // strong reference from there would keep the window and its view alive for good.
    let finish = Rc::new(glib::clone!(
        #[strong]
        result,
        #[strong]
        done_tx,
        #[weak]
        window,
        move |r: Result<SelectedFiles, PortalError>| {
            if result.borrow().is_none() {
                result.replace(Some(r));
                if let Some(tx) = done_tx.borrow_mut().take() {
                    let _ = tx.send(());
                }
                window.close();
            }
        }
    ));

    let collect_choices = {
        let choice_widgets = choice_widgets.clone();
        let filter_dropdown = filter_dropdown.clone();
        let filters = filters.clone();
        move |mut sel: SelectedFiles| {
            for (id, w) in &choice_widgets {
                // Combo choices are a label plus dropdown box; the dropdown is the last child.
                let dropdown = w.last_child().and_downcast::<gtk::DropDown>();
                let value = if let Some(dd) = dropdown {
                    CHOICE_KEYS
                        .get(&dd)
                        .and_then(|keys| keys.get(dd.selected() as usize).cloned())
                        .unwrap_or_default()
                } else if let Some(cb) = w.downcast_ref::<gtk::CheckButton>() {
                    if cb.is_active() {
                        "true".into()
                    } else {
                        "false".into()
                    }
                } else {
                    String::new()
                };
                sel = sel.choice(id, &value);
            }
            if let Some(f) = filters.get(filter_dropdown.selected() as usize) {
                sel = sel.current_filter(Some(f.clone()));
            }
            sel
        }
    };

    let mode = Rc::new(mode);
    // `activated` is the file opened in the view, which is not always one selected there:
    // an entry of the network folder opens the file it stands for.
    let on_accept: Rc<dyn Fn(Option<gio::File>)> = Rc::new(glib::clone!(
        #[strong]
        finish,
        #[weak]
        view,
        #[weak]
        name_entry,
        #[weak]
        window,
        #[strong]
        mode,
        move |activated| {
            let Some(location) = view.location() else {
                return;
            };
            let infos = view.model().selected_infos();
            let (finish, collect, window) =
                (finish.clone(), collect_choices.clone(), window.clone());
            match &*mode {
                Mode::Open {
                    multiple,
                    directory,
                } => {
                    let mut files: Vec<gio::File> = match activated {
                        Some(f)
                            if !*directory
                                && !infos.iter().any(|i| file_utils::file_of(i).equal(&f)) =>
                        {
                            vec![f]
                        }
                        _ => infos
                            .iter()
                            .filter(|i| *directory == file_utils::is_dir(i))
                            .map(file_utils::file_of)
                            .collect(),
                    };
                    if files.is_empty() && *directory {
                        files.push(location);
                    }
                    if files.is_empty() {
                        // Accepting with only a folder selected enters it, as GTK's chooser does.
                        if let [info] = infos.as_slice()
                            && file_utils::is_dir(info)
                        {
                            view.go_to(&file_utils::file_of(info));
                        }
                        return;
                    }
                    if !*multiple {
                        files.truncate(1);
                    }
                    let Some(uris) = files.iter().map(local_uri).collect::<Option<Vec<_>>>() else {
                        glib::spawn_future_local(not_passable(window));
                        return;
                    };
                    let sel = uris
                        .into_iter()
                        .fold(SelectedFiles::default(), |s, u| s.uri(u));
                    finish(Ok(collect(sel)));
                }
                Mode::Save => {
                    let text = name_entry.text().trim().to_string();
                    if text.is_empty() || text == "." || text == ".." {
                        name_entry.add_css_class("error");
                        return;
                    }
                    // A path typed in place of a name: from the folder shown, or from the
                    // home folder or the root as the location bar takes it.
                    let dest = if !text.contains('/') {
                        location.child(&text)
                    } else if text.starts_with('/') || text.starts_with('~') {
                        crate::location_entry::resolve(&text)
                    } else {
                        location.resolve_relative_path(&text)
                    };
                    glib::spawn_future_local(async move {
                        let kind = file_type(&dest).await;
                        // The name of a folder is a place to save in, not a file to
                        // replace: go there, as GTK's chooser does.
                        if kind == Some(gio::FileType::Directory) {
                            view.go_to(&dest);
                            name_entry.set_text("");
                            name_entry.grab_focus();
                            return;
                        }
                        let Some(parent) = dest.parent() else { return };
                        if file_type(&parent).await != Some(gio::FileType::Directory) {
                            name_entry.add_css_class("error");
                            return;
                        }
                        let Some(uri) =
                            passable_folder(&parent).then(|| local_uri(&dest)).flatten()
                        else {
                            not_passable(window).await;
                            return;
                        };
                        if kind.is_some()
                            && !confirm_replace(&window, &[crate::ops::name(&dest)]).await
                        {
                            return;
                        }
                        finish(Ok(collect(SelectedFiles::default().uri(uri))));
                    });
                }
                Mode::SaveFiles(names) => {
                    // A folder selected is the one being chosen, as it is when a folder is
                    // asked for; otherwise the one shown.
                    let folder = match infos.as_slice() {
                        [info] if file_utils::is_dir(info) => file_utils::file_of(info),
                        _ => location,
                    };
                    let dests: Vec<gio::File> = names
                        .iter()
                        .filter_map(|n| n.file_name())
                        .map(|base| folder.child(base))
                        .collect();
                    glib::spawn_future_local(async move {
                        let uris = passable_folder(&folder)
                            .then(|| dests.iter().map(local_uri).collect::<Option<Vec<_>>>())
                            .flatten();
                        let Some(uris) = uris else {
                            not_passable(window).await;
                            return;
                        };
                        let mut existing = Vec::new();
                        for dest in &dests {
                            if file_type(dest).await.is_some() {
                                existing.push(crate::ops::name(dest));
                            }
                        }
                        if !existing.is_empty() && !confirm_replace(&window, &existing).await {
                            return;
                        }
                        let sel = uris
                            .into_iter()
                            .fold(SelectedFiles::default(), |s, u| s.uri(u));
                        finish(Ok(collect(sel)));
                    });
                }
            }
        }
    ));
    accept.connect_clicked(glib::clone!(
        #[strong]
        on_accept,
        move |_| on_accept(None)
    ));
    view.connect_file_activated(glib::clone!(
        #[strong]
        on_accept,
        move |_, file| on_accept(Some(file.clone()))
    ));
    // A file typed into the location bar is the file being asked for: opened, or saved
    // under once the name is confirmed, whether or not it exists yet. Where a folder is
    // asked for, the default picks it out in its folder.
    match &*mode {
        Mode::Open {
            directory: false, ..
        } => location_bar.connect_file(glib::clone!(
            #[strong]
            on_accept,
            #[weak]
            view,
            move |file, exists| {
                if exists {
                    on_accept(Some(file.clone()));
                } else {
                    view.go_to(file);
                }
            }
        )),
        Mode::Save => location_bar.connect_file(glib::clone!(
            #[weak]
            view,
            #[weak]
            name_entry,
            move |file, _| {
                if let Some(parent) = file.parent() {
                    view.go_to(&parent);
                }
                name_entry.set_text(&crate::ops::name(file));
                name_entry.grab_focus();
            }
        )),
        _ => {}
    }
    // Nothing to accept yet: no name to save under, or nothing picked to open.
    match &*mode {
        Mode::Save => {
            let sync = |e: &gtk::Entry, accept: &gtk::Button| {
                accept.set_sensitive(!e.text().trim().is_empty());
            };
            sync(&name_entry, &accept);
            name_entry.connect_changed(glib::clone!(
                #[weak]
                accept,
                move |e| sync(e, &accept)
            ));
        }
        Mode::Open {
            directory: false, ..
        } => {
            let sync = |sel: &gtk::MultiSelection, accept: &gtk::Button| {
                accept.set_sensitive(!sel.selection().is_empty());
            };
            sync(&model.selection(), &accept);
            model.selection().connect_selection_changed(glib::clone!(
                #[weak]
                accept,
                move |sel, _, _| sync(sel, &accept)
            ));
            // Leaving a folder takes what was selected in it with the items, and says so
            // only as a change of items.
            model.selection().connect_items_changed(glib::clone!(
                #[weak]
                accept,
                move |sel, _, _, _| sync(sel, &accept)
            ));
        }
        _ => {}
    }
    name_entry.connect_changed(|e| e.remove_css_class("error"));
    window.connect_close_request(glib::clone!(
        #[strong]
        finish,
        move |_| {
            finish(Err(PortalError::Cancelled("dismissed".into())));
            glib::Propagation::Proceed
        }
    ));
    // The keys the file manager's windows answer to, for the ones the dialog has. The
    // view carries its own -- Ctrl+A, Delete and the rest -- but only while the keyboard
    // is in it, so the shortcuts a dialog needs from anywhere sit on the window.
    let keys = gtk::ShortcutController::new();
    keys.set_scope(gtk::ShortcutScope::Managed);
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    let add_key = |trigger: &str, f: Box<dyn Fn()>| {
        keys.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                f();
                glib::Propagation::Stop
            })),
        ));
    };
    add_key(
        "<Control>f",
        Box::new(glib::clone!(
            #[weak]
            search_button,
            move || search_button.set_active(true)
        )),
    );
    for trigger in ["<Control>r", "F5"] {
        add_key(
            trigger,
            Box::new(glib::clone!(
                #[weak]
                view,
                move || view.reload()
            )),
        );
    }
    add_key(
        "<Control>s",
        Box::new(glib::clone!(
            #[weak]
            view,
            move || {
                gtk::prelude::WidgetExt::activate_action(&view, "view.select-pattern", None).ok();
            }
        )),
    );
    add_key("<Control>l", Box::new(move || location_bar.edit()));
    // Named rather than a callback like the ones above: the menu item then shows the key.
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>h"),
        Some(gtk::NamedAction::new("chooser.show-hidden")),
    ));
    window.add_controller(keys);

    window.present();
    if let Some(WindowIdentifierType::Wayland(handle)) = parent
        && let Some(toplevel) = window
            .surface()
            .and_downcast::<gdk4_wayland::WaylandToplevel>()
        && !toplevel.set_transient_for_exported(&handle)
    {
        glib::g_debug!(
            "spiral",
            "compositor did not accept exported parent handle {handle}, showing unparented"
        );
    }
    if matches!(&*name_entry.text(), "") {
        view.grab_view_focus();
    } else {
        name_entry.grab_focus();
        name_entry.select_region(0, crate::naming::stem_end(&name_entry.text(), false));
    }

    // Wait for the user, or for the portal to close the request.
    let window_for_close = window.clone();
    futures_util::select! {
        _ = done_rx.fuse() => {}
        _ = closed.fuse() => {
            finish(Err(PortalError::Cancelled("closed by portal".into())));
            window_for_close.close();
        }
    }
    result
        .borrow_mut()
        .take()
        .unwrap_or(Err(PortalError::Cancelled(
            "chooser closed without a selection".into(),
        )))
}

/// The order the dialog was left in, as the "key-direction" the sort action takes.
fn sort_state(settings: &gio::Settings) -> String {
    let key = settings.string("chooser-sort-key");
    let dir = if settings.boolean("chooser-sort-reversed") {
        "desc"
    } else {
        "asc"
    };
    format!("{key}-{dir}")
}

/// Folders always pass; files must match a glob or MIME type of the filter.
fn make_filter(filter: Option<FileFilter>, directory: bool) -> gtk::Filter {
    let patterns: Vec<String> = filter
        .as_ref()
        .map(|f| {
            f.pattern_filters()
                .iter()
                .map(|p| p.to_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let mimes: Vec<String> = filter
        .as_ref()
        .map(|f| f.mimetype_filters().iter().map(|m| m.to_string()).collect())
        .unwrap_or_default();
    gtk::CustomFilter::new(move |obj| {
        let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
        if file_utils::is_dir(info) {
            return true;
        }
        if directory {
            return false;
        }
        if patterns.is_empty() && mimes.is_empty() {
            return true;
        }
        let name: Vec<char> = info.display_name().to_lowercase().chars().collect();
        if patterns
            .iter()
            .any(|p| glob_match(&p.chars().collect::<Vec<_>>(), &name))
        {
            return true;
        }
        crate::file_utils::content_type_of(info)
            .is_some_and(|ct| mimes.iter().any(|m| gio::content_type_is_a(&ct, m)))
    })
    .upcast()
}

/// Shell-style glob with `*`, `?` and `[...]` (what file filters use: GTK sends a suffix
/// as "*.[pP][nN][gG]").
fn glob_match(pat: &[char], text: &[char]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            glob_match(&pat[1..], text) || (!text.is_empty() && glob_match(pat, &text[1..]))
        }
        (Some('?'), Some(_)) => glob_match(&pat[1..], &text[1..]),
        (Some('['), Some(&c)) => match glob_class(&pat[1..], c) {
            Some((matched, rest)) => matched && glob_match(rest, &text[1..]),
            None => c == '[' && glob_match(&pat[1..], &text[1..]),
        },
        (Some(p), Some(t)) if p == t => glob_match(&pat[1..], &text[1..]),
        _ => false,
    }
}

/// Whether `c` is in the class a `[` opened, `pat` being what follows the `[`, and the
/// pattern after the class; `None` for a `[` that is never closed and stands for itself.
fn glob_class(pat: &[char], c: char) -> Option<(bool, &[char])> {
    let (negated, body) = match pat.first() {
        Some('!' | '^') => (true, &pat[1..]),
        _ => (false, pat),
    };
    // A `]` first is one of the characters, not the end.
    let end = body.iter().skip(1).position(|&x| x == ']')? + 1;
    let set = &body[..end];
    let mut found = false;
    let mut i = 0;
    while i < set.len() {
        if i + 2 < set.len() && set[i + 1] == '-' {
            found |= (set[i]..=set[i + 2]).contains(&c);
            i += 3;
        } else {
            found |= set[i] == c;
            i += 1;
        }
    }
    Some((found != negated, &body[end + 1..]))
}

fn choice_widget(c: &Choice) -> gtk::Widget {
    let pairs = c.pairs();
    if pairs.is_empty() {
        let cb = gtk::CheckButton::with_label(c.label());
        cb.set_active(c.initial_selection() == "true");
        return cb.upcast();
    }
    let labels: Vec<&str> = pairs.iter().map(|(_, l)| *l).collect();
    let keys: Vec<String> = pairs.iter().map(|(k, _)| k.to_string()).collect();
    let dd = gtk::DropDown::from_strings(&labels);
    if let Some(i) = keys.iter().position(|k| k == c.initial_selection()) {
        dd.set_selected(i as u32);
    }
    CHOICE_KEYS.set(&dd, keys);
    let bx = gtk::Box::builder().spacing(12).build();
    bx.append(
        &gtk::Label::builder()
            .label(c.label())
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    bx.append(&dd);
    bx.upcast()
}

async fn confirm_replace(parent: &impl IsA<gtk::Widget>, names: &[String]) -> bool {
    const SHOWN: usize = 5;
    let heading = match names {
        [name] => gettext("Replace “%s”?").replace("%s", name),
        _ => ngettext("Replace %d File?", "Replace %d Files?", names.len() as u32)
            .replace("%d", &names.len().to_string()),
    };
    let body = match names {
        [_] => gettext(
            "A file with that name already exists. Replacing it will overwrite its content.",
        ),
        // A few names say which; a long list of them would only make the dialog tall.
        _ if names.len() <= SHOWN => gettext("Files with these names already exist: %s. Replacing them will overwrite their content.")
            .replace("%s", &names.join(", ")),
        _ => {
            let more = names.len() - SHOWN;
            ngettext(
                "Files with these names already exist: %s, and %d more. Replacing them will overwrite their content.",
                "Files with these names already exist: %s, and %d more. Replacing them will overwrite their content.",
                more as u32,
            )
            .replace("%s", &names[..SHOWN].join(", "))
            .replace("%d", &more.to_string())
        }
    };
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("replace", &gettext("_Replace")),
    ]);
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    dialog.choose_future(Some(parent)).await == "replace"
}

/// What `file` is, following links; `None` where there is nothing.
async fn file_type(file: &gio::File) -> Option<gio::FileType> {
    file.query_info_future(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .ok()
    .map(|i| i.file_type())
}

/// `file` as the application can be given it. xdg-desktop-portal passes on `file://` URIs
/// only and drops the rest, so a file on a share goes as the path GVfs makes for it; one
/// with no path at all, in the trash's listing or the network's, cannot go. The URI is made
/// from the path itself: GIO turns a file made from that path back into the share's.
fn local_uri(file: &gio::File) -> Option<Uri> {
    let uri = glib::filename_to_uri(file.path()?, None).ok()?;
    Uri::parse(&uri).ok()
}

/// Whether what is saved in `folder` reaches the application as a file there. The trash
/// has a path under GVfs, but a file written through it is not put in the trash.
fn passable_folder(folder: &gio::File) -> bool {
    !folder.has_uri_scheme("trash") && local_uri(folder).is_some()
}

/// Say that what was picked cannot be handed over, rather than hand over nothing.
async fn not_passable(window: adw::Window) {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Not Available to the Application"))
        .body(gettext(
            "Only files in a folder on this computer or on a connected server can be passed on. Choose another folder.",
        ))
        .close_response("ok")
        .build();
    dialog.add_response("ok", &gettext("_OK"));
    dialog.choose_future(Some(&window)).await;
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    fn matches(pattern: &str, name: &str) -> bool {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        glob_match(&chars(pattern), &chars(name))
    }

    #[test]
    fn globs() {
        assert!(matches("*.txt", "notes.txt"));
        assert!(!matches("*.txt", "notes.md"));
        assert!(matches("*.[pp][nn][gg]", "photo.png"));
        assert!(!matches("*.[pp][nn][gg]", "photo.jpg"));
        assert!(matches("scan[0-9].pdf", "scan3.pdf"));
        assert!(!matches("scan[!0-9].pdf", "scan3.pdf"));
        assert!(matches("[]x]", "]"));
        assert!(matches("a[b", "a[b"));
        assert!(matches("résumé.*", "résumé.odt"));
    }
}
