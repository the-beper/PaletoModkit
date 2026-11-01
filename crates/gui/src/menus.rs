//! The menus and bars of the window: the menu bar, the side tree of game folders, the
//! location bar, the welcome screen, and the help windows.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText};
use paleto_core::vfs;
use paleto_core::ArchivePath;
use paleto_formats::Format;

use crate::actions::Action;
use crate::app::{App, Info, Location, Status, right_width, INSTALL, INSTALL_SHORT, MODS_COPY, MODS_LINK, MODS_LINK_HOVER, ORIGINAL, REPO_URL, TWEAKS, TWEAKS_SHORT};
use crate::browser::{Browser, filter_box};
use crate::preview::PreviewSource;
use crate::settings::{Density, FileClass, Grouping, TextSize, Theme};
use crate::theme;

impl App {
    // ---- UI ----------------------------------------------------------------

    fn icon(&self, class: FileClass, dark: bool) -> RichText {
        let text = RichText::new(class.icon());
        if self.settings.colored_icons { text.color(class.color(dark)) } else { text }
    }

    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let can_install = self.keys.is_some();
        // Menu titles as tall as the bar's buttons, their text centred.
        let height = crate::widgets::bar_height(ui);
        let text = ui.text_style_height(&egui::TextStyle::Button);
        let padding = egui::vec2(8.0, ((height - text) / 2.0).max(0.0));
        let style = move |s: &mut egui::Style| {
            egui::containers::menu::menu_style(s);
            s.spacing.button_padding = padding;
        };
        egui::MenuBar::new().style(style).ui(ui, |ui| {
            ui.set_min_height(height);
            let has_game = self.game.is_some();
            let need_game = "Choose the game folder first (Game menu)";
            let need_keys = if has_game { "Reading the game's encryption keys…" } else { need_game };
            let file = menu_title(ui, "File", |ui| {
                if ui.button("Open archive…").on_hover_text("Browse an .rpf archive, also outside the game folder").clicked() {
                    actions.push(Action::PickArchive);
                }
                if ui.button("Favorites and recent locations…").on_hover_text("The places of the game you come back to, kept between sessions").clicked() {
                    actions.push(Action::Favorites(true));
                }
                ui.separator();
                if ui
                    .add_enabled(can_install, egui::Button::new("Export mods as an OIV package…"))
                    .on_hover_text("Pack your changes to the game's files into one .oiv, to share them or install them again later")
                    .on_disabled_hover_text(need_keys)
                    .clicked()
                {
                    actions.push(Action::Export(true));
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    actions.push(Action::Quit);
                }
            });
            let game = menu_title(ui, "Game", |ui| {
                if !self.installs.is_empty() {
                    ui.label(RichText::new("Installed games").weak());
                }
                for game in &self.installs {
                    let current = self.game.as_ref().is_some_and(|g| g.root == game.root);
                    let folder = game.root.file_name().map_or_else(|| game.root.display().to_string(), |n| n.to_string_lossy().into_owned());
                    if ui.selectable_label(current, format!("{:?}: {folder}", game.edition)).on_hover_text(game.root.display().to_string()).clicked() {
                        actions.push(Action::SelectGame(game.root.clone()));
                    }
                }
                if ui.button("Choose the game folder…").on_hover_text("The folder holding GTA5.exe or GTA5_Enhanced.exe").clicked() {
                    actions.push(Action::PickGame);
                }
            });
            let tools = menu_title(ui, "Tools", |ui| {
                if ui.button("Hash generator…").on_hover_text("The hashes of names and the names of hashes, with or without a game open").clicked() {
                    actions.push(Action::Hashes(true));
                }
                ui.separator();
                if ui
                    .add_enabled(has_game, egui::Button::new("Mods in game…"))
                    .on_hover_text("Every mod the game folder holds, whether the game loads it, and what is wrong with the setup")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::CheckMods(true));
                }
                if ui
                    .add_enabled(has_game, egui::Button::new("Snapshots of the mods folder…"))
                    .on_hover_text("Save the mods folder as it is now, and go back to it if a change breaks the game")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::Snapshots(true));
                }
                ui.separator();
                if ui
                    .add_enabled(has_game, egui::Button::new("Crash detection…"))
                    .on_hover_text("Why the game crashed, which mod is likely to blame, and a search that finds it")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::CrashDetection(true));
                }
                ui.separator();
                if ui
                    .add_enabled(has_game, egui::Button::new("Script Hook V…"))
                    .on_hover_text("The installed and the latest Script Hook V, whether it suits the game, and the update")
                    .on_disabled_hover_text(need_game)
                    .clicked()
                {
                    actions.push(Action::ScriptHook(true));
                }
            });
            let view = menu_title(ui, "View", |ui| {
                let mut s = self.settings.clone();
                ui.label(RichText::new("Theme").weak());
                for (value, label) in Theme::ALL {
                    ui.radio_value(&mut s.theme, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Text and icon size").weak());
                for (value, label) in TextSize::ALL {
                    ui.radio_value(&mut s.text_size, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Density").weak());
                for (value, label) in Density::ALL {
                    ui.radio_value(&mut s.density, value, label);
                }
                ui.separator();
                ui.label(RichText::new("Folders and files").weak());
                for (value, label) in Grouping::ALL {
                    ui.radio_value(&mut s.grouping, value, label);
                }
                ui.separator();
                ui.checkbox(&mut s.colored_icons, "Coloured icons");
                ui.checkbox(&mut s.show_tree, "Side tree");
                ui.add_enabled(s.show_tree, egui::Checkbox::new(&mut s.tree_all_files, "Show all files in the side tree"));
                ui.checkbox(&mut s.remember_open_folders, "Remember open folders");
                if s != self.settings {
                    actions.push(Action::Settings(s));
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.show_originals.clone(), "Show original files (ignore mods folder)")
                    .on_hover_text("Show the game's own files where the mods folder has a modified copy")
                    .clicked()
                {
                    actions.push(Action::ToggleOriginals);
                }
                ui.separator();
                if ui.add_enabled(self.settings.show_tree, egui::Button::new("Expand all folders")).clicked() {
                    actions.push(Action::ExpandAll);
                }
                if ui.add_enabled(self.settings.show_tree, egui::Button::new("Collapse all folders")).clicked() {
                    actions.push(Action::CollapseAll);
                }
                if ui.add(egui::Button::new("Refresh").shortcut_text("F5")).clicked() {
                    actions.push(Action::Refresh);
                }
            });
            let help = menu_title(ui, "Help", |ui| {
                if ui.button("Getting started").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::GettingStarted)));
                }
                if ui.button("Keyboard and mouse shortcuts").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::Shortcuts)));
                }
                ui.separator();
                ui.hyperlink_to("Online documentation", format!("{REPO_URL}/tree/main/docs"));
                ui.hyperlink_to("Report a problem", format!("{REPO_URL}/issues"));
                ui.separator();
                if ui.button("About").clicked() {
                    actions.push(Action::ShowInfo(Some(Info::About)));
                }
            });
            switch_menus(ui.ctx(), &[file, game, tools, view, help]);
            let narrow = ui.ctx().content_rect().width() < 760.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Like the other bars, not flat as menu titles.
                ui.reset_style();
                // The main action, at the far right.
                if can_install {
                    let label = if narrow { INSTALL_SHORT } else { INSTALL };
                    // In the accent colour, to stand out.
                    let selection = ui.visuals().selection;
                    let install = egui::Button::new(RichText::new(label).color(selection.stroke.color)).fill(selection.bg_fill);
                    // Short on a narrow window, where it saves room.
                    let width = if narrow { 0.0 } else { right_width(ui) };
                    if crate::widgets::bar_button(ui, install, width).on_hover_text("Install an OpenIV package (.oiv) or an add-on pack (dlc.rpf) into the mods folder").clicked() {
                        actions.push(Action::PickPackage);
                    }
                }
                // The Wine prefix, beside it.
                if self.game.is_some() {
                    let label = if narrow { TWEAKS_SHORT } else { TWEAKS };
                    let width = if narrow { 0.0 } else { right_width(ui) };
                    // Wine red: not the accent of Install, but not a plain button either.
                    let wine = if ui.visuals().dark_mode { egui::Color32::from_rgb(0x7A, 0x2B, 0x38) } else { egui::Color32::from_rgb(0xA4, 0x3A, 0x48) };
                    let tweaks = egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(wine);
                    if crate::widgets::bar_button(ui, tweaks, width)
                        .on_hover_text("Changes to the game's Wine prefix that mods need, e.g. .NET for ScriptHookVDotNet mods")
                        .clicked()
                    {
                        actions.push(Action::PrefixTweaks(true));
                    }
                }
                // The filter, centred in the window when there is room for it.
                let Some(b) = self.browser.as_mut() else { return };
                let gap = 2.0 * ui.spacing().item_spacing.x;
                // This layout starts where the menus end.
                let (left, right) = (ui.max_rect().left() + gap, ui.available_rect_before_wrap().right() - gap);
                let width = (right - left).min(260.0);
                if width < 80.0 {
                    return;
                }
                let x = (ui.ctx().content_rect().center().x - width / 2.0).clamp(left, right - width);
                let rect = egui::Rect::from_min_size(egui::pos2(x, ui.max_rect().center().y - height / 2.0), egui::vec2(width, height));
                filter_box(ui, b, rect);
            });
        });
    }

    pub(crate) fn side_tree(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(root) = self.game.as_ref().map(|g| g.root.clone()) else { return };
        let current = self.tree_selection();
        let root_selected = current.as_deref() == Some(root.as_path());
        // Two buttons sharing the width, as tall as the file toolbar beside them.
        ui.horizontal(|ui| {
            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            if crate::widgets::bar_button(ui, egui::Button::new("⏷ Expand all"), width).on_hover_text("Expand every folder").clicked() {
                actions.push(Action::ExpandAll);
            }
            if crate::widgets::bar_button(ui, egui::Button::new("⏶ Collapse all"), width).on_hover_text("Collapse every folder").clicked() {
                actions.push(Action::CollapseAll);
            }
        });
        ui.separator();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.icon(FileClass::Folder, ui.visuals().dark_mode));
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.selectable_label(root_selected, RichText::new(name).strong()).clicked() {
                    actions.push(Action::Go(Location::Folder(root.clone())));
                }
            });
            self.tree_dir(ui, &root, current.as_deref(), actions);
        });
        self.read_tree();
        // Until the location asked for is shown, and the folders above it read, it is still to reveal.
        if self.listing_task.is_none() && self.tree_task.is_none() && self.tree_loading.is_empty() {
            self.reveal = false;
        }
    }

    /// One folder of the side tree: clicking a name opens it, the arrow
    /// expands it.
    fn tree_dir(&mut self, ui: &mut egui::Ui, dir: &Path, current: Option<&Path>, actions: &mut Vec<Action>) {
        let entries: Vec<(PathBuf, String, bool, bool)> =
            self.listing(dir).iter().map(|e| (e.path.clone(), e.name.clone(), e.is_dir, e.modded)).collect();
        if self.tree_loading.contains(dir) {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                ui.spinner();
            });
        }
        for (path, name, is_dir, modded) in entries {
            let selected = current == Some(path.as_path());
            if is_dir {
                // Open/closed state lives in `open_dirs`, so it can be expanded,
                // collapsed and remembered as a whole.
                let id = egui::Id::unique(("tree", &path));
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                if self.reveal && current.is_some_and(|c| c.starts_with(&path) && c != path) {
                    self.open_dirs.insert(path.clone());
                }
                state.set_open(self.open_dirs.contains(&path));
                let icon = self.icon(FileClass::Folder, ui.visuals().dark_mode);
                state
                    .show_header(ui, |ui| {
                        ui.label(icon);
                        if ui.selectable_label(selected, &name).clicked() {
                            actions.push(Action::Go(Location::Folder(path.clone())));
                        }
                    })
                    .body(|ui| self.tree_dir(ui, &path, current, actions));
                if egui::collapsing_header::CollapsingState::load(ui.ctx(), id).is_some_and(|s| s.is_open()) {
                    self.open_dirs.insert(path);
                } else {
                    self.open_dirs.remove(&path);
                }
            } else {
                let class = FileClass::of(&name);
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    ui.label(self.icon(class, ui.visuals().dark_mode));
                    let text = if modded { RichText::new(&name).color(theme::modded(ui.visuals().dark_mode)) } else { RichText::new(&name) };
                    let response = ui.selectable_label(selected, text);
                    let response = if modded {
                        let link = ui.label(RichText::new(MODS_LINK).color(theme::modded(ui.visuals().dark_mode)));
                        link.on_hover_text(MODS_LINK_HOVER);
                        response.on_hover_text(MODS_LINK_HOVER)
                    } else {
                        response
                    };
                    if response.clicked() {
                        actions.push(if class == FileClass::Archive {
                            Action::Go(Location::Archive(ArchivePath::new(path.clone())))
                        } else {
                            Action::Reveal(path.clone())
                        });
                    }
                    if selected && self.reveal {
                        response.scroll_to_me(None);
                    }
                });
            }
        }
    }

    /// The clickable segments of the location bar.
    pub(crate) fn breadcrumbs(&self, b: &Browser) -> Vec<(String, Location)> {
        let location = &b.location;
        let root = self.game.as_ref().map(|g| g.root.clone());
        let (disk, inner) = match location {
            Location::Folder(p) => (p.clone(), None),
            Location::Archive(a) => (a.disk.clone(), Some(a)),
        };
        let mut out = Vec::new();
        match root.as_ref().and_then(|r| disk.strip_prefix(r).ok().map(|rel| (r, rel))) {
            Some((root, rel)) => {
                let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                out.push((name, Location::Folder(root.clone())));
                let mut acc = root.clone();
                let parts: Vec<_> = rel.components().collect();
                for (i, part) in parts.iter().enumerate() {
                    acc.push(part);
                    let is_archive = inner.is_some() && i + 1 == parts.len();
                    let target = if is_archive { Location::Archive(ArchivePath::new(acc.clone())) } else { Location::Folder(acc.clone()) };
                    out.push((part.as_os_str().to_string_lossy().into_owned(), target));
                }
            }
            None => {
                let target = if inner.is_some() { Location::Archive(ArchivePath::new(disk.clone())) } else { Location::Folder(disk.clone()) };
                out.push((disk.display().to_string(), target));
            }
        }
        if let Some(a) = inner {
            for i in 0..a.inner.len() {
                let mut target = ArchivePath::new(a.disk.clone());
                target.inner = a.inner[..=i].to_vec();
                out.push((a.inner[i].clone(), Location::Archive(target)));
            }
        }
        out.into_iter().map(|(label, target)| (label, self.within(b, target))).collect()
    }

    /// The location bar across the window, with the package installer on the right.
    pub(crate) fn location_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(b) = &self.browser else { return };
        let crumbs = self.breadcrumbs(b);
        let (can_back, can_forward) = (!self.back.is_empty(), !self.forward.is_empty());
        let peeking = self.peek_original.is_some();
        // A mods copy of a game archive, opened through either.
        let has_original = matches!(&b.location, Location::Archive(_)) && self.original_of(&b.location) != b.location;
        let square = crate::widgets::bar_height(ui);
        // Each piece of text is as tall as the buttons, its text centred, so a
        // row it wraps onto is centred like the first.
        let width = |ui: &egui::Ui, text: egui::WidgetText| text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x + 2.0;
        // The switch at the right end, on the first row; the path wraps in the room left.
        ui.horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if has_original || peeking {
                let (label, hover) = if peeking {
                    (MODS_COPY, "Show the modified copy in the mods folder, which the game loads")
                } else {
                    (ORIGINAL, "Show the game's own archive, without the changes in the mods folder")
                };
                if crate::widgets::centred_bar_button(ui, label, right_width(ui)).on_hover_text(hover).clicked() {
                    actions.push(Action::PeekOriginal(!peeking));
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
            ui.set_min_height(square);
            let nav = |ui: &mut egui::Ui, enabled: bool, icon: &str| ui.add_enabled_ui(enabled, |ui| crate::widgets::bar_button(ui, egui::Button::new(icon), square)).inner;
            if nav(ui, can_back, "⏴").on_hover_text("Back (mouse back button, Alt+Left)").clicked() {
                actions.push(Action::Back);
            }
            if nav(ui, can_forward, "⏵").on_hover_text("Forward (mouse forward button, Alt+Right)").clicked() {
                actions.push(Action::Forward);
            }
            if nav(ui, crumbs.len() > 1, "⏶").on_hover_text("Up (Backspace)").clicked() {
                actions.push(Action::Go(crumbs[crumbs.len() - 2].1.clone()));
            }
            for (i, (label, target)) in crumbs.iter().enumerate() {
                if i > 0 {
                    ui.add_sized([width(ui, "/".into()), square], egui::Label::new("/"));
                }
                if ui.add_sized([width(ui, label.as_str().into()), square], egui::Link::new(label)).clicked() {
                    actions.push(Action::Go(target.clone()));
                }
            }
            if b.from_mods {
                let text = RichText::new("MODS").strong().color(theme::modded(ui.visuals().dark_mode));
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text)).on_hover_text("Showing the modified copy in the mods folder");
            }
            if peeking {
                let text = RichText::new("ORIGINAL").strong();
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text)).on_hover_text("Showing the game's own archive, not its copy in the mods folder");
            }
            if let Some(enc) = b.encryption {
                let text = RichText::new(format!("{enc:?}")).weak();
                ui.add_sized([width(ui, text.clone().into()), square], egui::Label::new(text));
            }
            });
        }));
    }

    /// The archive folder being browsed, where dropped files go.
    /// The folder on disk being browsed, when not inside an archive.
    pub(crate) fn disk_folder(&self) -> Option<PathBuf> {
        match &self.browser.as_ref()?.location {
            Location::Folder(dir) => Some(dir.clone()),
            Location::Archive(_) => None,
        }
    }

    /// The first of `names` in the open disk folder that is not renamed or deleted on disk, with why:
    /// original game data, programs, the game's own files and this tool's folders, of whichever game the
    /// folder is in (not only the one that is open), or a path through a link that leaves the game and mods folders.
    pub(crate) fn original_data(&self, names: &[String]) -> Option<(String, String)> {
        vfs::local_refusal(&self.disk_folder()?, names)
    }

    /// Refuses, with a message, to change original game data on disk.
    pub(crate) fn refuse_original(&mut self, names: &[String]) -> bool {
        let Some((name, why)) = self.original_data(names) else { return false };
        self.status = Status::Error(format!("{name} {why}"));
        true
    }

    pub(crate) fn archive_folder(&self) -> Option<&ArchivePath> {
        match &self.browser.as_ref()?.location {
            Location::Archive(a) => Some(a),
            Location::Folder(_) => None,
        }
    }

    /// Shown while files are dragged over the window: what dropping them does here.
    pub(crate) fn drop_overlay(&self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::unique("drop-dim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
        });
        let ytd = self.preview.as_ref().filter(|p| p.textures.is_some() && matches!(p.source, PreviewSource::Archive(_))).map(|p| p.name.clone());
        let (title, detail) = match self.archive_folder() {
            Some(_) if let Some(ytd) = ytd => (
                format!("Replace textures in {ytd}"),
                "Dropping only DDS or PNG files (or folders of them) replaces the textures they are named after; other files are added to this archive folder.".to_string(),
            ),
            Some(folder) => {
                let name = folder.inner.last().cloned().unwrap_or_else(|| folder.disk.file_name().unwrap_or_default().to_string_lossy().into_owned());
                (format!("Add to {name}"), "Files are added to this archive folder; .oiv packages open the installer.".to_string())
            }
            None => (
                "Install an .oiv package".to_string(),
                "To add other files, first open a folder inside an archive (.rpf).".to_string(),
            ),
        };
        egui::Area::new(egui::Id::unique("drop-card"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(24.0).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("⬇").size(36.0).color(ui.visuals().hyperlink_color));
                        ui.heading(title);
                        ui.label(RichText::new(detail).weak());
                    });
                });
            });
    }

    pub(crate) fn welcome(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.add(egui::Image::from_bytes("bytes://paleto-modkit-icon.png", crate::ICON_PNG).fit_to_exact_size(egui::vec2(96.0, 96.0)));
            ui.heading(crate::NAME);
            ui.label("Choose your GTA V installation.");
            ui.add_space(16.0);
            for game in &self.installs {
                if ui.button(format!("{:?} — {}", game.edition, game.root.display())).clicked() {
                    actions.push(Action::SelectGame(game.root.clone()));
                }
            }
            if ui.button("Choose folder…").clicked() {
                actions.push(Action::PickGame);
            }
            if crate::sandboxed() {
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Flatpak: Steam libraries outside ~/.local/share/Steam, ~/Games, /mnt, /media and /run/media\n\
                         are not visible. Grant access to yours with\n\
                         flatpak override --user --filesystem=/path/to/SteamLibrary {}",
                        crate::APP_ID
                    ))
                    .weak(),
                );
            }
        });
    }

}

/// A menu title that stays highlighted while its menu is open, as the hovered one is.
fn menu_title<R>(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let key = egui::Id::unique(("menu-title-open", title));
    let was_open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let (response, open) = ui
        .scope(|ui| {
            if was_open {
                let hovered = ui.visuals().widgets.hovered.weak_bg_fill;
                ui.visuals_mut().widgets.inactive.weak_bg_fill = hovered;
            }
            let (response, inner) = egui::containers::menu::MenuButton::new(title).ui(ui, contents);
            (response, inner.is_some())
        })
        .inner;
    ui.data_mut(|d| d.insert_temp(key, open));
    if open != was_open {
        ui.ctx().request_repaint();
    }
    response
}

/// Every folder below `root` (hidden ones excluded), for "Expand all".
/// With a menu of the bar open, pointing at another menu's title opens that
/// one, as in desktop menu bars (egui only opens them on click).
fn switch_menus(ctx: &egui::Context, titles: &[egui::Response]) {
    let ids: Vec<egui::Id> = titles.iter().map(egui::Popup::default_response_id).collect();
    if !ids.iter().any(|&id| egui::Popup::is_id_open(ctx, id)) {
        return;
    }
    if let Some((_, &id)) = titles.iter().zip(&ids).find(|(title, id)| title.hovered() && !egui::Popup::is_id_open(ctx, **id)) {
        egui::Popup::open_id(ctx, id);
        ctx.request_repaint();
    }
}

/// Adds a detail to the preview's info line.
pub(crate) fn add_info(info: &mut String, part: impl std::fmt::Display) {
    if !info.is_empty() {
        info.push_str(" · ");
    }
    info.push_str(&part.to_string());
}

pub(crate) fn getting_started(ui: &mut egui::Ui) {
    let section = |ui: &mut egui::Ui, title: &str, paragraphs: &[&str]| {
        ui.strong(title);
        for (i, text) in paragraphs.iter().enumerate() {
            if i > 0 {
                ui.add_space(3.0);
            }
            ui.label(*text);
        }
        ui.add_space(8.0);
    };
    section(
        ui,
        "1. Choose the game",
        &["Paleto Modkit finds GTA V in your Steam libraries; the Game menu picks another install. \
           The first time, the encryption keys are read from your game executable."],
    );
    section(
        ui,
        "2. Browse",
        &["The side tree shows the game folder: click a name to open it, the arrow to expand it. \
           Double-click archives (.rpf) to look inside them, nested archives included."],
    );
    section(
        ui,
        "3. Edit",
        &[
            "Inside an archive, add files with the toolbar or by dropping them on the window; extract, rename or delete them, \
             and create folders. The preview panel edits text files, and views, exports or replaces the textures of .ytd files.",
            "Original game archives are never modified: they are copied to the mods folder first. Archives with a modified copy \
             show in orange, and so do the files inside them that differ from the game's.",
            "Outside archives, rename and delete work on the files on disk.",
        ],
    );
    section(
        ui,
        "4. Install mods",
        &[
            "Install .oiv packages with Install package… (top right) or by dropping them on the window; add-on packs (dlc.rpf) \
             the same way. Game data goes to the mods folder, plugins and scripts to the game folder.",
            "On the Enhanced edition, files made for Legacy are found and can be converted while they are installed.",
            "Prefix tweaks (beside Install) prepares the game's Wine prefix for ScriptHookVDotNet mods: it installs .NET Framework 4.8, \
             after a backup of the prefix. You add ScriptHookVDotNet and the mods yourself.",
        ],
    );
    section(
        ui,
        "5. Play",
        &["The game loads the mods folder through an ASI loader with OpenIV.asi (Legacy) or OpenRPF.asi (Enhanced), which you \
           install separately. Tools > Mods in game checks them, and shows which modded archives the last game session read."],
    );
    section(
        ui,
        "6. When the game breaks",
        &["Tools > Snapshots of the mods folder saves the mods folder before a change and puts it back after. Tools > Crash detection reads the game's \
           crash reports, names the likely causes and narrows the mod down by halves."],
    );
}

pub(crate) fn shortcuts(ui: &mut egui::Ui) {
    let rows = [
        ("Type letters", "Filter the current folder"),
        ("⬆ / ⬇", "Move the selection"),
        ("Enter / double-click", "Open folder or archive, preview file"),
        ("Backspace", "Go up one level"),
        ("Alt+Left / Alt+Right, mouse side buttons", "Back / forward"),
        ("Ctrl+click, Shift+click", "Select several items"),
        ("Delete", "Delete the selection"),
        ("F2", "Rename the selected item"),
        ("Ctrl+Shift+N", "New folder"),
        ("F5", "Read the folders again"),
        ("Esc", "Clear the filter, close dialogs"),
        ("Ctrl+S", "Save the file being edited"),
        ("Ctrl+F", "Search in the text being previewed (Enter: next, Shift+Enter: previous, Esc: close)"),
        ("Right click", "Actions for the item"),
    ];
    egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 6.0]).striped(true).show(ui, |ui| {
        for (keys, what) in rows {
            ui.strong(keys);
            ui.label(what);
            ui.end_row();
        }
    });
}

pub(crate) fn about(ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add(egui::Image::from_bytes("bytes://paleto-modkit-icon.png", crate::ICON_PNG).fit_to_exact_size(egui::vec2(96.0, 96.0)));
        ui.heading(crate::NAME);
        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.add_space(4.0);
        ui.label("GTA V modding for Linux: archives, mods folder and OIV packages.");
        ui.hyperlink_to("github.com/the-beper/PaletoModkit", REPO_URL);
    });
    ui.separator();
    ui.label("Licence: GNU General Public License v3.0 or later.");
    ui.add_space(4.0);
    ui.strong("Thanks to");
    ui.horizontal_wrapped(|ui| {
        ui.hyperlink_to("CodeWalker", "https://github.com/dexyfex/CodeWalker");
        ui.label("and");
        ui.hyperlink_to("gta-toolkit", "https://github.com/Neodymium146/gta-toolkit");
        ui.label("for the RPF format and keys,");
        ui.hyperlink_to("OpenIV", "https://openiv.com");
        ui.label("for the package format,");
        ui.hyperlink_to("egui", "https://github.com/emilk/egui");
        ui.label("for the interface.");
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Not affiliated with Rockstar Games or the OpenIV team.").weak());
}

/// Compression for an imported picture; None is the automatic choice.
pub(crate) fn format_combo(ui: &mut egui::Ui, format: &mut Option<Format>, auto: String) {
    let selected = format.map_or(auto.clone(), |f| f.name().to_string());
    egui::ComboBox::from_label("Format").selected_text(selected).show_ui(ui, |ui| {
        ui.selectable_value(format, None, auto);
        for f in Format::ALL.into_iter().filter(|f| f.can_encode()) {
            ui.selectable_value(format, Some(f), f.name());
        }
    });
}
