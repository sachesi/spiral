//! The file chooser window shown for portal requests, built from Spiral's own browser widgets.

use std::cell::RefCell;
use std::rc::Rc;

use ashpd::PortalError;
use ashpd::desktop::file_chooser::{Choice, FileFilter, SelectedFiles};
use ashpd::{Uri, WindowIdentifierType};
use futures_util::FutureExt;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::browser_view::BrowserView;
use crate::places_sidebar::PlacesSidebar;
use crate::{adw, file_utils, gio, glib, gtk};

use super::{Kind, Request};

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
                let name = o.current_name().map(str::to_string).or_else(|| {
                    o.current_file().and_then(|f| {
                        f.as_ref()
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    })
                });
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
    let model = view.model();
    let sidebar: PlacesSidebar = glib::Object::new();
    let path_bar: crate::path_bar::PathBar = glib::Object::new();
    path_bar.set_location(Some(&start));

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
                let grid = v.view_mode() == crate::enums::ViewMode::Grid;
                view_button.set_icon_name(if grid {
                    "view-list-symbolic"
                } else {
                    "view-grid-symbolic"
                });
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

    let header = adw::HeaderBar::builder().title_widget(&path_bar).build();
    header.pack_start(&nav);
    header.pack_end(&view_button);
    header.pack_end(&new_folder);

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
        && let Some(i) = filters.iter().position(|f| f.label() == cur.label())
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
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        682.0,
        adw::LengthUnit::Sp,
    ));
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(bp);

    // ---- wiring ---------------------------------------------------------------------------
    sidebar.connect_open_location(glib::clone!(
        #[weak]
        view,
        move |_, f, _| view.go_to(f)
    ));
    path_bar.connect_navigate(glib::clone!(
        #[weak]
        view,
        move |_, f| view.go_to(f)
    ));
    view.connect_location_notify(glib::clone!(
        #[weak]
        path_bar,
        #[weak]
        sidebar,
        move |v| {
            path_bar.set_location(v.location().as_ref());
            sidebar.set_selected_location(v.location().as_ref());
        }
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
    let finish = {
        let result = result.clone();
        let done_tx = done_tx.clone();
        let window = window.clone();
        Rc::new(move |r: Result<SelectedFiles, PortalError>| {
            if result.borrow().is_none() {
                result.replace(Some(r));
                if let Some(tx) = done_tx.borrow_mut().take() {
                    let _ = tx.send(());
                }
                window.close();
            }
        })
    };

    let collect_choices = {
        let choice_widgets = choice_widgets.clone();
        let filter_dropdown = filter_dropdown.clone();
        let filters = filters.clone();
        move |mut sel: SelectedFiles| {
            for (id, w) in &choice_widgets {
                // Combo choices are a label plus dropdown box; the dropdown is the last child.
                let dropdown = w.last_child().and_downcast::<gtk::DropDown>();
                let value = if let Some(dd) = dropdown {
                    unsafe {
                        dd.data::<Vec<String>>("choice-keys")
                            .map(|k| k.as_ref()[dd.selected() as usize].clone())
                    }
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

    let on_accept: Rc<dyn Fn()> = {
        let finish = finish.clone();
        let view = view.clone();
        let name_entry = name_entry.clone();
        let window = window.clone();
        let mode = Rc::new(mode);
        Rc::new(move || {
            let Some(location) = view.location() else {
                return;
            };
            match &*mode {
                Mode::Open {
                    multiple,
                    directory,
                } => {
                    let mut files: Vec<gio::File> = view
                        .model()
                        .selected_infos()
                        .iter()
                        .filter(|i| *directory == file_utils::is_dir(i))
                        .map(file_utils::file_of)
                        .collect();
                    if files.is_empty() && *directory {
                        files.push(location);
                    }
                    if files.is_empty() {
                        // Accepting with only a folder selected enters it, as GTK's chooser does.
                        if let [info] = view.model().selected_infos().as_slice()
                            && file_utils::is_dir(info)
                        {
                            view.go_to(&file_utils::file_of(info));
                        }
                        return;
                    }
                    if !*multiple {
                        files.truncate(1);
                    }
                    let mut sel = SelectedFiles::default();
                    for f in files {
                        if let Ok(u) = Uri::parse(&f.uri()) {
                            sel = sel.uri(u);
                        }
                    }
                    finish(Ok(collect_choices(sel)));
                }
                Mode::Save => {
                    let name = name_entry.text().trim().to_string();
                    if name.is_empty() || name.contains('/') {
                        name_entry.add_css_class("error");
                        return;
                    }
                    let dest = location.child(&name);
                    let finish = finish.clone();
                    let collect = collect_choices.clone();
                    let window = window.clone();
                    glib::spawn_future_local(async move {
                        let exists = dest
                            .query_info_future(
                                "standard::type",
                                gio::FileQueryInfoFlags::NONE,
                                glib::Priority::DEFAULT,
                            )
                            .await
                            .is_ok();
                        if exists && !confirm_replace(&window, &name).await {
                            return;
                        }
                        let mut sel = SelectedFiles::default();
                        if let Ok(u) = Uri::parse(&dest.uri()) {
                            sel = sel.uri(u);
                        }
                        finish(Ok(collect(sel)));
                    });
                }
                Mode::SaveFiles(names) => {
                    let mut sel = SelectedFiles::default();
                    for n in names {
                        let Some(base) = n.file_name() else { continue };
                        if let Ok(u) = Uri::parse(&location.child(base).uri()) {
                            sel = sel.uri(u);
                        }
                    }
                    finish(Ok(collect_choices(sel)));
                }
            }
        })
    };
    accept.connect_clicked(glib::clone!(
        #[strong]
        on_accept,
        move |_| on_accept()
    ));
    view.connect_file_activated(glib::clone!(
        #[strong]
        on_accept,
        move |_, _| on_accept()
    ));
    name_entry.connect_changed(|e| e.remove_css_class("error"));
    window.connect_close_request(glib::clone!(
        #[strong]
        finish,
        move |_| {
            finish(Err(PortalError::Cancelled("dismissed".into())));
            glib::Propagation::Proceed
        }
    ));
    let esc = gtk::ShortcutController::new();
    esc.set_scope(gtk::ShortcutScope::Managed);
    esc.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(esc);

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
        let t = name_entry.text();
        let stem = t.rfind('.').filter(|&i| i > 0).unwrap_or(t.len());
        name_entry.select_region(0, stem as i32);
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
        let name = info.display_name().to_lowercase();
        if patterns
            .iter()
            .any(|p| glob_match(p.as_bytes(), name.as_bytes()))
        {
            return true;
        }
        info.content_type()
            .is_some_and(|ct| mimes.iter().any(|m| gio::content_type_is_a(&ct, m)))
    })
    .upcast()
}

/// Shell-style glob with `*` and `?` (what file filters use).
fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob_match(&pat[1..], text) || (!text.is_empty() && glob_match(pat, &text[1..]))
        }
        (Some(b'?'), Some(_)) => glob_match(&pat[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob_match(&pat[1..], &text[1..]),
        _ => false,
    }
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
    unsafe { dd.set_data("choice-keys", keys) };
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

async fn confirm_replace(parent: &impl IsA<gtk::Widget>, name: &str) -> bool {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Replace “%s”?").replace("%s", name))
        .body(gettext(
            "A file with that name already exists. Replacing it will overwrite its content.",
        ))
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
