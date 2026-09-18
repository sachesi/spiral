//! Pictures decoded by glycin, where it is installed and its sandbox starts. Each format's
//! loader runs in a process of its own under bubblewrap, with a seccomp filter of the calls
//! it may make and a memory limit, and hands back the pixels, turned the way the file says
//! and in sRGB or the colour space the file names. The library is looked for when first
//! wanted rather than linked, so Spiral still runs where it is missing, and decodes
//! pictures in its own sandbox there instead (see [`crate::picture`]).
//!
//! Glycin is always asked for its bubblewrap sandbox. Left to choose, it would load files
//! with no sandbox at all where bubblewrap does not start.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk::prelude::*;

use crate::{gdk, gio, glib, gtk};
use glib::gobject_ffi::GObject;
use glib::translate::*;

/// How long one step of a load may take before it is cancelled: as long as a thumbnailer.
const LIMIT: Duration = crate::thumbnails::TIMEOUT;
/// How long a cancelled step is waited for before it is left to finish on its own.
const GRACE: Duration = Duration::from_secs(2);
/// `GLY_SANDBOX_SELECTOR_BWRAP`.
const SANDBOX_BWRAP: c_int = 1;

type Ptr = *mut c_void;
type Callback = gio::ffi::GAsyncReadyCallback;
type GError = glib::ffi::GError;

/// The functions of `libglycin-2` used here, as its header declares them.
struct Api {
    loader_new: unsafe extern "C" fn(*mut gio::ffi::GFile) -> Ptr,
    loader_new_for_bytes: unsafe extern "C" fn(*mut glib::ffi::GBytes) -> Ptr,
    loader_set_sandbox_selector: unsafe extern "C" fn(Ptr, c_int),
    loader_get_mime_types_async: unsafe extern "C" fn(*mut gio::ffi::GCancellable, Callback, Ptr),
    loader_get_mime_types_finish:
        unsafe extern "C" fn(*mut gio::ffi::GAsyncResult, *mut *mut GError) -> *mut *mut c_char,
    loader_load_async: unsafe extern "C" fn(Ptr, *mut gio::ffi::GCancellable, Callback, Ptr),
    loader_load_finish:
        unsafe extern "C" fn(Ptr, *mut gio::ffi::GAsyncResult, *mut *mut GError) -> Ptr,
    image_get_width: unsafe extern "C" fn(Ptr) -> u32,
    image_get_height: unsafe extern "C" fn(Ptr) -> u32,
    frame_request_new: unsafe extern "C" fn() -> Ptr,
    frame_request_set_scale: unsafe extern "C" fn(Ptr, u32, u32),
    image_get_specific_frame_async:
        unsafe extern "C" fn(Ptr, Ptr, *mut gio::ffi::GCancellable, Callback, Ptr),
    image_get_specific_frame_finish:
        unsafe extern "C" fn(Ptr, *mut gio::ffi::GAsyncResult, *mut *mut GError) -> Ptr,
    frame_get_width: unsafe extern "C" fn(Ptr) -> u32,
    frame_get_height: unsafe extern "C" fn(Ptr) -> u32,
    frame_get_stride: unsafe extern "C" fn(Ptr) -> u32,
    frame_get_buf_bytes: unsafe extern "C" fn(Ptr) -> *mut glib::ffi::GBytes,
    frame_get_memory_format: unsafe extern "C" fn(Ptr) -> c_int,
    frame_get_color_cicp: unsafe extern "C" fn(Ptr) -> *mut Cicp,
    cicp_free: unsafe extern "C" fn(*mut Cicp),
}

/// `GlyCicp`.
#[repr(C)]
struct Cicp {
    color_primaries: u8,
    transfer_characteristics: u8,
    matrix_coefficients: u8,
    video_full_range_flag: u8,
}

impl Api {
    // Each symbol takes the type of the field it is put in, which is the one the header
    // declares.
    #[allow(clippy::missing_transmute_annotations)]
    fn open() -> Option<Self> {
        // SAFETY: a library opened once and never closed; every symbol is looked up by
        // the name the header gives it and used with the type it declares.
        unsafe {
            let lib = libc::dlopen(
                c"libglycin-2.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            );
            if lib.is_null() {
                return None;
            }
            macro_rules! sym {
                ($name:literal) => {{
                    let sym = libc::dlsym(lib, concat!($name, "\0").as_ptr().cast());
                    if sym.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, _>(sym)
                }};
            }
            Some(Self {
                loader_new: sym!("gly_loader_new"),
                loader_new_for_bytes: sym!("gly_loader_new_for_bytes"),
                loader_set_sandbox_selector: sym!("gly_loader_set_sandbox_selector"),
                loader_get_mime_types_async: sym!("gly_loader_get_mime_types_async"),
                loader_get_mime_types_finish: sym!("gly_loader_get_mime_types_finish"),
                loader_load_async: sym!("gly_loader_load_async"),
                loader_load_finish: sym!("gly_loader_load_finish"),
                image_get_width: sym!("gly_image_get_width"),
                image_get_height: sym!("gly_image_get_height"),
                frame_request_new: sym!("gly_frame_request_new"),
                frame_request_set_scale: sym!("gly_frame_request_set_scale"),
                image_get_specific_frame_async: sym!("gly_image_get_specific_frame_async"),
                image_get_specific_frame_finish: sym!("gly_image_get_specific_frame_finish"),
                frame_get_width: sym!("gly_frame_get_width"),
                frame_get_height: sym!("gly_frame_get_height"),
                frame_get_stride: sym!("gly_frame_get_stride"),
                frame_get_buf_bytes: sym!("gly_frame_get_buf_bytes"),
                frame_get_memory_format: sym!("gly_frame_get_memory_format"),
                frame_get_color_cicp: sym!("gly_frame_get_color_cicp"),
                cicp_free: sym!("gly_cicp_free"),
            })
        }
    }
}

/// Glycin, ready to decode.
pub(crate) struct Glycin {
    api: Api,
    /// The MIME types its loaders take.
    types: Vec<String>,
}

/// Why glycin gave no picture.
#[derive(Debug)]
pub(crate) enum Refused {
    /// It looked at the file and could not draw it, or would not at the size it has.
    File(String),
    /// It took longer than [`LIMIT`], which says nothing of the file.
    Time,
}

/// Glycin, if it is installed and its sandbox starts here. Found out on first use, which
/// starts a loader, so never on the main loop. `SPIRAL_GLYCIN=0` leaves it out, to try
/// what happens where it is missing.
pub(crate) fn get() -> Option<&'static Glycin> {
    static GLYCIN: OnceLock<Option<Glycin>> = OnceLock::new();
    GLYCIN
        .get_or_init(|| {
            let found = find();
            glib::g_debug!(
                "spiral",
                "pictures are decoded {}",
                if found.is_some() {
                    "by glycin, in its sandbox"
                } else {
                    "by spiral-thumbnailer, in Spiral's sandbox"
                }
            );
            found
        })
        .as_ref()
}

fn find() -> Option<Glycin> {
    if std::env::var_os("SPIRAL_GLYCIN").is_some_and(|v| v == "0") {
        return None;
    }
    let api = Api::open()?;
    // Asked the asynchronous way, as everything here is: glycin's blocking calls wait on
    // the main loop's context, which the main thread holds.
    let context = glib::MainContext::new();
    let types: Vec<String> = context
        .with_thread_default(|| {
            let result = wait(&context, |cancellable, done, data| unsafe {
                (api.loader_get_mime_types_async)(cancellable, done, data)
            })
            .ok()?;
            let mut error = std::ptr::null_mut();
            // SAFETY: the result of the call above; a NULL-terminated array of strings,
            // owned by the caller, or an error.
            unsafe {
                let types = (api.loader_get_mime_types_finish)(result.as_ptr() as _, &mut error);
                if !error.is_null() {
                    glib::ffi::g_error_free(error);
                    return None;
                }
                Some(FromGlibPtrContainer::from_glib_full(types))
            }
        })
        .ok()
        .flatten()?;
    let glycin = Glycin { api, types };
    // Whether the sandbox starts is only known by starting it: a picture of one pixel.
    let pixel = gdk::MemoryTexture::new(
        1,
        1,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_static(&[0, 0, 0, 0]),
        4,
    )
    .save_to_png_bytes();
    match glycin.load_bytes(&pixel, 1, None) {
        Ok(_) => Some(glycin),
        Err(e) => {
            glib::g_debug!("spiral", "glycin's sandbox does not start here: {e:?}");
            None
        }
    }
}

impl Glycin {
    /// Whether one of glycin's loaders takes files of `content_type`.
    pub fn handles(&self, content_type: &str) -> bool {
        self.types.iter().any(|t| {
            gio::content_type_equals(content_type, t) || gio::content_type_is_a(content_type, t)
        })
    }

    /// The picture in `file`, of at most `max_pixels`. `fit` asks for it no larger than
    /// that many pixels either way, which loaders that draw rather than decode, as SVG's
    /// does, honour; the others give it at its own size.
    pub fn load_file(
        &self,
        file: &gio::File,
        max_pixels: i64,
        fit: Option<u32>,
    ) -> Result<gdk::Texture, Refused> {
        // SAFETY: a new loader for the file, owned from here on.
        let loader = unsafe { self.own((self.api.loader_new)(file.to_glib_none().0)) };
        self.load(loader, max_pixels, fit)
    }

    /// As [`Self::load_file`], for a picture already read.
    pub fn load_bytes(
        &self,
        bytes: &glib::Bytes,
        max_pixels: i64,
        fit: Option<u32>,
    ) -> Result<gdk::Texture, Refused> {
        // SAFETY: as above.
        let loader = unsafe { self.own((self.api.loader_new_for_bytes)(bytes.to_glib_none().0)) };
        self.load(loader, max_pixels, fit)
    }

    /// A new object of glycin's, owned. It never hands back NULL for one it makes.
    unsafe fn own(&self, ptr: Ptr) -> glib::Object {
        unsafe { from_glib_full(ptr as *mut GObject) }
    }

    fn load(
        &self,
        loader: glib::Object,
        max_pixels: i64,
        fit: Option<u32>,
    ) -> Result<gdk::Texture, Refused> {
        let api = &self.api;
        let loader_ptr = loader.as_ptr() as Ptr;
        // SAFETY: each call is made with objects of the type it takes; what comes back
        // is owned as the header says, and every step is finished on this thread.
        unsafe { (api.loader_set_sandbox_selector)(loader_ptr, SANDBOX_BWRAP) };
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let image = wait(&context, |cancellable, done, data| unsafe {
                    (api.loader_load_async)(loader_ptr, cancellable, done, data)
                })
                .and_then(|result| unsafe {
                    finish(|error| {
                        (api.loader_load_finish)(loader_ptr, result.as_ptr() as _, error)
                    })
                })
                .map(|ptr| unsafe { self.own(ptr) })?;
                let image_ptr = image.as_ptr() as Ptr;
                let (width, height) = unsafe {
                    (
                        (api.image_get_width)(image_ptr),
                        (api.image_get_height)(image_ptr),
                    )
                };
                if i64::from(width) * i64::from(height) > max_pixels {
                    return Err(Refused::File(format!("{width}×{height} is too large")));
                }
                let request = unsafe { self.own((api.frame_request_new)()) };
                let request_ptr = request.as_ptr() as Ptr;
                if let Some(side) = fit {
                    unsafe { (api.frame_request_set_scale)(request_ptr, side, side) };
                }
                let frame = wait(&context, |cancellable, done, data| unsafe {
                    (api.image_get_specific_frame_async)(
                        image_ptr,
                        request_ptr,
                        cancellable,
                        done,
                        data,
                    )
                })
                .and_then(|result| unsafe {
                    finish(|error| {
                        (api.image_get_specific_frame_finish)(
                            image_ptr,
                            result.as_ptr() as _,
                            error,
                        )
                    })
                })
                .map(|ptr| unsafe { self.own(ptr) })?;
                self.texture(frame.as_ptr() as Ptr, max_pixels)
            })
            .map_err(|e| Refused::File(e.to_string()))?
    }

    /// The texture a frame holds, once its numbers agree with its pixels.
    fn texture(&self, frame: Ptr, max_pixels: i64) -> Result<gdk::Texture, Refused> {
        let api = &self.api;
        let refused = |what: &str| Refused::File(what.to_string());
        // SAFETY: a frame glycin made; its bytes are borrowed and taken a reference to,
        // and its colour numbers are owned.
        let (width, height, stride, bytes, format, cicp) = unsafe {
            let cicp = (api.frame_get_color_cicp)(frame);
            let numbers = (!cicp.is_null()).then(|| {
                let c = &*cicp;
                let numbers = (
                    c.color_primaries,
                    c.transfer_characteristics,
                    c.matrix_coefficients,
                    c.video_full_range_flag,
                );
                (api.cicp_free)(cicp);
                numbers
            });
            let bytes: Option<glib::Bytes> = from_glib_none((api.frame_get_buf_bytes)(frame));
            (
                (api.frame_get_width)(frame),
                (api.frame_get_height)(frame),
                (api.frame_get_stride)(frame) as usize,
                bytes.ok_or_else(|| refused("no pixels"))?,
                (api.frame_get_memory_format)(frame),
                numbers,
            )
        };
        let (format, bpp) = memory_format(format).ok_or_else(|| refused("unknown format"))?;
        let pixels = i64::from(width) * i64::from(height);
        if pixels == 0 || pixels > max_pixels || width > i32::MAX as u32 || height > i32::MAX as u32
        {
            return Err(refused("size out of bounds"));
        }
        let row = width as usize * bpp;
        let needed = stride * (height as usize - 1) + row;
        if stride < row || bytes.len() < needed {
            return Err(refused("fewer pixels than the frame says"));
        }
        let mut builder = gdk::MemoryTextureBuilder::new()
            .set_bytes(Some(&bytes))
            .set_width(width as i32)
            .set_height(height as i32)
            .set_stride(stride)
            .set_format(format);
        if let Some((primaries, transfer, matrix, full)) = cicp {
            let params = gdk::CicpParams::new();
            params.set_color_primaries(primaries.into());
            params.set_transfer_function(transfer.into());
            params.set_matrix_coefficients(matrix.into());
            params.set_range(if full != 0 {
                gdk::CicpRange::Full
            } else {
                gdk::CicpRange::Narrow
            });
            let state = params
                .build_color_state()
                .map_err(|e| Refused::File(e.to_string()))?;
            builder = builder.set_color_state(&state);
        }
        Ok(builder.build())
    }
}

/// The GDK format a `GlyMemoryFormat` names, and the bytes one pixel takes in it.
fn memory_format(format: c_int) -> Option<(gdk::MemoryFormat, usize)> {
    use gdk::MemoryFormat as F;
    Some(match format {
        0 => (F::B8g8r8a8Premultiplied, 4),
        1 => (F::A8r8g8b8Premultiplied, 4),
        2 => (F::R8g8b8a8Premultiplied, 4),
        3 => (F::B8g8r8a8, 4),
        4 => (F::A8r8g8b8, 4),
        5 => (F::R8g8b8a8, 4),
        6 => (F::A8b8g8r8, 4),
        7 => (F::R8g8b8, 3),
        8 => (F::B8g8r8, 3),
        9 => (F::R16g16b16, 6),
        10 => (F::R16g16b16a16Premultiplied, 8),
        11 => (F::R16g16b16a16, 8),
        12 => (F::R16g16b16Float, 6),
        13 => (F::R16g16b16a16Float, 8),
        14 => (F::R32g32b32Float, 12),
        15 => (F::R32g32b32a32FloatPremultiplied, 16),
        16 => (F::R32g32b32a32Float, 16),
        17 => (F::G8a8Premultiplied, 2),
        18 => (F::G8a8, 2),
        19 => (F::G8, 1),
        20 => (F::G16a16Premultiplied, 4),
        21 => (F::G16a16, 4),
        22 => (F::G16, 2),
        _ => return None,
    })
}

/// Start one asynchronous step of glycin's with `start`, on `context`, which is this
/// thread's default, and run the context until the step has finished: what it finished
/// with, to hand its `finish` function. A step still running after [`LIMIT`] is cancelled, and
/// one that does not finish even then is left to itself.
fn wait(
    context: &glib::MainContext,
    start: impl FnOnce(*mut gio::ffi::GCancellable, Callback, Ptr),
) -> Result<glib::Object, Refused> {
    type Slot = RefCell<Option<glib::Object>>;
    unsafe extern "C" fn done(_: *mut GObject, result: *mut gio::ffi::GAsyncResult, data: Ptr) {
        // SAFETY: `data` is the slot below, which outlives the step (or is leaked).
        let slot = unsafe { &*(data as *const Slot) };
        *slot.borrow_mut() = Some(unsafe { from_glib_none(result as *mut GObject) });
    }
    let cancellable = gio::Cancellable::new();
    let abandoned = std::sync::Arc::new(AtomicBool::new(false));
    let timer = glib::timeout_source_new(LIMIT, None, glib::Priority::DEFAULT, {
        let cancellable = cancellable.clone();
        move || {
            cancellable.cancel();
            glib::ControlFlow::Break
        }
    });
    timer.attach(Some(context));
    let giving_up = glib::timeout_source_new(LIMIT + GRACE, None, glib::Priority::DEFAULT, {
        let abandoned = abandoned.clone();
        move || {
            abandoned.store(true, Ordering::SeqCst);
            glib::ControlFlow::Break
        }
    });
    giving_up.attach(Some(context));
    let slot: Box<Slot> = Box::default();
    start(
        cancellable.to_glib_none().0,
        Some(done),
        &*slot as *const Slot as Ptr,
    );
    while slot.borrow().is_none() && !abandoned.load(Ordering::SeqCst) {
        context.iteration(true);
    }
    timer.destroy();
    giving_up.destroy();
    let result = slot.borrow_mut().take();
    match result {
        Some(result) if !cancellable.is_cancelled() => Ok(result),
        Some(_) => Err(Refused::Time),
        None => {
            // Still running: the slot stays for its callback to write into.
            Box::leak(slot);
            Err(Refused::Time)
        }
    }
}

/// Call a `finish` function, turning the error it sets into a refusal.
unsafe fn finish(call: impl FnOnce(*mut *mut GError) -> Ptr) -> Result<Ptr, Refused> {
    let mut error = std::ptr::null_mut();
    let ptr = call(&mut error);
    if !error.is_null() {
        let error: glib::Error = unsafe { from_glib_full(error) };
        // The first line says what went wrong; glycin follows it with its whole setup.
        let message = error.message().lines().next().unwrap_or_default();
        return Err(Refused::File(message.to_string()));
    }
    if ptr.is_null() {
        return Err(Refused::File("nothing loaded".into()));
    }
    Ok(ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where glycin works, a picture comes back whole, and one larger than allowed is
    /// refused before it is decoded.
    #[test]
    fn glycin_decodes_in_its_sandbox() {
        let Some(glycin) = get() else {
            eprintln!("glycin is not available here; nothing to test");
            return;
        };
        let png = gdk::MemoryTexture::new(
            3,
            2,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(vec![0x80u8; 3 * 2 * 4]),
            12,
        )
        .save_to_png_bytes();
        let texture = glycin.load_bytes(&png, 100, None).unwrap();
        assert_eq!((texture.width(), texture.height()), (3, 2));
        assert!(matches!(
            glycin.load_bytes(&png, 5, None),
            Err(Refused::File(_))
        ));
        assert!(glycin.handles("image/png"));
        assert!(!glycin.handles("video/webm"));
    }
}
