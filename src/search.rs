//! Search below a folder: names, and optionally file contents, filtered by kind and date.
//! Everything is async GIO on the main loop; a run stops as soon as `alive` says no.

use std::collections::VecDeque;

use crate::gio::prelude::*;
use crate::{file_utils, gio, glib};

const PRIO: glib::Priority = glib::Priority::LOW;
/// Files bigger than this are not read for content matches.
const CONTENT_LIMIT: i64 = 10 << 20;

/// What the text is matched against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Match {
    #[default]
    Name,
    NameOrContent,
    Content,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Lower-cased needle.
    pub text: String,
    pub matching: Match,
    /// One of the `KINDS` nicks; "any" means no restriction.
    pub kind: String,
    /// Only files modified after this instant.
    pub since: Option<glib::DateTime>,
    pub recursive: bool,
    pub show_hidden: bool,
}

/// File kinds offered by the filter popover: nick and mime check.
pub const KINDS: [&str; 9] = [
    "any",
    "folders",
    "documents",
    "images",
    "audio",
    "videos",
    "pdf",
    "text",
    "spreadsheets",
];

/// Date choices: nick and days back (0 is "since midnight today", -1 "any time").
pub const DATES: [(&str, i64); 6] = [
    ("any", -1),
    ("today", 0),
    ("yesterday", 1),
    ("week", 7),
    ("month", 30),
    ("year", 365),
];

pub fn since_for(nick: &str) -> Option<glib::DateTime> {
    let days = DATES.iter().find(|(n, _)| *n == nick)?.1;
    if days < 0 {
        return None;
    }
    let now = glib::DateTime::now_local().ok()?;
    let midnight =
        glib::DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0).ok()?;
    midnight.add_days(-(days as i32)).ok()
}

fn kind_matches(kind: &str, info: &gio::FileInfo) -> bool {
    let is = |t: &str| {
        file_utils::content_type_of(info).is_some_and(|ct| gio::content_type_is_a(&ct, t))
    };
    let any = |types: &[&str]| types.iter().any(|t| is(t));
    match kind {
        "folders" => file_utils::is_dir(info),
        "images" => is("image/*"),
        "audio" => is("audio/*"),
        "videos" => is("video/*"),
        "pdf" => is("application/pdf"),
        "text" => is("text/plain"),
        "spreadsheets" => any(&[
            "application/vnd.ms-excel",
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/x-gnumeric",
            "application/vnd.sun.xml.calc",
            "text/csv",
        ]),
        "documents" => any(&[
            "application/pdf",
            "application/rtf",
            "application/msword",
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/vnd.ms-powerpoint",
            "application/vnd.sun.xml.writer",
            "application/x-abiword",
            "application/epub+zip",
        ]),
        _ => true,
    }
}

fn date_matches(since: Option<&glib::DateTime>, info: &gio::FileInfo) -> bool {
    let Some(since) = since else { return true };
    info.modification_date_time()
        .is_some_and(|m| m.difference(since).as_seconds() >= 0)
}

/// Text files of a sane size are worth reading for a content match.
fn readable(info: &gio::FileInfo) -> bool {
    !file_utils::is_dir(info)
        && file_utils::size_of(info) <= CONTENT_LIMIT as u64
        && file_utils::content_type_of(info)
            .is_some_and(|ct| gio::content_type_is_a(&ct, "text/plain"))
}

async fn content_matches(file: &gio::File, needle: &str) -> bool {
    let Ok((data, _)) = file.load_contents_future().await else {
        return false;
    };
    // Lowercasing megabytes of text and scanning them is work for a worker thread. The
    // walk runs on the main loop, which is also drawing the results as they arrive.
    let needle = needle.to_string();
    gio::spawn_blocking(move || {
        String::from_utf8_lossy(&data)
            .to_lowercase()
            .contains(&needle)
    })
    .await
    .unwrap_or(false)
}

/// Walk `root` breadth-first and hand matches to `found` in batches. Returns early once
/// `alive` is false. Symlinked folders are not entered.
pub async fn run(
    root: gio::File,
    query: Query,
    alive: impl Fn() -> bool,
    mut found: impl FnMut(Vec<gio::FileInfo>),
) {
    let mut queue = VecDeque::from([root]);
    while let Some(dir) = queue.pop_front() {
        if !alive() {
            return;
        }
        let Ok(en) = dir
            .enumerate_children_future(file_utils::ATTRIBUTES, gio::FileQueryInfoFlags::NONE, PRIO)
            .await
        else {
            continue;
        };
        loop {
            let Ok(batch) = en.next_files_future(64, PRIO).await else {
                break;
            };
            if batch.is_empty() || !alive() {
                break;
            }
            let mut hits = Vec::new();
            for info in batch {
                let file = en.child(&info);
                let hidden = file_utils::is_hidden(&info);
                if query.recursive
                    && file_utils::is_dir(&info)
                    && !info.is_symlink()
                    && (query.show_hidden || !hidden)
                {
                    queue.push_back(file.clone());
                }
                if !kind_matches(&query.kind, &info) || !date_matches(query.since.as_ref(), &info) {
                    continue;
                }
                let by_name = query.matching != Match::Content
                    && info.display_name().to_lowercase().contains(&query.text);
                let hit = by_name
                    || (query.matching != Match::Name
                        && readable(&info)
                        && content_matches(&file, &query.text).await);
                if hit {
                    info.set_attribute_object("standard::file", &file);
                    hits.push(info);
                }
            }
            if !hits.is_empty() {
                found(hits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link found by a search is described by what it points to, as it is in its folder,
    /// and a linked folder is a hit without what is in it being found a second time.
    #[test]
    fn a_link_is_found_as_what_it_points_to() {
        let dir = std::env::temp_dir().join(format!("spiral-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inside")).unwrap();
        std::fs::write(dir.join("inside/note.txt"), "words").unwrap();
        std::os::unix::fs::symlink("inside/note.txt", dir.join("note-link.txt")).unwrap();
        std::os::unix::fs::symlink("inside", dir.join("note-folder")).unwrap();
        let query = Query {
            text: "note".into(),
            kind: "any".into(),
            recursive: true,
            ..Default::default()
        };
        let mut hits = Vec::new();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(run(
                    gio::File::for_path(&dir),
                    query,
                    || true,
                    |found| hits.extend(found),
                ));
            })
            .unwrap();
        let mut names: Vec<String> = hits.iter().map(|i| i.display_name().into()).collect();
        names.sort();
        assert_eq!(names, ["note-folder", "note-link.txt", "note.txt"]);
        let named = |name: &str| hits.iter().find(|i| i.display_name() == name).unwrap();
        let link = named("note-link.txt");
        assert!(link.is_symlink());
        assert_eq!(
            file_utils::content_type_of(link),
            file_utils::content_type_of(named("note.txt"))
        );
        assert!(file_utils::is_dir(named("note-folder")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
