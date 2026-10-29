# Implementation status (Register)

The single tracking file for the hardening and feature mandates (M1–M5). Each mandate resumes from the latest checkpoint here. Result labels: **VERIFIED**, **PARTIAL**, **NOT-RUN**, **BLOCKED**, **N/A**, always with the environment (local Linux, CI runner, Windows Server runner, Windows 10/11 client).

- [Checkpoints](#checkpoints)
- [V0 baseline](#v0-baseline)
- [Findings](#findings)
- [Gates](#gates)
- [Platforms](#platforms)
- [CI and release inventory](#ci-and-release-inventory)
- [Verification map](#verification-map)
- [Features](#features)
- [Conflicts and decisions](#conflicts-and-decisions)
- [Agents and escalations](#agents-and-escalations)
- [Pending actions that need authorization](#pending-actions-that-need-authorization)
- [Follow-ups](#follow-ups)

## Checkpoints

Newest first.

| When | Mandate | Done | In progress | Next step | Open questions | Commit |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| 2026-10-29 | M1 | V0 inventory, findings triage (6/6 Confirmed) | V0 baseline commands, P0-A design | P0-A A1 | — | 24d13f5 (start) |

## V0 baseline

Recorded once, at the start of M1.

| Item | Value |
| :--- | :--- |
| Commit | `24d13f598656e5a30bcdaf85d151eb650f22c75e` (`main`, release 1.5.1), `git status --short` clean |
| Machine | Fedora Linux 44 on WSL2, kernel 6.18.33.2-microsoft-standard-WSL2, Intel Core Ultra 5 238V (8 threads), 15 GiB RAM |
| Rust | rustc / cargo 1.99.1 (Fedora packages, no rustup); clippy 0.1.99 installed for this baseline (`dnf install clippy`); workspace `rust-version = "1.95"` (MSRV not checked: no 1.95 toolchain, NOT-RUN) |
| Workspace | version 1.5.1, edition 2024, resolver 3; crates `paleto-rpf`, `paleto-formats`, `paleto-core`, `paleto-cli` (`paleto`), `paleto-modkit` (GUI); `Cargo.lock` with 659 packages; `cargo metadata --locked --no-deps` exit 0 |
| .NET | `tools/cwconv` targets `net10.0`, one `ProjectReference` to CodeWalker.Core (submodule `third_party/codewalker` at `485d56be`, not initialized in this checkout); no .NET SDK on this machine: converter build NOT-RUN |
| OS packages added for the baseline | `clippy`, `wayland-devel` (wayland-sys probes `wayland-client` with pkg-config), `ShellCheck` |
| Tools missing | podman, docker, flatpak, flatpak-builder, ostree, actionlint, zizmor, appstreamcli, desktop-file-validate, dotnet |
| Game, keys | none on this machine (no Steam library, no `~/.local/share/paleto`) |

### Baseline commands (local Linux)

| Command | Exit | Result | Wall time | Notes |
| :--- | :--- | :--- | :--- | :--- |
| `cargo metadata --locked --no-deps` | 0 | VERIFIED | < 1 s | |
| `cargo test --locked --workspace --all-features` | 0 | VERIFIED | 1477 s (cold) | 282 passed, 0 failed, 1 ignored (`installs_dotnet_from_the_window`: needs `PALETO_PREFIX_SOURCE`, the network and ~5 min) |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | 0 | VERIFIED | 1011 s (cold) | no warnings |
| `cargo build --locked --release` | 0 | VERIFIED | 751 s (cold) | |
| same tests with an isolated `HOME` (`HOME=$(mktemp -d)`, `CARGO_HOME` kept) | 0 | VERIFIED | 24 s (warm) | same counts as above |

Pre-existing failures: none. The cold timings ran while other agents compiled in separate target folders: they are not a performance baseline.
| MSVC diagnostic build (Windows runner) | — | NOT-RUN | — | no Windows runner reachable without a push or `workflow_dispatch` (both need authorization) |

### Test inventory

`#[test]` functions: rpf 15, formats 87, core 88, cli 4, gui 89 (unit tests in `src/` plus `tests/`). The GUI tests run headless with `egui_kittest`; the screenshot tests (`crates/gui/tests/screens.rs`) skip unless `PALETO_SCREENS` is set.

### Actions history (read-only sample, `gh run list`)

Small sample, not a statistic: CI on `main` 82–182 s on most runs, 598 s once (2026-10-28 13:32, likely a cold cache); Release for v1.5.0 and v1.5.1 1685–1686 s; `pages-build-deployment` 59–65 s.

## Findings

Historical findings of the M1 mandate, classified against commit `24d13f5`.

| ID | Summary | State | Evidence | Fix | Tests |
| :--- | :--- | :--- | :--- | :--- | :--- |
| OIV-02 | Path confinement bypass via symlinks / text-only checks | Confirmed | `safe_path` is textual (`crates/core/src/oiv.rs:551`); `resolve_ci` joins text and follows links through `exists()` (`crates/core/src/game.rs:227`); `Game::target` (`game.rs:54`) feeds `<add>`, `<delete>`, `<text>`, `<xml>`, `<pso>` destinations (`oiv.rs:310-371`) and add-on packs (`addon.rs:68`) with no check that the resolved path stays in the game or mods folder; only `mods_copy`/`unlink_copy` (`game.rs:68-121`) look at links | | |
| FS-04 | Predictable temp/undo names, unsafe cleanup, no concurrency control | Confirmed | `.<name>.paleto-tmp`: `remove_file` of whatever has the name, then `fs::write` (follows a link planted in between), then `rename` (`game.rs:125-134`); `.<name>.paleto-undo`: `remove_file` of an unknown file, then `hard_link`/`copy` (`oiv.rs:258-277`); `.ScriptHookV.dll.new` (`scripthook.rs:211-216`); `.mods.restoring`/`.mods.replaced` removed with `remove_dir_all` (`snapshots.rs:86-108`); no lock: two installs on one game share these names | | |
| OIV-03 | Unbounded ZIP/OIV reads | Confirmed | `read_entry` preallocates the declared `entry.size()` and `read_to_end`s without a limit (`oiv.rs:626-632`), used for `assembly.xml` (`oiv.rs:56`), `icon.png` (`oiv.rs:100`) and every `<add>` source; no entry-count or total budget | | |
| OIV-01 | Non-recoverable multi-file commit; in-place RPF save | Confirmed | loose files are written as commands run, `undo.finish()` drops the backups, then each archive is saved (`oiv.rs:204-213`); `ArchiveEditor::save` writes in place when saving to the archive it opened (`crates/rpf/src/edit.rs:350-357`), through a read-write handle (`edit.rs:111-112`); a failure while saving archives is not undone (`oiv.rs:243-246`); the undo state lives only in memory | rpf part: staged save (`fix: stage RPF saves in a new copy`); engine: A4 | `save_staged.rs`, `crates/rpf/tests/edit.rs` (8 new) |
| OIV-06 | Rollback failures swallowed / misreported | Confirmed | `Undo::roll_back` ignores every error (`let _ =`, `oiv.rs:279-286`); `install_with` always reports "(nothing was installed)" (`oiv.rs:204-207`); `Undo::finish` ignores removal errors (`oiv.rs:288-293`) | | |
| DLC-05 | DLC install not transactional; `update.rpf` touched | Confirmed | `install_pack` writes `dlc.rpf` (`addon.rs:89-91`), then opens the mods copy of `update.rpf` and saves it in place (`addon.rs:95-102`); a failure after the first write leaves the pack installed but not listed, with no rollback; the game's own `update.rpf` is not written (the mods copy is), but the mods copy is edited in place | | |

## Gates

| Gate | Local Linux | CI runner | Windows Server runner | Windows 10/11 client | Commit | Notes |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| V0 baseline | VERIFIED | — | NOT-RUN | NOT-RUN | 24d13f5 | test, clippy, release build, isolated-HOME test green; MSRV 1.95, converter, Flatpak build NOT-RUN (no toolchain / SDK / podman) |
| V1 FS security (P0-A) | | | | | | |
| V2 release (P0-B) | | | | | | |
| V6 integrated regression | | | | | | |

## Platforms

| Platform | State | Evidence |
| :--- | :--- | :--- |
| Linux x86-64 (local, Fedora 44 WSL2) | V0 VERIFIED at 24d13f5 | [V0 baseline](#v0-baseline) |
| Linux x86-64 (GitHub CI runner, ubuntu-latest / ubuntu-22.04) | last CI and Release runs green (see Actions sample) | `gh run list` |
| Windows Server runner (MSVC) | NOT-RUN | the code uses `std::os::unix` in `rpf/src/edit.rs`, `core/src/game.rs` and elsewhere: no Windows build expected before M2 |
| Windows 10 22H2 / 11 client | NOT-RUN | none available |

## CI and release inventory

At `24d13f5` (read-only review).

| Item | State |
| :--- | :--- |
| Workflows | `ci.yml`: push to `main` and every `pull_request`, docs-only paths ignored; one job `check` (clippy `--locked --all-targets`, `cargo test --locked`, desktop/AppStream/MIME validation). `release.yml`: `v*` tags and `workflow_dispatch` (input `tag`); jobs `release` (ubuntu-22.04, push only) and `flatpak` (needs `release`, `concurrency: flatpak-pages`) |
| Permissions | `ci.yml` has no `permissions:` (repository default token); `release.yml` sets `contents: write` for the whole workflow |
| Action pins | all `uses:` pinned to full SHAs with a version comment; `dtolnay/rust-toolchain` comment is `stable` (toolchain floats); `actions/setup-dotnet` `10.0.x` |
| Checkouts | `persist-credentials` default (true) everywhere; the Flatpak job checks out the tag, then `git checkout origin/main -- packaging/` (`release.yml:67-68`): packaging comes from `main`, not from the tag |
| Secrets | `FLATPAK_GPG_PRIVATE_KEY` imported on the runner into `$RUNNER_TEMP/gnupg` (never deleted), bind-mounted read-write into the build container, which runs `--privileged --security-opt label=disable` (`packaging/build-flatpak.sh:21`) together with the whole build (cargo/NuGet source generation, flatpak-builder) and the checkout holding the persisted token |
| Signing | inside the build container: `flatpak-builder --gpg-sign` (commit) and `flatpak build-update-repo --gpg-sign` (summary); the bundle is made from the signed repo (`build-flatpak.sh:28-30`) |
| Publishing | `gh-pages` rebuilt as a single orphan commit and `git push --force` (`release.yml:105-112`), no check of what is published; the release job attaches tarball, AppImage, `.zsync` and `SHA256SUMS` (which covers only those); the Flatpak job attaches the bundle afterwards, outside `SHA256SUMS` |
| Version checks | the release job checks tag = `v` + `Cargo.toml` version (`release.yml:32-35`); a `workflow_dispatch` run skips it; metainfo release, published version and downgrade are not checked |
| Downloads | `appimagetool` from the `continuous` release, not verified, executed (`build-release.sh:63-68`); flatpak-builder-tools generators pinned to a commit and SHA-256 verified; build image `ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-26.08` by tag (no digest); Flathub runtime/SDK `26.08` by branch |
| Caches | `Swatinem/rust-cache` in CI and in the release job; PR caches are scoped to the PR ref (GitHub behaviour, not verified here) |
| Dependabot | GitHub Actions and Cargo weekly; Cargo updates grouped into one PR |
| Repository settings (read-only `gh api`) | default workflow token `read`; Actions may approve PRs; `main` unprotected, no rulesets; one environment (`github-pages`); releases not immutable; Pages = legacy site from `gh-pages` `/` |
| Published Flatpak repo | `gh-pages` = one commit `649395a` "Flatpak repository for v1.5.1" (114 MB): refs `app/…/x86_64/master`, `runtime/….Debug/x86_64/master`, `appstream/x86_64`, `appstream2/x86_64`; `archive-z2`, signed summary; key Ed25519 `FBC1FA631AD56F05322DB693320B3555C885070B` "Paleto Modkit Flatpak"; no published-version record |
| Lint baseline (local) | actionlint 1.7.12: 0 findings; zizmor 1.31.1 `--offline`: 2 high (`excessive-permissions`, `cache-poisoning` in `release.yml`), 4 medium (3 `artipacked`, 1 `excessive-permissions`), 2 info; shellcheck 0.11: 3 warnings in `build-release.sh` (SC2115, SC2097, SC2098), info notes in `build-cwconv.sh`, `build-flatpak.sh` |

## Verification map

Per milestone: module touched → risk → test → result.

| Milestone | Module | Risk | Test | Result |
| :--- | :--- | :--- | :--- | :--- |
| M1 A4a | `crates/rpf/src/edit.rs` (`open` read-only, `open_file`, `add_file_from`, `save_to`, staged `save`) | the opened archive written before commit; torn save; a link at the target written through | `crates/rpf/tests/edit.rs` (hard link keeps old bytes, nested fit/rebuild, AES, symlink replaced not followed, read-only refused, failed save leaves the archive and folder unchanged, `add_file_from`), `save_staged.rs`; security review (opus): 1 Medium (link followed) fixed, Lows fixed or documented | local Linux: `cargo test -p paleto-rpf` 23 passed; `-p paleto-core` 95 passed, 19 ignored, 3 runs |

## Features

From the M3–M5 mandates; none started.

| ID | Feature | Mandate | State | Commit |
| :--- | :--- | :--- | :--- | :--- |
| F01 | Hash Generator & Name Resolver | M3 | not started | |
| F02 | Favorites & Recent Locations | M3 | not started | |
| F03 | CodeWalker / Sollumz / Blender integration | M3 | not started | |
| F04 | Diff original vs modified | M3 | not started | |
| F05 | Thumbnail Generator & Preview Cache | M3 | not started | |
| F06 | Advanced 3D Viewer | M3 | not started | |
| F07 | Global Search + Asset Index | M3 | not started | |
| F08 | Deep Asset Search: YTD & AWC | M3 | not started | |
| F09 | 3D Export GLB & OBJ | M3 | not started | |
| F10 | Smart Installer ZIP/7z/RAR | M4 | not started | |
| F11 | Vehicle Pack Renamer & ELS Support | M4 | not started | |
| F12 | OIV Installation Preview / Dry Run | M4 | not started | |
| F13 | Conflict Detection & Dependency Checker | M4 | not started | |
| F14 | FiveM Resource Exporter | M4 | not started | |
| F15 | GTA V Asset Validator | M4 | not started | |
| F16 | OpenFormats Import/Export | M4 | not started | |
| F17 | Tracked Installations & Safe Uninstall + Mod Registry | M4 | not started | |
| F18 | Mod Profiles | M4 | not started | |
| F19 | YMAP Entity Editor | M5 | not started | |
| F20 | Weapon Studio | M5 | not started | |
| F21 | Clothing Studio | M5 | not started | |
| F22 | Game Update Repair | M5 | not started | |
| F23 | Full World Editor | M5 | not started | |

## Conflicts and decisions

Precedence per the common rules (§2): safety > user > data integrity > release security > repo rules > mandate > common defaults > historical data.

| # | Conflict or decision | Sources | Outcome |
| :--- | :--- | :--- | :--- |
| 1 | Formatting: CI runs no `cargo fmt --check`, the repo forbids `cargo fmt` | repo rules vs CI | no conflict: code is formatted by hand, `cargo fmt` is never run |
| 2 | Repo rule "start `.claude/flatpak-bg.sh` after a set of changes" | repo rules vs this machine | podman/docker are missing here: the Flatpak build cannot run (NOT-RUN, recorded per gate) |
| 3 | Subagent definitions in `.claude/agents/` | common §6.2 vs repo rules | the repo does not say they may be committed: kept local (listed in `.git/info/exclude`) |
| 4 | `TODO.md` | repo rules | the M1 request was added to the local, uncommitted `TODO.md` |
| 5 | Work-package order: the P0-A architect proposed starting A3a and A4a before A1 | mandate agent plan vs design note | mandate wins: A1 → A2 stay sequential (Owner modules `fsroot`, `txn`); only A4a (`crates/rpf` staged save, a leaf crate with disjoint files, not an Owner module) runs alongside A1 |
| 6 | P0-B after P0-A | mandate order vs common §6.1 (parallel disjoint write sets) | P0-B is prepared in its own worktree (`packaging/`, `.github/` only) while P0-A runs; it is merged and gated (V2) only after the P0-A gate (V1); release-graph edits themselves stay sequential (CI-1 scripts, then CI-2 workflows) |
| 7 | P0-A open questions | design note §9 | final link in mods pointing outside: refused; case-duplicate names: exact match, else sorted first + warning; package paths `mods/...` routed into the mods root; links inside mods to another drive: refused (link the whole `mods` folder instead); after `commit` roll back, after `committed` finish; `save(path)` outside a game follows a final symlink; a full copy per edit where the filesystem has no reflinks is accepted, with a free-space check |
| 7a | `save(path)` and a symlink at `path` (revises #7) | security review of A4a (Medium) vs design §9.6 | data integrity wins: `save` replaces the link itself and never writes or replaces its target, because the rpf crate cannot tell whether the link leads into an original game folder |
| 8 | P0-B open questions | design note | prereleases only `-rc.N`, never published to `master`/gh-pages; tag must be on `main`; `gh-pages-previous` kept as backup; release published only after Pages serves the new record (10 min wait); no artifact reuse across runs; the Flatpak build stays rootless-podman `--privileged` until the minimal mode is proven on a runner; SDK commits pinned (release refuses while a pin is empty); no attestations in M1 |
| 9 | Local tools installed for V2 | common §5 (minimum tool for a concrete risk) | `flatpak` 1.18.4 and `ostree` 2026.4 (dnf), actionlint 1.7.12 (release binary, checksum verified), zizmor 1.31.1 (pip, user) — local only; tests use temporary flatpak installations |

## Agents and escalations

| When | Agent | Model / effort | Note |
| :--- | :--- | :--- | :--- |
| 2026-10-29 | all custom agents | — | `.claude/agents/*.md` created this session are only loaded by the next session: this session uses the built-in `Explore` (scouts), `Plan` (architect) and `general-purpose` agents with the roster's model and effort passed per call |

## Pending actions that need authorization

Local work is done; these need the user (common §4).

| Action | Why | Prepared |
| :--- | :--- | :--- |
| MSVC diagnostic build on a GitHub Windows runner | V0 step 4 | needs a push or `workflow_dispatch` |

## Follow-ups

Out of scope for the current mandate; not implemented.

| Item | Found in | Note |
| :--- | :--- | :--- |
| Intermittent: `convert::tests::converts_inside_mods_archives` and `prefix::tests::the_running_check_resolves_links` failed once together under heavy parallel load (A4a worktree), then passed 3 times there and 3 times on `main` | M1 A4a | probably the fork/exec "Text file busy" race that `core::write_script` works around; watch in V6 |
| `.paleto-save-*` temporaries left by a killed `ArchiveEditor::save` | M1 A4a review | to be listed by the leftovers scan (A2) |
