//! What a drag of items shows under the pointer, over every window: their icons, spread as
//! they stood when the drag began or gathered into a pile on the one leading it, with how
//! many there are.

use gtk::graphene::{Point as GPoint, Rect};

use super::*;

/// How long the icons take to gather, or to spread out again.
const MOVE_MS: u32 = 250;
/// The last part of the gathering, in which the names of the icons under the leading one
/// fade.
const LANDING: f32 = 0.4;
/// Icons in the pile at most, those nearest the leading one; the others fade on the way,
/// and the count says how many there are.
const DRAWN: usize = 12;
/// The turns, in degrees, that the icons under the leading one come to rest at, in turn.
const TURNS: [f32; 6] = [-6.0, 5.0, -3.0, 7.0, -5.0, 3.0];

/// The icons that go nowhere but where they stand: carried along at first, they leave the
/// drag the first time it gathers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Staying {
    #[default]
    Carried,
    Leaving,
    Gone,
}

/// An icon of the flock: what its tile looks like; its image alone, where that is on the
/// tile and where the icon is drawn in it; where the tile stood, measured from the pointer;
/// and how it lies in the pile, if it goes there.
struct Member {
    tile: gdk::Paintable,
    icon: Option<(gdk::Paintable, Rect, Rect)>,
    from: Rect,
    turn: f32,
    staying: bool,
    /// Past the icons the pile shows: it fades on the way there.
    far: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Flock {
        /// The leading icon last.
        pub(super) members: RefCell<Vec<Member>>,
        /// All of the flock as it stood, and the pile it gathers into.
        pub(super) spread: Cell<(f32, f32, f32, f32)>,
        pub(super) pile: Cell<(f32, f32, f32, f32)>,
        /// How far it has gathered, from spread out at 0 to piled up at 1.
        pub(super) progress: Cell<f64>,
        /// Whether it is the pile alone, and holds no more than that.
        pub(super) piled: Cell<bool>,
        pub(super) staying: Cell<Staying>,
        pub(super) count: RefCell<Option<gtk::Label>>,
        pub(super) animation: RefCell<Option<adw::TimedAnimation>>,
        pub(super) drag: glib::WeakRef<gdk::Drag>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Flock {
        const NAME: &'static str = "SpiralFlock";
        type Type = super::Flock;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Flock {
        fn dispose(&self) {
            if let Some(count) = self.count.take() {
                count.unparent();
            }
        }
    }

    impl WidgetImpl for Flock {
        fn measure(&self, orientation: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let (_, _, w, h) = self.frame();
            let size = match orientation {
                gtk::Orientation::Horizontal => w,
                _ => h,
            }
            .ceil() as i32;
            (size, size, -1, -1)
        }

        fn size_allocate(&self, _: i32, _: i32, _: i32) {
            let Some(count) = self.count.borrow().clone() else {
                return;
            };
            let (fx, fy, ..) = self.frame();
            let members = self.members.borrow();
            let Some(lead) = members.last() else {
                return;
            };
            // On the top right corner of the leading icon, or of its tile when it has none.
            let (right, top) = match lead.icon {
                Some((_, _, icon)) => (
                    lead.from.x() + icon.x() + icon.width(),
                    lead.from.y() + icon.y(),
                ),
                None => (lead.from.x() + lead.from.width(), lead.from.y()),
            };
            let (_, w, _, _) = count.measure(gtk::Orientation::Horizontal, -1);
            let (_, h, _, _) = count.measure(gtk::Orientation::Vertical, w);
            let x = (right - fx) as i32 - w / 2;
            let y = (top - fy) as i32 - h / 2;
            count.size_allocate(&gtk::Allocation::new(x, y, w, h), -1);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (fx, fy, ..) = self.frame();
            let t = self.progress.get() as f32;
            let fade = ((1.0 - t) / LANDING).min(1.0);
            let staying = self.staying.get();
            let members = self.members.borrow();
            let Some(lead) = members.last().map(|m| m.from) else {
                return;
            };
            snapshot.save();
            snapshot.translate(&GPoint::new(-fx, -fy));
            for (i, member) in members.iter().enumerate() {
                let under = i + 1 < members.len();
                // What stays behind fades where it stands; what goes gathers onto the lead.
                let (to, opacity) = match (member.staying, staying) {
                    (true, Staying::Gone) => continue,
                    (true, Staying::Leaving) => (member.from, 1.0 - t),
                    (true, Staying::Carried) => (member.from, 1.0),
                    (false, _) if member.far => (lead, 1.0 - t),
                    (false, _) => (lead, 1.0),
                };
                if opacity <= 0.0 {
                    continue;
                }
                let at = |a: f32, b: f32| a + (b - a) * t;
                let (w, h) = (member.from.width(), member.from.height());
                snapshot.save();
                snapshot.translate(&GPoint::new(
                    at(member.from.x(), to.x()) + w / 2.0,
                    at(member.from.y(), to.y()) + h / 2.0,
                ));
                if !member.staying {
                    snapshot.rotate(member.turn * t);
                }
                snapshot.translate(&GPoint::new(-w / 2.0, -h / 2.0));
                if opacity < 1.0 {
                    snapshot.push_opacity(f64::from(opacity));
                }
                // What lands under the leading icon keeps its icon and lets its name go, so
                // the names do not show through each other.
                match &member.icon {
                    Some((icon, rect, _)) if under && !member.staying => {
                        snapshot.save();
                        snapshot.translate(&GPoint::new(rect.x(), rect.y()));
                        icon.snapshot(snapshot, f64::from(rect.width()), f64::from(rect.height()));
                        snapshot.restore();
                        if fade > 0.0 {
                            snapshot.push_opacity(f64::from(fade));
                            member.tile.snapshot(snapshot, f64::from(w), f64::from(h));
                            snapshot.pop();
                        }
                    }
                    _ => member.tile.snapshot(snapshot, f64::from(w), f64::from(h)),
                }
                if opacity < 1.0 {
                    snapshot.pop();
                }
                snapshot.restore();
            }
            snapshot.restore();
            if self.piled.get()
                && let Some(count) = self.count.borrow().as_ref()
            {
                self.obj().snapshot_child(count, snapshot);
            }
        }
    }

    impl Flock {
        /// What of the flock the drag icon holds: all of it until it has piled up.
        pub(super) fn frame(&self) -> (f32, f32, f32, f32) {
            if self.piled.get() {
                self.pile.get()
            } else {
                self.spread.get()
            }
        }
    }
}

glib::wrapper! {
    pub struct Flock(ObjectSubclass<imp::Flock>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Flock {
    /// The icons of `going`, `leader` leading, and of `staying`, with the pointer at
    /// (`x`, `y`) of `root`, and `count` items going in all.
    pub fn new(
        root: &gtk::Widget,
        going: &[gtk::Widget],
        staying: &[gtk::Widget],
        leader: usize,
        x: f64,
        y: f64,
        count: usize,
    ) -> Option<Self> {
        let shot = |tile: &gtk::Widget| {
            let from = tile.compute_bounds(root)?.offset_r(-x as f32, -y as f32);
            // The icon as drawn: a square of its size in the middle of the image.
            let icon = tile_image(tile).and_then(|image| {
                let bounds = image.compute_bounds(tile)?;
                let size = image.pixel_size() as f32;
                let rect = Rect::new(
                    bounds.x() + (bounds.width() - size) / 2.0,
                    bounds.y() + (bounds.height() - size) / 2.0,
                    size,
                    size,
                );
                let paintable = gtk::WidgetPaintable::new(Some(&image)).current_image();
                Some((paintable, rect, bounds))
            });
            let image = gtk::WidgetPaintable::new(Some(tile)).current_image();
            Some((image, icon, from))
        };
        let (lead_image, lead_icon, lead) = shot(going.get(leader)?)?;
        let distance = |r: &Rect| {
            let (dx, dy) = (r.x() - lead.x(), r.y() - lead.y());
            dx * dx + dy * dy
        };
        let mut others: Vec<_> = going
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != leader)
            .filter_map(|(_, tile)| shot(tile))
            .collect();
        others.sort_by(|a, b| distance(&a.2).total_cmp(&distance(&b.2)));
        // The places under all, then the farthest under the rest, and the leading icon on
        // top of all.
        let mut members: Vec<Member> = staying
            .iter()
            .filter_map(shot)
            .map(|(tile, icon, from)| Member {
                tile,
                icon,
                from,
                turn: 0.0,
                staying: true,
                far: false,
            })
            .collect();
        members.extend(
            others
                .into_iter()
                .enumerate()
                .rev()
                .map(|(i, (tile, icon, from))| Member {
                    tile,
                    icon,
                    from,
                    turn: TURNS[i % TURNS.len()],
                    staying: false,
                    far: i + 1 >= DRAWN,
                }),
        );
        members.push(Member {
            tile: lead_image,
            icon: lead_icon,
            from: lead,
            turn: 0.0,
            staying: false,
            far: false,
        });
        // Room for the icons turned under the leading one, and for the count.
        let margin = lead.width().max(lead.height()) * 0.15;
        let pile = lead.inset_r(-margin, -margin);
        let spread = members
            .iter()
            .fold(pile, |all, m| all.union(&m.from.inset_r(-margin, -margin)));
        let rect = |r: Rect| (r.x(), r.y(), r.width(), r.height());
        let flock: Self = glib::Object::new();
        let imp = flock.imp();
        imp.spread.set(rect(spread));
        imp.pile.set(rect(pile));
        imp.members.replace(members);
        if count > 1 {
            let label = gtk::Label::builder()
                .label(count.to_string())
                .css_classes(["spiral-flock-count", "numeric"])
                .build();
            label.set_parent(&flock);
            imp.count.replace(Some(label));
        }
        Some(flock)
    }

    /// Becomes the icon of `drag`, spread out as the icons stood.
    pub fn begin(&self, drag: &gdk::Drag) {
        self.imp().drag.set(Some(drag));
        gtk::DragIcon::for_drag(drag)
            .downcast::<gtk::DragIcon>()
            .expect("drag icon")
            .set_child(Some(self));
        self.settle();
    }

    /// Gathers the icons into the pile; what stays behind leaves the drag.
    pub fn gather(&self) {
        let imp = self.imp();
        if imp.staying.get() == Staying::Carried {
            imp.staying.set(Staying::Leaving);
        }
        self.move_to(1.0);
    }

    /// Spreads the icons out of the pile, to where they stood.
    pub fn spread(&self) {
        let imp = self.imp();
        if imp.staying.get() == Staying::Leaving {
            imp.staying.set(Staying::Gone);
        }
        self.move_to(0.0);
    }

    /// Moves on from wherever the icons are, a gathering cut short included.
    fn move_to(&self, to: f64) {
        let imp = self.imp();
        if let Some(animation) = imp.animation.take() {
            animation.pause();
        }
        let from = imp.progress.get();
        let still = gtk::Settings::for_display(&self.display()).gtk_interface_reduced_motion()
            == gtk::ReducedMotion::Reduce;
        if still || from == to {
            self.arrive(to);
            return;
        }
        imp.piled.set(false);
        self.settle();
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = flock)]
            self,
            move |value| {
                flock.imp().progress.set(value);
                flock.queue_draw();
            }
        ));
        let animation = adw::TimedAnimation::builder()
            .widget(self)
            .value_from(from)
            .value_to(to)
            .duration((f64::from(MOVE_MS) * (to - from).abs()) as u32)
            .easing(adw::Easing::Ease)
            .target(&target)
            .build();
        animation.connect_done(glib::clone!(
            #[weak(rename_to = flock)]
            self,
            move |_| flock.arrive(to)
        ));
        animation.play();
        imp.animation.replace(Some(animation));
    }

    fn arrive(&self, to: f64) {
        let imp = self.imp();
        imp.progress.set(to);
        let piled = to >= 1.0;
        if piled && imp.staying.get() == Staying::Leaving {
            imp.staying.set(Staying::Gone);
        }
        imp.piled.set(piled);
        self.settle();
    }

    /// The flock as it is now, the pointer kept where it was on it.
    fn settle(&self) {
        let imp = self.imp();
        if let Some(drag) = imp.drag.upgrade() {
            let (fx, fy, ..) = imp.frame();
            drag.set_hotspot((-fx).round() as i32, (-fy).round() as i32);
        }
        self.queue_resize();
        self.queue_draw();
    }
}
