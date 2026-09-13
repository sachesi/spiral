//! How the facts are put into words for the details panel.

use super::*;

impl Facts {
    /// The facts worth a row, in the order they are shown: title and value.
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut add = |title: String, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                rows.push((title, value));
            }
        };
        add(gettext("Title"), self.title.clone());
        add(gettext("Author"), self.author.clone());
        add(gettext("Pages"), self.pages.map(|n| n.to_string()));
        add(gettext("Page Size"), self.page_text());
        add(gettext("Words"), self.words.map(|n| n.to_string()));
        add(gettext("Slides"), self.slides.map(|n| n.to_string()));
        add(gettext("Artist"), self.artist.clone());
        add(gettext("Album"), self.album.clone());
        // Said only where it is not the artist again.
        add(
            gettext("Album Artist"),
            self.album_artist
                .clone()
                .filter(|a| Some(a) != self.artist.as_ref()),
        );
        add(
            gettext("Track"),
            self.track.map(|n| match self.tracks {
                Some(of) if of >= n => gettext("%s of %s")
                    .replacen("%s", &n.to_string(), 1)
                    .replacen("%s", &of.to_string(), 1),
                _ => n.to_string(),
            }),
        );
        add(gettext("Year"), self.year.map(|y| y.to_string()));
        add(gettext("Genre"), self.genre.clone());
        add(
            gettext("Dimensions"),
            self.width
                .zip(self.height)
                .map(|(w, h)| format!("{w} × {h}")),
        );
        add(gettext("Duration"), self.duration.map(duration_text));
        add(gettext("Taken"), self.taken.as_deref().and_then(taken_text));
        add(gettext("Camera"), self.camera.clone());
        add(gettext("Lens"), self.lens.clone());
        add(gettext("Exposure"), self.exposure_text());
        add(gettext("Video"), self.video_text());
        add(gettext("Audio"), self.audio_text());
        add(
            gettext("Audio Tracks"),
            self.audio_tracks.map(|n| n.to_string()),
        );
        add(
            gettext("Subtitles"),
            self.subtitles
                .as_deref()
                .map(language_names)
                .or_else(|| self.subtitle_tracks.map(|n| n.to_string())),
        );
        rows
    }

    /// "210 × 297 mm (A4)"
    pub(super) fn page_text(&self) -> Option<String> {
        let (w, h) = self.page_width.zip(self.page_height)?;
        let mm = |points: f64| (points * 25.4 / 72.0).round().to_string();
        let size = unit(gettext("%s mm").replace("%s", &format!("{} × {}", mm(w), mm(h))));
        Some(match &self.paper {
            Some(paper) => {
                let mut name = paper.chars();
                let first = name.next().map(|c| c.to_uppercase().collect::<String>());
                format!("{size} ({}{})", first.unwrap_or_default(), name.as_str())
            }
            None => size,
        })
    }

    fn exposure_text(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.aperture.map(|f| format!("f/{}", trimmed(f, 1))),
            self.exposure
                .map(|t| unit(gettext("%s s").replace("%s", &shutter(t)))),
            self.iso.map(|n| unit(format!("ISO {n}"))),
            self.focal
                .map(|mm| unit(gettext("%s mm").replace("%s", &trimmed(mm, 1)))),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn video_text(&self) -> Option<String> {
        let fps = self
            .fps
            .map(|f| unit(gettext("%s fps").replace("%s", &trimmed(f, 2))));
        join([self.video.clone(), fps])
    }

    fn audio_text(&self) -> Option<String> {
        let channels = self.channels.map(|n| match n {
            1 => gettext("Mono"),
            2 => gettext("Stereo"),
            n => ngettext("%d channel", "%d channels", n).replace("%d", &n.to_string()),
        });
        let rate = self
            .rate
            .map(|hz| unit(gettext("%s kHz").replace("%s", &trimmed(f64::from(hz) / 1000.0, 3))));
        let bitrate = self
            .bitrate
            .map(|b| unit(gettext("%s kbit/s").replace("%s", &(b / 1000).to_string())));
        let depth = self
            .depth
            .map(|d| unit(gettext("%s-bit").replace("%s", &d.to_string())));
        join([self.audio.clone(), channels, rate, depth, bitrate])
    }
}

/// "en, de" as "English, German", in the language of the interface where iso-codes has it.
pub(super) fn language_names(codes: &str) -> String {
    codes
        .split(", ")
        .map(|code| {
            gstreamer_tag::language_codes::language_name(code)
                .map(|name| name.to_string())
                .unwrap_or_else(|| code.to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A number and its unit, kept on one line when the panel wraps the rest.
pub(super) fn unit(text: String) -> String {
    text.replace(' ', "\u{a0}")
}

pub(super) fn join<const N: usize>(parts: [Option<String>; N]) -> Option<String> {
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `n` with at most `places` decimals and none that are zero: 2.8, 35, 29.97.
pub(super) fn trimmed(n: f64, places: usize) -> String {
    let s = format!("{n:.places$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A shutter speed as photographers write it: 1/250 up to a quarter of a second, 0.6 and
/// 2.5 above.
pub(super) fn shutter(seconds: f64) -> String {
    if seconds <= 0.25 {
        format!("1/{}", (1.0 / seconds).round())
    } else {
        trimmed(seconds, 1)
    }
}

/// 3:07, or 1:02:03 past the hour.
pub(super) fn duration_text(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The day and the time on the camera's clock, whatever the date preference: "2 years ago"
/// says nothing of a photo, and the hour it was taken is the hour where it was taken.
pub(super) fn taken_text(iso: &str) -> Option<String> {
    let date = glib::DateTime::from_iso8601(iso, Some(&glib::TimeZone::local())).ok()?;
    Some(date.format("%x, %H:%M").ok()?.to_string())
}
