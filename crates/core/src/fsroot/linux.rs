//! Linux backend of `fsroot`: directory handles and the `*at` system calls.
//! Every operation names one entry of an open directory, never a path.

use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

use sha2::{Digest, Sha256};

use super::{Caps, Existing, Kind};

/// An open directory: the anchor every name below it is resolved against.
#[derive(Debug)]
pub struct Dir(OwnedFd);

/// What tells two states of a file apart. `dev` and `ino` only identify a
/// file within one run (they change across remounts of FAT and exFAT); the
/// rest, and the digest of the first and last 64 KiB, tell what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fingerprint {
    pub dev: u64,
    pub ino: u128,
    pub size: u64,
    pub mtime_ns: i128,
    pub head_tail: [u8; 32],
}

impl Fingerprint {
    /// Of an open file, reading at most 128 KiB.
    pub fn of(file: &File) -> io::Result<Self> {
        let mut fp = Fingerprint::from_meta(&file.metadata()?);
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 64 << 10];
        for offset in [0, fp.size.saturating_sub(64 << 10)] {
            let mut filled = 0;
            while filled < buf.len() {
                match file.read_at(&mut buf[filled..], offset + filled as u64) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            hash.update(&buf[..filled]);
        }
        fp.head_tail = hash.finalize().into();
        Ok(fp)
    }

    fn from_meta(m: &std::fs::Metadata) -> Self {
        Fingerprint { dev: m.dev(), ino: u128::from(m.ino()), size: m.size(), mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()), head_tail: [0; 32] }
    }

    fn from_stat(st: &libc::stat) -> Self {
        Fingerprint { dev: st.st_dev, ino: u128::from(st.st_ino), size: st.st_size as u64, mtime_ns: i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec), head_tail: [0; 32] }
    }

    /// Whether both name the same file on disk.
    pub fn same_file(&self, other: &Fingerprint) -> bool {
        (self.dev, self.ino) == (other.dev, other.ino)
    }
}

fn cstr(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is not the name of one entry")));
    }
    CString::new(name).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            r => return r,
        }
    }
}

fn kind_of(mode: libc::mode_t) -> Kind {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Link,
        _ => Kind::Other,
    }
}

fn not_found(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOENT)
}

/// A pseudo-random number for unique names.
fn random() -> u64 {
    let mut bytes = [0u8; 8];
    if unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) } == bytes.len() as isize {
        return u64::from_le_bytes(bytes);
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    (nanos ^ (u64::from(std::process::id()) << 40)).wrapping_add(COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed))
}

/// 0 = not known yet, 1 = renameat2 works, 2 = the kernel or the filesystem lacks it.
static NOREPLACE: AtomicU8 = AtomicU8::new(0);
/// The same for openat2.
static OPENAT2: AtomicU8 = AtomicU8::new(0);

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// An error that says the system call (not the path) is unavailable here.
pub(super) fn syscall_missing(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM | libc::EINVAL | libc::E2BIG | libc::EOPNOTSUPP))
}

pub(super) fn mark_openat2_missing() {
    OPENAT2.store(2, Ordering::Relaxed);
}

impl Dir {
    /// Opens a folder by path, following links on the way (a Steam library
    /// reached through `~/.steam/steam` is the same folder).
    pub fn open_path(path: &Path) -> io::Result<Dir> {
        fault!(Open);
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let fd = retry(|| cvt(unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) }))?;
        Ok(Dir(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub fn try_clone(&self) -> io::Result<Dir> {
        Ok(Dir(self.0.try_clone()?))
    }

    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    fn openat(&self, name: &str, flags: libc::c_int, mode: libc::c_uint) -> io::Result<OwnedFd> {
        self.openat_c(&cstr(name)?, flags, mode)
    }

    fn openat_c(&self, c: &CStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<OwnedFd> {
        let fd = retry(|| cvt(unsafe { libc::openat(self.fd(), c.as_ptr(), flags | libc::O_CLOEXEC, mode) }))?;
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A folder inside this one, never through a link.
    pub fn open_dir(&self, name: &str) -> io::Result<Dir> {
        fault!(Open);
        Ok(Dir(self.openat(name, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)?))
    }

    /// A folder inside this one, following a link at `name`.
    pub fn open_dir_following(&self, name: &str) -> io::Result<Dir> {
        fault!(Open);
        Ok(Dir(self.openat(name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?))
    }

    fn regular(fd: OwnedFd) -> io::Result<File> {
        let file = File::from(fd);
        if file.metadata()?.is_file() { Ok(file) } else { Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")) }
    }

    /// A regular file for reading, never through a link.
    pub fn open_read(&self, name: &str) -> io::Result<File> {
        fault!(Open);
        Dir::regular(self.openat(name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY, 0)?)
    }

    /// A regular file for reading, following a link at `name` (for original game files).
    pub fn open_read_following(&self, name: &str) -> io::Result<File> {
        fault!(Open);
        Dir::regular(self.openat(name, libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY, 0)?)
    }

    /// Makes a new file, failing if the name is taken by anything (a link included).
    pub fn create_excl(&self, name: &str) -> io::Result<File> {
        fault!(Create);
        Ok(File::from(self.openat(name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW, 0o666)?))
    }

    /// Makes a new file under a name nobody can predict, and returns it with the name.
    pub fn create_unique(&self) -> io::Result<(File, String)> {
        for _ in 0..32 {
            let name = format!(".paleto-{:016x}.new", random());
            match self.create_excl(&name) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                result => return result.map(|file| (file, name)),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free temporary name"))
    }

    pub fn mkdir(&self, name: &str) -> io::Result<Dir> {
        fault!(Mkdir);
        let c = cstr(name)?;
        retry(|| cvt(unsafe { libc::mkdirat(self.fd(), c.as_ptr(), 0o755) }))?;
        self.open_dir(name)
    }

    /// Hard-links `from` as `to`; fails if `to` exists. A link at `from` is linked itself.
    pub fn link(&self, from: &str, to: &str) -> io::Result<()> {
        fault!(Link);
        if !self.caps().hard_links {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links on this filesystem"));
        }
        let (f, t) = (cstr(from)?, cstr(to)?);
        retry(|| cvt(unsafe { libc::linkat(self.fd(), f.as_ptr(), self.fd(), t.as_ptr(), 0) })).map(drop)
    }

    /// Renames `from` to `to`. Without `replace` it fails if `to` exists: with
    /// renameat2, else by a link and an unlink, else (last resort) by a check
    /// and a rename.
    pub fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        fault!(Rename);
        let (f, t) = (cstr(from)?, cstr(to)?);
        let plain = || retry(|| cvt(unsafe { libc::renameat(self.fd(), f.as_ptr(), self.fd(), t.as_ptr()) })).map(drop);
        if replace {
            return plain();
        }
        if self.caps().noreplace {
            let r = retry(|| cvt(unsafe { libc::syscall(libc::SYS_renameat2, self.fd(), f.as_ptr(), self.fd(), t.as_ptr(), libc::RENAME_NOREPLACE) } as libc::c_int));
            match r {
                Ok(_) => return Ok(()),
                Err(e) if syscall_missing(&e) || e.raw_os_error() == Some(libc::ENOTSUP) => NOREPLACE.store(2, Ordering::Relaxed),
                Err(e) => return Err(e),
            }
        }
        match self.link(from, to) {
            Ok(()) => self.unlink(from),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(_) if self.stat_raw(to, false)?.is_some() => Err(io::ErrorKind::AlreadyExists.into()),
            Err(_) => plain(),
        }
    }

    pub fn unlink(&self, name: &str) -> io::Result<()> {
        fault!(Unlink);
        let c = cstr(name)?;
        retry(|| cvt(unsafe { libc::unlinkat(self.fd(), c.as_ptr(), 0) })).map(drop)
    }

    pub fn rmdir(&self, name: &str) -> io::Result<()> {
        fault!(Rmdir);
        let c = cstr(name)?;
        retry(|| cvt(unsafe { libc::unlinkat(self.fd(), c.as_ptr(), libc::AT_REMOVEDIR) })).map(drop)
    }

    /// Flushes the directory's entries to disk.
    pub fn sync(&self) -> io::Result<()> {
        fault!(Sync);
        retry(|| cvt(unsafe { libc::fsync(self.fd()) })).map(drop)
    }

    /// Bytes an unprivileged user can still write on this filesystem.
    pub fn free_space(&self) -> io::Result<u64> {
        let mut st = unsafe { std::mem::zeroed::<libc::statvfs>() };
        retry(|| cvt(unsafe { libc::fstatvfs(self.fd(), &mut st) }))?;
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }

    /// What this filesystem and kernel can do, as far as is known: the
    /// system calls are tried once and a missing one is remembered.
    pub fn caps(&self) -> Caps {
        let caps = Caps { hard_links: true, noreplace: NOREPLACE.load(Ordering::Relaxed) != 2, openat2: OPENAT2.load(Ordering::Relaxed) != 2 };
        #[cfg(test)]
        {
            if let Some(o) = super::faults::caps_override() {
                return Caps { hard_links: caps.hard_links && o.hard_links, noreplace: caps.noreplace && o.noreplace, openat2: caps.openat2 && o.openat2 };
            }
        }
        caps
    }

    fn stat_raw(&self, name: &str, follow: bool) -> io::Result<Option<libc::stat>> {
        fault!(Stat);
        let c = cstr(name)?;
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
        match retry(|| cvt(unsafe { libc::fstatat(self.fd(), c.as_ptr(), &mut st, flags) })) {
            Ok(_) => Ok(Some(st)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// What `name` is, without following a link; `None` if there is nothing.
    pub fn kind(&self, name: &str) -> io::Result<Option<Kind>> {
        Ok(self.stat_raw(name, false)?.map(|st| kind_of(st.st_mode)))
    }

    /// What `name` is after following links; `None` if a link leads nowhere.
    /// A loop or a link through a file count as leading nowhere.
    pub fn kind_following(&self, name: &str) -> io::Result<Option<Kind>> {
        match self.stat_raw(name, true) {
            Ok(st) => Ok(st.map(|st| kind_of(st.st_mode))),
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// What `name` is, with the fingerprint of a file and the target of a link.
    pub fn stat(&self, name: &str) -> io::Result<Option<Existing>> {
        Ok(match self.kind(name)? {
            None => None,
            Some(Kind::Dir) => Some(Existing::Dir),
            Some(Kind::Link) => Some(Existing::Link(self.read_link(name)?)),
            Some(Kind::Other) => Some(Existing::Other),
            Some(Kind::File) => match self.open_read(name) {
                Ok(file) => Some(Existing::File(Fingerprint::of(&file)?)),
                Err(e) if not_found(&e) => None,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => self.stat_raw(name, false)?.map(|st| Existing::File(Fingerprint::from_stat(&st))),
                Err(e) => return Err(e),
            },
        })
    }

    pub fn read_link(&self, name: &str) -> io::Result<OsString> {
        fault!(ReadLink);
        let c = cstr(name)?;
        let mut buf = vec![0u8; 4096];
        loop {
            let n = retry(|| {
                let n = unsafe { libc::readlinkat(self.fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
            })?;
            if n < buf.len() {
                buf.truncate(n);
                return Ok(OsString::from_vec(buf));
            }
            if buf.len() >= 1 << 16 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "link target too long"));
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    /// The names in this folder that are valid UTF-8 (others cannot be matched
    /// against a package's names), without `.` and `..`.
    pub fn names(&self) -> io::Result<Vec<String>> {
        fault!(List);
        // A new open file description: reading it leaves other handles' positions alone.
        let fd = self.openat_c(c".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let dirp = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        if dirp.is_null() {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(fd);
        let mut names = Vec::new();
        let result = loop {
            unsafe { *libc::__errno_location() = 0 };
            let entry = unsafe { libc::readdir(dirp) };
            if entry.is_null() {
                break match io::Error::last_os_error() {
                    e if e.raw_os_error() == Some(0) => Ok(()),
                    e => Err(e),
                };
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if let Ok(name) = name.to_str()
                && name != "."
                && name != ".."
            {
                names.push(name.to_string());
            }
        };
        unsafe { libc::closedir(dirp) };
        result.map(|()| names)
    }

    /// Device and inode of the directory itself.
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        retry(|| cvt(unsafe { libc::fstat(self.fd(), &mut st) }))?;
        Ok((st.st_dev, st.st_ino))
    }

    pub fn fingerprint(&self) -> io::Result<Fingerprint> {
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        retry(|| cvt(unsafe { libc::fstat(self.fd(), &mut st) }))?;
        Ok(Fingerprint::from_stat(&st))
    }

    /// Device and inode of the folder at `rel` (`/`-separated, `.` for this
    /// one) found without crossing a link or leaving this folder, by openat2.
    pub fn identity_beneath(&self, rel: &str) -> io::Result<(u64, u64)> {
        fault!(Openat2);
        let c = CString::new(rel).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let how = OpenHow { flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64, mode: 0, resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS };
        let r = retry(|| {
            let r = unsafe { libc::syscall(libc::SYS_openat2, self.fd(), c.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>()) };
            if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r as libc::c_int) }
        })?;
        OPENAT2.store(1, Ordering::Relaxed);
        Dir(unsafe { OwnedFd::from_raw_fd(r) }).identity()
    }
}
