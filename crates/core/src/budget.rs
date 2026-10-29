//! Limits for reading archives that come from somebody else (a package, a zip).
//!
//! A header that says how big a file is proves nothing: what counts is what
//! the reader yields. [`Budget::read`] and [`Budget::copy`] never read more
//! than the header declared (and never more than a limit), count the bytes they
//! really got, stop a file that expands past the ratio the format allows, and fail
//! on a file that holds more or less than it declared. [`zip_precheck`] reads the
//! end records of a zip before a zip reader allocates anything for its list of
//! files. Nothing here knows what the archive is for.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Error;

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// What the first allocation for a file may be, whatever its header says.
const FIRST_ALLOC: u64 = MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The file that describes an archive (OIV: `assembly.xml`).
    pub manifest: u64,
    /// The picture of an archive.
    pub icon: u64,
    /// Files in the archive.
    pub entries: u64,
    /// Size of the archive's list of files, from its end record.
    pub central_dir: u64,
    /// One file held in memory.
    pub in_memory: u64,
    /// All the files held in memory, together.
    pub in_memory_total: u64,
    /// One file copied to disk without being held.
    pub streamed: u64,
    /// Every byte read, over one operation.
    pub total: u64,
    /// Largest expansion (bytes read over bytes stored) a file may have; none if the format has no such ratio to check.
    pub ratio: Option<u64>,
    /// A file is judged by its ratio only once it has yielded more than this (small text files compress well).
    pub ratio_after: u64,
}

impl Limits {
    /// For a mod package: far above any real one, far below what takes a machine down.
    pub const PACKAGE: Limits = Limits {
        manifest: 16 * MIB,
        icon: 4 * MIB,
        entries: 100_000,
        central_dir: 64 * MIB,
        in_memory: GIB,
        in_memory_total: 4 * GIB,
        streamed: 8 * GIB,
        total: 64 * GIB,
        ratio: Some(200),
        ratio_after: 16 * MIB,
    };
}

/// A size as people write it: "16 MiB".
pub fn size(bytes: u64) -> String {
    match bytes {
        n if n >= GIB && n % GIB == 0 => format!("{} GiB", n / GIB),
        n if n >= MIB && n % MIB == 0 => format!("{} MiB", n / MIB),
        n if n >= KIB && n % KIB == 0 => format!("{} KiB", n / KIB),
        n => format!("{n} bytes"),
    }
}

/// What has been read so far, against [`Limits`]. One per operation; the counters
/// only grow, and a limit that is passed stays passed.
#[derive(Debug)]
pub struct Budget {
    limits: Limits,
    memory: AtomicU64,
    total: AtomicU64,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Budget { limits, memory: AtomicU64::new(0), total: AtomicU64::new(0) }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Adds to a counter; an overflow counts as everything (the counter is pinned at the top).
    fn count(counter: &AtomicU64, n: u64) {
        let mut seen = counter.load(Ordering::SeqCst);
        while let Err(now) = counter.compare_exchange_weak(seen, seen.saturating_add(n), Ordering::SeqCst, Ordering::SeqCst) {
            seen = now;
        }
    }

    /// Fails if a file that is `declared` bytes by its header may not be read: over `cap`, or over what is left.
    fn admit(&self, declared: u64, cap: u64, what: &str, held: bool) -> Result<(), Error> {
        let limit = |what: &str, limit: u64| Err(Error::Limit { what: what.to_string(), limit });
        if declared > cap {
            return limit(what, cap);
        }
        if declared > self.limits.total.saturating_sub(self.total.load(Ordering::SeqCst)) {
            return limit("everything read from the archive", self.limits.total);
        }
        if held && declared > self.limits.in_memory_total.saturating_sub(self.memory.load(Ordering::SeqCst)) {
            return limit("the files held in memory together", self.limits.in_memory_total);
        }
        Ok(())
    }

    /// Fails if a file that has yielded `got` bytes out of `stored` expands more than the format
    /// allows: nothing is judged until it has yielded more than [`Limits::ratio_after`], or if there is no ratio.
    pub fn check_ratio(&self, got: u64, stored: u64, what: &str) -> Result<(), Error> {
        match self.limits.ratio {
            Some(ratio) if got > self.limits.ratio_after && got > stored.saturating_mul(ratio) => {
                Err(Error::Limit { what: format!("{what} (packed to {}, {ratio} times that at most)", size(stored)), limit: stored.saturating_mul(ratio) })
            }
            _ => Ok(()),
        }
    }

    /// A file into memory: at most `cap` bytes (and what the budget has left), and exactly
    /// the `declared` ones. The first allocation is at most 1 MiB whatever the header says.
    pub fn read(&self, reader: &mut impl Read, declared: u64, cap: u64, what: &str) -> Result<Vec<u8>, Error> {
        self.read_from(reader, declared, None, cap, what)
    }

    /// [`Budget::read`] of a file that is `stored` bytes in its archive, which it may not expand past the ratio.
    pub fn read_packed(&self, reader: &mut impl Read, declared: u64, stored: u64, cap: u64, what: &str) -> Result<Vec<u8>, Error> {
        self.read_from(reader, declared, Some(stored), cap, what)
    }

    fn read_from(&self, reader: &mut impl Read, declared: u64, stored: Option<u64>, cap: u64, what: &str) -> Result<Vec<u8>, Error> {
        self.admit(declared, cap, what, true)?;
        let mut data = Vec::with_capacity(usize::try_from(declared.min(FIRST_ALLOC)).unwrap_or(0));
        let result = self.pump(reader, declared, stored, what, |piece| {
            data.extend_from_slice(piece);
            Ok(())
        });
        Self::count(&self.memory, data.len() as u64);
        result?;
        Ok(data)
    }

    /// A file to `out` in pieces, never held: at most [`Limits::streamed`] bytes (and what the
    /// budget has left), and exactly the `declared` ones. Returns the bytes written; what was
    /// written before a failure is the caller's to throw away.
    pub fn copy(&self, reader: &mut impl Read, declared: u64, out: &mut impl Write, what: &str) -> Result<u64, Error> {
        self.copy_from(reader, declared, None, out, what)
    }

    /// [`Budget::copy`] of a file that is `stored` bytes in its archive, which it may not expand past the ratio.
    pub fn copy_packed(&self, reader: &mut impl Read, declared: u64, stored: u64, out: &mut impl Write, what: &str) -> Result<u64, Error> {
        self.copy_from(reader, declared, Some(stored), out, what)
    }

    fn copy_from(&self, reader: &mut impl Read, declared: u64, stored: Option<u64>, out: &mut impl Write, what: &str) -> Result<u64, Error> {
        self.admit(declared, self.limits.streamed, what, false)?;
        self.pump(reader, declared, stored, what, |piece| Ok(out.write_all(piece)?))
    }

    /// What a file yields, piece by piece, to `sink`: never past one byte more than `declared`
    /// (which is an error, and not passed on), counting every byte really got, and judging its ratio as it goes.
    fn pump(&self, reader: &mut impl Read, declared: u64, stored: Option<u64>, what: &str, mut sink: impl FnMut(&[u8]) -> Result<(), Error>) -> Result<u64, Error> {
        // At most 256 KiB, so it fits.
        let mut buf = vec![0; declared.saturating_add(1).min(256 * KIB) as usize];
        let mut reader = reader.take(declared.saturating_add(1));
        let mut got = 0u64;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            Self::count(&self.total, n as u64);
            got = got.saturating_add(n as u64);
            if got > declared {
                break;
            }
            if let Some(stored) = stored {
                self.check_ratio(got, stored, what)?;
            }
            sink(&buf[..n])?;
        }
        check_length(got, declared, what)?;
        Ok(got)
    }
}

/// A file must hold what its header says: more is damage or a bomb, less is damage.
fn check_length(got: u64, declared: u64, what: &str) -> Result<(), Error> {
    if got > declared {
        Err(Error::Package(format!("{what} holds more than the {declared} bytes its header says: the file is damaged or made to fill the disk")))
    } else if got < declared {
        Err(Error::Package(format!("{what} holds {got} bytes, not the {declared} its header says: the file is damaged")))
    } else {
        Ok(())
    }
}

/// A folder of our own in the temporary folder, private to us and removed with it.
/// A file made in it and kept open lives on after the folder is gone.
pub struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    pub fn new() -> Result<Self, Error> {
        Ok(Scratch { dir: crate::convert::scratch_dir()? })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// A new file in it (never one that exists), open for reading and writing.
    pub fn create(&self, name: &str) -> io::Result<File> {
        std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(self.dir.join(name))
    }

    /// What `write` writes into a new file, and its length. The file has no name from the start
    /// (the handle keeps it), so nothing is left behind if the program dies, and it stays readable
    /// through the handle until that is dropped.
    pub fn spool(write: impl FnOnce(&mut File) -> Result<u64, Error>) -> Result<(File, u64), Error> {
        let scratch = Scratch::new()?;
        let mut file = scratch.create("spool")?;
        std::fs::remove_file(scratch.path().join("spool"))?;
        drop(scratch);
        let len = write(&mut file)?;
        file.flush()?;
        Ok((file, len))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The smallest list-of-files record in a zip's central directory.
const CENTRAL_HEADER: u64 = 46;

/// How far from the end of a file the end records are looked for: twice what a zip reader looks
/// at (the longest comment, 64 KiB, and the record).
const TAIL: u64 = 2 * 65_536;

/// A zip64 end record is this long, if it has no extra data.
const ZIP64_RECORD: usize = 56;

/// What an end record says about the list of files, judged. Breaking a limit is an error;
/// a claim that cannot be true is no error here (it may be a stray record), it says why not.
fn judge(entries: u64, size: u64, offset: u64, end: u64, limits: &Limits) -> Result<Option<&'static str>, Error> {
    if entries > limits.entries {
        return Err(Error::Package(format!("the archive has {entries} files; more than {} are refused", limits.entries)));
    }
    if size > limits.central_dir {
        return Err(Error::Limit { what: "the list of the archive's files".into(), limit: limits.central_dir });
    }
    if entries.checked_mul(CENTRAL_HEADER).is_none_or(|least| least > size) {
        return Ok(Some("the file count does not fit the list of files"));
    }
    if offset.checked_add(size).is_none_or(|e| e > end) {
        return Ok(Some("the list of files is not inside the file"));
    }
    Ok(None)
}

/// Where the zip64 locator that ends at `end` says the zip64 end record is; none if there is no locator.
fn zip64_locator<R: Read + Seek>(file: &mut R, end: u64) -> Result<Option<u64>, Error> {
    let Some(at) = end.checked_sub(20) else { return Ok(None) };
    let mut buf = [0; 20];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut buf)?;
    Ok((buf[..4] == *b"PK\x06\x07").then(|| u64::from_le_bytes(buf[8..16].try_into().unwrap())))
}

/// Every place in `from..to` a zip64 end record could start (one that fits before `to`), with its first 56 bytes.
fn zip64_records<R: Read + Seek>(file: &mut R, from: u64, to: u64, mut each: impl FnMut(u64, &[u8; ZIP64_RECORD]) -> Result<(), Error>) -> Result<(), Error> {
    let mut buf = vec![0; MIB as usize + ZIP64_RECORD];
    let mut at = from;
    while at.checked_add(ZIP64_RECORD as u64).is_some_and(|e| e <= to) {
        let want = (to - at).min(buf.len() as u64) as usize;
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut buf[..want])?;
        // Records that start in this piece and end in it; the next piece starts where they stop.
        let last = want - ZIP64_RECORD;
        let mut i = 0;
        while let Some(found) = buf[i..=last].iter().position(|&b| b == b'P') {
            i += found;
            if buf[i..i + 4] == *b"PK\x06\x06" {
                each(at + i as u64, buf[i..i + ZIP64_RECORD].try_into().unwrap())?;
            }
            i += 1;
        }
        at += last as u64 + 1;
    }
    Ok(())
}

/// Looks at the end records of a zip before a zip reader allocates for its list of files: the
/// count, the size and the place of the list must be believable, and within `limits`.
/// A zip reader takes the first end record (looking back from the end) that is good, not the one
/// a comment ends with, so every record in the last 128 KiB is judged, and so is every zip64 end
/// record a locator among them leads to: one that breaks a limit refuses the file, whichever the
/// reader would take; at least one must be believable. Says nothing about the files themselves.
pub fn zip_precheck<R: Read + Seek>(file: &mut R, limits: &Limits) -> Result<(), Error> {
    let len = file.seek(SeekFrom::End(0))?;
    let start = len - len.min(TAIL);
    file.seek(SeekFrom::Start(start))?;
    // At most 128 KiB, so it fits.
    let mut tail = vec![0; (len - start) as usize];
    file.read_exact(&mut tail)?;
    let (mut believable, mut why) = (false, None);
    // Where each zip64 locator says its record is, by where the locator ends and the end record starts.
    let mut zip64 = BTreeMap::new();
    for pos in (0..tail.len().saturating_sub(21)).rev() {
        let comment = usize::from(u16::from_le_bytes([tail[pos + 20], tail[pos + 21]]));
        // A record whose comment goes past the end of the file is no record.
        if tail[pos..pos + 4] != *b"PK\x05\x06" || pos + 22 + comment > tail.len() {
            continue;
        }
        let record = &tail[pos..pos + 22];
        let (entries, size, offset) = (u64::from(u16::from_le_bytes([record[10], record[11]])), u64::from(u32::from_le_bytes(record[12..16].try_into().unwrap())), u64::from(u32::from_le_bytes(record[16..20].try_into().unwrap())));
        let end = start + pos as u64;
        // Markers say the real numbers are in a zip64 record, if there is a locator to it; else they are the numbers.
        let named = if entries == 0xFFFF || size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF { zip64_locator(file, end)? } else { None };
        match named {
            Some(named) => {
                zip64.insert(end - 20, (named, end));
            }
            None => match judge(entries, size, offset, end, limits)? {
                None => believable = true,
                Some(reason) => {
                    why.get_or_insert(reason);
                }
            },
        }
    }
    if let (Some(from), Some(to)) = (zip64.values().map(|&(named, _)| named).min(), zip64.keys().copied().max()) {
        // A zip reader tries every zip64 end record between where a locator points and the locator, and
        // reads as much of each as it says it has; a real zip has one, so more than one for each locator
        // (none of the records is wanted twice) is not a zip, and the pass stops at the first excess.
        let too_many = || Error::Package("not a usable zip file: a zip64 locator has more than one end record to choose from".into());
        let mut records = Vec::new();
        zip64_records(file, from, to, |at, record| {
            if records.len() >= zip64.len() {
                return Err(too_many());
            }
            records.push((at, *record));
            Ok(())
        })?;
        // What the zip reader would read, over every locator and record it would try.
        let mut work = 0u64;
        for (&locator, &(named, end)) in &zip64 {
            let (low, high) = (records.partition_point(|&(at, _)| at < named), records.partition_point(|&(at, _)| at < locator));
            if high - low > 1 {
                return Err(too_many());
            }
            let Some(&(at, record)) = records[low..high].first() else {
                why.get_or_insert("the zip64 end record is missing");
                continue;
            };
            let number = |from: usize| u64::from_le_bytes(record[from..from + 8].try_into().unwrap());
            let (size, distance) = (number(4), locator - at);
            work = work.saturating_add(if size < 44 { ZIP64_RECORD as u64 } else { size.min(distance) });
            if work > limits.central_dir {
                return Err(Error::Limit { what: "what the zip64 end records ask to read".into(), limit: limits.central_dir });
            }
            // The record must reach to the locator it was found for, as a zip reader insists.
            if size < 44 || size.checked_add(12) != Some(distance) {
                why.get_or_insert("the zip64 end record does not end at its locator");
                continue;
            }
            match judge(number(32), number(40), number(48), end, limits)? {
                None => believable = true,
                Some(reason) => {
                    why.get_or_insert(reason);
                }
            }
        }
    }
    if believable {
        Ok(())
    } else {
        Err(Error::Package(format!("not a usable zip file: {}", why.unwrap_or("no end of central directory"))))
    }
}

/// Every file of a zip must lie in a place of its own, between the start of the file and its list of
/// files: no two share a byte, and none says it holds more than the room up to the next. A file that
/// says so is not believed, so what an entry says it takes up (which the expansion ratio is judged
/// against) is bytes the zip really has, and all the entries together no more than the zip.
pub fn zip_extents<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Result<(), Error> {
    match archive.has_overlapping_entries() {
        Ok(false) => Ok(()),
        Ok(true) => Err(Error::Package("damaged zip file: two files share bytes, or one says it holds more than the room it has".into())),
        Err(e) => Err(Error::Package(format!("damaged zip file: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn small() -> Limits {
        Limits { manifest: 100, icon: 50, entries: 3, central_dir: 1000, in_memory: 100, in_memory_total: 150, streamed: 1000, total: 300, ratio: None, ratio_after: 50 }
    }

    #[test]
    fn a_file_is_read_to_the_byte_it_declared() {
        let budget = Budget::new(small());
        assert_eq!(budget.read(&mut Cursor::new(vec![7; 40]), 40, 100, "a").unwrap(), vec![7; 40]);
        // More than declared: the extra byte is found, however large the rest is.
        let err = budget.read(&mut io::repeat(1), 10, 100, "b").unwrap_err().to_string();
        assert!(err.contains("holds more than the 10 bytes"), "{err}");
        // Less than declared.
        let err = budget.read(&mut Cursor::new(vec![1; 5]), 10, 100, "c").unwrap_err().to_string();
        assert!(err.contains("holds 5 bytes, not the 10"), "{err}");
    }

    #[test]
    fn the_cap_is_checked_on_the_declared_size_before_reading() {
        struct Never;
        impl Read for Never {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("read though the file is over the cap");
            }
        }
        let err = Budget::new(small()).read(&mut Never, 101, 100, "assembly.xml").unwrap_err();
        assert_eq!(err.to_string(), "assembly.xml is larger than 100 bytes");
        let err = Budget::new(Limits { manifest: 16 << 20, ..small() }).read(&mut Never, (16 << 20) + 1, 16 << 20, "assembly.xml").unwrap_err();
        assert_eq!(err.to_string(), "assembly.xml is larger than 16 MiB");
    }

    #[test]
    fn the_budget_runs_out_across_files() {
        // Memory: 150 in all.
        let budget = Budget::new(small());
        budget.read(&mut Cursor::new(vec![0; 100]), 100, 100, "a").unwrap();
        let err = budget.read(&mut Cursor::new(vec![0; 60]), 60, 100, "b").unwrap_err().to_string();
        assert!(err.contains("held in memory together"), "{err}");
        // Streaming does not count against memory, but against the total: 300.
        let mut sink = Vec::new();
        budget.copy(&mut Cursor::new(vec![0; 150]), 150, &mut sink, "c").unwrap();
        let err = budget.copy(&mut Cursor::new(vec![0; 100]), 100, &mut sink, "d").unwrap_err().to_string();
        assert!(err.contains("everything read from the archive is larger than 300 bytes"), "{err}");
        assert_eq!(sink.len(), 150);
    }

    #[test]
    fn copy_stops_at_the_declared_size() {
        let budget = Budget::new(Limits { total: 10_000, ..small() });
        let mut sink = Vec::new();
        assert_eq!(budget.copy(&mut Cursor::new(vec![3; 600]), 600, &mut sink, "a").unwrap(), 600);
        assert_eq!(sink, vec![3; 600]);
        let mut sink = Vec::new();
        assert!(budget.copy(&mut io::repeat(1), 5, &mut sink, "b").is_err());
        assert!(sink.is_empty(), "bytes past the declared size were written");
        assert!(budget.copy(&mut Cursor::new(vec![1; 5]), 6, &mut sink, "c").is_err());
    }

    #[test]
    fn the_ratio_is_only_checked_when_there_is_one() {
        assert!(Budget::new(small()).check_ratio(u64::MAX, 1, "a").is_ok());
        let budget = Budget::new(Limits { ratio: Some(10), ..small() });
        // Nothing is judged up to 50 bytes, then 10 times what was stored is the most.
        assert!(budget.check_ratio(50, 0, "a").is_ok());
        assert!(budget.check_ratio(60, 6, "a").is_ok());
        let err = budget.check_ratio(60, 5, "a").unwrap_err().to_string();
        assert_eq!(err, "a (packed to 5 bytes, 10 times that at most) is larger than 50 bytes");
        assert!(budget.check_ratio(51, 0, "a").is_err());
    }

    #[test]
    fn a_file_is_cut_short_when_it_yields_more_than_its_ratio() {
        let limits = Limits { ratio: Some(10), ratio_after: 1000, in_memory: 1 << 20, in_memory_total: 1 << 20, streamed: 1 << 20, total: 4 << 20, ..small() };
        let budget = Budget::new(limits);
        // Under the floor, whatever it was stored in; over it, within the ratio.
        assert_eq!(budget.read_packed(&mut Cursor::new(vec![0; 1000]), 1000, 1, 1 << 20, "a").unwrap().len(), 1000);
        assert_eq!(budget.read_packed(&mut Cursor::new(vec![0; 5000]), 5000, 500, 1 << 20, "b").unwrap().len(), 5000);
        // Over both: stopped at the first piece that shows it, long before the end, and nothing of it is passed on.
        let mut sink = Vec::new();
        let used = budget.total.load(Ordering::SeqCst);
        let err = budget.copy_packed(&mut io::repeat(0).take(1 << 20), 1 << 20, 100, &mut sink, "c").unwrap_err().to_string();
        assert_eq!(err, "c (packed to 100 bytes, 10 times that at most) is larger than 1000 bytes");
        assert!(sink.is_empty());
        assert!(budget.total.load(Ordering::SeqCst) - used <= 256 << 10, "read on after the ratio was passed");
        let err = budget.read_packed(&mut Cursor::new(vec![0; 5000]), 5000, 499, 1 << 20, "d").unwrap_err().to_string();
        assert!(err.contains("packed to 499 bytes"), "{err}");
        // Without a stored size there is nothing to compare.
        assert_eq!(budget.read(&mut Cursor::new(vec![0; 5000]), 5000, 1 << 20, "e").unwrap().len(), 5000);
    }

    #[test]
    fn a_spooled_file_has_no_name() {
        use std::os::unix::fs::MetadataExt;
        let (mut file, len) = Scratch::spool(|f| {
            // Gone from its folder while it is written, as it would be if the program died now.
            assert_eq!(f.metadata()?.nlink(), 0);
            f.write_all(b"hello")?;
            Ok(5)
        })
        .unwrap();
        assert_eq!(len, 5);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn a_spooled_file_outlives_its_folder() {
        let (mut file, len) = Scratch::spool(|f| {
            f.write_all(b"hello")?;
            Ok(5)
        })
        .unwrap();
        assert_eq!(len, 5);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
        let failed = Scratch::spool(|_| Err(Error::Cancelled));
        assert!(failed.is_err());
    }

    /// An end record with these fields and this comment.
    fn end_record(entries: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
        let mut data = b"PK\x05\x06\0\0\0\0".to_vec();
        data.extend_from_slice(&entries.to_le_bytes());
        data.extend_from_slice(&entries.to_le_bytes());
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&offset.to_le_bytes());
        data.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        data.extend_from_slice(comment);
        data
    }

    /// An end record with these fields, after `body`.
    fn zip_with(body: usize, entries: u16, size: u32, offset: u32) -> Vec<u8> {
        [vec![0; body], end_record(entries, size, offset, &[])].concat()
    }

    /// A zip64 end record that says it is `record_size` long, with these numbers.
    fn zip64_record(record_size: u64, entries: u64, size: u64) -> Vec<u8> {
        let mut data = b"PK\x06\x06".to_vec();
        data.extend_from_slice(&record_size.to_le_bytes());
        data.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        data.extend_from_slice(&entries.to_le_bytes());
        data.extend_from_slice(&entries.to_le_bytes());
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&10u64.to_le_bytes());
        data
    }

    /// A zip64 locator that says the record is at `named`, and an end record with the markers.
    fn zip64_tail(named: u64) -> Vec<u8> {
        let mut data = b"PK\x06\x07\0\0\0\0".to_vec();
        data.extend_from_slice(&named.to_le_bytes());
        data.extend_from_slice(&[1, 0, 0, 0]);
        data.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        data.extend_from_slice(&[0xFF; 12]);
        data.extend_from_slice(&[0, 0]);
        data
    }

    /// A zip64 end record with these numbers, its locator (which says the record is at `named`), and
    /// an end record with the markers.
    fn zip64_block(named: u64, entries: u64, size: u64) -> Vec<u8> {
        [zip64_record(44, entries, size), zip64_tail(named)].concat()
    }

    #[test]
    fn the_end_record_must_be_believable() {
        let check = |data: Vec<u8>| zip_precheck(&mut Cursor::new(data), &small());
        assert!(check(zip_with(200, 3, 150, 20)).is_ok());
        assert!(check(zip_with(200, 0, 0, 0)).is_ok());
        let err = check(zip_with(200, 4, 184, 10)).unwrap_err().to_string();
        assert!(err.contains("the archive has 4 files"), "{err}");
        let err = check(zip_with(2000, 2, 1500, 10)).unwrap_err().to_string();
        assert_eq!(err, "the list of the archive's files is larger than 1000 bytes");
        // A count the list cannot hold, a list outside the file, no record at all.
        assert!(check(zip_with(200, 3, 100, 20)).is_err());
        assert!(check(zip_with(200, 3, 150, 100)).is_err());
        assert!(check(zip_with(200, 3, 150, u32::MAX - 10)).is_err());
        assert!(check(vec![0; 100]).is_err());
        assert!(check(Vec::new()).is_err());
    }

    #[test]
    fn the_zip64_record_is_read() {
        // body, zip64 end record, locator, end record with the markers.
        let make = |entries: u64, size: u64| [vec![0; 300], zip64_block(300, entries, size)].concat();
        let check = |data: Vec<u8>| zip_precheck(&mut Cursor::new(data), &small());
        assert!(check(make(3, 150)).is_ok());
        assert!(check(make(1 << 40, 150)).is_err());
        assert!(check(make(3, 1 << 40)).is_err());
        assert!(check(make(u64::MAX, 150)).is_err());
        // The markers without the record.
        assert!(check(zip_with(100, 0xFFFF, 10, 10)).is_err());
        // A locator that leads to nothing.
        assert!(check(make(3, 150)[..300].iter().chain(&make(3, 150)[300 + 56..]).copied().collect()).is_err());
    }

    #[test]
    fn every_end_record_near_the_end_is_judged() {
        let check = |data: Vec<u8>| zip_precheck(&mut Cursor::new(data), &small()).map_err(|e| e.to_string());
        // The record the file ends with is fine; a second one hides in its comment, followed by a
        // byte its own comment does not cover, so the file does not end with it. A zip reader that looks back
        // from the end finds that one first.
        let hidden = |fake: &[u8]| [vec![0; 200], end_record(3, 150, 20, &[fake, &[0]].concat())].concat();
        assert!(check(hidden(&end_record(2, 100, 20, &[]))).is_ok());
        let err = check(hidden(&end_record(60_000, 150, 20, &[]))).unwrap_err();
        assert!(err.contains("the archive has 60000 files"), "{err}");
        // The same with a zip64 record that has its locator and markers in the comment, naming the record by an offset that is not where it is.
        // Where that record is: after the 200 bytes and the 22 of the end record it is in the comment of.
        for named in [222, 0] {
            let err = check(hidden(&zip64_block(named, 1 << 40, 150))).unwrap_err();
            assert!(err.contains("the archive has 1099511627776 files"), "{err}");
            let err = check(hidden(&zip64_block(named, 3, 1 << 40))).unwrap_err();
            assert!(err.contains("the list of the archive's files is larger than 1000 bytes"), "{err}");
        }
        assert!(check(hidden(&zip64_block(0, 2, 100))).is_ok());
        // One that is not believable does not refuse a file with one that is.
        assert!(check(hidden(&end_record(3, 100, 20, &[]))).is_ok());
        // Nor does a record whose comment goes past the end of the file.
        let mut past = end_record(60_000, 150, 20, &[]);
        past[20] = 50;
        assert!(check(hidden(&past)).is_ok());
    }

    #[test]
    fn what_the_zip_reader_would_read_is_counted() {
        let check = |data: Vec<u8>| zip_precheck(&mut Cursor::new(data), &small()).map_err(|e| e.to_string());
        // A record that says it is 2000 long, 2056 before the locator: a zip reader allocates and reads
        // those 2000 before it sees the length is wrong, and the limit is 1000.
        let long = [vec![0; 100], zip64_record(2000, 3, 150), vec![0; 2000], zip64_tail(100)].concat();
        assert_eq!(check(long).unwrap_err(), "what the zip64 end records ask to read is larger than 1000 bytes");
        // One of 400 is within the limit (and not believable, being the wrong length); three locators that
        // point at it make 1200 together.
        let body = [vec![0; 100], zip64_record(400, 3, 150), vec![0; 600]].concat();
        let two = [body.clone(), zip64_tail(100), zip64_tail(100)].concat();
        assert_eq!(check(two).unwrap_err(), "package: not a usable zip file: the zip64 end record does not end at its locator");
        let three = [body, zip64_tail(100), zip64_tail(100), zip64_tail(100)].concat();
        assert_eq!(check(three).unwrap_err(), "what the zip64 end records ask to read is larger than 1000 bytes");
    }

    #[test]
    fn a_pile_of_fake_zip64_records_is_refused_at_once() {
        // 4 MiB with a record signature every 56 bytes, each saying it holds nearly all that follows,
        // then the real one: a zip reader reads about size^2 / 112 bytes of them.
        let len = 4 << 20;
        let mut data = vec![0; len];
        for at in (0..len).step_by(ZIP64_RECORD) {
            data[at..at + 4].copy_from_slice(b"PK\x06\x06");
            data[at + 4..at + 12].copy_from_slice(&((len - at) as u64).to_le_bytes());
        }
        data.extend_from_slice(&zip64_block(0, 3, 150));
        let start = std::time::Instant::now();
        let err = zip_precheck(&mut Cursor::new(data), &small()).unwrap_err().to_string();
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
        assert_eq!(err, "package: not a usable zip file: a zip64 locator has more than one end record to choose from");
    }
}
