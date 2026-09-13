//! A list of lines kept in memory, with the callbacks that want to hear when it changes.
//! The tag index and the favorites are this: data that jobs change as much as views read,
//! so it holds no GTK object and works wherever GLib does.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What [`Lines::watch`] hands back, for [`Lines::unwatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchId(u64);

type Watcher = (WatchId, Rc<dyn Fn()>);

#[derive(Default)]
pub struct Lines {
    items: RefCell<Vec<String>>,
    watchers: RefCell<Vec<Watcher>>,
    next: Cell<u64>,
}

impl Lines {
    pub fn new(items: Vec<String>) -> Self {
        Self {
            items: RefCell::new(items),
            ..Default::default()
        }
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.items.borrow().clone()
    }

    pub fn len(&self) -> usize {
        self.items.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    pub fn position(&self, line: &str) -> Option<usize> {
        self.items.borrow().iter().position(|l| l == line)
    }

    pub fn contains(&self, line: &str) -> bool {
        self.position(line).is_some()
    }

    pub fn push(&self, line: String) {
        self.items.borrow_mut().push(line);
        self.changed();
    }

    pub fn extend(&self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        self.items.borrow_mut().extend(lines);
        self.changed();
    }

    pub fn remove(&self, position: usize) {
        self.items.borrow_mut().remove(position);
        self.changed();
    }

    /// Everything at once, told as one change; none when there was nothing and is nothing.
    pub fn replace(&self, lines: Vec<String>) {
        let was_empty = std::mem::replace(&mut *self.items.borrow_mut(), lines).is_empty();
        if !(was_empty && self.is_empty()) {
            self.changed();
        }
    }

    pub fn watch(&self, f: impl Fn() + 'static) -> WatchId {
        let id = WatchId(self.next.get());
        self.next.set(id.0 + 1);
        self.watchers.borrow_mut().push((id, Rc::new(f)));
        id
    }

    pub fn unwatch(&self, id: WatchId) {
        self.watchers.borrow_mut().retain(|(w, _)| *w != id);
    }

    /// Called with nothing borrowed, so a watcher may read the lines, change them, or come
    /// and go itself.
    fn changed(&self) {
        let watchers: Vec<Rc<dyn Fn()>> = self
            .watchers
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for f in watchers {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::Lines;

    #[test]
    fn a_watcher_hears_each_change_and_nothing_once_it_has_gone() {
        let lines = Rc::new(Lines::new(vec!["a".into()]));
        let heard = Rc::new(Cell::new(0));
        let id = lines.watch({
            let (lines, heard) = (lines.clone(), heard.clone());
            // Reading the lines from inside the watcher is how a view reloads.
            move || heard.set(heard.get() + lines.len())
        });
        lines.push("b".into());
        assert_eq!(heard.get(), 2);
        lines.replace(vec!["c".into()]);
        assert_eq!(heard.get(), 3);
        lines.extend(Vec::new());
        assert_eq!(heard.get(), 3);
        lines.unwatch(id);
        lines.remove(0);
        assert_eq!(heard.get(), 3);
        assert!(lines.is_empty());
    }

    #[test]
    fn replacing_nothing_with_nothing_is_no_change() {
        let lines = Lines::default();
        let heard = Rc::new(Cell::new(false));
        lines.watch({
            let heard = heard.clone();
            move || heard.set(true)
        });
        lines.replace(Vec::new());
        assert!(!heard.get());
    }
}
