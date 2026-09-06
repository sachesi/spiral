use crate::glib;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "SpiralViewMode")]
pub enum ViewMode {
    #[default]
    #[enum_value(name = "Grid", nick = "grid")]
    Grid = 0,
    #[enum_value(name = "List", nick = "list")]
    List = 1,
}

impl ViewMode {
    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "grid" => Some(Self::Grid),
            "list" => Some(Self::List),
            _ => None,
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
}
