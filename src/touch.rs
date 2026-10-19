//! What a finger does where a mouse would do it another way: a long press stands for a
//! right click, a swipe from the edge brings the folded sidebar out, and a window used by
//! touch gives small targets more room.

use std::cell::Cell;
use std::rc::Rc;

use crate::{adw, gdk, glib, gtk};
use adw::prelude::*;

/// Whether `device` is a touchscreen.
pub(crate) fn is_touchscreen(device: Option<&gdk::Device>) -> bool {
    device.is_some_and(|d| d.source() == gdk::InputSource::Touchscreen)
}

/// Keep the `touch` style class on `window` while it is being used by a finger, so that
/// what is too small to hit with one can grow; a press of a mouse or a pen takes it off.
pub(crate) fn follow_input(window: &impl IsA<gtk::Widget>) {
    let input = gtk::EventControllerLegacy::new();
    input.set_propagation_phase(gtk::PropagationPhase::Capture);
    input.connect_event(|input, event| {
        if matches!(
            event.event_type(),
            gdk::EventType::TouchBegin | gdk::EventType::ButtonPress
        ) && let Some(window) = input.widget()
        {
            let touch = is_touchscreen(event.device().as_ref());
            if touch != window.has_css_class("touch") {
                if touch {
                    window.add_css_class("touch");
                } else {
                    window.remove_css_class("touch");
                }
            }
        }
        glib::Propagation::Proceed
    });
    window.add_controller(input);
}

/// A finger swiping the folded sidebar of `split` in from the edge it sits on, or away
/// again. libadwaita's own swipe sees a finger only after the views inside have, and their
/// scrolling takes it first; this one looks before them. A touchpad is left alone, so its
/// sideways scrolling never pulls the sidebar out.
pub(crate) fn sidebar_swipe(split: &adw::OverlaySplitView) {
    /// How near the edge a swipe that brings the sidebar in starts.
    const EDGE: f64 = 24.0;
    /// How far a finger goes before its way is told. The views take it to scroll at GTK's
    /// drag threshold, 8 pixels, so it has to be told before that.
    const DECIDED: f64 = 6.0;
    /// How far it goes before it counts.
    const FAR: f64 = 48.0;
    let swipe = gtk::GestureDrag::builder()
        .touch_only(true)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    // Whether the sidebar was out as the swipe started, while one is going on.
    let shown = Rc::new(Cell::new(None::<bool>));
    // Distances measured from the side the sidebar is on.
    let inward = |split: &adw::OverlaySplitView, dx: f64| {
        if split.direction() == gtk::TextDirection::Rtl {
            -dx
        } else {
            dx
        }
    };
    swipe.connect_drag_begin(glib::clone!(
        #[weak]
        split,
        #[strong]
        shown,
        move |swipe, x, _| {
            let from_edge = if split.direction() == gtk::TextDirection::Rtl {
                f64::from(split.width()) - x
            } else {
                x
            };
            let out = split.shows_sidebar();
            if !split.is_collapsed() || (!out && from_edge > EDGE) {
                swipe.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            shown.set(Some(out));
        }
    ));
    swipe.connect_drag_update(glib::clone!(
        #[weak]
        split,
        #[strong]
        shown,
        move |swipe, dx, dy| {
            let Some(out) = shown.get() else { return };
            let sequence = swipe.current_sequence();
            if sequence
                .as_ref()
                .is_some_and(|s| swipe.sequence_state(s) == gtk::EventSequenceState::Claimed)
                || dx.abs().max(dy.abs()) < DECIDED
            {
                return;
            }
            let towards = inward(&split, dx);
            if dy.abs() < dx.abs() && (towards > 0.0) != out {
                swipe.set_state(gtk::EventSequenceState::Claimed);
            } else {
                shown.set(None);
                swipe.set_state(gtk::EventSequenceState::Denied);
            }
        }
    ));
    swipe.connect_drag_end(glib::clone!(
        #[weak]
        split,
        #[strong]
        shown,
        move |_, dx, _| {
            if let Some(out) = shown.take()
                && inward(&split, dx).abs() > FAR
            {
                split.set_show_sidebar(!out);
            }
        }
    ));
    split.add_controller(swipe);
}

/// A finger held on `widget` long enough. `held` is told where as soon as it is, and says
/// whether the press is taken: a press taken is no tap any more and does not scroll.
/// `lifted` is told the same point once the finger leaves the screen there, which is when
/// a menu opens; a finger that moves on instead leaves the press to a drag, and `lifted`
/// is not told.
pub(crate) fn long_press(
    widget: &impl IsA<gtk::Widget>,
    held: impl Fn(f64, f64) -> bool + 'static,
    lifted: impl Fn(f64, f64) + 'static,
) -> gtk::GestureLongPress {
    let gesture = gtk::GestureLongPress::builder()
        .touch_only(true)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let at = Rc::new(Cell::new(None::<(f64, f64)>));
    gesture.connect_pressed({
        let at = at.clone();
        move |gesture, x, y| {
            if held(x, y) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                at.set(Some((x, y)));
            }
        }
    });
    // A drag that starts resets the widget's gestures, and the press is no menu then.
    gesture.connect_cancel({
        let at = at.clone();
        move |_, _| at.set(None)
    });
    // The press ends as the finger is lifted, with that as its last event, or as it moves
    // too far, with the move as its last.
    gesture.connect_end(move |gesture, sequence| {
        let Some((x, y)) = at.take() else {
            return;
        };
        if gesture
            .last_event(sequence)
            .is_some_and(|e| e.event_type() == gdk::EventType::TouchEnd)
        {
            lifted(x, y);
        }
    });
    widget.add_controller(gesture.clone());
    gesture
}
