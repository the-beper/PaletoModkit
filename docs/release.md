# Release and recovery

For the maintainer who cuts releases and fixes them when they fail. The workflow is [`.github/workflows/release.yml`](../.github/workflows/release.yml); the scripts are in [`packaging/`](../packaging) (see [development.md](development.md#packaging) for what each one does). The pins are in [`packaging/release.conf`](../packaging/release.conf).

- [One-time setup](#one-time-setup)
- [Cutting a release](#cutting-a-release)
- [The job graph](#the-job-graph)
- [Artifacts and how to verify them](#artifacts-and-how-to-verify-them)
- [The signing key](#the-signing-key)
- [Draft, gh-pages, publish](#draft-gh-pages-publish)
- [Downgrade and old tags](#downgrade-and-old-tags)
- [Broken gh-pages](#broken-gh-pages)
- [Lost signing key](#lost-signing-key)
- [Re-running](#re-running)
- [Partial failures](#partial-failures)
- [How users get updates](#how-users-get-updates)
- [Bumping pins](#bumping-pins)
- [Testing the pipeline locally](#testing-the-pipeline-locally)

Exit codes of every release script: 0 ok or nothing to do, 1 usage or internal error, 2 validation (format, mismatch, missing file, checksum, structure), 3 refused by policy (downgrade, same tag with other content, Flatpak identity change), 4 signing, 5 conflict on the remote (lease lost, gh-pages missing, tag moved, several releases for one tag, Pages ahead of the release).

## One-time setup

These are GitHub settings. Only the maintainer can change them, from the web page of the repository (or with `gh` under the maintainer's own login). The workflow cannot, and a task or an agent must not. Until they are done, the workflow still runs, but its protections are not in place.

| Setting | Where | Value |
| :--- | :--- | :--- |
| Environment `flatpak-signing` | Settings, Environments | Required reviewer: the maintainer. Deployment branches and tags: selected, tag rule `v*` and branch `main`. |
| Signing key | the same environment | Secret `FLATPAK_GPG_PRIVATE_KEY`. Move it here with `gh secret set FLATPAK_GPG_PRIVATE_KEY --env flatpak-signing`. |
| Repository-level secret | Settings, Secrets and variables | Delete `FLATPAK_GPG_PRIVATE_KEY`. This also stops old runs, made before this workflow, from signing when they are re-run. |
| Tag ruleset | Settings, Rules | Target `refs/tags/v*`: restrict updates and deletions. A released tag can never be moved. |
| `main` ruleset | Settings, Rules | Pull request and CI required. Restrict deletion of `gh-pages` and `gh-pages-previous` too. |
| Actions approving pull requests | Settings, Actions, General | Turn off "Allow GitHub Actions to create and approve pull requests". |
| Immutable releases | Settings, General | Turn on. A published release can no longer get other files. |
| Pages | Settings, Pages | Deploy from `gh-pages`, root. Set it after the first run. |

The first release also needs the first publication: run the workflow by hand with `bootstrap_pages` (see [Broken gh-pages](#broken-gh-pages)).

## Cutting a release

The tag must point to a commit that is on `main`. `release-validate.sh` refuses anything else.

1. Bump the version to `X.Y.Z` (or `X.Y.Z-rc.N` for a candidate) in all three places. The check compares every one with the tag:

   | File | What |
   | :--- | :--- |
   | `Cargo.toml` | `[workspace.package] version`. Every crate must use `version.workspace = true`. |
   | `Cargo.lock` | Every `paleto-*` package at the same version (run `cargo build` to update it). |
   | `packaging/io.github.the_beper.PaletoModkit.metainfo.xml` | A new first `<release version="X.Y.Z" date="YYYY-MM-DD">`. A candidate needs `type="development"`, a stable release must not have it. A stable release replaces the `<release>` entries of its own candidates: AppStream sorts `X.Y.Z-rc.N` above `X.Y.Z`, and CI's `appstreamcli validate` fails with `releases-not-in-order`. |

2. Merge it to `main` through a pull request, with CI green.
3. Tag and push:

   ```bash
   git switch main && git pull
   git tag -a vX.Y.Z -m "Paleto Modkit X.Y.Z"
   git push origin vX.Y.Z
   ```

   The tag is `vMAJOR.MINOR.PATCH`, or `-rc.N` after it. Nothing else (`v1.6`, `v1.6.0-beta.1`) is accepted. A tag push is always a release.
4. Watch the run. The builds take most of the time. When `sign-flatpak` waits, open the run and approve the deployment to `flatpak-signing`. Check first that the run is yours and the tag is the one you pushed.
5. When `publish` ends, check the release page and `flatpak remote-info` as in [the next section](#artifacts-and-how-to-verify-them).

A release candidate becomes a GitHub prerelease. It never touches `gh-pages`, and its Flatpak repository is a fresh one of its own.

To try the builds without publishing, start the workflow by hand from `main` with the tag and `mode: build-only` (the default). The graph ends at `verify`.

## The job graph

Every job checks out the commit the tag points to. Nothing comes from `main`. Only `publish` can write to the repository.

| Job | Runs on | What it does | Secret or write access |
| :--- | :--- | :--- | :--- |
| `validate` | ubuntu-24.04 | Checks the tag, the versions in the three files, the submodule, the pins and the downgrade rules. Decides the plan: `build`, `publish` or `noop`. | none |
| `build-linux` | ubuntu-22.04 | `cargo test --locked`, then `build-release.sh`: tarball, AppImage and `.zsync`. Skipped in `publish-flatpak` mode. | none |
| `build-flatpak` | ubuntu-24.04 | `build-flatpak.sh` with `EXPORT_REPO=1`: the unsigned OSTree repository, in the pinned Flathub image. | none |
| `verify` | ubuntu-24.04 | `release-verify.sh builds`: structure and hashes of everything built; writes `release-manifest.txt`. | none |
| `prepare-flatpak` | ubuntu-24.04 | Reads the build and `gh-pages` (the untrusted data), checks the version rules, adds the new unsigned commits on top of the published repository, makes the summary. | none |
| `sign-flatpak` | ubuntu-24.04 | First `flatpak-sign.sh --check-prepared`, without the secret: unpacks the prepared repository, checks its refs and the content checksums of the app and Debug commits against `release-manifest.txt` from `verify`. Then `--sign` (the only step with the key): signs the heads of the four named refs and the summary, and fails if a head moved. It builds nothing and makes no bundle. | environment `flatpak-signing` (approval) |
| `finish-flatpak` | ubuntu-24.04 | Verifies the signed repository with the public key only, on a machine that never held the key, and builds the bundle. | none (read-only, no environment) |
| `publish` | ubuntu-24.04 | Needs `finish-flatpak`. `release-publish.sh`: the draft release with its files, then `gh-pages`, then the release itself. One at a time (`concurrency: release-publish`). | `contents: write` |

Jobs that parse untrusted data (`verify`, `prepare-flatpak`, `finish-flatpak`) run on other virtual machines than the one with the key. The Windows build is a slot for a later milestone.

Modes of a manual run:

| Mode | Does |
| :--- | :--- |
| `build-only` (default) | Builds and verifies. Publishes nothing. |
| `release` | The full release of an existing tag. |
| `publish-flatpak` | Only `gh-pages`, for a release that is published already and is the highest. No Linux build, the release is not touched. |

A manual run must start from `main`.

## Artifacts and how to verify them

A release has five files:

| File | What |
| :--- | :--- |
| `PaletoModkit-x86_64.tar.gz` | The tarball with `install.sh`. |
| `PaletoModkit-x86_64.AppImage` and `.zsync` | The AppImage and its update data. |
| `PaletoModkit-x86_64.flatpak` | The Flatpak bundle, signed with the repository key. |
| `SHA256SUMS` | The SHA-256 of the four files above, sorted. |

The files are built on GitHub's runners and are not byte-reproducible: a rebuild of the same tag is a new build, not the same one.

The run also keeps workflow artifacts for 14 days (30 for the Linux build). The ones to know when you debug a run:

| Artifact | Made by | What |
| :--- | :--- | :--- |
| `verified-<commit>` | `verify` | `release-manifest.txt`, the checksums every later check compares with. |
| `flatpak-prepared-<commit>` | `prepare-flatpak` | The unsigned repository with the new commits. `sign-flatpak` runs `--check-prepared` on it before the key is used. |
| `flatpak-sign-result-<commit>` | `sign-flatpak` | The signed repository and the exported public key. `finish-flatpak` verifies it. |
| `flatpak-signed-<commit>` | `finish-flatpak` | The verified repository and the bundle. `publish` uploads from it. |

A user checks them like this:

```bash
sha256sum --strict -c SHA256SUMS
flatpak install --user PaletoModkit-x86_64.flatpak        # refuses a bundle whose signature does not verify
flatpak remote-info --log paletomodkit io.github.the_beper.PaletoModkit
```

`remote-info --log` lists the commits of the repository, newest first. Each is named `Paleto Modkit X.Y.Z (vX.Y.Z)` and its body says `source <commit>`, the commit of the tag. `https://the-beper.github.io/PaletoModkit/release.txt` records the tag, the source commit, the OSTree commits, the hash of `SHA256SUMS` and the key fingerprint of the repository.

## The signing key

| | |
| :--- | :--- |
| Fingerprint | `FBC1FA631AD56F05322DB693320B3555C885070B` (the same as `FLATPAK_KEY_FPR` in `release.conf`) |
| Private key | Only in the secret `FLATPAK_GPG_PRIVATE_KEY` of the environment `flatpak-signing`, and in an offline backup of the maintainer. Never in the repository, never in the build jobs. |
| Public key | In `paleto-modkit.flatpakrepo` on GitHub Pages, which every user added the remote with. |

`flatpak-sign.sh --sign` imports the key into a private keyring in memory (`/dev/shm/paleto-gnupg.*`), stops with exit 4 if its fingerprint is not the pinned one, signs, and removes the keyring. An `always()` step runs `--cleanup`, which removes any keyring left over.

There is no rotation. Clients trust the key in the file they added the remote with, and the scripts refuse any other key (exit 3 in `--prepare`, exit 4 in `--sign`). A new key means a new remote: see [Lost signing key](#lost-signing-key).

## Draft, gh-pages, publish

`release-publish.sh` always does the same three steps, in this order. After any failure it reads the remote again and continues, replaces or refuses.

1. **Draft release.** It creates a draft (no draft is public), uploads `SHA256SUMS` first and then the files, and checks name, size and SHA-256 of each asset. A draft with another build's `SHA256SUMS` is deleted and made again (the tag stays).
2. **gh-pages** (stable tags only, and only if the published version is lower). It makes one new orphan commit with the signed repository, `release.txt` and the two remote files, and pushes it atomically with leases:

   ```text
   gh-pages            = the new commit
   gh-pages-previous   = the old gh-pages
   ```

   The `.flatpakrepo` and `.flatpakref` must be byte-identical to the published ones (exit 3 otherwise). Then it waits up to `PAGES_WAIT` (600 s) until the site serves `release.txt` for this tag.
3. **Publish.** `gh release edit --draft=false --latest` (a candidate: `--prerelease --latest=false`).

How to read a partial state:

| You see | It means | Next |
| :--- | :--- | :--- |
| A draft with some or all files, `gh-pages` still at the old version | The job stopped in step 1 or before the push. Nothing is public. | Re-run the `publish` job. |
| A complete draft, `gh-pages` at this tag, release still a draft | The job stopped in the wait or before the edit. Users of the Flatpak remote already get the update. | Re-run the `publish` job: it skips the push and publishes the draft. |
| Published release, `gh-pages` still old | Only `publish-flatpak` mode leaves this, or a hand edit. | Run the workflow in `publish-flatpak` mode for the tag. |
| `gh-pages` at this tag, no draft or another one | Pages is ahead of the release (exit 5). | See [Partial failures](#partial-failures). |

## Downgrade and old tags

The release is refused with exit 3, in `validate` and again in `publish`, when:

- a stable tag is lower than the highest published stable release, or than the version on `gh-pages`;
- a candidate is not above every published release and the version on `gh-pages`;
- the tag is already on `gh-pages` for another commit, or is published with other files than this build;
- the Flatpak id, branch or remote files would change.

The Flatpak history is a chain: each commit has the published one as parent, and an older commit on top would be refused by clients. The answer to a bad release is always a new, higher version. There is no rollback mode. A user can go back locally with `flatpak update --commit`.

Tag the same version again after a failure only if nothing was published. An equal tag with the same commit and a complete release is a no-op (`plan=noop`, exit 0).

## Broken gh-pages

Every release keeps the previous state in the branch `gh-pages-previous`. If `gh-pages` is damaged (it fails to clone, `release.txt` is unreadable, the exit codes 5 and 2 come from `release-pages.sh`):

1. Restore the backup, with a lease on what you saw. Use your own credentials, from a clone:

   ```bash
   git fetch origin gh-pages gh-pages-previous
   git push --force-with-lease=gh-pages:<broken-sha> origin origin/gh-pages-previous:gh-pages
   ```

2. Start the workflow by hand from `main`, with the highest published tag and `mode: publish-flatpak`. It signs the new version on top of the restored repository and pushes `gh-pages` again. The release itself is not touched.

Bootstrap only if both branches are gone. Then run `publish-flatpak` (or, for the very first release, `release`) with `bootstrap_pages` ticked. It creates `gh-pages` from nothing, with the pinned key, and refuses to run if `gh-pages` exists. Users keep their remote: the files and the key are the same. Their next update starts a new history, which `flatpak update` takes as a plain update.

## Lost signing key

Treat it as an incident, not as a release.

| Case | What it means |
| :--- | :--- |
| Backup exists | Put the key back in the environment secret. Nothing else changes. |
| Key lost, no backup | Nothing can be signed with the published key, and the pipeline refuses any other (exit 4). |
| Key leaked | Remove the secret from the environment first, then proceed as for a lost key. Whoever has it can sign commits clients accept. |

With a new key the Flatpak repository is a new remote, because clients trust only the key they were given. The pipeline does not support that case today: it needs a change of `FLATPAK_KEY_FPR` and of the identity rules in `release.conf` and the scripts, in a reviewed pull request. Then:

1. Publish a notice (README, the release page, an issue) that says the old remote is dead.
2. Give users the new commands: remove the old remote, add the new `.flatpakrepo`, install again (see [installation.md](installation.md#flatpak)). User data in `~/.var/app/io.github.the_beper.PaletoModkit` is not touched.
3. Keep the release files (tarball, AppImage, bundle) as they are: their checksums do not depend on the repository key.

## Re-running

| Button | Use it when |
| :--- | :--- |
| Re-run failed jobs | A job failed for a cause outside the code (network, runner, timeout), or `publish` stopped halfway. The artifacts of the same run are reused, so it is the same build. |
| Re-run all jobs | The remote changed since the signing: `gh-pages` moved ("run all jobs again", exit 5), or a run stayed behind another one. Everything is built, verified and signed again; it needs a new approval. |

Rules:

- `publish` runs one at a time (`release-publish` group). GitHub keeps only one pending run per group. When a third publication queues while one runs and one waits, the waiting one is cancelled. Its release stays a draft and `gh-pages` is untouched. Re-run it after the others have finished.
- Do not re-run a run made before this workflow (for example the one of v1.5.1). It uses an old workflow, and the repository-level secret is gone, so it cannot sign. If it somehow could, it would fail on the downgrade rules anyway.
- To build a published tag again, do not re-run it: a complete release is a no-op, and a different build of the same tag is refused (exit 3). Release a higher version.

## Partial failures

| State | Exit | What to do |
| :--- | :--- | :--- |
| `validate` fails on a version, date or submodule | 2 | Fix the file on `main`. The tag is wrong: if it was never published, delete it (the tag ruleset blocks this, so lift it for the moment) and tag again; else release a higher version. |
| `validate` says the tag is not on `main` | 2 | Merge to `main`, tag the merge commit. |
| A pin in `release.conf` is empty or malformed | 2 | Fix the pin ([Bumping pins](#bumping-pins)). |
| A build job fails | - | Nothing later runs, the remote is unchanged. Re-run failed jobs, or fix and release a higher version. |
| `verify` fails (missing file, checksum, converter missing from the tarball) | 2 | The build is wrong, not the pipeline. Fix, release a higher version. |
| `sign-flatpak` not approved, or rejected | - | Nothing was published. Re-run the job and approve. |
| `sign-flatpak` fails in `--check-prepared` (refs or checksums differ from the manifest) | see log | The prepared repository is wrong. Nothing was signed. Re-run all jobs; if it repeats, release a higher version. |
| `sign-flatpak` fails because a head moved | see log | The repository changed between the check and the signing. Nothing was published. Re-run all jobs. |
| `finish-flatpak` fails (signature or content does not verify) | see log | Do not publish. Re-run all jobs; if it repeats, treat it as an incident. |
| `sign-flatpak` fails on the key | 4 | The secret is empty or is not the pinned key. Put the right one in the environment, re-run failed jobs. |
| Draft partly uploaded | 1 or 5 | Re-run `publish`: it deletes wrong or incomplete assets and uploads the missing ones. |
| `gh-pages` changed since signing | 5 | Someone else wrote to it. Check why, then Re-run all jobs. Nothing was written. |
| Tag moved on the remote | 5 | A tag must never move. Find out who moved it; release a higher version. |
| Several releases for one tag | 5 | Delete the extra ones by hand (keep the right draft or the published one), re-run. |
| Pages wait timed out | 5 | The push worked but the site is slow. The draft stays. Re-run `publish`. |
| Pages ahead of the release (gh-pages at the tag, draft missing or another build) | 5 | Do not push again. If the draft is missing and the build is the right one, re-run the original run's `publish`. Else the repository serves a build with no release: release a higher version. |
| Published but mismatched with the record | 3 | Do not re-run. Release a higher version. |
| Publication cancelled while pending | - | The release is a draft. Re-run after the other runs finish. |

## How users get updates

`flatpak update` (or the software center) pulls the new commit from the remote and checks its signature and the signed summary. The new commit has the old one as its parent.

There are no static deltas. A delta carries the signature of its commit, so it can only be made after signing, and the signed summary would have to list it and be signed again. Making them with the key at hand would mean reading every file of the build in the signing job. So an update downloads the objects that changed (compressed files), not a delta. Files that did not change are not downloaded again. The first install downloads everything.

`gh-pages` is replaced by one commit each release, so OSTree objects do not pile up in git history. Old OSTree commits are pruned to a depth of 3.

## Bumping pins

All are bumped by hand, in a pull request, and then tried with `build-only` (see [Cutting a release](#cutting-a-release)). Actions are pinned to commit SHAs and updated by Dependabot.

| Pin | Where | Re-check |
| :--- | :--- | :--- |
| Build image (digest) | `FLATPAK_IMAGE` in `release.conf` | `skopeo inspect --format '{{.Digest}}' docker://ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-26.08` |
| Flathub SDK, Rust and .NET extensions | `SDK_COMMIT`, `RUST_EXT_COMMIT`, `DOTNET_EXT_COMMIT` | `flatpak remote-info --user --show-commit flathub org.freedesktop.Sdk//26.08` (use a throwaway `FLATPAK_USER_DIR`; the same for the `rust-stable` and `dotnet10` extension ids) |
| appimagetool | `APPIMAGETOOL_URL`, `APPIMAGETOOL_SHA256` | Download into an empty folder, run `sha256sum`; compare with the `digest` of `gh api repos/AppImage/appimagetool/releases/tags/<tag> --jq '.assets[] \| [.name,.digest]'` |
| AppImage runtime | `APPIMAGE_RUNTIME_URL`, `APPIMAGE_RUNTIME_SHA256` | The same, for `AppImage/type2-runtime`. appimagetool 1.9 or later fetches its own runtime unless given one, so it is pinned too. |
| Rust | `RUST_VERSION` in `release.yml` | The Flatpak uses the SDK extension, so check that both build with the same version |
| .NET SDK | `DOTNET_VERSION` in `release.yml` | The same with `DOTNET_EXT_COMMIT` |

An empty or malformed hash or commit makes `release-validate.sh` stop with exit 2. `fetch-pinned.sh` stops with exit 2 when a download does not match its hash, and runs nothing it did not check. The Flatpak build updates the SDK to the pinned commits and checks them with `flatpak info --show-commit`. The Flatpak id (`io.github.the_beper.PaletoModkit`), the branch `master` and the key fingerprint are not pins: do not change them.

## Testing the pipeline locally

Both tests need `flatpak`, `ostree` and `gpg` (and `git`, `jq`, `tar`, `curl` for the first); they print `SKIP` and exit 77 when something is missing. They use throwaway keys and folders, no network and no GitHub, and never touch your real Flatpak installation or home. In the simulation `RELEASE_SIM=1` makes the scripts refuse a remote, repository or site on GitHub.

```bash
sh packaging/tests/release-sim.sh            # the whole pipeline against a local remote and a stub of gh
sh packaging/tests/release-sim.sh -l         # list the scenarios
sh packaging/tests/release-sim.sh <name>     # one scenario
sh packaging/tests/flatpak-repo-test.sh      # signing, install, update and tampering with the real flatpak client
```

`KEEP=1` keeps the scratch folder and `VERBOSE=1` prints the log of a failed scenario. The simulation uses synthetic build output, so it does not prove the container build or the real appimagetool. The repository test does not prove the build with the Flathub SDK, runtime dependencies, the HTTP serving of GitHub Pages (a local `http.server` stands in) or the software center. `build-only` runs on a real runner are the proof for the builds.
