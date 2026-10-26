# Command line: `paleto`

`paleto` does from a terminal what the app does, for scripts and quick jobs. It comes with the tarball and the Flatpak, not with the AppImage. Inside the Flatpak, run it as `flatpak run --command=paleto io.github.the_beper.PaletoModkit …`.

- [Paths and the game folder](#paths-and-the-game-folder)
- [Archives](#archives)
- [Metadata, text tables and audio](#metadata-text-tables-and-audio)
- [Textures](#textures)
- [Installing mods](#installing-mods)
- [Legacy → Enhanced conversion](#legacy--enhanced-conversion)
- [Checks and troubleshooting](#checks-and-troubleshooting)
- [Command reference](#command-reference)

## Paths and the game folder

Paths can point inside archives, nested ones included:

```
x64e.rpf/levels/gta5/vehicles.rpf/adder.yft
```

The game folder is the one given with `--game`, or `$PALETO_GAME`, or the game folder that holds the given path or the current directory. So inside the game folder, paths can be relative to it:

```bash
cd ~/.local/share/Steam/steamapps/common/Grand\ Theft\ Auto\ V\ Enhanced
paleto ls update/update.rpf/common/data
```

**Editing commands never change the original game files**: archives and loose game data files (under `update/` or `x64/`) in the game folder are copied to its `mods/` folder first and edited there (`paleto` says `editing mods copy …`); other loose files, such as plugin configs, are edited where they are. An archive of another game folder (one that holds it, or a Steam install) goes to that folder's `mods/`, and is refused when `--game` names a different one. Archives outside every game folder are edited in place.

The first command that needs the encryption keys prepares them (see [installation.md](installation.md#first-run-the-encryption-keys)); `paleto setup` does it on purpose.

## Archives

```bash
# Archive header and statistics
paleto info update/update.rpf

# List a folder inside an archive (-r: with its subfolders)
paleto ls update/update.rpf/common/data
paleto ls -r x64e.rpf/levels/gta5/vehicles.rpf

# Extract a file or a folder (into the current folder, or -o FOLDER)
paleto extract update/update.rpf/common/data/dlclist.xml -o out/

# Add or replace files and folders in an archive folder
paleto add update/update.rpf/common/data handling.meta carcols.meta

# Delete, rename (the new name only, it stays in its folder), create folders
paleto rm update/update.rpf/common/data/old.meta
paleto mv update/update.rpf/common/data/handling.meta handling_old.meta
paleto mkdir update/update.rpf/common/data/my_mod

# Create a new, empty archive
paleto new custom_data.rpf
```

On the Enhanced edition, `paleto add` converts files made for Legacy on the way (the files on disk are not changed); `--no-convert` adds them as they are.

## Metadata, text tables and audio

```bash
# Binary metadata (.ymt, .ytyp, .ymap, .pso) as XML: print, save, or replace from XML
paleto xml update/update.rpf/x64/data/carmodcols.ymt
paleto xml update/update.rpf/x64/data/carmodcols.ymt -o carmodcols.xml
paleto xml update/update.rpf/x64/data/carmodcols.ymt --import carmodcols.xml

# Text tables (.gxt2) as "0xHASH = text" lines, with the same -o / --import
paleto text x64b.rpf/data/lang/american_rel.rpf/global.gxt2 -o global.txt
paleto text x64b.rpf/data/lang/american_rel.rpf/global.gxt2 --import global.txt

# List the sounds of an audio container, and export them as WAV
paleto audio x64/audio/sfx/RESIDENT.rpf/doors.awc
paleto audio x64/audio/sfx/RESIDENT.rpf/doors.awc -o sounds/

# Replace a sound (by its hexadecimal id, as listed) with a WAV or MP3 file
paleto audio x64/audio/sfx/RESIDENT.rpf/doors.awc --replace 009C1A82=knock.wav
# Radio songs are streams of channels: a stereo file replaces the listed channel and the next
paleto audio x64/audio/sfx/RADIO_01_CLASS_ROCK.rpf/baker_street.awc --replace 0F68AC13=song.mp3
# Enhanced edition streams are MP3: one mono MP3 per channel
paleto audio x64/audio/sfx/RADIO_01_CLASS_ROCK.rpf/baker_street.awc \
    --replace 0F68AC13=left.mp3 --replace 1C3738EA=right.mp3
```

## Textures

```bash
# List the textures of a dictionary
paleto textures x64e.rpf/levels/gta5/vehicles.rpf/adder.ytd

# Export them all as DDS, or as PNG
paleto textures x64e.rpf/levels/gta5/vehicles.rpf/adder.ytd -o adder/
paleto textures x64e.rpf/levels/gta5/vehicles.rpf/adder.ytd -o adder/ --png

# Replace a texture with a DDS or PNG file (a PNG keeps the texture's format)
paleto textures x64e.rpf/levels/gta5/vehicles.rpf/adder.ytd --replace adder_badges=new.dds

# Add, rename and remove textures (each option can be repeated)
paleto textures my.ytd --add decal.png --add sign_n=normals.png --format bc5
paleto textures my.ytd --rename old_name=new_name --remove unused

# Replace every texture named like a DDS/PNG file of a mod's folder, and add the others
paleto textures update/update.rpf/x64/textures/fxdecal.ytd --replace-matching ~/Downloads/decals --add-unmatched
```

New textures from PNG pictures become BC1, or BC3 when they have transparency. `--format` picks another one: `bc1`, `bc2`, `bc3`, `bc4`, `bc5`, `bc7`, `a8r8g8b8`, `x8r8g8b8`, `a8b8g8r8`, `a1r5g5b5`, `a8` or `l8`. Mipmaps are generated when the size is a power of two.

## Installing mods

```bash
# Install an OIV package
paleto install my-mod.oiv

# Install an add-on pack: a dlc.rpf, or a mod's folder holding it, into
# mods/update/x64/dlcpacks/<name>/ and the DLC list
paleto install ~/Downloads/SuperCar
paleto install ~/Downloads/SuperCar --name supercar

# Make an OIV package of the mods folder's changes (without a file: list them)
paleto export
paleto export my-mods.oiv --name "My mods" --version 1.2 --author me
paleto export visuals.oiv --only timecycle --only visualsettings
```

On the Enhanced edition, `paleto install` converts Legacy files first; `--no-convert` installs them as they are. See [mods.md](mods.md) for what installing and exporting do.

## Legacy → Enhanced conversion

```bash
# Convert every Legacy model, texture and particle file in the mods folder's archives
# (the originals are kept in mods_legacy/<time>/; --no-backup does not keep them)
paleto convert

# Put back the Legacy files of the last conversion, or of a given one
paleto convert --undo
paleto convert --undo 1791655114

# Convert loose files into a folder
paleto convert car.yft car.ytd -o converted/
```

## Checks and troubleshooting

```bash
# Setup, plugins, DLC list, Legacy files, and what the last game session read
paleto check

# The game's last crash (or a given dump)
paleto crash
paleto crash path/to/dump.dmp

# Copies of game files left unchanged in the mods folder; --delete removes them
paleto clean
paleto clean --delete

# Snapshots of the mods folder
paleto snapshot                      # list them
paleto snapshot save "before VisualV"
paleto snapshot restore last         # or a snapshot's id, as listed
paleto snapshot delete ID

# The game's Wine prefix (Proton): .NET for ScriptHookVDotNet mods, backups
paleto prefix                        # where it is, and whether .NET is in it
paleto prefix dotnet                 # install .NET Framework 4.8 (asks before downloading; --yes, --no-backup)
paleto prefix backups                # list the backups
paleto prefix restore last           # or a backup's id, as listed
paleto prefix delete-backup ID
paleto prefix use PATH                # a compatdata/<app id> folder, for a non-Steam game; no PATH forgets it

# Find the mod that crashes the game, by halves
paleto hunt start      # half of the suspects are turned off; start the game
paleto hunt crashed    # it still crashed
paleto hunt works      # it worked
paleto hunt            # where the search is
paleto hunt inside     # look inside the archive found (or: paleto hunt inside mods/update/update.rpf)
paleto hunt stop       # turn everything back on
```

See [troubleshooting.md](troubleshooting.md) for what these find.

## Command reference

| Command | What it does |
| :--- | :--- |
| `setup` | Prepare the encryption keys (downloads CodeWalker's `magic.dat` once) |
| `check` | Check that the game will load the mods folder, and which of its files the last game session read |
| `info PATH` | Archive header and statistics |
| `ls PATH [-r]` | List an archive or a folder inside it |
| `extract PATH [-o DIR]` | Extract a file or folder |
| `add DEST FILES… [--no-convert]` | Add or replace files in an archive folder |
| `rm PATH` | Delete a file or folder inside an archive |
| `mv PATH NEW_NAME` | Rename a file or folder inside an archive |
| `mkdir PATH` | Create a folder (and missing parents) inside an archive |
| `new PATH` | Create a new, empty archive |
| `install PACKAGE [--name NAME] [--no-convert]` | Install an OIV package or an add-on pack |
| `convert [FILES…] [-o DIR] [--no-backup] [--undo [TIME]]` | Convert Legacy files to the Enhanced format, or undo a conversion |
| `export [OUTPUT] [--only TEXT…] [--name] [--version] [--author]` | Make an OIV package of the mods folder's changes |
| `crash [DUMP]` | Read the game's last crash |
| `clean [--delete]` | Copies of game files left unchanged in the mods folder |
| `hunt [status\|start\|crashed\|works\|stop\|inside [ARCHIVE]]` | Find the mod that crashes the game |
| `snapshot [list\|save NOTE\|restore ID\|delete ID]` | Snapshots of the mods folder |
| `prefix [status\|dotnet\|backups\|restore ID\|delete-backup ID\|use [PATH]]` | The game's Wine prefix: install .NET for ScriptHookVDotNet mods, backups |
| `xml PATH [-o FILE] [--import FILE]` | Binary metadata as XML |
| `text PATH [-o FILE] [--import FILE]` | Text tables as lines |
| `audio PATH [-o DIR] [--replace ID=FILE…]` | Sounds of an audio container |
| `textures PATH [options]` | Textures of a dictionary |

Every command takes `--game FOLDER`. `paleto help COMMAND` shows all of its options.
