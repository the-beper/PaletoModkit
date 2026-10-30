# Development

- [Project layout](#project-layout)
- [Building](#building)
- [Tests](#tests)
- [Screenshots](#screenshots)
- [Checks against a real game](#checks-against-a-real-game)
- [Contributing](#contributing)
- [Packaging](#packaging)

## Project layout

A Cargo workspace, in Rust (edition 2024, Rust 1.95 or later):

| Crate | What it holds |
| :--- | :--- |
| [`crates/rpf`](../crates/rpf) | `paleto-rpf`: the RPF7 archive reader and editor, with raw, AES and NG encryption and nested archives; a save writes a new copy and renames it over the archive, which is only ever read. |
| [`crates/formats`](../crates/formats) | `paleto-formats`: the game's file formats: RSC7 resources, texture dictionaries (BCn decoding and encoding, DDS and PNG), PSO and resource metadata ↔ XML, text tables, audio containers, model meshes, collision, navmeshes and clip dictionaries. |
| [`crates/core`](../crates/core) | `paleto-core`: game install detection and keys, the `mods/` routing (`vfs`), the OIV engine, and the mod tools: Legacy → Enhanced conversion, add-on packs, the loader and session checks, snapshots, the Wine prefix (.NET for ScriptHookVDotNet mods, backups), the crash report and crash hunt, export and clean-up. |
| [`crates/gui`](../crates/gui) | `paleto-modkit`: the desktop app, built with egui and eframe (native Wayland, with its own drag and drop until winit supports it). |
| [`crates/cli`](../crates/cli) | `paleto-cli`: the `paleto` command-line tool. |

Next to them:

- [`tools/cwconv`](../tools/cwconv): `paleto-cwconv`, the Legacy → Enhanced converter. A small .NET program around CodeWalker.Core, from the [`third_party/codewalker`](../third_party/codewalker) git submodule; `paleto-core` runs it as a separate process (`src/convert.rs`).
- [`packaging/`](../packaging): desktop entry, AppStream metadata, MIME types, icons, the local Flatpak manifest and the build scripts.
- [`screenshots/`](../screenshots) and [`docs/images/`](images): the pictures of the README and of these pages.

## Building

```bash
git clone --recurse-submodules https://github.com/the-beper/PaletoModkit.git
cd paleto-modkit
cargo build --release              # target/release/paleto-modkit and paleto
./packaging/build-cwconv.sh        # target/cwconv/paleto-cwconv
```

`build-cwconv.sh` uses the .NET 10 SDK when it is installed, otherwise Microsoft's SDK container through podman. The converter is found at run time in `$PALETO_CWCONV`, in `../lib/paleto-modkit/cwconv/` next to the program (installed layout), in `cwconv/` next to it, or in `../cwconv/` (so `target/release/paleto` finds `target/cwconv/`).

## Tests

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests need no game files: they build small archives, packages and game folders of their own. The GUI tests drive the real app headless with `egui_kittest`, without a display or a compositor. Disk and archive reads run on worker threads (`Task::spawn_cancellable` in `crates/gui/src/task.rs`), so a test calls `settle` (step until `App::idle`) after opening or previewing something, never a fixed number of frames; `App::delay_loads` slows the reads down to test what shows meanwhile. CI runs both, and validates the desktop entry, the AppStream metadata and the MIME types.

The tests must not depend on your computer either: on yours the app finds your installed game and cached keys, on CI there are none. Run them as CI does, with an empty home folder:

```bash
HOME=$(mktemp -d) CARGO_HOME=~/.cargo RUSTUP_HOME=~/.rustup cargo test
```

## Screenshots

The screenshots are rendered by tests from a real game install, and are skipped unless `PALETO_SCREENS` names the output folder. They run against a clean copy of the game, made of links to the game's own files in a temporary folder, so they never touch the real game folder and show none of your mods.

```bash
# README and AppStream screenshots (an Enhanced install when there is one, else Legacy)
PALETO_SCREENS=$PWD/screenshots cargo test --release -p paleto-modkit --test screens readme -- --nocapture

# docs/images (needs an Enhanced install; for the conversion dialogs, put a Legacy add-on
# pack at tmp/sppiccolo/dlc.rpf, a folder git ignores, and build target/cwconv)
PALETO_SCREENS=$PWD/docs/images cargo test --release -p paleto-modkit --test screens docs -- --nocapture

# Every menu and window, to review how they read (uses the installed game as it is, read-only)
PALETO_SCREENS=/tmp/tour cargo test -p paleto-modkit --test screens tour -- --nocapture
```

## Checks against a real game

Examples that read a whole game folder and check the formats against it; they change nothing:

```bash
cargo run --release -p paleto-rpf --example scan -- <game folder> <keys.bin>                # open every archive
cargo run --release -p paleto-formats --example scan_formats -- <game folder> [kinds…]      # metadata, text, audio, models round trips
cargo run --release -p paleto-formats --example scan_ytd -- <game folder> <keys.bin> [--rebuild] # every texture dictionary
cargo run --release -p paleto-rpf --example compare -- <keys.bin> <a.rpf> <b.rpf> [c.rpf]   # what differs between archives

PROJECT=cwxml ./packaging/build-cwconv.sh   # target/cwxml/paleto-cwxml (tools/cwxml, not shipped), for roundtrip_xml
cargo run --release -p paleto-formats --example roundtrip_xml -- <game folder> <N> [kinds…] [--keep] # every Nth model's XML export, imported by CodeWalker and compared
```

The keys are in `~/.local/share/paleto/keys.bin` once the app or `paleto setup` has prepared them. Each example's header describes it. `roundtrip` edits a copy of an archive in a work folder you give it.

## Contributing

Pull requests and issues are welcome; [CONTRIBUTING.md](../CONTRIBUTING.md) says what helps. Before sending a change:

- `cargo clippy --all-targets -- -D warnings` is clean, and `cargo test` passes, also with an empty home folder (see [Tests](#tests));
- the Rust code is formatted by hand, in one style (long lines, method chains on one line) that `rustfmt` does not reproduce: do not run `cargo fmt` over the tree, and match the code around your change;
- a change that users see comes with its test (the GUI tests are small and fast) and an update of the page of [docs/](.) that describes it.

## Packaging

| Script | What it does |
| :--- | :--- |
| `packaging/build-release.sh` | Builds the tarball (`PaletoModkit-x86_64.tar.gz`) and the AppImage (`PaletoModkit-x86_64.AppImage`, with its `.zsync`) in `dist/`, with `paleto-cwconv`, and `dist/linux-build.txt`. appimagetool and the AppImage runtime are downloaded by URL and SHA-256 from `packaging/release.conf` (`fetch-pinned.sh`) and hashed again before use; `EXPECT_VERSION` must match `Cargo.toml`. It writes no checksum file: `SHA256SUMS` is made for the final files by `release-verify.sh`. |
| `packaging/build-flatpak.sh` | Builds `PaletoModkit-x86_64.flatpak` in `dist/` (or `OUT`), unsigned, with flatpak-builder in the Flathub build image (by digest) through rootless podman (docker or rootful podman only with `ALLOW_ROOTFUL=1`). It needs the network and about 15 GB; `WORK` sets where its cache lives (default `~/.cache/paleto-flatpak`), including the Flatpak installation of the SDK (`WORK/flatpak`, kept between builds: the image's own copy sits in overlay layers, where an update fails). The SDK and its extensions are updated to the commits pinned in `release.conf` and checked. It refuses to run with `GPG_KEY_ID` or `GNUPGHOME` set: no key enters the container. `EXPORT_REPO=1` (release mode) also writes `flatpak-unsigned-repo.tar` and `flatpak-build.txt` and refuses an empty pin; `BUNDLE=0` skips the bundle. |
| `packaging/flatpak-sign.sh` | Signs the repository, outside the build, in three steps run by different jobs: `--prepare` (no key) adds the new commits on top of the published ones and makes the summary; `--sign` signs the commits and the summary with the key; `--finish` (no key) checks the result with the public key only and makes the bundle. There are no static deltas. The key comes in `FLATPAK_GPG_PRIVATE_KEY` and must be the one of `release.conf`. |
| `packaging/publish-flatpak-repo.sh` | Writes `paleto-modkit.flatpakrepo`, `.flatpakref` and the icon next to the signed repository, for GitHub Pages; it takes the public key file as its third argument and needs no keyring. |
| `packaging/release.conf` | The pins of the release: identity of the Flatpak, signing key fingerprint, build image digest, Flathub commits of the SDK, tool downloads with their hashes, the files of a release. |
| `packaging/release-lib.sh`, `release-validate.sh`, `release-verify.sh`, `release-pages.sh`, `release-publish.sh`, `fetch-pinned.sh` | The release pipeline: checks of the tag and the versions, of the build outputs and of the signed repository; the published Flatpak repository; draft release, gh-pages and publication, safe to run again. Exit codes: 0 ok, 1 usage, 2 validation, 3 refused by policy, 4 signing, 5 conflict on the remote. |
| `packaging/tests/release-sim.sh` | Simulates the whole pipeline against a throwaway remote, a stub of `gh` and throwaway keys (needs `flatpak`, `ostree`, `gpg`, `jq`; no network). |
| `packaging/install.sh` | Installs into `~/.local` (or `$PREFIX`) from a release tarball or from a source tree; `--uninstall` removes it. |
| `packaging/build-cwconv.sh` | Builds `paleto-cwconv`, self-contained. |
| `packaging/flatpak-cargo-sources.sh` | Generates `cargo-sources.json` from `Cargo.lock` with flatpak-cargo-generator, for offline Flatpak builds (`build-flatpak.sh` runs it). |
| `packaging/flatpak-nuget-sources.sh` | Generates `nuget-sources.json` for the converter with flatpak-dotnet-generator (`build-flatpak.sh` runs it too). |

`packaging/io.github.the_beper.PaletoModkit.yml` is the Flatpak manifest; it builds from the working tree. To install what `build-flatpak.sh` made:

```bash
./packaging/build-flatpak.sh
flatpak install --user dist/PaletoModkit-x86_64.flatpak
```

The release workflow (`.github/workflows/release.yml`) builds the tarball and the AppImage (on `ubuntu-22.04`, so the AppImage links an old glibc and carries `libxkbcommon-x11`, which winit loads when there is no Wayland session) and the Flatpak (on the runner itself, after freeing its disk: the build needs more room than a container job has) for each `v*` tag. The builds never see the signing key: it is only in the approved `sign-flatpak` job, and `publish` then makes the release and the `gh-pages` branch (served by GitHub Pages). The one-time setup, cutting a release, recovery and pin bumps are in [release.md](release.md).

Dependabot updates the GitHub Actions weekly; they are pinned to commit SHAs. CI skips pushes and pull requests that only touch documentation, screenshots or other non-code files.

Third-party code and data, and their licences, are listed in [THIRD_PARTY.md](../THIRD_PARTY.md).
