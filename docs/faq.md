# FAQ

- [What it is, and what it is not](#what-it-is-and-what-it-is-not)
- [Is it safe for my game and my account?](#is-it-safe-for-my-game-and-my-account)
- [What has not been tried in game](#what-has-not-been-tried-in-game)
- [Questions](#questions)

## What it is, and what it is not

Paleto Modkit is a native Linux tool for the files of GTA V: it opens the game's archives, changes them in a copy, installs mods packaged for OpenIV, converts what was made for the Legacy edition to the Enhanced one, and helps find out why a modded game does not start or crashes.

It is **not** a replacement for everything on Windows:

- it is not a 3D map editor like CodeWalker, and not a model editor (use Blender with Sollumz for that); it previews models and edits their textures;
- it does not install or run the mods themselves: the game loads them through an ASI loader and `OpenIV.asi` (Legacy) or `OpenRPF.asi` (Enhanced), which you install;
- it is not a launcher and does not start the game.

## Is it safe for my game and my account?

- **Your game files**: the original archives are never modified. Every change goes to a copy in the `mods/` folder, and deleting that folder undoes everything. **Game → Snapshots of the mods folder** keeps a state you can come back to.
- **Your account**: use it for single player only. Modified files in GTA Online can get an account banned, whichever tool made them.
- **Keys and copyright**: Paleto Modkit ships no game data and no keys. It reads the AES key from your own game executable, and downloads once CodeWalker's `magic.dat` table (checked against a pinned SHA-256) to derive the rest. See [installation.md](installation.md#first-run-the-encryption-keys).
- **The network**: it is used for those keys, and for **Prefix tweaks**, which downloads winetricks and Microsoft's .NET installers when you ask for them. Nothing else, and there is no telemetry.

## What has not been tried in game

I developed it and tried it in game on one machine: KDE Plasma on Wayland, Proton, mostly the Enhanced edition. The file formats are checked more widely: every file of both editions was read, rebuilt and compared. What I have **not** verified in game:

- **Conversion of models and particles** (`.ydr`, `.ydd`, `.ypt`) from Legacy to Enhanced. Textures (`.ytd`) and vehicles (`.yft`) were tried in game; the others go through the same converter and have not been tried.
- **Audio data** (`.rel`) made for Legacy on the Enhanced edition. It is not converted, and whether the game reads it is not known.
- **Binary metadata** saved from the XML view: it is rebuilt from the XML. Of the 693 PSO files of the game, 676 come back byte for byte; the other 17 come back equivalent but not identical. No game problem has pointed there, but take a snapshot before editing binary metadata.
- **Prefix tweaks** (installing .NET Framework 4.8 for ScriptHookVDotNet mods): tried with one Proton build (Wine 11) and the native build of the app, not with other Proton versions. In the Flatpak it runs Wine on the host (`flatpak-spawn --host`), because the sandbox forbids the 32-bit processes Proton's Wine starts; that needs the Flatpak's permission to talk to `org.freedesktop.Flatpak` (see [installation.md](installation.md#flatpak)).
- **The Legacy edition in game**: its archives and formats are tested like the Enhanced ones, but most of the testing in game was on Enhanced.
- **Other launchers and distributions**: Steam's folders are found by themselves; a game from another launcher can be opened with **Game → Choose the game folder…**, but the crash report, **Mods in game** and **Prefix tweaks** look for Steam's `compatdata` folder to find the game's logs and prefix.

If one of these fails for you, [open an issue](https://github.com/the-beper/PaletoModkit/issues/new/choose) with the details: that is the most useful thing you can do.

## Questions

**Does it work on Windows or macOS?** No. It is a Linux app, with Steam, Proton and Wayland in mind. On Windows, OpenIV and CodeWalker do the same jobs.

**Does it need Wine?** No: the app and the converter are native Linux programs. Only the game runs through Proton, and only **Prefix tweaks** touches the game's Wine prefix.

**Why is nothing loaded in game after I installed a mod?** The game ignores the `mods/` folder without an ASI loader and `OpenIV.asi` / `OpenRPF.asi`, and under Proton the loader needs a DLL override. **Game → Mods in game** checks all of that; see [installation.md](installation.md#what-the-game-needs-to-load-your-mods) and [troubleshooting.md](troubleshooting.md).

**The game crashes at startup with a mod installed. What now?** **Game → Last crash** reads the crash report and often names the cause: a full asset store, a plugin, a pack that was loading. If it does not, **Game → Find the crashing mod** finds it in about six game runs for forty suspects. See [troubleshooting.md](troubleshooting.md).

**Most mods are for Legacy. Do they work on Enhanced?** Data files (`.xml`, `.meta`, `.ymap`…) do. Models, textures and particles must be converted, which Paleto Modkit does when you install the mod, add the files, or from **Mods in game**; see [mods.md](mods.md#legacy-files-on-the-enhanced-edition) for what is converted and what is not.

**Can I share my setup?** **File → Export mods as an OIV package…** turns your changes to the game's files, and your add-on packs, into one `.oiv` that rebuilds them on a clean mods folder. See [mods.md](mods.md#exporting-your-mods-as-an-oiv-package).

**Why is most of the code written by AI?** Because that is how it was built: [Claude Code](https://claude.com/claude-code) wrote it, and I tested it by hand and reviewed parts of it. That is why this page says what was not tried in game. Issues and pull requests are welcome whatever tool wrote the code.

**A script mod (ScriptHookVDotNet, a `scripts` folder with `.dll` files) does nothing under Proton. Why?** It needs the real .NET Framework 4.8 in the game's Wine prefix, and Wine only has stubs. **Game → Prefix tweaks…** installs it: backup, install with the Proton's own Wine, Windows version put back, check. See [mods.md](mods.md#scripthookvdotnet-mods-net-on-linux), including what has been tried (one Proton build; native app and Flatpak).

**Which OIV packages work?** OpenIV's package format 2.x, with all its actions; see [oiv.md](oiv.md).
