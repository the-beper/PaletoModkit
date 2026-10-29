//! A relative path from a package or a caller (untrusted), checked once and
//! kept as its components. The game runs with Windows rules (also under Wine
//! and Proton), so a name Windows cannot open is never a legitimate target.

use std::fmt;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelPath(Vec<String>);

const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

impl RelPath {
    /// Splits on `/` and `\`, skipping empty parts and `.` (a single leading
    /// separator means "from the game folder", as OpenIV packages write it), and
    /// refuses what could leave the folder or name a special file.
    pub fn parse(path: &str) -> Result<Self, Error> {
        let bad = |why: &str| Error::UnsafePath { path: path.to_string(), why: why.to_string() };
        if path.len() > 4096 {
            return Err(bad("longer than 4096 bytes"));
        }
        let mut chars = path.chars();
        if chars.next().is_some_and(is_sep) && chars.next().is_some_and(is_sep) {
            return Err(bad("a network or device path"));
        }
        let mut parts = Vec::new();
        for part in path.split(is_sep).filter(|p| !p.is_empty() && *p != ".") {
            check(part).map_err(|why| bad(&format!("\"{part}\" {why}")))?;
            parts.push(part.to_string());
        }
        if parts.is_empty() {
            return Err(bad("names nothing"));
        }
        if parts.len() > 64 {
            return Err(bad("more than 64 folders deep"));
        }
        Ok(RelPath(parts))
    }

    pub fn parts(&self) -> &[String] {
        &self.0
    }

    pub fn name(&self) -> &str {
        self.0.last().map_or("", String::as_str)
    }

    /// Whether the game streams this from the mods folder: archives, `update/` and `x64/`.
    pub fn is_game_data(&self) -> bool {
        is_game_data_parts(&self.0)
    }

    /// The path inside the mods folder for one that starts with `mods/` (see
    /// [`RelPath::in_mods_folder`]); any other path as it is.
    pub fn without_mods(&self) -> RelPath {
        if self.in_mods_folder() { RelPath(self.0[1..].to_vec()) } else { self.clone() }
    }

    /// Whether the first folder is `mods`: the path is then inside the mods folder.
    pub fn in_mods_folder(&self) -> bool {
        self.0.len() > 1 && self.0[0].eq_ignore_ascii_case("mods")
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("/"))
    }
}

/// [`RelPath::is_game_data`] for components already known to be clean.
pub fn is_game_data_parts(parts: &[String]) -> bool {
    let first = parts.first().map_or(String::new(), |p| p.to_ascii_lowercase());
    first == "update" || first == "x64" || parts.last().is_some_and(|p| p.to_ascii_lowercase().ends_with(".rpf"))
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

fn check(part: &str) -> Result<(), &'static str> {
    if part == ".." {
        return Err("goes up a folder");
    }
    if part.len() > 255 {
        return Err("is longer than 255 bytes");
    }
    if part.contains(':') {
        return Err("names a drive or a data stream");
    }
    if part.chars().any(|c| (c as u32) < 0x20 || c == '\x7f' || "<>\"|?*".contains(c)) {
        return Err("has a character Windows does not allow");
    }
    if part.ends_with(['.', ' ']) {
        return Err("ends with a dot or a space");
    }
    if is_device(part) {
        return Err("is a Windows device name");
    }
    if part.as_bytes().windows(2).any(|w| w[0] == b'~' && w[1].is_ascii_digit()) {
        return Err("looks like a short (8.3) name");
    }
    if part.len() >= 8 && part.as_bytes()[..8].eq_ignore_ascii_case(b".paleto-") {
        return Err("is reserved for this tool's temporary files");
    }
    Ok(())
}

/// `CON`, `NUL`, `COM1`, `LPT3`, `CONIN$`... with any extension, in any case.
fn is_device(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    if DEVICES.contains(&stem.as_str()) {
        return true;
    }
    let digit = |s: &str| s.chars().count() == 1 && s.chars().all(|c| c.is_ascii_digit() || "¹²³".contains(c));
    stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")).is_some_and(digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_what_packages_write() {
        for (path, parts) in [
            ("scripts\\a.asi", &["scripts", "a.asi"][..]),
            ("\\scripts\\.\\a..b.asi", &["scripts", "a..b.asi"]),
            ("/x64/./deep//er/f.bin", &["x64", "deep", "er", "f.bin"]),
            ("update\\x64\\dlcpacks\\car\\dlc.rpf", &["update", "x64", "dlcpacks", "car", "dlc.rpf"]),
            ("a b\\.hidden\\caf\u{e9}.txt", &["a b", ".hidden", "caf\u{e9}.txt"]),
            ("console.txt", &["console.txt"]),
            ("com10.txt", &["com10.txt"]),
            ("a~b.txt", &["a~b.txt"]),
            ("paleto-x", &["paleto-x"]),
        ] {
            assert_eq!(RelPath::parse(path).unwrap().parts(), parts, "{path}");
        }
    }

    #[test]
    fn refuses_what_could_leave_or_name_a_special_file() {
        let long = "a".repeat(256);
        let deep = vec!["d"; 65].join("/");
        let huge = format!("{}x", "a/".repeat(2100));
        for path in [
            "", ".", "./", "\\", "..", "a/../b", "..\\..\\evil.asi", "scripts/..", "\\\\server\\share\\a", "//server/a", "\\\\?\\C:\\a", "\\\\.\\pipe\\x",
            "C:\\Windows\\a.dll", "c:a", "a.asi:x", "a.asi:x:$DATA", "scripts\\a:b\\c",
            "con", "CON.txt", "scripts\\nul", "aux.cfg", "PRN", "com1", "LPT9.log", "COM\u{b2}", "conin$", "CONOUT$.x", "con .txt",
            "a.", "a ", "scripts\\a.asi.", "scripts\\a.asi ", "dir.\\x",
            ".paleto-x", "scripts\\.PALETO-1234.new", "MODS_S~1", "x\\prog~1.exe",
            "a\u{1}b", "a\0b", "a\x7fb", "a\tb", "a\nb", "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b",
            long.as_str(), deep.as_str(), huge.as_str(),
        ] {
            let err = RelPath::parse(path).unwrap_err().to_string();
            assert!(err.starts_with("unsafe path"), "{path:?}: {err}");
        }
    }

    #[test]
    fn routing_helpers() {
        let rel = |p: &str| RelPath::parse(p).unwrap();
        assert!(rel("x64\\a.bin").is_game_data() && rel("UPDATE/x").is_game_data() && rel("plugins/a.RPF").is_game_data());
        assert!(!rel("scripts/a.asi").is_game_data() && !rel("x64e.dat").is_game_data());
        assert!(rel("Mods/x").in_mods_folder() && !rel("mods").in_mods_folder() && !rel("scripts/mods/x").in_mods_folder());
        assert_eq!(rel("a\\b//c").to_string(), "a/b/c");
        assert_eq!(rel("a/b.txt").name(), "b.txt");
    }
}
