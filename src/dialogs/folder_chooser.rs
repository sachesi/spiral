//! The folder picker "Copy To", "Move To" and "Extract To" ask with: the chooser's view
//! and sidebar in a dialog attached to the window, rather than a file dialog of its own.

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::browser_view::BrowserView;
use crate::places_sidebar::PlacesSidebar;
use crate::{adw, file_utils, gio, glib, gtk};

/// Ask for a folder, starting at `start`. Resolves to the folder chosen, or None if the
/// dialog was dismissed.
pub async fn folder_chooser_dialog(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    accept_label: &str,
    start: &gio::File,
) -> Option<gio::File> {
    let view = BrowserView::new_chooser(start);
    // Only folders: what is being asked for is a destination, and a file is never one.
    view.model()
        .set_extra_filter(Some(gtk::CustomFilter::new(|obj| {
            obj.downcast_ref::<gio::FileInfo>()
                .is_some_and(file_utils::is_dir)
        })));
    let sidebar: PlacesSidebar = glib::Object::new();
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
    // Narrow enough and the sidebar goes over the folders instead of beside them; this
    // is how it is asked back.
    let show_sidebar = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(gettext("Show Sidebar"))
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let new_folder = gtk::Button::builder()
        .icon_name("folder-new-symbolic")
        .tooltip_text(gettext("New Folder"))
        .action_name("view.new-folder")
        .valign(gtk::Align::Center)
        .build();

    let header = adw::HeaderBar::builder()
        .title_widget(location_bar.widget())
        .build();
    header.pack_start(&show_sidebar);
    header.pack_start(&nav);
    header.pack_end(&new_folder);

    let accept = gtk::Button::builder()
        .label(accept_label)
        .use_underline(true)
        .css_classes(["pill", "suggested-action"])
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    let bottom = gtk::CenterBox::builder().end_widget(&accept).build();

    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(&view));
    content.add_bottom_bar(&bottom);

    let sidebar_toolbar = adw::ToolbarView::new();
    sidebar_toolbar.add_top_bar(
        &adw::HeaderBar::builder()
            .title_widget(
                &gtk::Label::builder()
                    .label(title)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .css_classes(["title"])
                    .build(),
            )
            .show_end_title_buttons(false)
            .build(),
    );
    sidebar_toolbar.set_content(Some(&sidebar));

    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_toolbar)
        .content(&content)
        .sidebar_width_fraction(0.25)
        .min_sidebar_width(180.0)
        .max_sidebar_width(260.0)
        .css_classes(["view"])
        .build();
    split
        .bind_property("show-sidebar", &show_sidebar, "active")
        .bidirectional()
        .sync_create()
        .build();
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(820)
        .content_height(540)
        .child(&split)
        .css_classes(["spiral-file-chooser"])
        .build();
    dialog.set_default_widget(Some(&accept));
    // A dialog is as wide as the window lets it be, and a narrow window leaves no room for
    // two panes: the sidebar folds away and the button in the header brings it back over
    // the folders.
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        550.0,
        adw::LengthUnit::Sp,
    ));
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    bp.add_setter(
        &split,
        "sidebar-width-unit",
        Some(&adw::LengthUnit::Px.to_value()),
    );
    bp.add_setter(&show_sidebar, "visible", Some(&true.to_value()));
    dialog.add_breakpoint(bp);
    dialog.insert_action_group("view", Some(view.action_group()));

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
    sidebar.set_selected_location(Some(start));
    view.connect_location_notify(glib::clone!(
        #[weak]
        sidebar,
        move |v| sidebar.set_selected_location(v.location().as_ref())
    ));

    let (tx, rx) = oneshot::channel::<Option<gio::File>>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    accept.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[strong]
        tx,
        move |_| {
            // The folder picked out in the view, or the one being looked at.
            let chosen = match view.model().selected_infos().as_slice() {
                [info] if file_utils::is_dir(info) => Some(file_utils::file_of(info)),
                _ => view.location(),
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(chosen);
            }
            dialog.close();
        }
    ));
    dialog.connect_closed(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(None);
        }
    });

    // The window's keys for the two things a picker can do with them.
    let keys = gtk::ShortcutController::new();
    let add_key = |trigger: &str, f: Box<dyn Fn()>| {
        keys.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                f();
                glib::Propagation::Stop
            })),
        ));
    };
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
    add_key("<Control>l", Box::new(move || location_bar.edit()));
    dialog.add_controller(keys);

    dialog.present(Some(parent));
    view.grab_view_focus();
    rx.await.ok().flatten()
}
