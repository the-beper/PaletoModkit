//! Syntax highlighting for the text editor: XML (.xml, .meta…) and
//! line-based config files (.ini, .dat…).

use eframe::egui::{self, Color32, FontId, text::LayoutJob};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Syntax {
    Xml,
    /// `#`/`;`/`//` comments, `[sections]`, `key = value`.
    Config,
    Plain,
}

impl Syntax {
    pub fn for_file(name: &str) -> Self {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "xml" | "meta" | "ymt" | "xsd" | "rbf" => Self::Xml,
            "ini" | "cfg" | "toml" | "dat" | "ide" | "ipl" | "conf" => Self::Config,
            _ => Self::Plain,
        }
    }
}

#[derive(Clone, Copy)]
enum Token {
    Text,
    Punct,
    Tag,
    Attr,
    Value,
    Comment,
    Decl,
    Entity,
}

fn color(token: Token, dark: bool, text: Color32) -> Color32 {
    let rgb = |r, g, b| Color32::from_rgb(r, g, b);
    match (token, dark) {
        (Token::Text, _) => text,
        (Token::Punct, true) => rgb(0x80, 0x80, 0x80),
        (Token::Tag, true) => rgb(0x56, 0x9C, 0xD6),
        (Token::Attr, true) => rgb(0x9C, 0xDC, 0xFE),
        (Token::Value, true) => rgb(0xCE, 0x91, 0x78),
        (Token::Comment, true) => rgb(0x6A, 0x99, 0x55),
        (Token::Decl, true) => rgb(0xC5, 0x86, 0xC0),
        (Token::Entity, true) => rgb(0xD7, 0xBA, 0x7D),
        (Token::Punct, false) => rgb(0x80, 0x00, 0x00),
        (Token::Tag, false) => rgb(0x80, 0x00, 0x00),
        (Token::Attr, false) => rgb(0xE5, 0x00, 0x00),
        (Token::Value, false) => rgb(0x00, 0x00, 0xFF),
        (Token::Comment, false) => rgb(0x00, 0x80, 0x00),
        (Token::Decl, false) => rgb(0xAF, 0x00, 0xDB),
        (Token::Entity, false) => rgb(0xA0, 0x50, 0x00),
    }
}

/// Splits `text` into highlighted spans (byte ranges).
fn tokens(text: &str, syntax: Syntax) -> Vec<(std::ops::Range<usize>, Token)> {
    match syntax {
        Syntax::Xml => xml_tokens(text),
        Syntax::Config => config_tokens(text),
        Syntax::Plain => vec![(0..text.len(), Token::Text)],
    }
}

fn xml_tokens(text: &str) -> Vec<(std::ops::Range<usize>, Token)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // Index just past `end` (or the end of the text).
    let until = |from: usize, end: &str| text[from..].find(end).map_or(text.len(), |p| from + p + end.len());
    while i < b.len() {
        if b[i] == b'<' {
            let start = i;
            if text[i..].starts_with("<!--") {
                i = until(i, "-->");
                out.push((start..i, Token::Comment));
            } else if text[i..].starts_with("<![CDATA[") {
                i = until(i, "]]>");
                out.push((start..i, Token::Value));
            } else if text[i..].starts_with("<?") || text[i..].starts_with("<!") {
                i = until(i, ">");
                out.push((start..i, Token::Decl));
            } else {
                // <name attr="value" ... > or </name> or <name/>
                i += if b.get(i + 1) == Some(&b'/') { 2 } else { 1 };
                out.push((start..i, Token::Punct));
                let name = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'>' | b'/') {
                    i += 1;
                }
                out.push((name..i, Token::Tag));
                while i < b.len() && b[i] != b'>' {
                    let s = i;
                    match b[i] {
                        b'"' | b'\'' => {
                            let quote = b[i];
                            i += 1;
                            while i < b.len() && b[i] != quote {
                                i += 1;
                            }
                            i = (i + 1).min(b.len());
                            out.push((s..i, Token::Value));
                        }
                        b'=' | b'/' => {
                            i += 1;
                            out.push((s..i, Token::Punct));
                        }
                        c if c.is_ascii_whitespace() => {
                            while i < b.len() && b[i].is_ascii_whitespace() {
                                i += 1;
                            }
                            out.push((s..i, Token::Text));
                        }
                        _ => {
                            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/' | b'"' | b'\'') {
                                i += 1;
                            }
                            out.push((s..i, Token::Attr));
                        }
                    }
                }
                if i < b.len() {
                    out.push((i..i + 1, Token::Punct));
                    i += 1;
                }
            }
        } else if b[i] == b'&' {
            let start = i;
            let end = text[i..].find(|c: char| c == ';' || c == '<' || c.is_whitespace()).map_or(text.len(), |p| i + p);
            i = if b.get(end) == Some(&b';') { end + 1 } else { end.max(i + 1) };
            out.push((start..i, Token::Entity));
        } else {
            let start = i;
            while i < b.len() && b[i] != b'<' && b[i] != b'&' {
                i += 1;
            }
            out.push((start..i, Token::Text));
        }
    }
    out
}

fn config_tokens(text: &str) -> Vec<(std::ops::Range<usize>, Token)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let range = |a: usize, b: usize| offset + a..offset + b;
        if trimmed.starts_with('#') || trimmed.starts_with(';') || trimmed.starts_with("//") {
            out.push((range(0, line.len()), Token::Comment));
        } else if trimmed.starts_with('[') {
            out.push((range(0, line.len()), Token::Tag));
        } else if let Some(eq) = line.find('=') {
            out.push((range(0, indent), Token::Text));
            out.push((range(indent, eq), Token::Attr));
            out.push((range(eq, eq + 1), Token::Punct));
            out.push((range(eq + 1, line.len()), Token::Value));
        } else {
            out.push((range(0, line.len()), Token::Text));
        }
        offset += line.len();
    }
    out
}

fn build(text: &str, syntax: Syntax, dark: bool, font: &FontId, text_color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (range, token) in tokens(text, syntax) {
        if range.is_empty() {
            continue;
        }
        let format = egui::TextFormat::simple(font.clone(), color(token, dark, text_color));
        job.append(&text[range], 0.0, format);
    }
    job
}

/// Where `query` is in `text` (the same letters in either case), as byte ranges.
pub fn find_all(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    // Lower-casing ASCII keeps every byte offset.
    let (haystack, needle) = (text.to_ascii_lowercase(), query.to_ascii_lowercase());
    haystack.match_indices(&needle).map(|(at, m)| at..at + m.len()).collect()
}

/// [`layout`] with the `matches` marked by a background, the `current` one
/// stronger. Not cached: only used while a search is open.
pub fn layout_marked(ui: &egui::Ui, text: &str, syntax: Syntax, matches: &[std::ops::Range<usize>], current: usize) -> LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let dark = ui.visuals().dark_mode;
    let text_color = ui.visuals().text_color();
    let soft = Color32::from_rgba_unmultiplied(255, 214, 0, if dark { 70 } else { 150 });
    let strong = Color32::from_rgba_unmultiplied(255, 140, 0, 190);
    let mut job = LayoutJob::default();
    let mut first = 0;
    for (range, token) in tokens(text, syntax) {
        if range.is_empty() {
            continue;
        }
        let base = egui::TextFormat::simple(font.clone(), color(token, dark, text_color));
        let mut at = range.start;
        // The first match that ends after this token starts.
        while first < matches.len() && matches[first].end <= at {
            first += 1;
        }
        let mut k = first;
        while at < range.end {
            match matches.get(k).filter(|m| m.start < range.end) {
                Some(m) if m.start > at => {
                    job.append(&text[at..m.start], 0.0, base.clone());
                    at = m.start;
                }
                Some(m) => {
                    let end = m.end.min(range.end);
                    let mut format = base.clone();
                    format.background = if k == current { strong } else { soft };
                    job.append(&text[at..end], 0.0, format);
                    at = end;
                    if m.end <= range.end {
                        k += 1;
                    }
                }
                None => {
                    job.append(&text[at..range.end], 0.0, base.clone());
                    at = range.end;
                }
            }
        }
    }
    job
}

#[derive(Default)]
struct Highlighter;

impl egui::cache::ComputerMut<(&str, Syntax, bool, &FontId, Color32), LayoutJob> for Highlighter {
    fn compute(&mut self, (text, syntax, dark, font, text_color): (&str, Syntax, bool, &FontId, Color32)) -> LayoutJob {
        build(text, syntax, dark, font, text_color)
    }
}

/// The highlighted layout of `text`, cached between frames.
pub fn layout(ui: &egui::Ui, text: &str, syntax: Syntax) -> LayoutJob {
    type Cache = egui::cache::FrameCache<LayoutJob, Highlighter>;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let dark = ui.visuals().dark_mode;
    let text_color = ui.visuals().text_color();
    ui.ctx().memory_mut(|m| m.caches.cache::<Cache>().get((text, syntax, dark, &font, text_color)).clone())
}

#[derive(Default)]
struct Tidiness;

impl egui::cache::ComputerMut<&str, bool> for Tidiness {
    fn compute(&mut self, text: &str) -> bool {
        paleto_core::xml::reindent(text) == text
    }
}

/// Whether an XML text is already indented by its structure, cached between frames.
pub fn is_tidy(ctx: &egui::Context, text: &str) -> bool {
    type Cache = egui::cache::FrameCache<bool, Tidiness>;
    ctx.memory_mut(|m| *m.caches.cache::<Cache>().get(text))
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_in_either_case() {
        assert_eq!(find_all("Fire fire FIRE", "fire"), [0..4, 5..9, 10..14]);
        assert_eq!(find_all("aaa", "aa").len(), 1);
        assert_eq!(find_all("aaa", "aa")[0], 0..2);
        assert!(find_all("abc", "").is_empty() && find_all("abc", "x").is_empty());
        // Offsets stay right around non-ASCII text.
        assert_eq!(find_all("é-FIRE", "fire")[0], 3..7);
    }

    use super::*;

    fn kinds(text: &str, syntax: Syntax) -> Vec<(&str, &'static str)> {
        tokens(text, syntax)
            .into_iter()
            .filter(|(r, _)| !r.is_empty())
            .map(|(r, t)| {
                let name = match t {
                    Token::Text => "text",
                    Token::Punct => "punct",
                    Token::Tag => "tag",
                    Token::Attr => "attr",
                    Token::Value => "value",
                    Token::Comment => "comment",
                    Token::Decl => "decl",
                    Token::Entity => "entity",
                };
                (&text[r], name)
            })
            .collect()
    }

    #[test]
    fn xml() {
        let t = r#"<?xml version="1.0"?><!-- c --><Item type="CHandlingData"><fMass value="1.0"/>a &amp; b</Item>"#;
        assert_eq!(
            kinds(t, Syntax::Xml),
            [
                (r#"<?xml version="1.0"?>"#, "decl"),
                ("<!-- c -->", "comment"),
                ("<", "punct"),
                ("Item", "tag"),
                (" ", "text"),
                ("type", "attr"),
                ("=", "punct"),
                (r#""CHandlingData""#, "value"),
                (">", "punct"),
                ("<", "punct"),
                ("fMass", "tag"),
                (" ", "text"),
                ("value", "attr"),
                ("=", "punct"),
                (r#""1.0""#, "value"),
                ("/", "punct"),
                (">", "punct"),
                ("a ", "text"),
                ("&amp;", "entity"),
                (" b", "text"),
                ("</", "punct"),
                ("Item", "tag"),
                (">", "punct"),
            ]
        );
    }

    #[test]
    fn covers_all_text() {
        // Every byte must be emitted exactly once, even for broken input.
        for t in ["<a b='1", "<!-- open", "x < y & z", "<a\n  b = \"c\" >", "è<é attr='ü'>"] {
            let joined: String = tokens(t, Syntax::Xml).into_iter().map(|(r, _)| &t[r]).collect();
            assert_eq!(joined, t);
        }
        let ini = "[s]\nkey = v\n# c\nplain";
        let joined: String = tokens(ini, Syntax::Config).into_iter().map(|(r, _)| &ini[r]).collect();
        assert_eq!(joined, ini);
    }
}
