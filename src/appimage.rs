//! The icon an AppImage carries, read without running it. An AppImage is a small ELF
//! program, its runtime, followed by a SquashFS image of the application with the icon at
//! `.DirIcon`. Asking the program for its icon would run a program the user only
//! downloaded, so the image is read where it starts, by `unsquashfs`, inside the thumbnail
//! sandbox.

use std::io::Read;
use std::path::Path;

use crate::gio;
use crate::glib;
use crate::gtk::gdk_pixbuf::Pixbuf;

/// Largest icon read out of the image. One that goes on past it is taken for an image made
/// to fill the memory of whatever reads it.
const ICON_BYTES: u64 = 16 << 20;

/// Where the sandbox's `PATH` finds `unsquashfs`, Fedora and Debian putting it apart.
const UNSQUASHFS: [&str; 2] = ["/usr/bin/unsquashfs", "/usr/sbin/unsquashfs"];

/// Whether files of `content_type` are AppImages whose icon can be read here: of the
/// SquashFS kind, with `unsquashfs` installed. Without it they keep the icon of their type
/// and leave no note that they failed, so installing it later brings their icons.
pub(crate) fn readable(content_type: &str) -> bool {
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    gio::content_type_equals(content_type, "application/vnd.appimage")
        && *INSTALLED.get_or_init(|| UNSQUASHFS.iter().any(|p| Path::new(p).exists()))
}

/// Where the image starts in a file beginning with `head`: right after the runtime, an
/// ELF file that ends with its table of section headers. None where `head` is no ELF
/// header.
fn image_offset(head: &[u8]) -> Option<u64> {
    if head.get(..4)? != b"\x7fELF" {
        return None;
    }
    let little = match head.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let number = |at: usize, len: usize| -> Option<u64> {
        let bytes = head.get(at..at + len)?;
        Some(bytes.iter().enumerate().fold(0, |n, (i, &b)| {
            let byte = if little { i } else { len - 1 - i };
            n | u64::from(b) << (8 * byte)
        }))
    };
    let (table, entry, entries) = match head.get(4)? {
        1 => (number(0x20, 4)?, number(0x2e, 2)?, number(0x30, 2)?),
        2 => (number(0x28, 8)?, number(0x3a, 2)?, number(0x3c, 2)?),
        _ => return None,
    };
    table.checked_add(entry * entries)
}

/// Draw the icon of the AppImage at `input` into a PNG at `output`, `size` pixels either
/// way. Runs in the thumbnail sandbox, as the bundled thumbnailer.
pub fn thumbnail(input: &Path, output: &Path, size: i32) -> Result<(), String> {
    let mut head = [0; 64];
    std::fs::File::open(input)
        .and_then(|mut file| file.read_exact(&mut head))
        .map_err(|e| e.to_string())?;
    let offset = image_offset(&head).ok_or("no ELF header")?;
    let mut unsquashfs = std::process::Command::new("unsquashfs")
        .arg("-offset")
        .arg(offset.to_string())
        .arg("-cat")
        .arg(input)
        .arg(".DirIcon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("unsquashfs: {e}"))?;
    let mut icon = Vec::new();
    let read = unsquashfs
        .stdout
        .take()
        .ok_or("unsquashfs: no output")?
        .take(ICON_BYTES + 1)
        .read_to_end(&mut icon);
    if read.is_err() || icon.len() as u64 > ICON_BYTES {
        let _ = unsquashfs.kill();
        let _ = unsquashfs.wait();
        read.map_err(|e| e.to_string())?;
        return Err(format!("the icon is over {ICON_BYTES} bytes"));
    }
    let status = unsquashfs.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("unsquashfs: {status}"));
    }
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(icon));
    Pixbuf::from_stream_at_scale(&stream, size, size, true, gio::Cancellable::NONE)
        .and_then(|pixbuf| pixbuf.savev(output, "png", &[]))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_starts_where_the_runtime_ends() {
        let mut elf64 = vec![0; 64];
        elf64[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf64[0x28..0x30].copy_from_slice(&190_000u64.to_le_bytes());
        elf64[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        elf64[0x3c..0x3e].copy_from_slice(&30u16.to_le_bytes());
        assert_eq!(image_offset(&elf64), Some(190_000 + 64 * 30));

        let mut elf32 = vec![0; 52];
        elf32[..6].copy_from_slice(b"\x7fELF\x01\x02");
        elf32[0x20..0x24].copy_from_slice(&90_000u32.to_be_bytes());
        elf32[0x2e..0x30].copy_from_slice(&40u16.to_be_bytes());
        elf32[0x30..0x32].copy_from_slice(&28u16.to_be_bytes());
        assert_eq!(image_offset(&elf32), Some(90_000 + 40 * 28));

        assert_eq!(image_offset(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(image_offset(&elf64[..0x30]), None);
    }

    /// The usual layout: `.DirIcon` a link to a link to an SVG that names a small size.
    #[test]
    fn the_icon_is_drawn_from_the_image_at_the_size_asked_for() {
        if !UNSQUASHFS.iter().any(|p| Path::new(p).exists())
            || glib::find_program_in_path("mksquashfs").is_none()
        {
            eprintln!("squashfs-tools are missing; nothing to test");
            return;
        }
        let dir = std::env::temp_dir().join(format!("spiral-appimage-{}", std::process::id()));
        let app = dir.join("AppDir");
        std::fs::create_dir_all(app.join("usr/share")).unwrap();
        std::fs::write(
            app.join("usr/share/app.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect width="48" height="48" fill="#5294e2"/></svg>"##,
        )
        .unwrap();
        std::os::unix::fs::symlink("usr/share/app.svg", app.join("app.svg")).unwrap();
        std::os::unix::fs::symlink("app.svg", app.join(".DirIcon")).unwrap();
        let image = dir.join("image");
        let made = std::process::Command::new("mksquashfs")
            .arg(&app)
            .arg(&image)
            .args(["-quiet", "-noappend", "-no-progress"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(made.success());
        // A runtime of nothing but its header, the section headers right after it.
        let mut bundle = vec![0; 64];
        bundle[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bundle[0x28..0x30].copy_from_slice(&64u64.to_le_bytes());
        bundle.extend(std::fs::read(&image).unwrap());
        let input = dir.join("App.AppImage");
        std::fs::write(&input, bundle).unwrap();

        let output = dir.join("icon.png");
        thumbnail(&input, &output, 256).unwrap();
        let icon = Pixbuf::from_file(&output).unwrap();
        assert_eq!((icon.width(), icon.height()), (256, 256));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A program built here ends with its section headers, as an AppImage's runtime does.
    #[test]
    fn a_program_ends_where_its_section_headers_do() {
        let program = std::fs::read("/proc/self/exe").unwrap();
        assert_eq!(image_offset(&program), Some(program.len() as u64));
    }
}
