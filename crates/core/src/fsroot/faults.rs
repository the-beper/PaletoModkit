//! Fault injection for tests (compiled only under `cfg(test)`): every system
//! call wrapper of `fsroot` announces itself with `fault!(Point)`, and a test
//! picks the n-th announcement to fail or to "crash" the process. A test runs
//! an operation with k = 1, 2, ... until no fault is reached any more.

use std::cell::{Cell, RefCell};
use std::io;

use super::Caps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    Stat,
    Open,
    Create,
    Mkdir,
    Link,
    Rename,
    Unlink,
    Rmdir,
    Sync,
    ReadLink,
    List,
    Openat2,
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// The call fails with this kind of error.
    Fail(io::ErrorKind),
    /// The call fails and so does every later one until [`disarm`]: the
    /// process is "dead", so nothing may clean up. The test then recovers.
    Crash,
}

#[derive(Default)]
struct Plan {
    at: Option<(u64, Mode, Option<Point>)>,
    hits: u64,
    fired: bool,
    crashed: bool,
}

thread_local! {
    static PLAN: RefCell<Plan> = RefCell::new(Plan::default());
    static CAPS: Cell<Option<Caps>> = const { Cell::new(None) };
}

/// Fails the `k`-th call (counting from 1) of any kind.
pub fn arm(k: u64, mode: Mode) {
    PLAN.with(|p| *p.borrow_mut() = Plan { at: Some((k, mode, None)), ..Plan::default() });
}

/// Fails the `k`-th call at this point only.
pub fn arm_at(point: Point, k: u64, mode: Mode) {
    PLAN.with(|p| *p.borrow_mut() = Plan { at: Some((k, mode, Some(point))), ..Plan::default() });
}

/// Stops injecting; whether the fault was reached.
pub fn disarm() -> bool {
    PLAN.with(|p| std::mem::take(&mut *p.borrow_mut()).fired)
}

/// Whether the armed `Crash` fault has fired.
pub fn crashed() -> bool {
    PLAN.with(|p| p.borrow().crashed)
}

pub fn hit(point: Point) -> Option<io::Error> {
    PLAN.with(|p| {
        let mut p = p.borrow_mut();
        if p.crashed {
            return Some(io::Error::other("injected crash"));
        }
        let (k, mode, only) = p.at?;
        if only.is_some_and(|o| o != point) {
            return None;
        }
        p.hits += 1;
        if p.hits != k {
            return None;
        }
        p.fired = true;
        match mode {
            Mode::Fail(kind) => Some(io::Error::new(kind, "injected fault")),
            Mode::Crash => {
                p.crashed = true;
                Some(io::Error::other("injected crash"))
            }
        }
    })
}

/// Restores the capabilities when dropped.
pub struct CapsGuard(Option<Caps>);

impl Drop for CapsGuard {
    fn drop(&mut self) {
        CAPS.with(|c| c.set(self.0));
    }
}

/// Makes this thread's directories behave as if they lacked what `caps` lacks
/// (hard links on exFAT, renameat2, openat2 under seccomp).
pub fn caps(caps: Caps) -> CapsGuard {
    CapsGuard(CAPS.with(|c| c.replace(Some(caps))))
}

pub(super) fn caps_override() -> Option<Caps> {
    CAPS.with(Cell::get)
}
