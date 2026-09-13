//! Values kept on a GObject under a typed key. GLib's object data is untyped, and a value
//! read back as another type than the one it was stored as is undefined behaviour. A key
//! here puts its type into the quark it stores under, so a value is only ever found again
//! as what it was stored as, and the unsafe calls stay in this file.

use std::marker::PhantomData;
use std::sync::OnceLock;

use crate::glib;
use crate::glib::prelude::*;

pub struct Key<T> {
    name: &'static str,
    quark: OnceLock<glib::Quark>,
    ty: PhantomData<fn() -> T>,
}

impl<T: 'static> Key<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            quark: OnceLock::new(),
            ty: PhantomData,
        }
    }

    fn quark(&self) -> glib::Quark {
        *self.quark.get_or_init(|| {
            glib::Quark::from_str(format!(
                "spiral-{}-{}",
                self.name,
                std::any::type_name::<T>()
            ))
        })
    }

    pub fn set(&self, object: &impl AsRef<glib::Object>, value: T) {
        // SAFETY: the quark names `T`, so what is stored under it is a `T`.
        unsafe { object.as_ref().set_qdata(self.quark(), value) }
    }

    pub fn take(&self, object: &impl AsRef<glib::Object>) -> Option<T> {
        // SAFETY: as for `set`.
        unsafe { object.as_ref().steal_qdata(self.quark()) }
    }

    pub fn has(&self, object: &impl AsRef<glib::Object>) -> bool {
        // SAFETY: as for `set`; the pointer is not followed.
        unsafe { object.as_ref().qdata::<T>(self.quark()).is_some() }
    }
}

impl<T: Clone + 'static> Key<T> {
    pub fn get(&self, object: &impl AsRef<glib::Object>) -> Option<T> {
        // SAFETY: as for `set`; the value is cloned while the object still holds it.
        unsafe {
            object
                .as_ref()
                .qdata::<T>(self.quark())
                .map(|value| value.as_ref().clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Key;
    use crate::glib;

    #[test]
    fn a_value_is_found_again_only_as_its_own_type() {
        static TEXT: Key<String> = Key::new("value");
        static NUMBER: Key<u32> = Key::new("value");
        let object = glib::Object::new::<glib::Object>();
        TEXT.set(&object, "kept".into());
        assert_eq!(TEXT.get(&object).as_deref(), Some("kept"));
        assert!(!NUMBER.has(&object));
        assert_eq!(NUMBER.get(&object), None);
        assert_eq!(TEXT.take(&object).as_deref(), Some("kept"));
        assert!(!TEXT.has(&object));
    }
}
