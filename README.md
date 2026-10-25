# Paleto Modkit

<p align="center">
  <img src="packaging/icons/hicolor/256x256/apps/io.github.the_beper.PaletoModkit.png" width="112" alt="Paleto Modkit Icon" />
</p>

<p align="center">
  <b>Native Linux tool for Grand Theft Auto V modding, Legacy and Enhanced editions.</b><br>
  Browse and edit the game's archives, install mods, get .NET script mods running under Proton, and find out why the game broke. Native, no Wine needed for the tool itself.
</p>

<p align="center">
  <a href="https://github.com/the-beper/PaletoModkit/releases"><img src="https://img.shields.io/badge/Download-Flatpak-3b7086?logo=flatpak&logoColor=white" alt="Flatpak" /></a>
  <a href="https://github.com/the-beper/PaletoModkit/releases"><img src="https://img.shields.io/badge/Download-AppImage-16a085?logo=linux&logoColor=white" alt="AppImage" /></a>
  <a href="https://github.com/the-beper/PaletoModkit/actions/workflows/ci.yml"><img src="https://github.com/the-beper/PaletoModkit/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <img src="https://img.shields.io/badge/License-GPL--3.0-blue" alt="License" />
</p>

> [!IMPORTANT]
> PaletoModkit began as a project for my own personal use, and is shared as-is. Claude Code assisted me in writing the code and drafted the documentation and diagrams.

![Paleto Modkit: archive browser with a 3D model preview](screenshots/archive-browser.png)

## What it does

**Browse and edit**
- Open `.rpf` archives like folders, nested ones included; add, extract, rename and delete files.
- Edit XML and `.meta` files, and binary metadata (`.ymt`, `.ytyp`, `.ymap`, `.pso`) as XML.
- View, export and replace textures (`.ytd`) from DDS or PNG; preview 3D models; play and replace sounds.

**Install mods**
- Install `.oiv` packages and add-on packs (`dlc.rpf`), with the DLC list updated for you.
- On Enhanced, convert models and textures made for Legacy, built in (CodeWalker's converter, no Wine).
- Export your own changes as an `.oiv`, to share them or reinstall them later.

**Script mods on Linux**
- **Prefix tweaks**: many script mods are written for ScriptHookVDotNet (a `scripts` folder with `.dll` files) and need the real **.NET Framework 4.8** in the game's Wine prefix. Installing it by hand fails in a Proton prefix. Prefix tweaks does it for you: it backs the prefix up, installs .NET with the Proton's own Wine, sets the Windows version back so the game still starts, and checks the result. **Restore** undoes it. Tried so far with one Proton build (in the Flatpak it runs Wine on the host, with a permission it asks for). See [Prefix tweaks](docs/mods.md#scripthookvdotnet-mods-net-on-linux).

**When the game breaks**
- **Mods in game**: an inventory of your mods with an on/off switch each, is the ASI loader in place, which mods did the game actually read.
- **Script Hook V**: installed, latest and game version side by side, with a one-click update of `ScriptHookV.dll`. Installed, it is also checked against dev-c.com at start ([what is sent](docs/troubleshooting.md#script-hook-v)).
- **Crash detection**: the likely causes of a crash with a fix for each (down to a full asset store and the pack that was loading), the crash history, and a search that turns mods off by halves until the culprit is left.
- **Snapshots** of the mods folder, instant on btrfs and XFS.

Most of it also works from a terminal, with the `paleto` command (in the Flatpak and the tarball, not in the AppImage):

```bash
paleto check            # will the game load my mods? what did it read last time?
paleto crash            # what does the last crash say?
paleto export my.oiv    # my changes to the game, as an OIV package
```

## Gallery

| Texture Dictionary Viewer (`.ytd`) | Binary Metadata as XML (`.ymt`) |
| :---: | :---: |
| ![Texture Dictionary Viewer](screenshots/texture-viewer.png) | ![Metadata as XML](screenshots/xml-editor.png) |
| **Mods in game** | **OpenIV Package Installer (`.oiv`)** |
| ![Mods in game](docs/images/mods-in-game.png) | ![OIV Package Installer](screenshots/oiv-installer.png) |
| **Audio Preview (`.awc`)** | **Crash detection** |
| ![Audio Preview](screenshots/audio-preview.png) | ![Crash detection](docs/images/crash-detection.png) |
| **Prefix tweaks: .NET for script mods** | **Snapshots of the mods folder** |
| ![Prefix tweaks](docs/images/prefix-tweaks.png) | ![Snapshots](docs/images/snapshots.png) |

## Install

The AppImage and the Flatpak bundle are on [GitHub Releases](https://github.com/the-beper/PaletoModkit/releases):

```bash
# Flatpak from the signed repository
flatpak remote-add --if-not-exists PaletoModkit https://the-beper.github.io/PaletoModkit/paleto-modkit.flatpakrepo
flatpak install io.github.the_beper.PaletoModkit

# AppImage
chmod +x PaletoModkit-x86_64.AppImage && ./PaletoModkit-x86_64.AppImage

# Tarball
tar -xzf PaletoModkit-x86_64.tar.gz && cd paleto-modkit-* && ./install.sh
```

A tarball and building from source are covered in [docs/installation.md](docs/installation.md).

**You need**: Linux on x86-64 with a Wayland or X11 desktop, and GTA V (Legacy or Enhanced) installed from Steam, or any other install you point the app to. To play the mods you also need an ASI loader and a plugin that reads the `mods/` folder, as [below](#getting-started).

## Getting started

1. **Open the app.** It finds GTA V in your Steam libraries, and reads the encryption keys from the game the first time.
2. **Set up the game to load mods**: an ASI loader plus `OpenIV.asi` (Legacy) or [`OpenRPF.asi`](https://www.gta5-mods.com/tools/openrpf-openiv-asi-for-gta-v-enhanced) (Enhanced). Under Proton, add `WINEDLLOVERRIDES="xinput1_4=n,b" %command%` (or `dinput8=n,b`) to the game's launch options. [Details](docs/installation.md#what-the-game-needs-to-load-your-mods).
3. **Install a mod**: drop its `.oiv` or `dlc.rpf` on the window, or use **Install package…** at the top right.
4. **Check it**: **Tools → Mods in game** tells you whether the game will load it and, after you play, whether it did.
5. **Script mods (`scripts/*.dll`, ScriptHookVDotNet)**: close the game and use **Prefix tweaks…** (the wine-red button beside Install) once to put .NET Framework 4.8 in the game's prefix; then copy ScriptHookVDotNet and the mods to the game folder. [Details](docs/mods.md#scripthookvdotnet-mods-net-on-linux).

## Documentation

| Page | For |
| :--- | :--- |
| [Installation](docs/installation.md) | Flatpak, AppImage, tarball, building from source; the encryption keys; what the game needs to load mods |
| [User guide](docs/user-guide.md) | The window, browsing and editing archives, text and XML, metadata, textures, models, audio, shortcuts |
| [Installing and managing mods](docs/mods.md) | The mods folder, OIV packages, add-on packs, Legacy → Enhanced conversion, ScriptHookVDotNet mods on Linux, exporting your mods, clean-up |
| [When the game breaks](docs/troubleshooting.md) | Mods in game, crash detection, Script Hook V, snapshots, common problems |
| [Command line](docs/cli.md) | Every `paleto` command, with examples |
| [OIV packages](docs/oiv.md) | The package actions Paleto Modkit supports, and where changes go |
| [FAQ](docs/faq.md) | What it is and is not, what has not been tried in game, common questions |
| [Development](docs/development.md) | Project layout, building, tests, screenshots, packaging |

Found a problem? [Open an issue](https://github.com/the-beper/PaletoModkit/issues/new/choose): the form asks for what helps most (edition, how you installed it, `paleto check`). Pull requests are welcome; start with [CONTRIBUTING.md](CONTRIBUTING.md).

## Credits

Paleto Modkit stands on [CodeWalker](https://github.com/dexyfex/CodeWalker) (the RPF and metadata formats, the key derivation and the Legacy → Enhanced converter, which it runs as `paleto-cwconv`), on [gta-toolkit](https://github.com/Neodymium146/gta-toolkit), and on OpenIV's package format. [THIRD_PARTY.md](THIRD_PARTY.md) lists everything it uses, with the licences.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE), and [THIRD_PARTY.md](THIRD_PARTY.md) for the third-party code and data it uses.

*Paleto Modkit is an independent open-source project, not affiliated with or endorsed by Rockstar Games, Take-Two Interactive or the OpenIV team.*
