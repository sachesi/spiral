//! What a finger does where a mouse would use another button: a long press stands for a
//! right click.

use std::cell::Cell;
use std::rc::Rc;

use crate::{gdk, gtk};
use gtk::prelude::*;

/// Whether `device` is a touchscreen.
pub(crate) fn is_touchscreen(device: Option<&gdk::Device>) -> bool {
    device.is_some_and(|d| d.source() == gdk::InputSource::Touchscreen)
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
