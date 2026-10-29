//! The journal file of a transaction: one line per record, `%XX`-escaped
//! fields and a checksum at the end of each line. A torn last line (the
//! process died while writing it) is ignored; a damaged line anywhere else
//! makes the whole journal unreadable, and nothing is then deleted on its word.
//!
//! ```text
//! paleto-txn 1
//! begin <txn> <pid> <unix> <label>
//! root game|mods <fingerprint> <path>
//! op <i> mkdir|create <root> <dir> <name>
//! op <i> replace <root> <dir> <name> <expect> link|move
//! op <i> delete <root> <dir> <name> <expect>
//! mode <i> move
//! made <i> <fingerprint>
//! kept <i>
//! commit
//! did <i>
//! committed
//! end ok|rolledback|stuck <reason>
//! ```

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use sha2::{Digest, Sha256};

use crate::fsroot::{Existing, Fingerprint, RootKind};

pub(super) const VERSION: &str = "1";
/// A journal larger than this is not one of ours.
pub(super) const MAX_SIZE: u64 = 16 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpKind {
    Mkdir,
    Create,
    Replace,
    Delete,
}

/// How the original of a replaced file is kept: a hard link made while staging,
/// or (no hard links, as on exFAT) by renaming it aside at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Link,
    Move,
}

#[derive(Debug, Clone)]
pub(super) struct JOp {
    pub kind: OpKind,
    pub anchor: RootKind,
    /// The folder, as link-free names below the root.
    pub dir: Vec<String>,
    pub name: String,
    /// What was at `name` when the operation was planned (replace and delete).
    pub expect: Option<Existing>,
    pub mode: Mode,
    /// The staged file (create, replace) or the folder made (mkdir), as it was when done.
    pub made: Option<Fingerprint>,
    /// A folder that was already there when it was to be made.
    pub kept: bool,
    pub did: bool,
}

#[derive(Debug, Clone)]
pub(super) struct RootRec {
    pub kind: RootKind,
    pub fp: Fingerprint,
    pub shown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum End {
    Ok,
    RolledBack,
    Stuck(String),
}

#[derive(Debug)]
pub(super) struct Journal {
    pub txn: String,
    pub pid: u32,
    pub started: u64,
    pub label: String,
    pub roots: Vec<RootRec>,
    pub ops: Vec<JOp>,
    pub commit: bool,
    pub committed: bool,
    pub end: Option<End>,
    /// Where the last whole record ends in the file (what follows is a torn record, to be overwritten).
    pub good_len: u64,
    /// That record has no newline yet: one is written before the next record.
    pub open_line: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// Finished and only waiting to be deleted.
    Done,
    /// Past `committed`: the new files are in place, only backups are left.
    Committed,
    /// Neither: roll back.
    Interrupted,
    /// A rollback that could not finish, kept for another try.
    Stuck,
}

impl Journal {
    pub fn state(&self) -> State {
        match &self.end {
            Some(End::Ok | End::RolledBack) => State::Done,
            Some(End::Stuck(_)) => State::Stuck,
            None if self.committed => State::Committed,
            None => State::Interrupted,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ParseError {
    /// Nothing was ever recorded (the header or `begin` is torn): no effect to undo.
    Empty,
    /// Written by a version of this program that this one cannot read.
    Version(String),
    Corrupt(String),
}

// ---- writing ----

fn sum(body: &str) -> String {
    let d = Sha256::digest(body.as_bytes());
    format!("{:02x}{:02x}{:02x}{:02x}", d[0], d[1], d[2], d[3])
}

fn seal(body: String) -> String {
    format!("{body} ~{}\n", sum(&body))
}

/// A field as it is written: bytes outside `!`..`~`, and `%`, as `%XX`; nothing at all as a lone `%`.
fn esc(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "%".into();
    }
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x21..=0x7E).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unesc(field: &str) -> Option<Vec<u8>> {
    if field == "%" {
        return Some(Vec::new());
    }
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

fn fp_enc(fp: &Fingerprint) -> String {
    let hex: String = fp.head_tail.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}.{}.{}.{}.{hex}", fp.dev, fp.ino, fp.size, fp.mtime_ns)
}

fn fp_dec(s: &str) -> Option<Fingerprint> {
    let mut parts = s.split('.');
    let (dev, ino, size, mtime_ns, hex) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || hex.len() != 64 {
        return None;
    }
    let mut head_tail = [0u8; 32];
    for (i, b) in head_tail.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(Fingerprint { dev: dev.parse().ok()?, ino: ino.parse().ok()?, size: size.parse().ok()?, mtime_ns: mtime_ns.parse().ok()?, head_tail })
}

fn expect_enc(e: &Option<Existing>) -> String {
    match e {
        Some(Existing::File(fp)) => format!("f:{}", fp_enc(fp)),
        Some(Existing::Link(target)) => format!("l:{}", esc(target.as_bytes())),
        _ => "-".into(),
    }
}

fn expect_dec(s: &str) -> Option<Option<Existing>> {
    if s == "-" {
        return Some(None);
    }
    match s.split_at_checked(2)? {
        ("f:", fp) => Some(Some(Existing::File(fp_dec(fp)?))),
        ("l:", target) => Some(Some(Existing::Link(OsString::from_vec(unesc(target)?)))),
        _ => None,
    }
}

fn root_enc(kind: RootKind) -> &'static str {
    match kind {
        RootKind::Game => "game",
        RootKind::Mods => "mods",
    }
}

fn root_dec(s: &str) -> Option<RootKind> {
    match s {
        "game" => Some(RootKind::Game),
        "mods" => Some(RootKind::Mods),
        _ => None,
    }
}

pub(super) fn header() -> String {
    seal(format!("paleto-txn {VERSION}"))
}

pub(super) fn begin(txn: &str, pid: u32, unix: u64, label: &str) -> String {
    seal(format!("begin {txn} {pid} {unix} {}", esc(label.as_bytes())))
}

pub(super) fn root(rec: &RootRec) -> String {
    seal(format!("root {} {} {}", root_enc(rec.kind), fp_enc(&rec.fp), esc(rec.shown.as_bytes())))
}

pub(super) fn op(i: usize, op: &JOp) -> String {
    let (dir, name) = (esc(op.dir.join("/").as_bytes()), esc(op.name.as_bytes()));
    let r = root_enc(op.anchor);
    seal(match op.kind {
        OpKind::Mkdir => format!("op {i} mkdir {r} {dir} {name}"),
        OpKind::Create => format!("op {i} create {r} {dir} {name}"),
        OpKind::Replace => format!("op {i} replace {r} {dir} {name} {} {}", expect_enc(&op.expect), if op.mode == Mode::Link { "link" } else { "move" }),
        OpKind::Delete => format!("op {i} delete {r} {dir} {name} {}", expect_enc(&op.expect)),
    })
}

pub(super) fn mode_move(i: usize) -> String {
    seal(format!("mode {i} move"))
}

pub(super) fn made(i: usize, fp: &Fingerprint) -> String {
    seal(format!("made {i} {}", fp_enc(fp)))
}

pub(super) fn kept(i: usize) -> String {
    seal(format!("kept {i}"))
}

pub(super) fn commit() -> String {
    seal("commit".into())
}

pub(super) fn did(i: usize) -> String {
    seal(format!("did {i}"))
}

pub(super) fn committed() -> String {
    seal("committed".into())
}

pub(super) fn end(end: &End) -> String {
    seal(match end {
        End::Ok => "end ok".into(),
        End::RolledBack => "end rolledback".into(),
        End::Stuck(why) => format!("end stuck {}", esc(why.as_bytes())),
    })
}

// ---- reading ----

/// The fields of a line whose checksum is right.
fn fields(raw: &[u8]) -> Option<Vec<&str>> {
    let line = std::str::from_utf8(raw).ok()?;
    let (body, tail) = line.rsplit_once(" ~")?;
    (tail == sum(body)).then(|| body.split(' ').collect())
}

fn text(field: &str) -> Result<String, String> {
    String::from_utf8(unesc(field).ok_or("a field is not escaped properly")?).map_err(|_| "a field is not text".into())
}

fn name(field: &str) -> Result<String, String> {
    let n = text(field)?;
    if n.is_empty() || n == "." || n == ".." || n.contains('/') {
        return Err(format!("{n:?} is not the name of one entry"));
    }
    Ok(n)
}

fn dir(field: &str) -> Result<Vec<String>, String> {
    let d = text(field)?;
    if d.is_empty() {
        return Ok(Vec::new());
    }
    d.split('/').map(|n| name(&esc(n.as_bytes()))).collect()
}

fn number<T: std::str::FromStr>(field: &str) -> Result<T, String> {
    field.parse().map_err(|_| format!("{field:?} is not a number"))
}

pub(super) fn parse(bytes: &[u8]) -> Result<Journal, ParseError> {
    let corrupt = |n: usize, why: String| ParseError::Corrupt(format!("record {n}: {why}"));
    let mut pieces: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    // What follows the last newline is a line that was being written.
    let tail = pieces.pop().unwrap_or_default();
    let open_tail = !tail.is_empty();
    if open_tail {
        pieces.push(tail);
    }
    if let Some(first) = pieces.first()
        && let Some(rest) = first.strip_prefix(b"paleto-txn ")
    {
        let version = String::from_utf8_lossy(rest.split(|b| *b == b' ').next().unwrap_or_default()).into_owned();
        if version != VERSION {
            return Err(ParseError::Version(version));
        }
    }
    let mut j = Journal { txn: String::new(), pid: 0, started: 0, label: String::new(), roots: Vec::new(), ops: Vec::new(), commit: false, committed: false, end: None, good_len: 0, open_line: false };
    let (mut began, mut seen, mut pos) = (false, 0, 0u64);
    for (n, raw) in pieces.iter().enumerate() {
        let n1 = n + 1;
        let Some(f) = fields(raw) else {
            // Only the last record can be a torn one.
            if n + 1 == pieces.len() {
                break;
            }
            return Err(corrupt(n1, "damaged (its checksum does not match)".into()));
        };
        seen += 1;
        // Where the last whole record ends: what is written next goes there, over any torn bytes after it.
        let last_open = open_tail && n + 1 == pieces.len();
        pos += raw.len() as u64 + u64::from(!last_open);
        (j.good_len, j.open_line) = (pos, last_open);
        if n == 0 {
            if f != ["paleto-txn", VERSION] {
                return Err(corrupt(n1, "not a journal header".into()));
            }
            continue;
        }
        let bad = |why: String| corrupt(n1, why);
        let index = |s: &str, j: &Journal| -> Result<usize, ParseError> {
            let i: usize = number(s).map_err(bad)?;
            if i < j.ops.len() { Ok(i) } else { Err(bad(format!("operation {i} was never announced"))) }
        };
        match (f[0], f.len()) {
            ("begin", 5) if !began => {
                let txn = f[1];
                if txn.len() != 16 || !txn.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                    return Err(bad("the transaction id is not 16 hex digits".into()));
                }
                (j.txn, j.pid, j.started, j.label) = (txn.to_string(), number(f[2]).map_err(bad)?, number(f[3]).map_err(bad)?, text(f[4]).map_err(bad)?);
                began = true;
            }
            _ if !began => return Err(bad("a record before `begin`".into())),
            ("root", 4) => {
                let kind = root_dec(f[1]).ok_or_else(|| bad("unknown root".into()))?;
                j.roots.push(RootRec { kind, fp: fp_dec(f[2]).ok_or_else(|| bad("bad fingerprint".into()))?, shown: text(f[3]).map_err(bad)? });
            }
            ("op", 6..=8) => {
                let i: usize = number(f[1]).map_err(bad)?;
                if i != j.ops.len() {
                    return Err(bad(format!("operation {i} out of order")));
                }
                let anchor = root_dec(f[3]).ok_or_else(|| bad("unknown root".into()))?;
                let (d, nm) = (dir(f[4]).map_err(bad)?, name(f[5]).map_err(bad)?);
                let mut o = JOp { kind: OpKind::Mkdir, anchor, dir: d, name: nm, expect: None, mode: Mode::Move, made: None, kept: false, did: false };
                match (f[2], f.len()) {
                    ("mkdir", 6) => {}
                    ("create", 6) => o.kind = OpKind::Create,
                    ("replace", 8) => {
                        o.kind = OpKind::Replace;
                        o.expect = expect_dec(f[6]).flatten();
                        o.mode = match f[7] {
                            "link" => Mode::Link,
                            "move" => Mode::Move,
                            _ => return Err(bad("unknown mode".into())),
                        };
                        if o.expect.is_none() {
                            return Err(bad("a replace without what it replaces".into()));
                        }
                    }
                    ("delete", 7) => {
                        o.kind = OpKind::Delete;
                        o.expect = expect_dec(f[6]).flatten();
                        if o.expect.is_none() {
                            return Err(bad("a delete without what it deletes".into()));
                        }
                    }
                    _ => return Err(bad("unknown operation".into())),
                }
                j.ops.push(o);
            }
            ("mode", 3) if f[2] == "move" => {
                let i = index(f[1], &j)?;
                j.ops[i].mode = Mode::Move;
            }
            ("made", 3) => {
                let i = index(f[1], &j)?;
                j.ops[i].made = Some(fp_dec(f[2]).ok_or_else(|| bad("bad fingerprint".into()))?);
            }
            ("kept", 2) => {
                let i = index(f[1], &j)?;
                j.ops[i].kept = true;
            }
            ("commit", 1) => j.commit = true,
            ("did", 2) => {
                let i = index(f[1], &j)?;
                j.ops[i].did = true;
            }
            ("committed", 1) => j.committed = true,
            ("end", 2) if f[1] == "ok" => j.end = Some(End::Ok),
            ("end", 2) if f[1] == "rolledback" => j.end = Some(End::RolledBack),
            ("end", 2..=3) if f[1] == "stuck" => j.end = Some(End::Stuck(f.get(2).map(|r| text(r)).transpose().map_err(bad)?.unwrap_or_default())),
            _ => return Err(bad(format!("unknown record {:?}", f[0]))),
        }
    }
    if !began {
        return Err(if seen <= 1 { ParseError::Empty } else { ParseError::Corrupt("no `begin` record".into()) });
    }
    Ok(j)
}
