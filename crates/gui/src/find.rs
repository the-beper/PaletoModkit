//! The search bar of a text preview, and how the previewed text maps back to the file.

use std::sync::Arc;

use paleto_formats::MetaFile;

use crate::app::App;
use crate::highlight;

/// The search of a text preview.
#[derive(Default)]
pub(crate) struct Find {
    pub(crate) query: String,
    pub(crate) matches: Vec<std::ops::Range<usize>>,
    /// The match shown: an index of `matches`.
    pub(crate) current: usize,
    /// What `matches` was found with: the query and the length of the text.
    found_with: (String, usize),
    /// The text was edited since: look again.
    pub(crate) stale: bool,
    /// Give the search field the keyboard on the next frame.
    pub(crate) focus: bool,
    /// Scroll the current match into view on the next frame.
    pub(crate) jump: bool,
}

impl Find {
    pub(crate) fn new() -> Self {
        Find { focus: true, ..Find::default() }
    }

    /// Looks again when the query or the text changed.
    pub(crate) fn refresh(&mut self, text: &str) {
        if self.stale || self.found_with.0 != self.query || self.found_with.1 != text.len() {
            self.matches = highlight::find_all(text, &self.query);
            self.found_with = (self.query.clone(), text.len());
            self.stale = false;
            self.current = self.current.min(self.matches.len().saturating_sub(1));
        }
    }

    /// Moves to the next (or previous) match, wrapping round.
    pub(crate) fn step(&mut self, forward: bool) {
        let n = self.matches.len();
        if n > 0 {
            self.current = if forward { (self.current + 1) % n } else { (self.current + n - 1) % n };
            self.jump = true;
        }
    }
}

/// What the text of a preview is: the file itself, or a conversion of it.
pub(crate) enum Codec {
    Plain,
    /// A text table (`.gxt2`) as "0xHASH = text" lines.
    Gxt2,
    /// Metadata (PSO or resource) as XML.
    Meta(Arc<MetaFile>),
    /// A summary of a file that has no text form (`.ycd`): read-only.
    Summary,
}

impl Codec {
    pub(crate) fn encode(&self, text: &str) -> Result<Vec<u8>, String> {
        match self {
            Codec::Plain => Ok(text.as_bytes().to_vec()),
            Codec::Gxt2 => paleto_formats::gxt2::from_text(text).map_err(|e| e.to_string()),
            Codec::Meta(meta) => meta.rebuild_from_xml(text).map_err(|e| e.to_string()),
            Codec::Summary => Err("this is a summary of the file, not its text".into()),
        }
    }
}

impl App {
    /// The text shown in the preview editor.
    /// Opens the search bar of the text preview with `query`, as typing it would; the
    /// number of matches found (for tests).
    pub fn search_preview(&mut self, query: &str) -> Option<usize> {
        let p = self.preview.as_mut()?;
        let text = &p.text.as_ref()?.0;
        let mut find = Find::new();
        find.focus = false;
        find.jump = true;
        find.query = query.to_string();
        find.refresh(text);
        let n = find.matches.len();
        p.find = Some(find);
        Some(n)
    }
}
