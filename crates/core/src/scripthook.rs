//! Script Hook V: finding the latest release, the installed and game versions, and updating the DLL.

use std::io::{Read, Write};
use std::time::Duration;

use crate::game::resolve_ci;
use crate::loader::{HookStatus, script_hook};
use crate::relpath::RelPath;
use crate::{Edition, Error, Game};

const PAGE_URL: &str = "https://www.dev-c.com/gtav/scripthookv/";
const FILES_URL: &str = "https://www.dev-c.com/files/";
// The server answers 406 (mod_security) to requests that do not look like a browser.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
const MAX_ZIP: u64 = 20 << 20;
/// The most of a PE file's resource section that is read.
const MAX_RESOURCES: u64 = 256 << 20;
const DLL_NAME: &str = "ScriptHookV.dll";

/// A Script Hook V release listed on the author's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// "3889.0/1158.13": the Legacy and Enhanced builds it supports.
    pub version: String,
    pub url: String,
    /// The supported patches ("1.0.3889.0 / 1.0.1158.13"), empty when the page does not say.
    pub supports: String,
}

/// How well the installed Script Hook V suits the installed game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compat {
    /// Its log says it started on this game.
    Confirmed,
    /// Its version is not older than the game's (inferred, not seen working).
    Likely,
    /// Its log says it failed, or it is older than the game.
    Incompatible { supports: String, game: String },
    /// Not installed, or a version is missing.
    Unknown,
}

impl Compat {
    /// One line for the user.
    pub fn describe(&self) -> String {
        match self {
            Compat::Confirmed => "Works with this game version".into(),
            Compat::Likely => "Should work with this game version".into(),
            Compat::Incompatible { supports, game } => format!("Supports {supports}, the game is {game}: update Script Hook V"),
            Compat::Unknown => "Compatibility unknown".into(),
        }
    }
}

fn numbers(s: &str) -> Vec<u32> {
    s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect()
}

/// Whether version `a` ("3889.0/1158.13", "v3889.0") is newer than `b`, comparing numbers component-wise.
pub fn newer(a: &str, b: &str) -> bool {
    numbers(a) > numbers(b)
}

/// Finds the Script Hook V download (not the SDK) in the release page.
pub fn parse_release_page(html: &str) -> Option<Release> {
    const START: &str = "/files/ScriptHookV_";
    let mut rest = html;
    while let Some(i) = rest.find(START) {
        let tail = &rest[i + START.len()..];
        rest = tail;
        let Some(end) = tail.find(".zip") else { continue };
        let stem = &tail[..end];
        if stem.contains("SDK") || stem.contains(['"', '<', '>', ' ']) {
            continue;
        }
        let Some((x, y)) = stem.split_once('_') else { continue };
        if numbers(x).is_empty() || numbers(y).is_empty() {
            continue;
        }
        let supports = html.find("Supported patches").map(|p| &html[p..]).and_then(|t| {
            let text = t.split('>').skip(1).map(|s| s.split('<').next().unwrap_or("").trim()).find(|s| s.starts_with("1.0."))?;
            Some(text.to_string())
        });
        return Some(Release { version: format!("{x}/{y}"), url: format!("{FILES_URL}ScriptHookV_{stem}.zip"), supports: supports.unwrap_or_default() });
    }
    None
}

/// Reads the latest release from the author's page.
pub fn latest() -> Result<Release, Error> {
    let html = ureq::get(PAGE_URL)
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .header("User-Agent", USER_AGENT)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.5")
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| Error::Download(e.to_string()))?;
    parse_release_page(&html).ok_or_else(|| Error::Download("no Script Hook V download on the page".into()))
}

/// The version inside a Script Hook V DLL ("3889.0/1158.13"), found as the text `v3889.0/1158.13`.
fn dll_version(bytes: &[u8]) -> Option<String> {
    let digits = |from: usize| bytes[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    for i in bytes.iter().enumerate().filter(|(_, b)| **b == b'v').map(|(i, _)| i) {
        let mut p = i + 1;
        let mut ok = true;
        for sep in *b"./." {
            let n = digits(p);
            if n == 0 || bytes.get(p + n) != Some(&sep) {
                ok = false;
                break;
            }
            p += n + 1;
        }
        let n = digits(p);
        if ok && n > 0 {
            return Some(String::from_utf8_lossy(&bytes[i + 1..p + n]).into_owned());
        }
    }
    None
}

/// The installed Script Hook V version, or `None` when there is no DLL or no version in it.
pub fn installed(game: &Game) -> Option<String> {
    dll_version(&std::fs::read(resolve_ci(&game.root, DLL_NAME)).ok()?)
}

/// The game executable's file version (1.0.1158.16 as `[1, 0, 1158, 16]`).
pub fn game_version(game: &Game) -> Option<[u16; 4]> {
    let mut file = std::fs::File::open(game.root.join(game.edition.exe_name())).ok()?;
    let bytes = resource_section(&mut file)?;
    let sig = 0xFEEF04BDu32.to_le_bytes();
    let at = bytes.windows(4).position(|w| w == sig)?;
    let field = |o: usize| bytes.get(at + o..at + o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    // After the signature and the structure version: file version MS (minor, major) and LS (revision, build), each a little-endian pair.
    Some([field(10)?, field(8)?, field(14)?, field(12)?])
}

/// The `.rsrc` section of a PE file, where the version block is: the executable is tens of MB,
/// the section a few. A file with no section table is read whole.
fn resource_section(file: &mut (impl Read + std::io::Seek)) -> Option<Vec<u8>> {
    use std::io::SeekFrom;
    let u16_at = |b: &[u8], o: usize| b.get(o..o + 2).map(|v| u16::from_le_bytes([v[0], v[1]]) as usize);
    let u32_at = |b: &[u8], o: usize| b.get(o..o + 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as u64);
    let mut head = vec![0u8; 4096];
    let n = file.read(&mut head).ok()?;
    head.truncate(n);
    let section = (|| {
        let pe = u32_at(&head, 0x3C)? as usize;
        let (count, optional) = (u16_at(&head, pe + 6)?, u16_at(&head, pe + 20)?);
        let table = pe + 24 + optional;
        (0..count).find_map(|i| {
            let entry = head.get(table + i * 40..table + i * 40 + 40)?;
            (entry.starts_with(b".rsrc\0")).then(|| Some((u32_at(entry, 20)?, u32_at(entry, 16)?)))?
        })
    })();
    file.seek(SeekFrom::Start(section.map_or(0, |(offset, _)| offset))).ok()?;
    let mut bytes = Vec::new();
    match section {
        Some((_, size)) => file.take(size.min(MAX_RESOURCES)).read_to_end(&mut bytes).ok()?,
        None => file.read_to_end(&mut bytes).ok()?,
    };
    Some(bytes)
}

/// Combines the known facts. `shv` is "3889.0/1158.13"; the edition's build is the part before
/// ('Legacy') or after ('Enhanced') the slash, compared with the game's third version number.
pub fn judge(edition: Edition, shv: Option<&str>, game: Option<[u16; 4]>, status: Option<&HookStatus>, stale: bool) -> Compat {
    let describe_game = game.map(|g| format!("{}.{}.{}.{}", g[0], g[1], g[2], g[3])).unwrap_or_default();
    let supports = shv.unwrap_or_default().to_string();
    let incompatible = || Compat::Incompatible { supports: supports.clone(), game: describe_game.clone() };
    if !stale {
        match status {
            Some(HookStatus::Ok(_)) => return Compat::Confirmed,
            Some(HookStatus::Failed(_)) => return incompatible(),
            _ => {}
        }
    }
    let (Some(shv), Some(game)) = (shv, game) else { return Compat::Unknown };
    let (legacy, enhanced) = shv.split_once('/').unwrap_or((shv, ""));
    let part = if edition == Edition::Legacy { legacy } else { enhanced };
    let Some(&supported) = numbers(part).first() else { return Compat::Unknown };
    if supported < u32::from(game[2]) { incompatible() } else { Compat::Likely }
}

/// How well the installed Script Hook V suits this game.
pub fn compat(game: &Game) -> Compat {
    let hook = script_hook(game);
    judge(game.edition, installed(game).as_deref(), game_version(game), hook.as_ref().map(|h| &h.status), hook.as_ref().is_some_and(|h| h.stale))
}

/// Replaces `ScriptHookV.dll` in the game folder with the one in a release zip, and nothing else
/// from it. Returns the installed version.
pub fn install_from_zip(game: &Game, zip: &[u8]) -> Result<String, Error> {
    let bad = |m: &str| Error::Package(m.to_string());
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| Error::Package(e.to_string()))?;
    let index = (0..archive.len())
        .find(|&i| archive.name_for_index(i).and_then(Result::ok).is_some_and(|n| n.rsplit(['/', '\\']).next().is_some_and(|f| f.eq_ignore_ascii_case(DLL_NAME))))
        .ok_or_else(|| bad("no ScriptHookV.dll in the zip"))?;
    let mut dll = Vec::new();
    archive.by_index(index).map_err(|e| Error::Package(e.to_string()))?.take(MAX_ZIP + 1).read_to_end(&mut dll)?;
    if dll.len() as u64 > MAX_ZIP {
        return Err(bad("ScriptHookV.dll is too big"));
    }
    if !dll.starts_with(b"MZ") {
        return Err(bad("ScriptHookV.dll is not a Windows DLL"));
    }
    let version = dll_version(&dll).ok_or_else(|| bad("no version in ScriptHookV.dll"))?;
    let roots = game.roots()?;
    let mut target = roots.dest(&RelPath::parse(DLL_NAME)?)?;
    roots.replace(&mut target, |f| f.write_all(&dll))?;
    Ok(version)
}

/// Downloads a release and installs its DLL. Returns the installed version.
pub fn update(game: &Game, release: &Release) -> Result<String, Error> {
    let data = ureq::get(&release.url)
        .config()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .header("User-Agent", USER_AGENT)
        .header("Referer", PAGE_URL)
        .call()
        .and_then(|mut r| r.body_mut().with_config().limit(MAX_ZIP).read_to_vec())
        .map_err(|e| Error::Download(e.to_string()))?;
    if !data.starts_with(b"PK") {
        return Err(Error::Download("the download is not a zip".into()));
    }
    install_from_zip(game, &data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PAGE: &str = r#"<table><tr><td>Version</td><td>v3889.0 / 1158.13</td><td>Supported patches</td><td>1.0.3889.0 / 1.0.1158.13</td><td>Download</td>
        <td><a href="/files/ScriptHookV_SDK_1.0.617.1a.zip">SDK</a></td>
        <td><a href="/files/ScriptHookV_3889.0_1158.13.zip">Download</a></td></tr></table>"#;

    fn game_dir(edition: Edition) -> (tempfile::TempDir, Game) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(edition.exe_name()), b"").unwrap();
        let game = Game::open(dir.path()).unwrap();
        (dir, game)
    }

    fn fake_dll(version: &str) -> Vec<u8> {
        let mut d = b"MZ\0\0junk".to_vec();
        d.extend_from_slice(format!("v{version}\0").as_bytes());
        d
    }

    fn pe_blob(v: [u16; 4]) -> Vec<u8> {
        let mut b = vec![0u8; 100];
        b.extend_from_slice(&0xFEEF04BDu32.to_le_bytes());
        b.extend_from_slice(&0x10000u32.to_le_bytes());
        b.extend_from_slice(&((u32::from(v[0]) << 16) | u32::from(v[1])).to_le_bytes());
        b.extend_from_slice(&((u32::from(v[2]) << 16) | u32::from(v[3])).to_le_bytes());
        b
    }

    #[test]
    fn page_parsing() {
        let r = parse_release_page(PAGE).unwrap();
        assert_eq!(r.version, "3889.0/1158.13");
        assert_eq!(r.url, "https://www.dev-c.com/files/ScriptHookV_3889.0_1158.13.zip");
        assert_eq!(r.supports, "1.0.3889.0 / 1.0.1158.13");
        assert!(parse_release_page(r#"<a href="/files/ScriptHookV_SDK_1.0.617.1a.zip">"#).is_none());
        assert!(parse_release_page("nothing").is_none());
    }

    #[test]
    fn version_order() {
        assert!(newer("3889.0/1158.13", "3788.0/1013.33"));
        assert!(newer("v3889.0/1158.14", "3889.0/1158.13"));
        assert!(newer("v3889.1", "v3889.0"));
        assert!(!newer("3889.0/1158.13", "v3889.0/1158.13"));
        assert!(!newer("3889.0/1158.9", "3889.0/1158.13"));
    }

    #[test]
    fn installed_version() {
        let (dir, game) = game_dir(Edition::Enhanced);
        assert_eq!(installed(&game), None);
        std::fs::write(dir.path().join("scripthookv.dll"), fake_dll("3889.0/1158.13")).unwrap();
        assert_eq!(installed(&game).as_deref(), Some("3889.0/1158.13"));
        std::fs::write(dir.path().join("scripthookv.dll"), b"MZ no version v1.2 here").unwrap();
        assert_eq!(installed(&game), None);
    }

    #[test]
    fn exe_version() {
        let (dir, game) = game_dir(Edition::Enhanced);
        assert_eq!(game_version(&game), None);
        std::fs::write(dir.path().join("GTA5_Enhanced.exe"), pe_blob([1, 0, 1158, 16])).unwrap();
        assert_eq!(game_version(&game), Some([1, 0, 1158, 16]));
    }

    #[test]
    fn exe_version_from_the_resource_section() {
        let (dir, game) = game_dir(Edition::Enhanced);
        // A PE header with one .rsrc section; a decoy signature sits before it and must not be read.
        let mut exe = vec![0u8; 0x200];
        exe[0x3C] = 0x80;
        let pe = 0x80;
        exe[pe..pe + 4].copy_from_slice(b"PE\0\0");
        exe[pe + 6] = 1;
        exe[pe + 20] = 0xE0;
        let entry = pe + 24 + 0xE0;
        exe[entry..entry + 5].copy_from_slice(b".rsrc");
        let block = pe_blob([1, 0, 1158, 16]);
        exe[entry + 16..entry + 20].copy_from_slice(&(block.len() as u32).to_le_bytes());
        exe[entry + 20..entry + 24].copy_from_slice(&0x400u32.to_le_bytes());
        exe.resize(0x400, 0);
        exe[0x300..0x304].copy_from_slice(&0xFEEF04BDu32.to_le_bytes());
        exe.extend_from_slice(&block);
        std::fs::write(dir.path().join("GTA5_Enhanced.exe"), &exe).unwrap();
        assert_eq!(game_version(&game), Some([1, 0, 1158, 16]));
    }

    #[test]
    fn compat_table() {
        let shv = Some("3889.0/1158.13");
        let ok = HookStatus::Ok("VER_EN_1_0_1158_13".into());
        let failed = HookStatus::Failed("unknown game version".into());
        let j = |e, g, s: Option<&HookStatus>, stale| judge(e, shv, Some(g), s, stale);
        assert_eq!(j(Edition::Enhanced, [1, 0, 1158, 16], None, false), Compat::Likely);
        assert_eq!(j(Edition::Enhanced, [1, 0, 1158, 13], Some(&HookStatus::NotRun), false), Compat::Likely);
        assert_eq!(j(Edition::Legacy, [1, 0, 3889, 0], None, false), Compat::Likely);
        assert_eq!(j(Edition::Legacy, [1, 0, 3000, 0], None, false), Compat::Likely);
        assert_eq!(
            j(Edition::Enhanced, [1, 0, 1172, 0], None, false),
            Compat::Incompatible { supports: "3889.0/1158.13".into(), game: "1.0.1172.0".into() }
        );
        assert!(matches!(j(Edition::Legacy, [1, 0, 4000, 0], None, false), Compat::Incompatible { .. }));
        assert_eq!(j(Edition::Enhanced, [1, 0, 1172, 0], Some(&ok), false), Compat::Confirmed);
        assert!(matches!(j(Edition::Enhanced, [1, 0, 1158, 13], Some(&failed), false), Compat::Incompatible { .. }));
        assert!(matches!(j(Edition::Enhanced, [1, 0, 1172, 0], Some(&ok), true), Compat::Incompatible { .. }));
        assert_eq!(judge(Edition::Enhanced, None, Some([1, 0, 1158, 16]), None, false), Compat::Unknown);
        assert_eq!(judge(Edition::Enhanced, shv, None, None, false), Compat::Unknown);
        assert!(!Compat::Likely.describe().is_empty());
    }

    #[test]
    fn install_only_the_dll() {
        let (dir, game) = game_dir(Edition::Enhanced);
        std::fs::write(dir.path().join("scripthookv.dll"), fake_dll("3788.0/1013.33")).unwrap();
        std::fs::write(dir.path().join("dinput8.dll"), b"old dinput").unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in [("bin/ScriptHookV.dll", fake_dll("3889.0/1158.13")), ("bin/dinput8.dll", b"new dinput".to_vec()), ("bin/NativeTrainer.asi", b"x".to_vec()), ("Readme.txt", b"x".to_vec())] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&data).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        assert_eq!(install_from_zip(&game, &bytes).unwrap(), "3889.0/1158.13");
        assert_eq!(installed(&game).as_deref(), Some("3889.0/1158.13"));
        assert_eq!(std::fs::read(dir.path().join("dinput8.dll")).unwrap(), b"old dinput");
        let mut names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["GTA5_Enhanced.exe", "dinput8.dll", "scripthookv.dll"]);
        assert!(install_from_zip(&game, b"not a zip").is_err());
    }
}
