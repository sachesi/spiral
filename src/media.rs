//! Video and sound for the preview, decoded in the sandbox. The helper runs the half of a
//! pipeline that reads the file: the demuxer, the decoders, the conversion to one plain
//! format. What comes out is handed to the player over sockets, a picture at a time and a
//! stretch of sound at a time, and the player's own pipeline only queues it, sets the
//! volume and draws it. Nothing the file says reaches the file manager but those samples,
//! the few facts below, and the cover it carries, which the preview decodes as it decodes
//! any picture.
//!
//! Three sockets, one way each but the first:
//!
//! - control: the helper's [`Note`]s one way, the player's seeks the other;
//! - video: pictures the GPU decoded, as DMA-BUF descriptors passed along with their layout
//!   and handed back once shown, or else BGRA rows of four bytes a pixel with nothing
//!   between them;
//! - sound: 32-bit float stereo at 48 kHz, interleaved.
//!
//! Every number is little-endian. A file on another machine, which the sandbox cannot
//! reach, is read through a fourth socket: the player says its size, then answers each
//! request for an offset and a length with the bytes found there.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gst::prelude::*;
use gstreamer_allocators as gst_allocators;
use gstreamer_video as gst_video;

use crate::gst;

/// Sound travels as 32-bit float stereo at 48 kHz, whatever the file holds.
pub(crate) const AUDIO_RATE: i32 = 48_000;
pub(crate) const AUDIO_CHANNELS: i32 = 2;
/// The most sound one packet may hold, under three seconds of it.
pub(crate) const AUDIO_PACKET_MAX: usize = 1 << 20;
/// Pictures are taken up to this many pixels across and down: 8K.
const SIDE_MAX: u32 = 8192;
const COVER_MAX: usize = 16 << 20;
const TEXT_MAX: usize = 400;
/// The longest colorimetry a picture may be said to have, as GStreamer writes one.
const COLORIMETRY_MAX: usize = 64;
/// The most bytes one read of a file on another machine asks for.
pub(crate) const READ_MAX: u32 = 1 << 20;
/// A time that is not known, as GStreamer writes it.
const NONE: u64 = u64::MAX;

/// What the pictures of a file are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    /// Frames a second as a fraction; 0/1 where it varies.
    pub fps: (i32, i32),
    /// The proportions of one pixel.
    pub par: (i32, i32),
}

impl Picture {
    /// Bytes in one picture.
    pub fn len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// BGRA rows, or DMA-BUFs of the format `drm` names, in the colours `colorimetry`
    /// says: the rows come converted to sRGB, the DMA-BUFs as the decoder left them.
    pub fn caps(&self, drm: Option<&str>, colorimetry: Option<&str>) -> gst::Caps {
        let mut caps = gst::Caps::builder("video/x-raw")
            .field("format", if drm.is_some() { "DMA_DRM" } else { "BGRA" })
            .field("width", self.width as i32)
            .field("height", self.height as i32)
            .field("framerate", gst::Fraction::new(self.fps.0, self.fps.1))
            .field(
                "pixel-aspect-ratio",
                gst::Fraction::new(self.par.0, self.par.1),
            )
            .build();
        if let Some(drm) = drm {
            let caps = caps.make_mut();
            if let Some(s) = caps.structure_mut(0) {
                s.set("drm-format", drm);
                // Taken only as GStreamer reads it back: without it, GTK would guess the
                // colours from the size of the picture.
                if let Some(colorimetry) =
                    colorimetry.and_then(|c| c.parse::<gst_video::VideoColorimetry>().ok())
                {
                    s.set("colorimetry", colorimetry.to_string());
                }
            }
            caps.set_features(
                0,
                Some(gst::CapsFeatures::new([
                    gst_allocators::CAPS_FEATURE_MEMORY_DMABUF,
                ])),
            );
        }
        caps
    }

    fn valid(&self) -> bool {
        let fraction = |(n, d): (i32, i32), zero: bool| {
            (if zero { n >= 0 } else { n > 0 }) && d > 0 && n <= 1_000_000 && d <= 1_000_000
        };
        (1..=SIDE_MAX).contains(&self.width)
            && (1..=SIDE_MAX).contains(&self.height)
            && fraction(self.fps, true)
            && fraction(self.par, false)
    }

    fn of(caps: &gst::CapsRef) -> Option<Self> {
        let s = caps.structure(0)?;
        let fraction = |name: &str, default: (i32, i32)| {
            s.get::<gst::Fraction>(name)
                .map_or(default, |f| (f.numer(), f.denom()))
        };
        let picture = Self {
            width: u32::try_from(s.get::<i32>("width").ok()?).ok()?,
            height: u32::try_from(s.get::<i32>("height").ok()?).ok()?,
            fps: fraction("framerate", (0, 1)),
            par: fraction("pixel-aspect-ratio", (1, 1)),
        };
        picture.valid().then_some(picture)
    }
}

/// What the helper tells the player.
#[derive(Debug, PartialEq)]
pub(crate) enum Note {
    /// The file has started, and holds this.
    Ready {
        audio: bool,
        video: Option<Picture>,
        /// Which of the DMA-BUF formats the player listed the pictures come in, as
        /// [`Packet::Frame`]s; `None` for BGRA rows.
        drm: Option<u32>,
        /// The colours of those pictures, as GStreamer writes them: range, matrix,
        /// transfer function and primaries.
        colorimetry: Option<String>,
        seekable: bool,
        /// Nanoseconds.
        duration: Option<u64>,
    },
    /// The length has turned out other than it was said to be.
    Duration(Option<u64>),
    /// The picture the file carries, as it is in the file.
    Cover(Vec<u8>),
    /// There is a picture in the file and nothing to decode it with.
    NoVideoDecoder,
    /// The element decoding the picture, for the log: whether it runs on the GPU.
    Decoder(String),
    Error(String),
}

/// One packet of pictures or sound.
#[derive(Debug, PartialEq)]
pub(crate) enum Packet {
    Data {
        /// Which seek it follows; see [`serve`].
        epoch: u32,
        /// Stream time, nanoseconds.
        pts: Option<u64>,
        duration: Option<u64>,
        bytes: Vec<u8>,
    },
    End {
        epoch: u32,
    },
    /// A picture in GPU memory: one descriptor for each of `memories`, sent along with the
    /// packet, and handed back by `id` once it has been shown.
    Frame {
        epoch: u32,
        pts: Option<u64>,
        duration: Option<u64>,
        id: u64,
        /// Offset and size of each memory in its DMA-BUF.
        memories: Vec<(u64, u64)>,
        /// Offset and stride of each plane, the memories taken one after another.
        planes: Vec<(u64, i32)>,
    },
}

/// The most memories or planes one picture may have.
const PARTS_MAX: usize = 4;
/// The most pictures in GPU memory sent and not yet handed back: more than the player
/// holds at once, in its source, its queue and on screen.
const FRAMES_AHEAD: usize = 16;

fn time(t: u64) -> Option<u64> {
    (t != NONE).then_some(t)
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_bytes(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

pub(crate) fn write_note(w: &mut impl Write, note: &Note) -> io::Result<()> {
    let mut body = Vec::new();
    let kind = match note {
        Note::Ready {
            audio,
            video,
            drm,
            colorimetry,
            seekable,
            duration,
        } => {
            let p = video.unwrap_or(Picture {
                width: 0,
                height: 0,
                fps: (0, 1),
                par: (1, 1),
            });
            body.push(u8::from(*audio));
            body.push(u8::from(video.is_some()));
            for n in [p.width, p.height] {
                body.extend_from_slice(&n.to_le_bytes());
            }
            for n in [p.fps.0, p.fps.1, p.par.0, p.par.1] {
                body.extend_from_slice(&n.to_le_bytes());
            }
            body.push(u8::from(*seekable));
            body.extend_from_slice(&duration.unwrap_or(NONE).to_le_bytes());
            body.extend_from_slice(&drm.unwrap_or(u32::MAX).to_le_bytes());
            body.extend_from_slice(colorimetry.as_deref().unwrap_or_default().as_bytes());
            1
        }
        Note::Duration(d) => {
            body.extend_from_slice(&d.unwrap_or(NONE).to_le_bytes());
            2
        }
        Note::Cover(bytes) => {
            body.extend_from_slice(bytes);
            3
        }
        Note::NoVideoDecoder => 4,
        Note::Decoder(name) => {
            body.extend_from_slice(name.as_bytes());
            6
        }
        Note::Error(text) => {
            body.extend_from_slice(text.as_bytes());
            5
        }
    };
    let mut out = vec![kind];
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out)
}

/// The next note, checked: anything malformed is an error, and the helper that sent it is
/// done with.
pub(crate) fn read_note(r: &mut impl Read) -> io::Result<Note> {
    let mut kind = [0];
    r.read_exact(&mut kind)?;
    let len = read_u32(r)? as usize;
    if len > COVER_MAX {
        return Err(invalid("note too long"));
    }
    let body = read_bytes(r, len)?;
    let fixed = |n: usize| {
        if body.len() == n {
            Ok(())
        } else {
            Err(invalid("note of the wrong length"))
        }
    };
    let u32_at = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
    let i32_at = |at: usize| i32::from_le_bytes(body[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
    Ok(match kind[0] {
        1 => {
            if !(39..=39 + COLORIMETRY_MAX).contains(&body.len()) {
                return Err(invalid("note of the wrong length"));
            }
            let colorimetry = &body[39..];
            if !plain_colorimetry(colorimetry) {
                return Err(invalid("colorimetry out of bounds"));
            }
            let picture = Picture {
                width: u32_at(2),
                height: u32_at(6),
                fps: (i32_at(10), i32_at(14)),
                par: (i32_at(18), i32_at(22)),
            };
            let video = match body[1] {
                0 => None,
                _ if picture.valid() => Some(picture),
                _ => return Err(invalid("picture out of bounds")),
            };
            Note::Ready {
                audio: body[0] != 0,
                video,
                drm: Some(u32_at(35)).filter(|&i| i != u32::MAX),
                colorimetry: (!colorimetry.is_empty())
                    .then(|| String::from_utf8_lossy(colorimetry).into_owned()),
                seekable: body[26] != 0,
                duration: time(u64_at(27)),
            }
        }
        2 => {
            fixed(8)?;
            Note::Duration(time(u64_at(0)))
        }
        3 => Note::Cover(body),
        4 => {
            fixed(0)?;
            Note::NoVideoDecoder
        }
        5 => Note::Error(
            String::from_utf8_lossy(&body)
                .chars()
                .filter(|c| !c.is_control())
                .take(TEXT_MAX)
                .collect(),
        ),
        6 => Note::Decoder(
            String::from_utf8_lossy(&body)
                .chars()
                .filter(|c| c.is_ascii_graphic())
                .take(64)
                .collect(),
        ),
        _ => return Err(invalid("unknown note")),
    })
}

/// Whether `colorimetry` is written in the letters GStreamer writes one in: its names, as
/// `bt709` or `bt2100-pq`, or four numbers, as `1:3:5:1`. The player takes no other, so
/// the helper sends no other.
fn plain_colorimetry(colorimetry: &[u8]) -> bool {
    colorimetry
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b':' || c == b'-')
}

pub(crate) fn write_packet(w: &mut impl Write, packet: &Packet) -> io::Result<()> {
    w.write_all(&encode_packet(packet))
}

fn encode_packet(packet: &Packet) -> Vec<u8> {
    let layout;
    let (epoch, kind, pts, duration, bytes): (u32, u8, u64, u64, &[u8]) = match packet {
        Packet::Data {
            epoch,
            pts,
            duration,
            bytes,
        } => (
            *epoch,
            0,
            pts.unwrap_or(NONE),
            duration.unwrap_or(NONE),
            bytes,
        ),
        Packet::End { epoch } => (*epoch, 1, NONE, NONE, &[]),
        Packet::Frame {
            epoch,
            pts,
            duration,
            id,
            memories,
            planes,
        } => {
            let mut out = id.to_le_bytes().to_vec();
            out.push(memories.len() as u8);
            for (offset, size) in memories {
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(&size.to_le_bytes());
            }
            out.push(planes.len() as u8);
            for (offset, stride) in planes {
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(&stride.to_le_bytes());
            }
            layout = out;
            (
                *epoch,
                2,
                pts.unwrap_or(NONE),
                duration.unwrap_or(NONE),
                &layout,
            )
        }
    };
    let mut out = Vec::with_capacity(25 + bytes.len());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&pts.to_le_bytes());
    out.extend_from_slice(&duration.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Send `packet` with `fds` passed along with it, as one message.
fn send_with_fds(socket: &UnixStream, packet: &Packet, fds: &[RawFd]) -> io::Result<()> {
    let bytes = encode_packet(packet);
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    let fd_bytes = std::mem::size_of_val(fds);
    // SAFETY: CMSG_SPACE only computes a size.
    let space = unsafe { libc::CMSG_SPACE(fd_bytes as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: an all-zero msghdr is a valid empty one; the pointers set below outlive the
    // call, and the one control message written fits the space made for it.
    let sent = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(fd_bytes as u32) as _;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(cmsg).cast::<RawFd>(),
                fds.len(),
            );
        }
        libc::sendmsg(socket.as_raw_fd(), &msg, libc::MSG_NOSIGNAL)
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    // The descriptors went with the first byte; whatever did not fit follows plainly.
    (&*socket).write_all(&bytes[sent as usize..])
}

#[cfg(test)]
pub(crate) fn send_frame_for_test(socket: &UnixStream, packet: &Packet, fds: &[RawFd]) {
    send_with_fds(socket, packet, fds).unwrap();
}

/// The player's end of the video socket: read as a stream, keeping the descriptors that
/// arrive with the bytes, in the order they came. A helper sending more than a picture's
/// worth without the packets they belong to is refused.
pub(crate) struct FdStream {
    socket: UnixStream,
    fds: VecDeque<OwnedFd>,
}

impl FdStream {
    pub fn new(socket: UnixStream) -> Self {
        Self {
            socket,
            fds: VecDeque::new(),
        }
    }

    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }
}

impl Read for FdStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        // SAFETY: as in `send_with_fds`; the kernel writes no more control data than the
        // space given, and each descriptor it passes is read out once and owned from then.
        unsafe {
            let space = libc::CMSG_SPACE((PARTS_MAX * std::mem::size_of::<RawFd>()) as u32);
            let mut control = vec![0u8; space as usize];
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            let n = libc::recvmsg(self.socket.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        self.fds
                            .push_back(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
            if msg.msg_flags & libc::MSG_CTRUNC != 0 || self.fds.len() > PARTS_MAX {
                return Err(invalid("too many descriptors"));
            }
            Ok(n as usize)
        }
    }
}

/// The next packet, of at most `max` bytes; `exact` bytes if given, as a picture has.
pub(crate) fn read_packet(
    r: &mut impl Read,
    max: usize,
    exact: Option<usize>,
) -> io::Result<Packet> {
    let epoch = read_u32(r)?;
    let mut end = [0];
    r.read_exact(&mut end)?;
    let pts = read_u64(r)?;
    let duration = read_u64(r)?;
    let len = read_u32(r)? as usize;
    if end[0] == 1 {
        return if len == 0 {
            Ok(Packet::End { epoch })
        } else {
            Err(invalid("end with data"))
        };
    }
    if end[0] == 2 {
        if len > 256 {
            return Err(invalid("layout too long"));
        }
        let body = read_bytes(r, len)?;
        let mut at = &body[..];
        let mut take = |n: usize| -> io::Result<&[u8]> {
            if at.len() < n {
                return Err(invalid("layout too short"));
            }
            let (head, rest) = at.split_at(n);
            at = rest;
            Ok(head)
        };
        let id = u64::from_le_bytes(take(8)?.try_into().unwrap());
        let count = |n: u8| (1..=PARTS_MAX).contains(&(n as usize));
        let n = take(1)?[0];
        if !count(n) {
            return Err(invalid("memories out of bounds"));
        }
        let mut memories = Vec::new();
        for _ in 0..n {
            let offset = u64::from_le_bytes(take(8)?.try_into().unwrap());
            let size = u64::from_le_bytes(take(8)?.try_into().unwrap());
            memories.push((offset, size));
        }
        let n = take(1)?[0];
        if !count(n) {
            return Err(invalid("planes out of bounds"));
        }
        let mut planes = Vec::new();
        for _ in 0..n {
            let offset = u64::from_le_bytes(take(8)?.try_into().unwrap());
            let stride = i32::from_le_bytes(take(4)?.try_into().unwrap());
            planes.push((offset, stride));
        }
        if !at.is_empty() {
            return Err(invalid("layout too long"));
        }
        return Ok(Packet::Frame {
            epoch,
            pts: time(pts),
            duration: time(duration),
            id,
            memories,
            planes,
        });
    }
    if end[0] != 0 {
        return Err(invalid("unknown packet"));
    }
    if len > max || exact.is_some_and(|n| n != len) || len == 0 {
        return Err(invalid("packet of the wrong length"));
    }
    Ok(Packet::Data {
        epoch,
        pts: time(pts),
        duration: time(duration),
        bytes: read_bytes(r, len)?,
    })
}

/// The player's side of a seek: to `position` nanoseconds, the packets after it marked
/// with `epoch`. Sent as [`send_now`] sends.
pub(crate) fn write_seek(socket: &UnixStream, epoch: u32, position: u64) -> bool {
    let mut out = epoch.to_le_bytes().to_vec();
    out.extend_from_slice(&position.to_le_bytes());
    send_now(socket, &out)
}

/// Send `bytes`, a few of them, whole or not at all, without waiting: a helper that has
/// stopped reading must not hold whoever sends, which may be the thread drawing the window.
/// The socket stays blocking for the thread reading from its other handle.
pub(crate) fn send_now(socket: &UnixStream, bytes: &[u8]) -> bool {
    // SAFETY: the buffer outlives the call.
    let sent = unsafe {
        libc::send(
            socket.as_raw_fd(),
            bytes.as_ptr().cast(),
            bytes.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    sent == bytes.len() as isize
}

/// A `missing-plugin` message left by a decoder nobody has, for a stream that carries the
/// picture. The detail of such a message is the caps that went unhandled.
fn missing_video_decoder(message: Option<&gst::StructureRef>) -> bool {
    let Some(message) = message else {
        return false;
    };
    if message.name() != "missing-plugin" {
        return false;
    }
    message
        .get::<gst::Caps>("detail")
        .ok()
        .and_then(|caps| Some(caps.structure(0)?.name().starts_with("video/")))
        .unwrap_or(false)
}

// ---- in the sandbox -----------------------------------------------------------------------

/// Where the decoded samples of one kind go.
struct Out {
    socket: Mutex<UnixStream>,
    /// The epoch of what the sink receives now: the one asked for by the last seek, from
    /// the moment its flush has gone through the sink. What was on its way before that is
    /// sent with the epoch before, and the player drops it.
    epoch: AtomicU32,
    /// Pictures in GPU memory the player has not handed back yet, by the id they were
    /// sent with: the decoder must not draw the next one over them while they are shown.
    held: Mutex<HashMap<u64, gst::Buffer>>,
    /// Told whenever one of them is handed back.
    returned: std::sync::Condvar,
    next: AtomicU64,
}

/// What the helper asks the GPU decoders for: DMA-BUFs in one of the formats the player's
/// sink can take as they are.
fn dmabuf_caps(formats: &[String]) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features([gst_allocators::CAPS_FEATURE_MEMORY_DMABUF])
        .field("format", "DMA_DRM")
        .field("drm-format", gst::List::new(formats.iter().cloned()))
        .build()
}

/// Plugins whose decoders run on a GPU. Where the player could give the helper no GPU,
/// or the one it gave does not work in here, they are left out and the file is decoded in
/// software.
const GPU_PLUGINS: [&str; 8] = [
    "va",
    "vaapi",
    "nvcodec",
    "vulkan",
    "qsv",
    "msdk",
    "v4l2codecs",
    "video4linux2",
];

fn on_gpu(factory: &gst::ElementFactory) -> bool {
    factory
        .plugin_name()
        .is_some_and(|name| GPU_PLUGINS.contains(&name.as_str()))
}

/// Whether a GPU the decoders could use is here: a render node, or NVIDIA's control
/// device, that opens.
fn gpu_here() -> bool {
    let opens = |path: std::path::PathBuf| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .is_ok()
    };
    let render = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .any(|e| opens(e.path()));
    render || opens("/dev/nvidiactl".into())
}

/// Leave the GPU decoders out of this process's choice.
fn without_gpu() {
    for factory in
        gst::ElementFactory::factories_with_type(gst::ElementFactoryType::DECODER, gst::Rank::NONE)
    {
        if on_gpu(&factory) {
            factory.set_rank(gst::Rank::NONE);
        }
    }
}

/// Put the GPU decoders ahead of the ones in software. The player's old sink took the
/// GPU's own buffers and so drew decodebin3 to them; the helper's sink takes plain
/// pixels, which leaves the choice to rank, where a software decoder such as libvpx's
/// can stand level with the GPU's. Decoders their plugin leaves unranked stay so.
fn gpu_first() {
    let decoders =
        gst::ElementFactory::factories_with_type(gst::ElementFactoryType::DECODER, gst::Rank::NONE);
    let top = decoders
        .iter()
        .filter(|factory| !on_gpu(factory))
        .map(|factory| factory.rank())
        .max()
        .unwrap_or(gst::Rank::PRIMARY);
    for factory in decoders.iter().filter(|factory| on_gpu(factory)) {
        if factory.rank() > gst::Rank::NONE {
            factory.set_rank(top + 1);
        }
    }
}

/// How one try at playing the file ended.
enum Tried {
    /// The player hung up, or the file failed and the player has been told.
    Done(Result<(), String>),
    /// A GPU decoder failed before anything was played: worth another go without.
    GpuFailed(String),
}

/// What stays the same from one try to the next.
struct Session {
    source: Source,
    notes: UnixStream,
    outs: [Arc<Out>; 2],
    pending: Arc<AtomicU32>,
    /// The pipeline seeks go to: the one of the try under way.
    pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    covered: bool,
    /// The DMA-BUF formats the player's sink takes, `FOURCC:0xMODIFIER`.
    formats: Vec<String>,
}

enum Source {
    File(String),
    /// Read through the player; the socket and where the next read starts, and the size.
    Player(Arc<Mutex<(UnixStream, u64)>>, u64),
}

/// In the helper: play `source`, a path or `fd:N` for a file read through the player,
/// into the sockets whose descriptors are given; `formats` are the DMA-BUF formats the
/// player takes, separated by commas. Returns when the player hangs up.
pub fn serve(
    source: &str,
    control: RawFd,
    video: RawFd,
    audio: RawFd,
    formats: &str,
) -> Result<(), String> {
    // SAFETY: the player hands these over for this process to own.
    let (control, video, audio) = unsafe {
        (
            UnixStream::from_raw_fd(control),
            UnixStream::from_raw_fd(video),
            UnixStream::from_raw_fd(audio),
        )
    };
    gst::init().map_err(|e| e.to_string())?;
    let source = match source.strip_prefix("fd:") {
        Some(fd) => {
            let fd: RawFd = fd.parse().map_err(|_| "bad descriptor".to_string())?;
            // SAFETY: as above.
            let mut data = unsafe { UnixStream::from_raw_fd(fd) };
            let size = read_u64(&mut data).map_err(|_| "the file could not be read".to_string())?;
            Source::Player(Arc::new(Mutex::new((data, 0))), size)
        }
        None => Source::File(source.to_string()),
    };
    let mut session = Session {
        source,
        notes: control.try_clone().map_err(|e| e.to_string())?,
        outs: [video, audio].map(|socket| {
            Arc::new(Out {
                socket: Mutex::new(socket),
                epoch: AtomicU32::new(0),
                held: Mutex::default(),
                returned: std::sync::Condvar::new(),
                next: AtomicU64::new(0),
            })
        }),
        pending: Arc::new(AtomicU32::new(0)),
        pipeline: Arc::default(),
        covered: false,
        formats: formats
            .split(',')
            .filter(|f| !f.is_empty() && *f != "-")
            .map(str::to_string)
            .collect(),
    };
    // The player hands pictures in GPU memory back by id once it has shown them.
    std::thread::spawn({
        let out = session.outs[0].clone();
        let mut returned = out
            .socket
            .lock()
            .unwrap()
            .try_clone()
            .map_err(|e| e.to_string())?;
        move || {
            let mut id = [0; 8];
            while returned.read_exact(&mut id).is_ok() {
                out.held.lock().unwrap().remove(&u64::from_le_bytes(id));
                out.returned.notify_all();
            }
        }
    });
    // Seeks come in on a thread of their own; the player hanging up ends the helper.
    std::thread::spawn({
        let (pipeline, pending, mut control) =
            (session.pipeline.clone(), session.pending.clone(), control);
        move || {
            let mut seek = [0; 12];
            while control.read_exact(&mut seek).is_ok() {
                let epoch = u32::from_le_bytes(seek[..4].try_into().unwrap());
                let position = u64::from_le_bytes(seek[4..].try_into().unwrap());
                pending.store(epoch, Ordering::SeqCst);
                let pipeline = pipeline.lock().unwrap().clone();
                if let Some(pipeline) = pipeline {
                    let _ = pipeline.seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::ClockTime::from_nseconds(position),
                    );
                }
            }
            std::process::exit(0);
        }
    });
    if gpu_here() {
        gpu_first();
    } else {
        without_gpu();
    }
    match attempt(&mut session) {
        Tried::Done(result) => result,
        Tried::GpuFailed(why) => {
            eprintln!("spiral-thumbnailer: decoding on the GPU failed ({why}), trying software");
            without_gpu();
            match attempt(&mut session) {
                Tried::Done(result) => result,
                Tried::GpuFailed(why) => {
                    let _ = write_note(&mut session.notes, &Note::Error(why.clone()));
                    Err(why)
                }
            }
        }
    }
}

/// One try at playing the file, with the decoders the registry now ranks.
fn attempt(session: &mut Session) -> Tried {
    match try_playing(session) {
        Ok(tried) => tried,
        Err(e) => Tried::Done(Err(e)),
    }
}

fn try_playing(session: &mut Session) -> Result<Tried, String> {
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("no {name}"))
    };
    let pipeline = gst::Pipeline::new();
    let src = match &session.source {
        Source::Player(state, size) => remote_source(state.clone(), *size)?,
        Source::File(path) => {
            let src = make("filesrc")?;
            src.set_property("location", path);
            src
        }
    };
    let decode = make("decodebin3")?;
    pipeline
        .add_many([&src, &decode])
        .map_err(|e| e.to_string())?;
    src.link(&decode).map_err(|e| e.to_string())?;

    // Which decoder decodebin3 picked for the picture goes to the log: on the GPU where
    // there is one to use, in software otherwise. Told from the bus loop, the one place
    // that writes notes.
    let used_gpu = Arc::new(std::sync::atomic::AtomicBool::new(false));
    pipeline.connect_deep_element_added({
        let used_gpu = used_gpu.clone();
        move |pipeline, _, element| {
            let Some(factory) = element.factory() else {
                return;
            };
            if factory.klass().contains("Decoder") && on_gpu(&factory) {
                used_gpu.store(true, Ordering::SeqCst);
            }
            if factory.klass().contains("Decoder") && factory.klass().contains("Video") {
                let _ = pipeline.post_message(gst::message::Application::new(
                    gst::Structure::builder("spiral-decoder")
                        .field("name", factory.name().to_string())
                        .build(),
                ));
            }
        }
    });

    let sinks: Arc<Mutex<[Option<gst::Element>; 2]>> = Arc::default();
    let direct = (!session.formats.is_empty()).then(|| dmabuf_caps(&session.formats));
    decode.connect_pad_added({
        let (pipeline, pending, outs, sinks) = (
            pipeline.clone(),
            session.pending.clone(),
            session.outs.clone(),
            sinks.clone(),
        );
        move |_, pad| {
            let Some(caps) = pad.current_caps().or_else(|| Some(pad.query_caps(None))) else {
                return;
            };
            let Some(name) = caps.structure(0).map(|s| s.name().to_string()) else {
                return;
            };
            let kind = if name.starts_with("video/") {
                0
            } else if name.starts_with("audio/") {
                1
            } else {
                return;
            };
            let mut sinks = sinks.lock().unwrap();
            // The first stream of each kind is played, as playbin would choose.
            if sinks[kind].is_some() {
                return;
            }
            // A decoder that can hand over GPU memory the sink takes does, as it did into
            // the player's own pipeline before; anything else is converted to BGRA rows.
            let direct = direct
                .as_ref()
                .filter(|caps| kind == 0 && pad.query_caps(None).can_intersect(caps));
            match branch(
                &pipeline,
                pad,
                kind == 0,
                direct,
                outs[kind].clone(),
                pending.clone(),
            ) {
                Ok(sink) => sinks[kind] = Some(sink),
                Err(e) => eprintln!("spiral-thumbnailer: {e}"),
            }
        }
    });
    session.pipeline.lock().unwrap().replace(pipeline.clone());

    let bus = pipeline.bus().ok_or("no bus")?;
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|e| e.to_string())?;
    let notes = &mut session.notes;
    // A note that cannot be written means the player has hung up, which is how every run
    // of the helper ends: quietly, as the seek thread ends it.
    let send = |notes: &mut UnixStream, note: Note| -> Result<(), String> {
        if write_note(notes, &note).is_err() {
            std::process::exit(0);
        }
        Ok(())
    };
    let (mut prerolled, mut ready) = (false, false);
    // The kinds of stream decodebin3 chose to play. The pipeline can preroll before the
    // pad of one of them is there, and before a sink knows what it takes, so the player
    // is only told the file has started once each chosen kind has a sink that does. Both
    // come later than the preroll as often as not, and each says so on the bus.
    let mut chosen: Option<[bool; 2]> = None;
    for message in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;
        match message.view() {
            MessageView::AsyncDone(_) => prerolled = true,
            MessageView::StreamsSelected(selected) => {
                let mut kinds = [false; 2];
                for stream in selected.streams() {
                    let kind = stream.stream_type();
                    kinds[0] |= kind.contains(gst::StreamType::VIDEO);
                    kinds[1] |= kind.contains(gst::StreamType::AUDIO);
                }
                chosen = Some(kinds);
            }
            _ => {}
        }
        match message.view() {
            MessageView::Application(a)
                if a.structure().is_some_and(|s| s.name() == "spiral-decoder") =>
            {
                if let Ok(name) = a.structure().unwrap().get::<String>("name") {
                    send(notes, Note::Decoder(name))?;
                }
            }
            MessageView::AsyncDone(_)
            | MessageView::StreamsSelected(_)
            | MessageView::Application(_)
                if prerolled && !ready =>
            {
                let [video, audio] = sinks.lock().unwrap().clone();
                let caps = |sink: &Option<gst::Element>| {
                    sink.as_ref()
                        .and_then(|sink| sink.static_pad("sink")?.current_caps())
                };
                let Some(wanted) = chosen else { continue };
                if wanted[0] && caps(&video).is_none() || wanted[1] && caps(&audio).is_none() {
                    continue;
                }
                ready = true;
                let picture = caps(&video).and_then(|caps| Picture::of(&caps));
                let drm = caps(&video)
                    .and_then(|caps| caps.structure(0)?.get::<String>("drm-format").ok())
                    .and_then(|drm| session.formats.iter().position(|f| *f == drm))
                    .map(|i| i as u32);
                // Pictures in GPU memory reach GTK as the decoder left them, so their
                // colours go along; rows are converted to sRGB here.
                let colorimetry = drm
                    .and(caps(&video))
                    .and_then(|caps| caps.structure(0)?.get::<String>("colorimetry").ok())
                    .filter(|c| c.len() <= COLORIMETRY_MAX && plain_colorimetry(c.as_bytes()));
                let mut seeking = gst::query::Seeking::new(gst::Format::Time);
                let seekable = pipeline.query(&mut seeking) && seeking.result().0;
                send(
                    notes,
                    Note::Ready {
                        audio: audio.is_some(),
                        video: picture,
                        drm,
                        colorimetry,
                        seekable,
                        duration: pipeline
                            .query_duration::<gst::ClockTime>()
                            .map(|d| d.nseconds()),
                    },
                )?;
                pipeline
                    .set_state(gst::State::Playing)
                    .map_err(|e| e.to_string())?;
            }
            MessageView::Error(e) => {
                // GStreamer's own account of it goes along: "Internal data stream error"
                // alone does not say which element stopped, or why.
                let text = match e.debug() {
                    Some(debug) => format!("{} ({debug})", e.error()),
                    None => e.error().to_string(),
                };
                let _ = pipeline.set_state(gst::State::Null);
                // Nothing has been played yet, so the player need not know of the first
                // try: it gets its file all the same, a little later.
                if !ready && used_gpu.load(Ordering::SeqCst) {
                    return Ok(Tried::GpuFailed(text));
                }
                send(notes, Note::Error(text.clone()))?;
                return Ok(Tried::Done(Err(text)));
            }
            MessageView::Element(e) if missing_video_decoder(e.structure()) => {
                send(notes, Note::NoVideoDecoder)?;
            }
            MessageView::DurationChanged(_) => send(
                notes,
                Note::Duration(
                    pipeline
                        .query_duration::<gst::ClockTime>()
                        .map(|d| d.nseconds()),
                ),
            )?,
            MessageView::Tag(t) if !session.covered => {
                let tags = t.tags();
                let image = tags
                    .get::<gst::tags::Image>()
                    .or_else(|| tags.get::<gst::tags::PreviewImage>());
                let Some(buffer) = image.and_then(|i| i.get().buffer_owned()) else {
                    continue;
                };
                let Ok(map) = buffer.map_readable() else {
                    continue;
                };
                if map.len() <= COVER_MAX {
                    session.covered = true;
                    send(notes, Note::Cover(map.to_vec()))?;
                }
            }
            _ => {}
        }
    }
    Ok(Tried::Done(Ok(())))
}

/// Convert what `pad` gives to the one format the player takes, and send it on.
fn branch(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    video: bool,
    direct: Option<&gst::Caps>,
    out: Arc<Out>,
    pending: Arc<AtomicU32>,
) -> Result<gst::Element, String> {
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("no {name}"))
    };
    let mut chain = Vec::new();
    let caps = if let Some(caps) = direct {
        caps.clone()
    } else if video {
        // Interlaced pictures are woven together here, as playbin does by default.
        chain.extend(make("deinterlace").ok());
        chain.push(make("videoconvert")?);
        gst::Caps::builder("video/x-raw")
            .field("format", "BGRA")
            .build()
    } else {
        chain.push(make("audioconvert")?);
        chain.push(make("audioresample")?);
        gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("layout", "interleaved")
            .field("rate", AUDIO_RATE)
            .field("channels", AUDIO_CHANNELS)
            .build()
    };
    let filter = make("capsfilter")?;
    filter.set_property("caps", &caps);
    chain.push(filter);
    let sink = make("appsink")?;
    sink.set_property("sync", false);
    sink.set_property("emit-signals", true);
    sink.set_property("max-buffers", 2u32);
    chain.push(sink.clone());
    pipeline.add_many(&chain).map_err(|e| e.to_string())?;
    gst::Element::link_many(&chain).map_err(|e| e.to_string())?;
    for element in &chain {
        let _ = element.sync_state_with_parent();
    }

    let sink_pad = sink.static_pad("sink").ok_or("appsink without a pad")?;
    // A decoder only hands over DMA-BUFs to a sink that reads where each plane lies from
    // the buffer's video meta, as the player does; appsink does not say so by itself.
    if direct.is_some() {
        sink_pad.add_probe(gst::PadProbeType::QUERY_DOWNSTREAM, |_, info| {
            if let Some(query) = info.query_mut()
                && let gst::QueryViewMut::Allocation(allocation) = query.view_mut()
            {
                allocation.add_allocation_meta::<gst_video::VideoMeta>(None);
            }
            gst::PadProbeReturn::Ok
        });
    }
    // Wake the bus loop when the sink learns what it takes; see `serve`.
    sink_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, {
        let pipeline = pipeline.downgrade();
        move |_, info| {
            if let Some(gst::PadProbeData::Event(event)) = &info.data
                && event.type_() == gst::EventType::Caps
                && let Some(pipeline) = pipeline.upgrade()
            {
                let _ = pipeline.post_message(gst::message::Application::new(
                    gst::Structure::new_empty("spiral-caps"),
                ));
            }
            gst::PadProbeReturn::Ok
        }
    });
    sink_pad.add_probe(gst::PadProbeType::EVENT_FLUSH, {
        let out = out.clone();
        move |_, info| {
            if let Some(gst::PadProbeData::Event(event)) = &info.data
                && event.type_() == gst::EventType::FlushStop
            {
                out.epoch
                    .store(pending.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            gst::PadProbeReturn::Ok
        }
    });
    sink.connect("new-sample", false, {
        let out = out.clone();
        move |args| {
            let sink = args[0].get::<gst::Element>().ok()?;
            let flow = match sink.emit_by_name::<Option<gst::Sample>>("pull-sample", &[]) {
                Some(sample) => send_sample(&out, &sample, video),
                None => gst::FlowReturn::Eos,
            };
            Some(flow.to_value())
        }
    });
    sink.connect("eos", false, move |_| {
        let epoch = out.epoch.load(Ordering::SeqCst);
        let _ = write_packet(&mut *out.socket.lock().unwrap(), &Packet::End { epoch });
        None
    });
    let first = chain.first().and_then(|e| e.static_pad("sink"));
    pad.link(&first.ok_or("chain without a sink pad")?)
        .map_err(|e| format!("{e:?}"))?;
    Ok(sink)
}

fn send_sample(out: &Out, sample: &gst::Sample, video: bool) -> gst::FlowReturn {
    let Some(buffer) = sample.buffer() else {
        return gst::FlowReturn::Ok;
    };
    let in_gpu_memory = sample.caps().is_some_and(|caps| {
        caps.features(0)
            .is_some_and(|f| f.contains(gst_allocators::CAPS_FEATURE_MEMORY_DMABUF))
    });
    if video && in_gpu_memory {
        return send_frame(out, sample, buffer);
    }
    let Ok(map) = buffer.map_readable() else {
        return gst::FlowReturn::Ok;
    };
    // A picture goes as the rows it is, with nothing between them; one laid out otherwise
    // is not one the player could take.
    if video {
        let picture = sample.caps().and_then(Picture::of);
        if picture.is_none_or(|p| p.len() != map.len()) {
            return gst::FlowReturn::Ok;
        }
    } else if map.len() > AUDIO_PACKET_MAX || map.is_empty() {
        return gst::FlowReturn::Ok;
    }
    let packet = Packet::Data {
        epoch: out.epoch.load(Ordering::SeqCst),
        pts: stream_time(sample, buffer.pts()),
        duration: buffer.duration().map(|d| d.nseconds()),
        bytes: map.to_vec(),
    };
    match write_packet(&mut *out.socket.lock().unwrap(), &packet) {
        Ok(()) => gst::FlowReturn::Ok,
        Err(_) => gst::FlowReturn::Error,
    }
}

/// `t` as stream time, which is what the player's segment counts in.
fn stream_time(sample: &gst::Sample, t: Option<gst::ClockTime>) -> Option<u64> {
    let segment = sample
        .segment()
        .and_then(|s| s.downcast_ref::<gst::ClockTime>());
    match segment {
        Some(segment) => t.and_then(|t| segment.to_stream_time(t)),
        None => t,
    }
    .map(|t| t.nseconds())
}

/// Send a picture in GPU memory: its DMA-BUFs and how the planes lie in them. The buffer
/// is kept until the player hands it back, so the decoder does not reuse it meanwhile.
fn send_frame(out: &Out, sample: &gst::Sample, buffer: &gst::BufferRef) -> gst::FlowReturn {
    let mut fds = Vec::new();
    let mut memories = Vec::new();
    for memory in buffer.iter_memories() {
        let Some(dmabuf) = memory.downcast_memory_ref::<gst_allocators::DmaBufMemory>() else {
            return gst::FlowReturn::Ok;
        };
        fds.push(dmabuf.fd());
        memories.push((memory.offset() as u64, memory.size() as u64));
    }
    let planes: Vec<(u64, i32)> = match buffer.meta::<gst_video::VideoMeta>() {
        Some(meta) => meta
            .offset()
            .iter()
            .zip(meta.stride())
            .map(|(&offset, &stride)| (offset as u64, stride))
            .collect(),
        None => {
            let Some(info) = sample
                .caps()
                .and_then(|caps| gst_video::VideoInfoDmaDrm::from_caps(caps).ok())
            else {
                return gst::FlowReturn::Ok;
            };
            info.offset()
                .iter()
                .zip(info.stride())
                .map(|(&offset, &stride)| (offset as u64, stride))
                .collect()
        }
    };
    if !(1..=PARTS_MAX).contains(&memories.len()) || !(1..=PARTS_MAX).contains(&planes.len()) {
        return gst::FlowReturn::Ok;
    }
    let id = out.next.fetch_add(1, Ordering::SeqCst);
    let Some(owned) = sample.buffer_owned() else {
        return gst::FlowReturn::Ok;
    };
    // A picture in GPU memory is a few bytes on the socket, which would never fill: the
    // decoder waits here instead, rather than run to the end of the file with a surface
    // held for every picture on the way. The player hands one back for each it takes in.
    let held = out.held.lock().unwrap();
    let mut held = out
        .returned
        .wait_while(held, |held| held.len() >= FRAMES_AHEAD)
        .unwrap();
    held.insert(id, owned);
    drop(held);
    let packet = Packet::Frame {
        epoch: out.epoch.load(Ordering::SeqCst),
        pts: stream_time(sample, buffer.pts()),
        duration: buffer.duration().map(|d| d.nseconds()),
        id,
        memories,
        planes,
    };
    let socket = out.socket.lock().unwrap();
    match send_with_fds(&socket, &packet, &fds) {
        Ok(()) => gst::FlowReturn::Ok,
        Err(_) => {
            out.held.lock().unwrap().remove(&id);
            gst::FlowReturn::Error
        }
    }
}

/// A source reading the file through the player, `size` bytes long: whatever part of it
/// the demuxer asks for, from where the last read left off or a seek put it.
fn remote_source(state: Arc<Mutex<(UnixStream, u64)>>, size: u64) -> Result<gst::Element, String> {
    let src = gst::ElementFactory::make("appsrc")
        .build()
        .map_err(|_| "no appsrc".to_string())?;
    src.set_property_from_str("stream-type", "seekable");
    src.set_property_from_str("format", "bytes");
    src.set_property("size", size as i64);
    src.connect("seek-data", false, {
        let state = state.clone();
        move |args| {
            state.lock().unwrap().1 = args[1].get::<u64>().ok()?;
            Some(true.to_value())
        }
    });
    src.connect("need-data", false, move |args| {
        let src = args[0].get::<gst::Element>().ok()?;
        let length = args[1].get::<u32>().ok()?.clamp(4096, READ_MAX);
        let mut state = state.lock().unwrap();
        let offset = state.1;
        let bytes = (|| {
            let mut request = offset.to_le_bytes().to_vec();
            request.extend_from_slice(&length.to_le_bytes());
            state.0.write_all(&request)?;
            let len = read_u32(&mut state.0)?;
            if len > length {
                return Err(invalid("more than asked for"));
            }
            read_bytes(&mut state.0, len as usize)
        })();
        match bytes {
            Ok(bytes) if !bytes.is_empty() => {
                state.1 += bytes.len() as u64;
                let mut buffer = gst::Buffer::from_mut_slice(bytes);
                buffer.get_mut()?.set_offset(offset);
                let _ = src.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]);
            }
            _ => {
                let _ = src.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
            }
        }
        None
    });
    Ok(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pictures in GPU memory carry the colours the decoder gave them; ones GStreamer
    /// does not read back are left out, and GTK guesses as before.
    #[test]
    fn pictures_in_gpu_memory_keep_their_colours() {
        gst::init().unwrap();
        let picture = Picture {
            width: 640,
            height: 360,
            fps: (25, 1),
            par: (1, 1),
        };
        let colorimetry = |c: Option<&str>| {
            let caps = picture.caps(Some("NV12"), c);
            caps.structure(0).unwrap().get::<String>("colorimetry").ok()
        };
        assert_eq!(colorimetry(Some("2:3:5:1")).as_deref(), Some("bt709"));
        // Ten bits and HDR, as GStreamer names them.
        for name in ["bt2020-10", "bt2100-pq", "bt2100-hlg"] {
            assert_eq!(colorimetry(Some(name)).as_deref(), Some(name));
        }
        // Full range, as phones record, is kept as such.
        assert_eq!(colorimetry(Some("1:3:5:1")).as_deref(), Some("1:3:5:1"));
        assert_eq!(colorimetry(Some("nonsense")), None);
        assert_eq!(colorimetry(None), None);
        let rows = picture.caps(None, Some("bt709"));
        assert!(
            rows.structure(0)
                .unwrap()
                .get::<String>("colorimetry")
                .is_err()
        );
    }

    /// What is written is what is read back, and what does not add up is refused.
    #[test]
    fn notes_and_packets_travel_and_are_checked() {
        let notes = [
            Note::Ready {
                audio: true,
                video: Some(Picture {
                    width: 1920,
                    height: 1080,
                    fps: (30000, 1001),
                    par: (1, 1),
                }),
                drm: Some(2),
                colorimetry: Some("bt2100-pq".into()),
                seekable: true,
                duration: Some(5_000_000_000),
            },
            Note::Ready {
                audio: true,
                video: None,
                drm: None,
                colorimetry: None,
                seekable: false,
                duration: None,
            },
            Note::Duration(Some(7)),
            Note::Cover(vec![1, 2, 3]),
            Note::NoVideoDecoder,
            Note::Decoder("vah264dec".into()),
            Note::Error("no decoder".into()),
        ];
        for note in notes {
            let mut wire = Vec::new();
            write_note(&mut wire, &note).unwrap();
            assert_eq!(read_note(&mut wire.as_slice()).unwrap(), note);
        }
        let huge = Note::Ready {
            audio: false,
            video: Some(Picture {
                width: 100_000,
                height: 1,
                fps: (0, 1),
                par: (1, 1),
            }),
            drm: None,
            colorimetry: None,
            seekable: false,
            duration: None,
        };
        let mut wire = Vec::new();
        write_note(&mut wire, &huge).unwrap();
        assert!(read_note(&mut wire.as_slice()).is_err());
        // Colorimetry is text, of GStreamer's letters only.
        let mut wire = Vec::new();
        write_note(
            &mut wire,
            &Note::Ready {
                audio: false,
                video: None,
                drm: None,
                colorimetry: Some("bt709; rm -rf".into()),
                seekable: false,
                duration: None,
            },
        )
        .unwrap();
        assert!(read_note(&mut wire.as_slice()).is_err());

        let packet = Packet::Data {
            epoch: 3,
            pts: Some(40),
            duration: None,
            bytes: vec![9; 16],
        };
        let mut wire = Vec::new();
        write_packet(&mut wire, &packet).unwrap();
        assert_eq!(
            read_packet(&mut wire.as_slice(), 16, Some(16)).unwrap(),
            packet
        );
        assert!(read_packet(&mut wire.as_slice(), 16, Some(12)).is_err());
        assert!(read_packet(&mut wire.as_slice(), 8, None).is_err());
        let mut wire = Vec::new();
        write_packet(&mut wire, &Packet::End { epoch: 3 }).unwrap();
        assert_eq!(
            read_packet(&mut wire.as_slice(), 16, None).unwrap(),
            Packet::End { epoch: 3 }
        );
    }
}
