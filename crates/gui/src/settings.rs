//! View preferences, saved between sessions.

use eframe::egui::{self, Color32};

#[derive(Clone, Copy, PartialEq)]
pub enum Theme {
    /// The desktop's light or dark preference.
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TextSize {
    Small,
    Normal,
    Large,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Density {
    Compact,
    Normal,
    Comfortable,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Grouping {
    FoldersFirst,
    ByType,
    None,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortBy {
    Name,
    Type,
    Size,
}

#[derive(Clone, PartialEq)]
pub struct Settings {
    pub theme: Theme,
    pub text_size: TextSize,
    pub density: Density,
    pub colored_icons: bool,
    pub grouping: Grouping,
    pub sort_by: SortBy,
    pub descending: bool,
    /// Show every file in the side tree, not only folders and archives.
    pub tree_all_files: bool,
    /// Reopen the side tree folders that were expanded in the last session.
    pub remember_open_folders: bool,
    /// Show the side tree of the game folder (View > Side tree).
    pub show_tree: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            text_size: TextSize::Normal,
            density: Density::Normal,
            colored_icons: true,
            grouping: Grouping::FoldersFirst,
            sort_by: SortBy::Name,
            descending: false,
            tree_all_files: false,
            remember_open_folders: false,
            show_tree: true,
        }
    }
}

impl Theme {
    pub const ALL: [(Theme, &str); 3] = [(Self::System, "Same as the system"), (Self::Light, "Light"), (Self::Dark, "Dark")];
}

impl TextSize {
    pub const ALL: [(TextSize, &str); 3] =
        [(Self::Small, "Small"), (Self::Normal, "Normal"), (Self::Large, "Large")];

    fn zoom(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Normal => 1.0,
            Self::Large => 1.15,
        }
    }
}

impl Density {
    pub const ALL: [(Density, &str); 3] = [(Self::Compact, "Compact"), (Self::Normal, "Normal"), (Self::Comfortable, "Comfortable")];

    /// Extra height of a table row over the text height.
    pub fn row_padding(self) -> f32 {
        match self {
            Self::Compact => 2.0,
            Self::Normal => 6.0,
            Self::Comfortable => 12.0,
        }
    }

    /// Vertical item spacing and button padding (Normal is egui's default).
    fn spacing(self) -> (f32, f32) {
        match self {
            Self::Compact => (1.0, 0.0),
            Self::Normal => (3.0, 1.0),
            Self::Comfortable => (5.0, 3.0),
        }
    }
}

impl Grouping {
    pub const ALL: [(Grouping, &str); 3] =
        [(Self::FoldersFirst, "Folders first"), (Self::ByType, "By type"), (Self::None, "None (mixed)")];
}

/// How much larger than egui's default the text is.
const TEXT_SCALE: f32 = 1.1;

impl Settings {
    /// Applies text size and density to the whole interface.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_zoom_factor(self.text_size.zoom());
        let (item, button) = self.density.spacing();
        ctx.all_styles_mut(|style| {
            // Text a little larger than egui's, at every size (from egui's
            // sizes each time, so applying twice changes nothing).
            for (text_style, font) in egui::Style::default().text_styles {
                style.text_styles.insert(text_style, egui::FontId::new(font.size * TEXT_SCALE, font.family));
            }
            // Names are click targets, not text to select.
            style.interaction.selectable_labels = false;
            style.spacing.item_spacing.y = item;
            style.spacing.button_padding.y = button;
        });
    }

    pub fn to_storage(&self) -> String {
        format!(
            "theme={};text={};density={};colors={};group={};sort={};desc={};treeall={};remember={};tree={}",
            self.theme as u8,
            self.text_size as u8,
            self.density as u8,
            self.colored_icons as u8,
            self.grouping as u8,
            self.sort_by as u8,
            self.descending as u8,
            self.tree_all_files as u8,
            self.remember_open_folders as u8,
            self.show_tree as u8
        )
    }

    pub fn from_storage(s: &str) -> Self {
        let mut out = Settings::default();
        for pair in s.split(';') {
            let Some((k, v)) = pair.split_once('=') else { continue };
            let Ok(v) = v.parse::<usize>() else { continue };
            match k {
                "theme" => out.theme = Theme::ALL.get(v).map_or(out.theme, |t| t.0),
                "text" => out.text_size = TextSize::ALL.get(v).map_or(out.text_size, |t| t.0),
                "density" => out.density = Density::ALL.get(v).map_or(out.density, |t| t.0),
                "colors" => out.colored_icons = v != 0,
                "group" => out.grouping = Grouping::ALL.get(v).map_or(out.grouping, |t| t.0),
                "sort" => out.sort_by = [SortBy::Name, SortBy::Type, SortBy::Size].get(v).copied().unwrap_or(out.sort_by),
                "desc" => out.descending = v != 0,
                "treeall" => out.tree_all_files = v != 0,
                "remember" => out.remember_open_folders = v != 0,
                "tree" => out.show_tree = v != 0,
                _ => {}
            }
        }
        out
    }
}

/// What a file is, for its icon and colour.
#[derive(Clone, Copy, PartialEq)]
pub enum FileClass {
    Folder,
    Archive,
    Texture,
    Model,
    Map,
    Audio,
    Script,
    Text,
    Other,
}

impl FileClass {
    pub fn of(name: &str) -> Self {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "rpf" | "oiv" => Self::Archive,
            "ytd" | "dds" | "png" | "jpg" | "jpeg" | "bmp" | "gfx" => Self::Texture,
            "ydr" | "ydd" | "yft" | "ybn" | "ycd" | "ypt" | "ymf" => Self::Model,
            "ymap" | "ytyp" | "ynd" | "ynv" | "ymt" => Self::Map,
            "awc" | "rel" | "dat151" | "dat54" | "bik" => Self::Audio,
            "ysc" | "asi" | "dll" | "exe" => Self::Script,
            "xml" | "meta" | "dat" | "txt" | "ini" | "json" | "gxt2" | "log" | "toml" | "cfg" | "lua" => Self::Text,
            _ => Self::Other,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Folder => "🗀",
            Self::Archive => "📦",
            Self::Texture => "🖼",
            Self::Model => "🔷",
            Self::Map => "🗺",
            Self::Audio => "🔊",
            Self::Script => "⚙",
            Self::Text => "📝",
            Self::Other => "🗋",
        }
    }

    /// The icon colour: lighter on the dark theme, deeper on the light one.
    pub fn color(self, dark: bool) -> Color32 {
        let (on_dark, on_light) = match self {
            Self::Folder => ([0xE8, 0xB0, 0x4B], [0xB7, 0x79, 0x1F]),
            Self::Archive => ([0xE0, 0x7B, 0x39], [0xC2, 0x5B, 0x1A]),
            Self::Texture => ([0xB0, 0x7C, 0xE8], [0x8A, 0x4F, 0xD0]),
            Self::Model => ([0x4F, 0xA3, 0xE8], [0x2A, 0x78, 0xC2]),
            Self::Map => ([0x5C, 0xC7, 0x7A], [0x2E, 0x9A, 0x4E]),
            Self::Audio => ([0xE8, 0x6F, 0xA6], [0xC8, 0x40, 0x7E]),
            Self::Script => ([0xE8, 0x5C, 0x5C], [0xC7, 0x38, 0x38]),
            Self::Text => ([0x4D, 0xC2, 0xC2], [0x1E, 0x8F, 0x8F]),
            Self::Other => ([0xA0, 0xA0, 0xA0], [0x80, 0x80, 0x80]),
        };
        let [r, g, b] = if dark { on_dark } else { on_light };
        Color32::from_rgb(r, g, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_roundtrip() {
        let s = Settings {
            theme: Theme::Light,
            text_size: TextSize::Large,
            density: Density::Compact,
            colored_icons: false,
            grouping: Grouping::ByType,
            sort_by: SortBy::Size,
            descending: true,
            tree_all_files: true,
            remember_open_folders: true,
            show_tree: false,
        };
        assert!(Settings::from_storage(&s.to_storage()) == s);
        // Older saved settings have no "tree" key: the tree is shown.
        assert!(Settings::from_storage("theme=1;text=1").show_tree);
    }
}
