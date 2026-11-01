//! Favorites and recent locations: places (a game folder, an archive on disk,
//! a path inside an archive) saved between sessions, reopened with a click.
//! A place is stored relative to its game folder, so an install that moved can
//! be re-pointed; the mods copy and its original are one location, stored by
//! the original's path when there is one.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use paleto_core::{ArchivePath, Error, Game};
use paleto_rpf::Keys;

use crate::app::Location;

/// How many recent locations the lists keep, oldest lost last.
pub(crate) const RECENTS_CAP: usize = 24;

/// A place of the game data: the game folder it belongs to, the item's path
/// relative to it (`mods/…` when it is only in the mods folder), the path
/// inside the archive (empty for a plain folder or file), and what kind it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    game: OsString,
    rel: OsString,
    inner: String,
    archive: bool,
}

impl Place {
    pub(crate) fn new(game: &Path, rel: &Path, inner: &str, archive: bool) -> Self {
        Place { game: game.into(), rel: rel.into(), inner: inner.into(), archive }
    }

    /// The item's name, as the lists show it.
    pub(crate) fn title(&self) -> String {
        match self.inner.is_empty() {
            true => self.rel.to_string_lossy().into_owned(),
            false => format!("{} ({})", self.rel.to_string_lossy(), self.inner),
        }
    }
    /// The place as one text, for the tests: "rel path" below "inner path".
    pub(crate) fn headline(&self) -> String {
        match self.inner.is_empty() {
            true => self.rel.to_string_lossy().into_owned(),
            false => format!("{}/{}", self.rel.to_string_lossy(), self.inner),
        }
    }

    /// Where the item is, trying its standing mods copy when the original is
    /// gone; when the game folder the entry remembers is gone, the current
    /// game (a moved install re-points itself here). Nothing when nothing is.
    fn disk(&self, game: Option<&Game>) -> Option<PathBuf> {
        let root = os_path(&self.game);
        if !root.is_dir() && let Some(game) = game {
            // The entry is re-pointed to where the game is now.
            return Some(os_path(&self.rel)).and_then(|rel| {
                let original = game.root.join(&rel);
                let candidate = match original.exists() {
                    true => original,
                    false => game.root.join("mods").join(&rel),
                };
                candidate.exists().then_some(candidate)
            });
        }
        let rel = os_path(&self.rel);
        if rel.starts_with("mods") || rel.is_absolute() {
            return root.join(&rel).exists().then_some(root.join(&rel));
        }
        let original = root.join(&rel);
        if original.exists() {
            return Some(original);
        }
        let copy = root.join("mods").join(&rel);
        copy.exists().then_some(copy)
    }

    /// The location to show for this place (`self.disk` is where it stands);
    /// no location for a missing one, and the lists say why.
    fn location(&self, game: Option<&Game>) -> Option<Location> {
        let disk = self.disk(game)?;
        match self.archive {
            true => {
                let inner = self.inner.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect();
                Some(Location::Archive(ArchivePath { disk, inner }))
            }
            false => Some(Location::Folder(disk)),
        }
    }

    /// Whether the item is there: the game folder, the item itself, and (with
    /// keys) the way to it through the archive. With no keys, an entry that
    /// needs them stays "cannot verify yet": nothing is pruned for it.
    pub(crate) fn check(&self, keys: Option<&Keys>, game: Option<&Game>) -> Status {
        if !os_path(&self.game).is_dir() && game.is_none() {
            return Status::Missing("the game folder is gone".into());
        }
        let Some(disk) = self.disk(game) else {
            let mods_only = self.rel.as_encoded_bytes().starts_with(b"mods");
            return Status::Missing(match mods_only {
                true => "the mods copy is gone".into(),
                false => "the file is gone (its mods copy too)".into(),
            });
        };
        if self.inner.is_empty() {
            return Status::Valid;
        }
        let path = ArchivePath { disk, inner: self.inner.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect() };
        match path.open(keys) {
            Ok(_) => Status::Valid,
            Err(Error::Rpf(paleto_rpf::Error::KeysRequired)) => Status::Unverified,
            Err(_) => Status::Missing("the way through the archive is gone".into()),
        }
    }
}

fn os_path(os: &OsStr) -> PathBuf {
    PathBuf::from(os.to_os_string())
}

/// What the lists say about an entry (valid entries say nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Valid,
    /// The game folder or the item is gone.
    Missing(String),
    /// The way through the archive needs keys that are not loaded yet.
    Unverified,
}

impl Status {
    fn text(&self) -> Option<&str> {
        match self {
            Status::Valid => None,
            Status::Missing(why) => Some(why),
            Status::Unverified => Some("cannot verify yet: reading it needs the keys"),
        }
    }
}

/// Several places as they are kept in the app's storage: one line per place,
/// every field `%`-escaped, so paths need not be Unicode and no line break or
/// tab inside them can break the rows apart.
pub(crate) fn encode(places: &[Place]) -> String {
    fn field(os: &OsStr) -> String {
        use std::os::unix::ffi::OsStrExt;
        escape(os.as_bytes())
    }
    places
        .iter()
        .map(|p| {
            [field(&p.game), if p.archive { "archive" } else { "folder" }.to_string(), field(&p.rel), escape(p.inner.as_bytes())].join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// [`encode`] read back; lines that are not of this form are dropped.
pub(crate) fn decode(text: &str) -> Vec<Place> {
    use std::os::unix::ffi::OsStringExt;
    text.lines()
        .filter_map(|line| {
            let [game, kind, rel, inner] = line.split('\t').collect::<Vec<_>>()[..] else { return None };
            if !matches!(kind, "archive" | "folder") {
                return None;
            }
            Some(Place {
                game: OsString::from_vec(unescape(game.as_bytes())),
                rel: OsString::from_vec(unescape(rel.as_bytes())),
                inner: String::from_utf8(unescape(inner.as_bytes())).ok()?,
                archive: kind == "archive",
            })
        })
        .collect()
}

/// `%XX` for the bytes that would not survive a line (the `%` itself, newlines,
/// tabs, control and non-ASCII bytes); everything else stays as it is, so the
/// storage stays readable for ordinary paths.
pub(crate) fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'%' | 0x00..=0x1F | 0x7F..=0xFF => out.push_str(&format!("%{b:02X}")),
            _ => out.push(b as char),
        }
    }
    out
}

/// [`escape`] read back; bytes that were not escaped stay as they are.
pub(crate) fn unescape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'%'
            && i + 3 <= text.len()
            && let (Some(hi), Some(lo)) = ((text[i + 1] as char).to_digit(16), (text[i + 2] as char).to_digit(16))
        {
            out.push(((hi as u8) << 4) | lo as u8);
            i += 3;
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

pub struct FavoritesWindow {
    /// The state of the favorites, checked again when the list or the keys change.
    checks: Vec<Status>,
    /// What the checks were made for (the keys' and game's addresses and the list length).
    checks_for: (Option<u64>, Option<u64>, usize),
}

pub enum Request {
    Close,
    Open(Location),
    /// Keep the place the user is looking at (its `Place` comes with it).
    Add(Place),
    /// Forget a favorite (its position in the app's list).
    ForgetFavorite(usize),
    /// Forget one recent location (its position).
    ForgetRecent(usize),
    /// Forget every recent location.
    ClearRecents,
}

impl FavoritesWindow {
    pub fn new() -> Self {
        FavoritesWindow { checks: Vec::new(), checks_for: (None, None, 0) }
    }

    /// The favorites checked again: after the window opens, when the list
    /// changes, and when keys load in (an "unverified" entry may become valid).
    /// Called from the app's task polling, never per frame. (The recents whose
    /// item is simply gone no longer are, which prunes them.)
    pub(crate) fn revalidate(&mut self, keys: Option<&Keys>, game: Option<&Game>, favorites: &[Place]) {
        self.checks = favorites.iter().map(|p| p.check(keys, game)).collect();
        self.checks_for = (keys.map(|k| k as *const Keys as u64), game.map(|g| g as *const Game as u64), favorites.len());
    }

    #[allow(clippy::too_many_arguments)] // the two lists, the shown place and the state around them
    pub fn show(&mut self, ctx: &egui::Context, busy: bool, keys: Option<&Keys>, game: Option<&Game>, favorites: &[Place], recents: &[Place], current: Option<&Place>) -> Option<Request> {
        let (request, closed) = crate::widgets::dialog(ctx, "favorites", "Favorites and recent locations", 640.0, |ui| self.contents(ui, busy, keys, game, favorites, recents, current));
        if closed {
            Some(Request::Close)
        } else {
            request
        }
    }

    #[allow(clippy::too_many_arguments)] // the two lists, the shown place and the state around them
    fn contents(&mut self, ui: &mut egui::Ui, busy: bool, keys: Option<&Keys>, game: Option<&Game>, favorites: &[Place], recents: &[Place], current: Option<&Place>) -> Option<Request> {
        let mut request = None;
        let marker = (keys.map(|k| k as *const Keys as u64), game.map(|g| g as *const Game as u64), favorites.len());
        if self.checks_for != marker {
            self.revalidate(keys, game, favorites);
        }
        ui.label("Keep the places of the game you come back to. A favorite remembers its game folder, so an install that moved still resolves, and the mods copy and its original are one place.");
        ui.add_space(4.0);
        if favorites.is_empty() {
            ui.label(RichText::new("No favorites yet: open somewhere, then 'add the shown place'.").weak());
        }
        egui::Grid::new("favorites").striped(true).num_columns(3).min_col_width(150.0).max_col_width(430.0).show(ui, |ui| {
            for (i, place) in favorites.iter().enumerate() {
                let status = self.checks.get(i);
                let gone = matches!(status, Some(Status::Missing(_)));
                let opened = ui.add_enabled(!gone, egui::Button::new(place.title())).on_hover_text(format!("Reopen {}", place.headline())).clicked();
                if opened
                    && let Some(location) = place.location(game)
                {
                    request = Some(Request::Open(location));
                }
                if let Some(why) = status.and_then(|s| s.text()) {
                    ui.label(RichText::new(why).weak());
                }
                if ui.small_button("Forget").on_hover_text("Take it out of the favorites").clicked() {
                    request = Some(Request::ForgetFavorite(i));
                }
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let already = current.is_some_and(|p| favorites.contains(p));
            let hover = match &current {
                Some(place) => format!("Keep {}", place.headline()),
                None => "Open the game folder, an archive or a folder in it, then add it here".into(),
            };
            if ui
                .add_enabled(current.is_some() && !already && !busy, egui::Button::new("Add the shown place…"))
                .on_hover_text(hover)
                .on_disabled_hover_text(match (already, busy, current.is_some()) {
                    (true, _, _) => "It is a favorite already",
                    (false, true, _) => "Wait for the current job to finish, then try again",
                    _ => "Open the game folder, an archive or a folder in it first",
                })
                .clicked()
                && let Some(place) = current
            {
                request = Some(Request::Add(place.clone()));
            }
        });
        ui.separator();
        ui.label(RichText::new("Recent locations").weak());
        if recents.is_empty() {
            ui.label(RichText::new("The places of the game you visit are kept here, joined to their game.").weak());
        }
        egui::Grid::new("recents").striped(true).num_columns(3).min_col_width(150.0).max_col_width(430.0).show(ui, |ui| {
            for (i, place) in recents.iter().enumerate() {
                let gone = matches!(place.check(keys, game), Status::Missing(_));
                let opened = ui.add_enabled(!gone, egui::Button::new(place.title())).on_hover_text("Reopen it").on_disabled_hover_text("the file is gone").clicked();
                if opened
                    && let Some(location) = place.location(game)
                {
                    request = Some(Request::Open(location));
                }
                if ui.small_button("Forget").on_hover_text("Take it out of the list").clicked() {
                    request = Some(Request::ForgetRecent(i));
                }
                ui.end_row();
            }
        });
        if !recents.is_empty() && ui.small_button("Forget them all").on_hover_text("Take every recent location out").clicked() {
            request = Some(Request::ClearRecents);
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(game: &str, rel: &str, inner: &str) -> Place {
        Place::new(Path::new(game), Path::new(rel), inner, true)
    }

    #[test]
    fn places_round_trip_through_the_storage_form() {
        for place in [
            place("/game", "update/update.rpf", "levels/gta5/vehicles.rpf"),
            place("/game", "mods/x64e.rpf", ""),
            place("/home", "«weird»=%", "«»\t\\\n%"),
        ] {
            assert_eq!(decode(&encode(std::slice::from_ref(&place))), [place]);
        }
        // Several places, one line each, in order.
        let places = [place("/a", "b.rpf", ""), place("/c", "d", "e")];
        let text = encode(&places);
        assert_eq!(text.lines().count(), 2);
        assert_eq!(decode(&text), places);
        // Lines that are not of this form are dropped, and the kind is kept.
        assert!(decode("one\n\n1\t2\t3\n/not/enough/fields\ttoo many\tfor a place\t").is_empty());
        let folder = Place::new(Path::new("/game"), Path::new("mods"), "", false);
        // The kind is kept: a folder is a folder.
        assert!(!decode(&encode(std::slice::from_ref(&folder)))[0].archive);
        assert_eq!(decode(&encode(std::slice::from_ref(&folder))), [folder]);
    }

    #[test]
    fn escapes_and_unescapes_bytes() {
        for bytes in [b"".to_vec(), b"plain path/rpf".to_vec(), vec![0], vec![b'\t', b'\n', b'%', 0x7F, 0xFF, 0x01], b"x\xFF%2".to_vec()] {
            let text = escape(&bytes);
            assert_eq!(unescape(text.as_bytes()), bytes);
        }
        // The separators escape, so the fields cannot run into each other, not even tabs in names.
        let place = Place::new(Path::new("/game"), Path::new("rel\tone"), "in ner", false);
        let text = encode(std::slice::from_ref(&place));
        assert_eq!(text.split('\t').count(), 4);
        assert_eq!(text.lines().count(), 1);
        assert_eq!(decode(&text), [place]);
    }
}
