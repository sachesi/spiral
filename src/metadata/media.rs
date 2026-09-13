//! What a recording or a video says: its length, its streams and its tags.

use super::*;

pub(super) fn probe_media(path: &Path) -> Vec<(&'static str, String)> {
    use crate::gst;
    use gstreamer_pbutils::prelude::*;

    let mut out = Vec::new();
    if gst::init().is_err() {
        return out;
    }
    let Ok(uri) = glib::filename_to_uri(path, None) else {
        return out;
    };
    let Ok(discoverer) =
        gstreamer_pbutils::Discoverer::new(gst::ClockTime::from_seconds(DISCOVER_TIMEOUT))
    else {
        return out;
    };
    let Ok(info) = discoverer.discover_uri(&uri) else {
        return out;
    };
    if let Some(duration) = info.duration() {
        out.push(("duration", duration.mseconds().to_string()));
    }
    let codec = |caps: Option<gst::Caps>| {
        caps.map(|c| gstreamer_pbutils::pb_utils_get_codec_description(&c).to_string())
    };
    // The title of a song is a tag of the file, or of its one sound stream where the file has
    // no tags of its own (Ogg, FLAC). The streams of a video are titled after the track
    // ("Audio", "English"), which says nothing of the video.
    // A picture in the file, a cover, is listed with the video; it is not one.
    let videos: Vec<_> = info
        .video_streams()
        .into_iter()
        .filter(|v| !v.is_image())
        .collect();
    let audios = info.audio_streams();
    let global = info
        .stream_info()
        .and_then(|s| {
            s.downcast::<gstreamer_pbutils::DiscovererContainerInfo>()
                .ok()
        })
        .and_then(|c| c.tags());
    let own = videos
        .is_empty()
        .then(|| audios.first().and_then(|a| a.tags()))
        .flatten();
    let tags: Vec<gst::TagList> = [global, own].into_iter().flatten().collect();
    let text = |get: fn(&gst::TagList) -> Option<String>| {
        tags.iter().find_map(get).filter(|s| !s.trim().is_empty())
    };
    let number = |get: fn(&gst::TagList) -> Option<u32>| tags.iter().find_map(get);

    if let Some(video) = videos.first() {
        // A phone held upright records the picture on its side and says so in a tag.
        let orientation = video
            .tags()
            .into_iter()
            .chain(tags.iter().cloned())
            .find_map(|t| {
                t.get::<gst::tags::ImageOrientation>()
                    .map(|o| o.get().to_string())
            });
        let upright = orientation.is_some_and(|o| o.ends_with("-90") || o.ends_with("-270"));
        let (width, height) = if upright {
            (video.height(), video.width())
        } else {
            (video.width(), video.height())
        };
        if width > 0 && height > 0 {
            out.push(("width", width.to_string()));
            out.push(("height", height.to_string()));
        }
        if let Some(name) = codec(video.caps()) {
            out.push(("video", name));
        }
        let rate = video.framerate();
        if rate.numer() > 0 && rate.denom() > 0 {
            out.push((
                "fps",
                (f64::from(rate.numer()) / f64::from(rate.denom())).to_string(),
            ));
        }
    }
    if let Some(audio) = audios.first() {
        if let Some(name) = codec(audio.caps()) {
            out.push(("audio", name));
        }
        // Only lossless sound keeps its bit depth; for the rest it is what the decoder would
        // hand out, 32-bit float for Vorbis or AAC.
        let lossless = audio
            .caps()
            .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
            .is_some_and(|name| LOSSLESS.contains(&name.as_str()));
        for (key, value) in [
            ("channels", audio.channels()),
            ("rate", audio.sample_rate()),
            ("depth", if lossless { audio.depth() } else { 0 }),
            ("bitrate", audio.bitrate()),
        ] {
            if value > 0 {
                out.push((key, value.to_string()));
            }
        }
    }
    if audios.len() > 1 {
        out.push(("audio_tracks", audios.len().to_string()));
    }
    let subtitles = info.subtitle_streams();
    if !subtitles.is_empty() {
        let languages: Vec<String> = subtitles
            .iter()
            .filter_map(|s| s.language())
            .map(|l| l.to_string())
            .collect();
        if languages.len() == subtitles.len() {
            out.push(("subtitles", languages.join(", ")));
        } else {
            out.push(("subtitle_tracks", subtitles.len().to_string()));
        }
    }
    for (key, value) in [
        (
            "title",
            text(|t| Some(t.get::<gst::tags::Title>()?.get().to_string())),
        ),
        (
            "artist",
            text(|t| Some(t.get::<gst::tags::Artist>()?.get().to_string())),
        ),
        (
            "album",
            text(|t| Some(t.get::<gst::tags::Album>()?.get().to_string())),
        ),
        (
            "album_artist",
            text(|t| Some(t.get::<gst::tags::AlbumArtist>()?.get().to_string())),
        ),
        (
            "genre",
            text(|t| Some(t.get::<gst::tags::Genre>()?.get().to_string())),
        ),
    ] {
        if let Some(value) = value {
            out.push((key, value));
        }
    }
    for (key, value) in [
        (
            "track",
            number(|t| Some(t.get::<gst::tags::TrackNumber>()?.get())),
        ),
        (
            "tracks",
            number(|t| Some(t.get::<gst::tags::TrackCount>()?.get())),
        ),
        // The date of a song is when it came out; a video's is when it was written, which
        // the muxer fills in, and says little.
        (
            "year",
            number(|t| {
                t.get::<gst::tags::DateTime>()
                    .map(|d| d.get().year() as u32)
                    .or_else(|| {
                        t.get::<gst::tags::Date>()
                            .map(|d| u32::from(d.get().year()))
                    })
            }),
        ),
    ] {
        if let Some(value) = value.filter(|&v| v > 0)
            && (key != "year" || videos.is_empty())
        {
            out.push((key, value.to_string()));
        }
    }
    out
}

/// Caps of sound that is stored as it is played.
pub(super) const LOSSLESS: [&str; 8] = [
    "audio/x-flac",
    "audio/x-alac",
    "audio/x-raw",
    "audio/x-wav",
    "audio/x-aiff",
    "audio/x-wavpack",
    "audio/x-ape",
    "audio/x-tta",
];
