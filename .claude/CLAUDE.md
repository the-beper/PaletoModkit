# Paleto Modkit

Linux-native replacement for OpenIV for GTA V (Legacy and Enhanced): a Rust workspace with a GUI (`crates/gui`, egui/eframe, native Wayland), a CLI (`crates/cli`, binary `paleto`), and the libraries `rpf`, `formats` and `core`. A small .NET converter lives in `tools/cwconv`. Licence GPL-3.0-or-later. [docs/development.md](../docs/development.md) is the reference for layout, tests, screenshots and packaging.

## After a change

- Run `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all-features`, and read the results before saying they pass.
- Before saying CI is fixed, also run the tests with an empty home, as CI does (locally the app finds the installed game and `keys.bin`, CI has neither):
  `HOME=$(mktemp -d) CARGO_HOME=~/.cargo RUSTUP_HOME=~/.rustup cargo test --all-features`
- Once a set of changes is done and the tests pass, start the Flatpak build in the background with `.claude/flatpak-bg.sh`. It runs at low priority on all but 4 cores (`FREE_CPUS=n` changes that), logs to `target/flatpak-build.log`, and does nothing if a build is already running. Do not start it after every small edit, and do not wait for it: carry on, and report the outcome (`exit: 0` on the last log line, or the error) when it is done or when asked. Never run `packaging/build-flatpak.sh` directly in the foreground.
- A change users see needs a test, and an update of the page of `docs/` that describes it.

## Screenshots

`screenshots/` (README) and `docs/images/` hold pictures of the GUI. When the interface changes (layout, labels, icons, colours, a new or moved control), redo every committed screenshot that shows a changed element, and only those:

1. Generate them into a scratch folder, not over the tree, with the commands in the Screenshots section of docs/development.md (`readme` for `screenshots/`, `docs` for `docs/images/`; use `--release`).
2. Look at each result (read the PNG) and compare it with the committed one.
3. Copy over only the pictures whose content changed on purpose. Leave the others alone, so git does not fill with pixel noise.
4. If a changed screen has no screenshot yet but the docs describe it, say so instead of adding one unasked.

The screenshot tests read the installed game read-only through links in a temporary folder; they never touch the real game folder.

## Rules of the repository

- The code is hand-formatted (long lines, method chains on one line). Never run `cargo fmt`: it rewrites about 80 files. Match the code around the change, and check `git diff --stat` after any tool that rewrites files.
- The GUI is a native Wayland client only. Never switch to X11/XWayland, even to get something winit lacks. Drag and drop on Wayland is `crates/gui/src/wayland_dnd.rs`, because winit 0.30 has none.
- Test the GUI offscreen with `egui_kittest`, never on the user's desktop. A headless compositor session (`kwin_wayland --virtual`) has twice broken the user's Flatpaks, so only start one when a test truly needs it, never while the user is busy, with a timeout, and with the environment isolated *before* `dbus-run-session`: `X=$(mktemp -d /tmp/claude-1000/x.XXXX); chmod 700 $X; env -u WAYLAND_DISPLAY -u DISPLAY XDG_RUNTIME_DIR=$X dbus-run-session -- sh script.sh`. Afterwards check that `mount | grep /run/user/1000/doc` still shows the portal mount and that no process is left with the private bus address (kill by PID, never `pkill -f`).
- Icons and emoji come from the fonts bundled by egui's `monochrome_emoji_fonts` feature; system fonts sit at another height. Do not rely on system fonts for a glyph.
- Follow file-manager conventions in the GUI (names are click targets, not selectable text; Save/Discard/Cancel before losing unsaved edits; keyboard and mouse-button navigation).
- Tests must not need game files or the user's home: build small archives and game folders of their own.
- Never edit or write inside an original game folder, and do not put game files or keys in the repository. Modified archives go to the `mods/` copy. A symlinked copy of a game once let an original get edited: test write paths on a fake first.
- `TODO.md` is the user's local to-do list: add each new request there first, remove it when done, and do not commit changes to it.
- There is no Flathub submission and none is planned. The Flathub remote and image are only where the Flatpak runtime and builder come from; keep them.
