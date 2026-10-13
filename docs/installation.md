# Installation

Paleto Modkit runs on x86-64 Linux, on a Wayland or X11 desktop (I use and test it on KDE Plasma with Wayland). Every package holds the desktop app (`paleto-modkit`), the command-line tool (`paleto`) and the Legacy → Enhanced converter (`paleto-cwconv`, with its own .NET runtime). Download them from [GitHub Releases](https://github.com/the-beper/PaletoModkit/releases).

- [Flatpak](#flatpak)
- [AppImage](#appimage)
- [Tarball](#tarball)
- [Build from source](#build-from-source)
- [First run: the encryption keys](#first-run-the-encryption-keys)
- [What the game needs to load your mods](#what-the-game-needs-to-load-your-mods)

## Flatpak

The bundle takes its runtime from Flathub, so the Flathub remote must be configured (it is on most distributions):

```bash
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user PaletoModkit-x86_64.flatpak
flatpak run io.github.the_beper.PaletoModkit
```

### Folders the sandbox can see

The sandbox sees the places where Steam libraries usually are:

- `~/.local/share/Steam` and `~/.steam` (native Steam),
- `~/.var/app/com.valvesoftware.Steam/data/Steam` (Flatpak Steam),
- `~/Games`,
- other drives under `/mnt`, `/media` and `/run/media`.

Files elsewhere can still be opened from the file chooser or dropped on the window, through the desktop portal. For a game installed somewhere else, grant access to its Steam library once:

```bash
flatpak override --user --filesystem="/path/to/SteamLibrary" io.github.the_beper.PaletoModkit
```

**One permission to know about**: the Flatpak is allowed to talk to `org.freedesktop.Flatpak`, which lets it run commands on your host through `flatpak-spawn --host`. It uses that for one thing, **Prefix tweaks**: Proton's Wine starts 32-bit processes, which the sandbox forbids and has no libraries for, so the Flatpak runs Wine on the host, with your Proton, exactly as the AppImage would. Nothing else in the app runs host commands. If you prefer to keep the sandbox closed, remove the permission:

```bash
flatpak override --user --no-talk-name=org.freedesktop.Flatpak io.github.the_beper.PaletoModkit
```

Prefix tweaks then says it is not available, and the AppImage or the tarball do the same job.

The command-line tool is inside the Flatpak too:

```bash
flatpak run --command=paleto io.github.the_beper.PaletoModkit check
```

## AppImage

```bash
chmod +x PaletoModkit-x86_64.AppImage
./PaletoModkit-x86_64.AppImage
```

On first run the AppImage adds itself to the application menu, unless Paleto Modkit is also installed as a Flatpak or from the tarball. If you delete the AppImage, delete `~/.local/share/applications/io.github.the_beper.PaletoModkit.desktop` too.

The AppImage starts the app only: it has no way to run the `paleto` command-line tool. For that, use the Flatpak or the tarball.

If it does not start, your system may lack FUSE 2 (`libfuse2` on Debian and Ubuntu, `fuse2` on Fedora and Arch). Install it, or run the AppImage without FUSE: `./PaletoModkit-x86_64.AppImage --appimage-extract-and-run`.

## Tarball

The tarball installs into your home folder, with the desktop entry, the icons and the file associations for `.rpf` and `.oiv` files:

```bash
tar -xzf PaletoModkit-x86_64.tar.gz
cd paleto-modkit-*
./install.sh             # installs to ~/.local
./install.sh --uninstall # removes it
```

`paleto` ends up in `~/.local/bin`; make sure that folder is in your `PATH`.

## Build from source

You need Rust 1.95 or later and the usual build tools. The converter also needs the .NET 10 SDK, or podman (the build script then uses Microsoft's .NET SDK container). The converter's source is a git submodule, so clone with `--recurse-submodules` (or run `git submodule update --init` in an existing clone):

```bash
git clone --recurse-submodules https://github.com/the-beper/PaletoModkit.git
cd paleto-modkit
cargo build --release
./packaging/build-cwconv.sh   # optional: Legacy -> Enhanced conversion
./packaging/install.sh
```

Without the converter everything else works; Legacy files are then only reported, not converted.

See [development.md](development.md) for the project layout, the tests and the packaging scripts.

## First run: the encryption keys

The game's archives are encrypted. Paleto Modkit does not ship the keys; it finds them on your computer the first time it needs them:

1. It reads the AES key from your `GTA5.exe` or `GTA5_Enhanced.exe`.
2. It downloads CodeWalker's `magic.dat` table once, checks it against a pinned SHA-256 hash, and derives the NG key tables from it and the executable.
3. It saves the result in `~/.local/share/paleto/keys.bin` (inside the Flatpak: `~/.var/app/io.github.the_beper.PaletoModkit/data/paleto/`), so this happens once.

`paleto setup` does the same from the terminal. The network is also used by **Prefix tweaks**, when you ask it to install .NET (see [mods.md](mods.md#scripthookvdotnet-mods-net-on-linux)); Paleto Modkit does nothing else online and has no telemetry.

## What the game needs to load your mods

Paleto Modkit never changes the game's own files: it writes your changes to the `mods/` folder inside the game folder. The game only reads that folder with two extra pieces, which you install yourself:

| Edition | ASI loader | Plugin that reads `mods/` |
| :--- | :--- | :--- |
| Legacy | `dinput8.dll` (from [Script Hook V](http://www.dev-c.com/gtav/scripthookv/)) | `OpenIV.asi` (from OpenIV's ASI manager) |
| Enhanced | `xinput1_4.dll` or `dinput8.dll` (an Ultimate ASI Loader build) | [`OpenRPF.asi`](https://www.gta5-mods.com/tools/openrpf-openiv-asi-for-gta-v-enhanced) |

Under Proton, a loader DLL is only used when Wine is told to prefer it. In the game's Steam launch options:

```
WINEDLLOVERRIDES="xinput1_4=n,b" %command%
```

(with `dinput8=n,b` for `dinput8.dll`; add `ScriptHookV=n,b` when you use Script Hook V).

**Game → Mods in game** then checks that everything is in place; see [troubleshooting.md](troubleshooting.md#mods-in-game).
