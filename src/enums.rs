use gettextrs::gettext;

use crate::glib;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "SpiralViewMode")]
pub enum ViewMode {
    #[default]
    #[enum_value(name = "Grid", nick = "grid")]
    Grid = 0,
    #[enum_value(name = "List", nick = "list")]
    List = 1,
    #[enum_value(name = "Columns", nick = "columns")]
    Columns = 2,
}

impl ViewMode {
    pub fn nick(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::List => "list",
            Self::Columns => "columns",
        }
    }

    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "grid" => Some(Self::Grid),
            "list" => Some(Self::List),
            "columns" => Some(Self::Columns),
            _ => None,
        }
    }

    /// The view the view button switches to next, leaving out the columns where they are
    /// turned off.
    pub fn next(self) -> Self {
        match self {
            Self::Grid => Self::List,
            Self::List if crate::prefs::column_view() => Self::Columns,
            _ => Self::Grid,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Grid => "view-grid-symbolic",
            Self::List => "view-list-symbolic",
            Self::Columns => "view-columns-symbolic",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Grid => gettext("Grid View"),
            Self::List => gettext("List View"),
            Self::Columns => gettext("Column View"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "SpiralSortKey")]
pub enum SortKey {
    #[default]
    #[enum_value(name = "Name", nick = "name")]
    Name = 0,
    #[enum_value(name = "Size", nick = "size")]
    Size = 1,
    #[enum_value(name = "Type", nick = "type")]
    Type = 2,
    #[enum_value(name = "Modified", nick = "modified")]
    Modified = 3,
    /// When an item was put in the trash; only the trash has it to sort by.
    #[enum_value(name = "Trashed", nick = "trashed")]
    Trashed = 4,
}

impl SortKey {
    pub fn nick(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Type => "type",
            Self::Modified => "modified",
            Self::Trashed => "trashed",
        }
    }

    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "name" => Some(Self::Name),
            "size" => Some(Self::Size),
            "type" => Some(Self::Type),
            "modified" => Some(Self::Modified),
            "trashed" => Some(Self::Trashed),
            _ => None,
        }
    }
}
