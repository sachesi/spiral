//! "Rename %d Files": one rule applied to a whole selection, with the names it would give
//! shown as they are typed.

use std::collections::HashSet;

use futures_channel::oneshot;
use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::{adw, glib, gtk, naming};

/// How the new names are made.
#[derive(Clone, Copy, PartialEq)]
enum Rule {
    /// A name shared by all of them, followed by a number.
    Numbered,
    /// Some text of the old name replaced by other text.
    Replace,
}

/// The numbering, and how wide it is written.
const WIDTHS: [usize; 3] = [1, 2, 3];

/// How many of the names being renamed are shown; the rest are counted.
const PREVIEW_ROWS: usize = 100;

/// Ask for a rule and return the new name of every file in `names`, in the same order, or
/// None if the dialog was dismissed. `others` are the names already in the folder that are
/// not being renamed: a new name may not take one of those. `max` is the longest name the
/// folder takes, where it is known.
pub async fn batch_rename_dialog(
    parent: &impl IsA<gtk::Widget>,
    names: Vec<String>,
    others: HashSet<String>,
    max: Option<usize>,
) -> Option<Vec<String>> {
    let rules = gtk::StringList::new(&[]);
    rules.append(&gettext("Name and Number"));
    rules.append(&gettext("Find and Replace"));
    let rule_row = adw::ComboRow::builder()
        .title(gettext("Rename Using"))
        .model(&rules)
        .build();

    let name_row = adw::EntryRow::builder()
        .title(gettext("_Name"))
        .use_underline(true)
        .text(common_stem(&names))
        .build();
    let numbers = gtk::StringList::new(&["1, 2, 3", "01, 02, 03", "001, 002, 003"]);
    let number_row = adw::ComboRow::builder()
        .title(gettext("Numbers"))
        .model(&numbers)
        .build();
    let find_row = adw::EntryRow::builder()
        .title(gettext("_Existing Text"))
        .use_underline(true)
        .build();
    let replace_row = adw::EntryRow::builder()
        .title(gettext("Replace _With"))
        .use_underline(true)
        .build();

    let group = adw::PreferencesGroup::new();
    group.add(&rule_row);
    group.add(&name_row);
    group.add(&number_row);
    group.add(&find_row);
    group.add(&replace_row);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(260)
        .child(&list)
        .build();
    let preview = adw::PreferencesGroup::builder()
        .title(
            ngettext(
                "%d File to Rename",
                "%d Files to Rename",
                names.len() as u32,
            )
            .replace("%d", &names.len().to_string()),
        )
        .build();
    preview.add(&scroll);

    let feedback = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["warning", "caption"])
        .build();
    let page = adw::PreferencesPage::new();
    page.add(&group);
    page.add(&preview);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&page);
    feedback.set_margin_bottom(12);
    feedback.set_margin_start(18);
    feedback.set_margin_end(18);
    content.append(&feedback);

    let cancel = gtk::Button::builder()
        .label(gettext("_Cancel"))
        .use_underline(true)
        .can_shrink(true)
        .build();
    let rename = gtk::Button::builder()
        .label(gettext("_Rename"))
        .use_underline(true)
        .can_shrink(true)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&rename);
    let toolbar = adw::ToolbarView::builder().content(&content).build();
    toolbar.add_top_bar(&header);
    let dialog = adw::Dialog::builder()
        .title(
            ngettext("Rename %d File", "Rename %d Files", names.len() as u32)
                .replace("%d", &names.len().to_string()),
        )
        .content_width(520)
        .content_height(560)
        .child(&toolbar)
        .focus_widget(&name_row)
        .build();

    let rows = preview_rows(&list, &names);

    // What the rows say now, and the names that follow from it.
    let result: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let update = {
        let (names, others) = (names.clone(), others.clone());
        glib::clone!(
            #[weak]
            rule_row,
            #[weak]
            name_row,
            #[weak]
            number_row,
            #[weak]
            find_row,
            #[weak]
            replace_row,
            #[strong]
            rows,
            #[weak]
            rename,
            #[weak]
            feedback,
            #[strong]
            result,
            move || {
                let rule = match rule_row.selected() {
                    0 => Rule::Numbered,
                    _ => Rule::Replace,
                };
                name_row.set_visible(rule == Rule::Numbered);
                number_row.set_visible(rule == Rule::Numbered);
                find_row.set_visible(rule == Rule::Replace);
                replace_row.set_visible(rule == Rule::Replace);
                let made = match rule {
                    Rule::Numbered => numbered(
                        &names,
                        name_row.text().trim(),
                        WIDTHS[number_row.selected().min(2) as usize],
                    ),
                    Rule::Replace => replaced(&names, &find_row.text(), &replace_row.text()),
                };
                let problem = check(&names, &made, &others, max);
                for (row, new) in rows.iter().zip(&made) {
                    row.set_subtitle(&glib::markup_escape_text(new));
                }
                feedback.set_text(problem.as_deref().unwrap_or_default());
                feedback.set_visible(problem.is_some());
                rename.set_sensitive(problem.is_none() && made != names);
                result.replace(made);
            }
        )
    };
    rule_row.connect_selected_notify(glib::clone!(
        #[strong]
        update,
        move |_| update()
    ));
    number_row.connect_selected_notify(glib::clone!(
        #[strong]
        update,
        move |_| update()
    ));
    for row in [&name_row, &find_row, &replace_row] {
        row.connect_changed(glib::clone!(
            #[strong]
            update,
            move |_| update()
        ));
    }
    update();

    let (tx, rx) = oneshot::channel::<Option<Vec<String>>>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let accept = glib::clone!(
        #[weak]
        rename,
        #[weak]
        dialog,
        #[strong]
        result,
        #[strong]
        tx,
        move || {
            if !rename.is_sensitive() {
                return;
            }
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Some(result.borrow().clone()));
            }
            dialog.close();
        }
    );
    rename.connect_clicked(glib::clone!(
        #[strong]
        accept,
        move |_| accept()
    ));
    for row in [&name_row, &find_row, &replace_row] {
        row.connect_entry_activated(glib::clone!(
            #[strong]
            accept,
            move |_| accept()
        ));
    }
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.connect_closed(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(None);
        }
    });
    dialog.present(Some(parent));
    rx.await.ok().flatten()
}

/// The stem of the first name, as a starting point that is usually close to what is wanted.
fn common_stem(names: &[String]) -> String {
    let first = names.first().map(String::as_str).unwrap_or_default();
    first[..stem_end(first)].to_string()
}

/// Where the extension starts, or the whole name where there is none. A leading dot is
/// part of the name, not an extension.
fn stem_end(name: &str) -> usize {
    name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len())
}

fn numbered(names: &[String], base: &str, width: usize) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, old)| format!("{base} {:0width$}{}", i + 1, &old[stem_end(old)..]))
        .collect()
}

fn replaced(names: &[String], find: &str, with: &str) -> Vec<String> {
    if find.is_empty() {
        return names.to_vec();
    }
    names.iter().map(|old| old.replace(find, with)).collect()
}

/// What is wrong with the names, if anything: the first complaint, in the order a reader
/// would notice them. `others` is what the folder holds besides the files being renamed,
/// as a set: a selection of thousands is checked on every keystroke.
fn check(
    names: &[String],
    made: &[String],
    others: &HashSet<String>,
    max: Option<usize>,
) -> Option<String> {
    for (old, new) in names.iter().zip(made) {
        if let naming::Verdict::Error(message) = naming::validate(new, Some(old), false, max) {
            return Some(if message.is_empty() {
                gettext("File names cannot be empty.")
            } else {
                message
            });
        }
    }
    let mut seen = HashSet::with_capacity(made.len());
    for new in made {
        if !seen.insert(new) {
            return Some(gettext("Two files would end up with the same name."));
        }
        if others.contains(new) {
            return Some(gettext("A file with that name already exists."));
        }
    }
    None
}

/// A row per file, up to `PREVIEW_ROWS` of them: the old name, with the new one under it.
/// The rows are made once and their subtitles follow the rule, since a selection of
/// thousands would otherwise be built again on every keystroke.
fn preview_rows(list: &gtk::ListBox, names: &[String]) -> Vec<adw::ActionRow> {
    let rows: Vec<adw::ActionRow> = names
        .iter()
        .take(PREVIEW_ROWS)
        .map(|old| {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(old))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("go-next-symbolic"));
            list.append(&row);
            row
        })
        .collect();
    if names.len() > rows.len() {
        let more = names.len() - rows.len();
        let row = adw::ActionRow::builder()
            .title(
                ngettext("and %d more file", "and %d more files", more as u32)
                    .replace("%d", &more.to_string()),
            )
            .css_classes(["dim-label"])
            .build();
        list.append(&row);
    }
    rows
}
