//! The desktop's menus, prompts and tooltips as the shell has its own, so the desktop looks a
//! part of it, where the compositor draws glass behind a shell's surfaces
//! (`meridian-glass-v1`): cards it blurs what is behind while its glass setting is on, and
//! the shell's plain cards while that is off. Their glass looks as the compositor's own
//! stylesheet has it, installed with it; without that, or elsewhere, they keep their usual
//! look.

use std::cell::RefCell;
use std::rc::Rc;

use gdk4_wayland::prelude::*;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

use crate::gtk::prelude::*;
use crate::{adw, gdk, gio, glib, gtk};

use self::protocol::meridian_glass_manager_v1::MeridianGlassManagerV1;
use self::protocol::meridian_glass_v1::{Contrast, MeridianGlassV1};

/// Where the compositor installs the look of glass, among the data directories.
const STYLESHEET: &str = "meridian/glass.css";
/// How much what is behind is blurred, as the stylesheet has it for menus and dialogs.
const FROST: u32 = 4;
/// The stylesheet's tiers the desktop's parts are of.
const MENU: &str = "glass-menu";
const DIALOG: &str = "glass-dialog";
/// A part set into another's glass, which keeps its own colours.
const INSET: &str = "glass-inset";
/// Of a menu's card, as the stylesheet rounds it.
const CARD_RADIUS: i32 = 36;
/// Of the window a prompt over the desktop opens in, as the stylesheet rounds it.
const PROMPT_RADIUS: i32 = 28;
/// Of a prompt's list of rows, as libadwaita rounds a boxed list.
const LIST_RADIUS: i32 = 12;
/// How far inside the rectangle GTK places a menu by its card is, sideways and at the top:
/// the stylesheet's margins around the card, less what GTK counts of them as the shadow's
/// (2px sideways and none at the top, for `0 2px 6px`).
const CARD_INSET: (i32, i32) = (12 - 2, 16);
/// Between a menu's card and its submenu's.
const SUBMENU_GAP: i32 = 6;

#[allow(clippy::all, clippy::pedantic, non_upper_case_globals)]
mod protocol {
    use wayland_client::{self, protocol::*};

    pub mod __interfaces {
        use wayland_client::{backend as wayland_backend, protocol::__interfaces::*};
        wayland_scanner::generate_interfaces!("data/protocols/meridian-glass-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("data/protocols/meridian-glass-v1.xml");
}

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore MeridianGlassManagerV1);
delegate_noop!(State: ignore MeridianGlassV1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corners {
    Radius(i32),
    /// Rounded by half its height.
    Pill,
}

/// A shape as sent: x, y, width, height and radius.
type Shape = (i32, i32, i32, i32, i32);

/// The glass of one surface: a popover's, a tooltip's, or the desktop's own under a
/// dialog, with the parts of it that are panes.
struct Pane {
    native: glib::WeakRef<gtk::Widget>,
    glass: MeridianGlassV1,
    parts: Vec<(glib::WeakRef<gtk::Widget>, Corners)>,
    sent: Vec<Shape>,
    handlers: Option<(
        gdk::FrameClock,
        glib::SignalHandlerId,
        glib::SignalHandlerId,
    )>,
}

struct Glass {
    connection: Connection,
    queue: RefCell<EventQueue<State>>,
    manager: MeridianGlassManagerV1,
    style: gtk::CssProvider,
    appearance: Option<gio::Settings>,
    glazed: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Corners, &'static str)>>,
    panes: RefCell<Vec<Pane>>,
}

thread_local! {
    static GLASS: Option<Rc<Glass>> = Glass::new();
}

fn shared() -> Option<Rc<Glass>> {
    GLASS.with(Clone::clone)
}

/// Whether `widget` is on the desktop, rather than in a window of the file manager.
pub(crate) fn on_desktop(widget: &impl IsA<gtk::Widget>) -> bool {
    widget
        .root()
        .is_some_and(|root| root.has_css_class("spiral-desktop-window"))
}

/// Makes `popover`, with its submenus, one of the shell's menus.
pub(crate) fn dress_menu(popover: &gtk::Popover) {
    let Some(glass) = shared() else {
        return;
    };
    let mut popovers = vec![popover.clone()];
    each_descendant(popover.upcast_ref(), &mut |widget| {
        if let Some(submenu) = widget.downcast_ref::<gtk::Popover>() {
            if !submenu.has_css_class("spiral-shell-menu") {
                submenu.connect_show(clear_item);
            }
            popovers.push(submenu.clone());
        }
    });
    for popover in &popovers {
        if popover.has_css_class("spiral-shell-menu") {
            continue;
        }
        popover.add_css_class("spiral-shell-menu");
        popover.set_has_arrow(false);
        let Some(card) = popover.child() else {
            continue;
        };
        card.add_css_class("spiral-shell-card");
        card.set_overflow(gtk::Overflow::Hidden);
        glass.glaze(&card, Corners::Radius(CARD_RADIUS), MENU);
    }
}

/// Points `submenu` past the sides of the menu's card, as far on the left as on the right,
/// since it opens on whichever side has room, and as high as puts its first row level with
/// the item it opens from.
fn clear_item(submenu: &gtk::Popover) {
    let Some(item) = submenu.parent() else {
        return;
    };
    let Some(card) = std::iter::successors(item.parent(), gtk::Widget::parent)
        .find(|widget| widget.has_css_class("spiral-shell-card"))
    else {
        return;
    };
    let mut first_row = None;
    each_descendant(&card, &mut |widget| {
        if first_row.is_none() && widget.css_name() == "modelbutton" && widget.is_visible() {
            first_row = widget.compute_bounds(&card);
        }
    });
    // Bounds of a widget in its own coordinates start at its content, inside its border.
    let (Some(item_box), Some(in_card), Some(card_box), Some(first_row)) = (
        item.compute_bounds(&item),
        item.compute_bounds(&card),
        card.compute_bounds(&card),
        first_row,
    ) else {
        return;
    };
    let side = card_box.x() + card_box.width() - (in_card.x() + in_card.width());
    let reach = side + (SUBMENU_GAP - CARD_INSET.0) as f32;
    let rise = first_row.y() - card_box.y() + CARD_INSET.1 as f32;
    submenu.set_pointing_to(Some(&gdk::Rectangle::new(
        (item_box.x() - reach).round() as i32,
        (item_box.y() - rise).round() as i32,
        (item_box.width() + 2.0 * reach).round() as i32,
        item_box.height().round() as i32,
    )));
}

/// Makes the window `dialog` opens in over the desktop a prompt of the shell's, of glass
/// where the compositor draws it. The desktop's own window is none of libadwaita's, so a
/// dialog there gets a window of its own.
pub(crate) fn dress_prompt(dialog: &adw::Dialog) {
    dialog.connect_map(move |dialog| {
        let Some(window) = dialog.parent().filter(|parent| parent.is::<gtk::Window>()) else {
            return;
        };
        window.add_css_class("spiral-prompt-window");
        if let Some(glass) = shared() {
            glass.glaze(&window, Corners::Radius(PROMPT_RADIUS), DIALOG);
            glass.attach(&window, Corners::Radius(PROMPT_RADIUS), DIALOG);
            // After the window, for the compositor to draw them over it.
            each_descendant(&window, &mut |widget| {
                let corners = if widget.is::<gtk::Button>() {
                    Corners::Pill
                } else if widget.has_css_class("boxed-list") {
                    Corners::Radius(LIST_RADIUS)
                } else {
                    return;
                };
                glass.glaze(widget, corners, INSET);
                glass.attach(widget, corners, INSET);
            });
        }
    });
}

/// Shows the whole of a name `label` cuts short in a tooltip like the shell's.
pub(crate) fn name_tooltip(label: &gtk::Label) {
    let Some(glass) = shared() else {
        crate::browser_view::name_tooltip(label);
        return;
    };
    label.set_has_tooltip(true);
    let text = gtk::Label::new(None);
    text.connect_map(move |text| {
        if let Some(tooltip) = text.native() {
            let tooltip: gtk::Widget = tooltip.upcast();
            tooltip.add_css_class("spiral-shell-tooltip");
            glass.glaze(&tooltip, Corners::Pill, MENU);
            glass.attach(&tooltip, Corners::Pill, MENU);
        }
    });
    label.connect_query_tooltip(move |label, _, _, _, tooltip| {
        let cut = label.layout().is_ellipsized();
        if cut {
            text.set_text(&label.text());
            tooltip.set_custom(Some(&text));
        }
        cut
    });
}

fn each_descendant(widget: &gtk::Widget, f: &mut impl FnMut(&gtk::Widget)) {
    let mut child = widget.first_child();
    while let Some(widget) = child {
        f(&widget);
        each_descendant(&widget, f);
        child = widget.next_sibling();
    }
}

impl Glass {
    /// On GTK's connection, where the compositor draws glass and its stylesheet for it is
    /// installed.
    fn new() -> Option<Rc<Self>> {
        let display = gdk::Display::default()?;
        let wayland = display.downcast_ref::<gdk4_wayland::WaylandDisplay>()?;
        let backend = wayland.wl_display()?.backend().upgrade()?;
        let connection = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&connection).ok()?;
        let manager = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
        let stylesheet = std::iter::once(glib::user_data_dir())
            .chain(glib::system_data_dirs())
            .map(|dir| dir.join(STYLESHEET))
            .find(|path| path.is_file())?;
        let style = gtk::CssProvider::new();
        style.load_from_path(stylesheet);
        // Above the app's own, so that glass' colours win over the cards' plain ones.
        gtk::style_context_add_provider_for_display(
            &display,
            &style,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        let appearance = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup("org.meridian.appearance", true))
            .filter(|schema| schema.has_key("glass"))
            .map(|_| gio::Settings::new("org.meridian.appearance"));
        let restyle = || {
            if let Some(glass) = shared() {
                glass.restyle();
            }
        };
        if let Some(appearance) = &appearance {
            appearance.connect_changed(Some("glass"), move |_, _| restyle());
        }
        adw::StyleManager::default().connect_dark_notify(move |_| restyle());
        let glass = Rc::new(Self {
            connection,
            queue: RefCell::new(queue),
            manager,
            style,
            appearance,
            glazed: RefCell::default(),
            panes: RefCell::default(),
        });
        glass.follow_style();
        Some(glass)
    }

    /// Has the stylesheet's colours take the style libadwaita has, which a style forced on it
    /// leaves the system's preference behind.
    fn follow_style(&self) {
        self.style
            .set_prefers_color_scheme(if adw::StyleManager::default().is_dark() {
                gtk::InterfaceColorScheme::Dark
            } else {
                gtk::InterfaceColorScheme::Light
            });
    }

    fn on(&self) -> bool {
        self.appearance
            .as_ref()
            .is_none_or(|settings| settings.boolean("glass"))
    }

    /// Makes `part` a pane of glass of `tier` each time it shows, while glass is on.
    fn glaze(self: &Rc<Self>, part: &gtk::Widget, corners: Corners, tier: &'static str) {
        if part.has_css_class("spiral-glazed") {
            return;
        }
        part.add_css_class("spiral-glazed");
        self.glazed
            .borrow_mut()
            .push((part.downgrade(), corners, tier));
        let glass = Rc::clone(self);
        part.connect_map(move |part| glass.attach(part, corners, tier));
        let glass = Rc::clone(self);
        part.connect_unmap(move |part| glass.detach(part));
    }

    fn attach(&self, part: &gtk::Widget, corners: Corners, tier: &str) {
        let on = self.on();
        let light = !adw::StyleManager::default().is_dark();
        let inset = tier == INSET;
        set_class(part, "glass", on && !inset);
        set_class(part, tier, on);
        set_class(part, "light", on && light);
        if !on {
            self.detach(part);
            return;
        }
        let Some(native) = part.native().map(Cast::upcast::<gtk::Widget>) else {
            return;
        };
        let mut panes = self.panes.borrow_mut();
        let at = match panes
            .iter()
            .position(|pane| pane.native.upgrade().as_ref() == Some(&native))
        {
            Some(at) => at,
            None => {
                let Some(pane) = self.pane(&native) else {
                    return;
                };
                panes.push(pane);
                panes.len() - 1
            }
        };
        let pane = &mut panes[at];
        pane.glass.set_contrast(if light {
            Contrast::Light
        } else {
            Contrast::Dark
        });
        if !pane
            .parts
            .iter()
            .any(|(kept, _)| kept.upgrade().as_ref() == Some(part))
        {
            pane.parts.push((part.downgrade(), corners));
        }
        self.send(pane);
    }

    /// The parts showing now, to the scheme and glass setting as they are now.
    fn restyle(&self) {
        self.follow_style();
        let showing: Vec<_> = {
            let mut glazed = self.glazed.borrow_mut();
            glazed.retain(|(part, _, _)| part.upgrade().is_some());
            glazed
                .iter()
                .filter_map(|(part, corners, tier)| {
                    Some((
                        part.upgrade().filter(WidgetExt::is_mapped)?,
                        *corners,
                        *tier,
                    ))
                })
                .collect()
        };
        for (part, corners, tier) in showing {
            self.attach(&part, corners, tier);
        }
    }

    fn detach(&self, part: &gtk::Widget) {
        for pane in self.panes.borrow_mut().iter_mut() {
            let before = pane.parts.len();
            pane.parts
                .retain(|(kept, _)| kept.upgrade().is_some_and(|kept| &kept != part));
            if pane.parts.len() != before {
                self.send(pane);
            }
        }
    }

    /// Glass for the surface of `native`, its shapes sent again each frame they move in.
    fn pane(&self, native: &gtk::Widget) -> Option<Pane> {
        let surface = native.native()?.surface()?;
        let wl_surface = surface
            .downcast_ref::<gdk4_wayland::WaylandSurface>()?
            .wl_surface()?;
        let glass = self
            .manager
            .get_glass(&wl_surface, &self.queue.borrow().handle(), ());
        glass.set_frost(FROST);
        let clock = surface.frame_clock();
        let weak = native.downgrade();
        let layout = clock.connect_layout(move |_| {
            if let (Some(glass), Some(native)) = (shared(), weak.upgrade()) {
                glass.resend(&native);
            }
        });
        let weak = native.downgrade();
        let unrealize = native.connect_unrealize(move |_| {
            if let (Some(glass), Some(native)) = (shared(), weak.upgrade()) {
                glass.drop_pane(&native);
            }
        });
        Some(Pane {
            native: native.downgrade(),
            glass,
            parts: Vec::new(),
            sent: Vec::new(),
            handlers: Some((clock, layout, unrealize)),
        })
    }

    fn resend(&self, native: &gtk::Widget) {
        if let Some(pane) = self
            .panes
            .borrow_mut()
            .iter_mut()
            .find(|pane| pane.native.upgrade().as_ref() == Some(native))
        {
            self.send(pane);
        }
    }

    /// The surface goes, and its glass with it.
    fn drop_pane(&self, native: &gtk::Widget) {
        let mut panes = self.panes.borrow_mut();
        let Some(at) = panes
            .iter()
            .position(|pane| pane.native.upgrade().as_ref() == Some(native))
        else {
            return;
        };
        let mut pane = panes.remove(at);
        if let Some((clock, layout, unrealize)) = pane.handlers.take() {
            clock.disconnect(layout);
            native.disconnect(unrealize);
        }
        pane.glass.destroy();
        self.flush();
    }

    /// Sends where the parts are now, to go with the frame drawn next, when that moved.
    fn send(&self, pane: &mut Pane) {
        let Some(native) = pane.native.upgrade() else {
            return;
        };
        let (dx, dy) = native
            .native()
            .map_or((0.0, 0.0), |native| native.surface_transform());
        let shapes: Vec<Shape> = pane
            .parts
            .iter()
            .filter_map(|(part, corners)| {
                let part = part.upgrade().filter(WidgetExt::is_mapped)?;
                let bounds = part.compute_bounds(&native)?;
                let (w, h) = (
                    f64::from(bounds.width()).round() as i32,
                    f64::from(bounds.height()).round() as i32,
                );
                let radius = match corners {
                    Corners::Radius(radius) => *radius,
                    Corners::Pill => h / 2,
                };
                Some((
                    (f64::from(bounds.x()) + dx).round() as i32,
                    (f64::from(bounds.y()) + dy).round() as i32,
                    w,
                    h,
                    radius,
                ))
            })
            .collect();
        if shapes == pane.sent {
            return;
        }
        pane.glass.clear();
        for &(x, y, w, h, radius) in &shapes {
            pane.glass.add(x, y, w, h, radius, 1.0);
        }
        pane.sent = shapes;
        native.queue_draw();
        self.flush();
    }

    fn flush(&self) {
        let _ = self.queue.borrow_mut().dispatch_pending(&mut State);
        let _ = self.connection.flush();
    }
}

fn set_class(widget: &gtk::Widget, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}
