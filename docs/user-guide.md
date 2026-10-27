# User guide

How to browse the game files and change them with the desktop app. For installing mods and keeping them working, see [mods.md](mods.md) and [troubleshooting.md](troubleshooting.md).

- [The window](#the-window)
- [What the colours mean](#what-the-colours-mean)
- [Browsing](#browsing)
- [Editing archives](#editing-archives)
- [Files and folders outside archives](#files-and-folders-outside-archives)
- [Text and XML files](#text-and-xml-files)
- [Binary metadata and text tables](#binary-metadata-and-text-tables)
- [Textures](#textures)
- [3D models](#3d-models)
- [Audio](#audio)
- [Other files](#other-files)
- [Keyboard and mouse](#keyboard-and-mouse)
- [Appearance](#appearance)
- [Menus at a glance](#menus-at-a-glance)

## The window

![The game folder](../screenshots/game-folder.png)

- **Menu bar**: File, Game, Tools, View and Help. The filter box sits in the middle. At the right end, **Install package…** is the one place for `.oiv` packages and add-on packs (`dlc.rpf`), with **Prefix tweaks…** beside it (the game's Wine prefix, for ScriptHookVDotNet mods; see [mods.md](mods.md#scripthookvdotnet-mods-net-on-linux)). Neither has a menu entry. [Menus at a glance](#menus-at-a-glance) lists what each menu holds.
- **Location bar**: back, forward and up buttons, then the path as clickable parts. Inside the mods copy of an archive, the path shows **MODS**, and **Show original** at the right end shows the game's own archive instead (marked **ORIGINAL**; **Show mods copy** goes back). This lasts until you leave that archive.
- **Side tree**: the game folder. Click a name to open it, its arrow to expand it. **Expand all** and **Collapse all** are above it. **View → Side tree** hides it when you want the room for the file list.
- **File list**: the open folder or archive folder, with its toolbar (add, new folder, rename, extract, delete). Click a column header to sort by it, and again to reverse the order.
- **Preview panel**: opens on the right when you select a file: text, XML, textures, models or sounds, and any other file as hex. A file is read without blocking the window; one that takes long shows **Loading…** above the previous preview until it is ready.
- **Status bar**: what is running and the result of the last operation on the left. One operation runs at a time: starting another one, or switching game, is refused until it is done, and closing the window while one runs asks for a second close, since it would stop halfway. Work that only reads, such as **Expand all**, shows **✖ Cancel** beside it; operations that change files have none, since stopping them halfway would leave a half-done change. On the right: how many items the folder holds and what is selected (with its size), the files of the mods folder made for Legacy on the Enhanced edition (until they are converted; click the count to see them), and the edition (the game folder on hover).

Paleto Modkit finds the games installed in your Steam libraries. **Game → Choose the game folder…** picks any other install (the folder holding `GTA5.exe` or `GTA5_Enhanced.exe`). Both editions are supported; the status bar says which one is open.

## What the colours mean

| Cue | Meaning |
| :--- | :--- |
| **Orange name with ↪** | **Modded**: a game archive or file with a copy in the `mods/` folder, which the game loads instead. Opening the archive shows that copy (the path shows **MODS**), so you see what the game actually loads. |
| **Orange name** | Inside a mods copy: a file that differs from the game's, or is not in it; hover over it to see which. |
| **Plain name** | **Original**: an untouched game file. **Show original** in the location bar shows one archive's original; **View → Show original files (ignore mods folder)** shows the originals everywhere, ignoring the mods folder. |
| **Icon colour** | **File type**: folders, archives, textures, models, maps, audio, executables and text each have their own colour. **View → Coloured icons** turns them off. |

## Browsing

Double-click an archive (`.rpf`), or select it and press **Enter**, to open it like a folder. Archives inside archives open the same way, as deep as they go.

**Type to filter**: start typing anywhere and the list shows only the names that contain what you typed; the first match is selected, so **Enter** opens it. **Esc** clears the filter.

![Typing filters the list](../screenshots/4-type-to-search.png)

**File → Open archive…** opens an archive outside the game folder, for example one that came with a mod. You can also open `.rpf` and `.oiv` files from your file manager.

## Editing archives

Inside an archive, the toolbar, the right-click menu and drag and drop:

- **Add files…** and **Add folder…**, or drop files and folders on the window: they are added to the open folder, replacing files with the same name. A folder keeps its structure.
- **New folder…** (Ctrl+Shift+N), **Rename…** (F2), **Delete** (Delete key).
- **Extract…**: copies the selection, folders included, to a folder you pick.

**Original game files are never modified.** The first change to an original archive copies it to the `mods/` folder (at the same place: `update/update.rpf` becomes `mods/update/update.rpf`) and changes the copy. On btrfs and XFS the copy is a reflink: instant, and taking no extra space until it differs. A copy in `mods/` that is only a link (symbolic or hard) is made a file of its own before it changes, so the change never reaches the file it pointed to. Archives you opened from outside the game folder are edited in place.

Names inside archives are stored in lower case, as the game expects. Scripts (`.ysc`) are stored encrypted, as the game reads them; Paleto Modkit can only write the AES encryption, so it adds scripts to AES-encrypted archives only and refuses them elsewhere.

On the Enhanced edition, files made for Legacy are noticed when you add them, and can be converted on the way; see [mods.md](mods.md#legacy-files-on-the-enhanced-edition).

## Files and folders outside archives

In the game folder itself, **New folder…**, **Rename…** and **Delete** work on the files on disk. Original game data (archives, `update/`, `x64/` and the game executables) is never renamed or deleted there; its copy in `mods/` can be. Deleting other files is permanent (there is no trash): the confirmation says so, and warns in red when the files are outside `mods/`, which Steam may have to download again (**Verify integrity of game files**). Adding and extracting only work inside archives.

On disk, a rename that only changes upper and lower case is allowed. A name already used in the folder, in any case, is not: the game does not tell them apart.

## Text and XML files

Selecting a text file shows it in the preview panel, where you can edit it:

- syntax highlighting for XML (`.xml`, `.meta`) and configuration files (`.ini`, `.cfg`, `.dat`, `.ide`…);
- **💾 Save** (Ctrl+S) writes it back into the archive (into its mods copy, for an original);
- **⚙ Format** re-indents messy XML and removes the blank lines between tags. Only whitespace changes;
- **🔍 Search** (Ctrl+F) finds text in the file, whatever its case: matches are highlighted, **Enter** goes to the next one, **Shift+Enter** to the previous one, **Esc** closes the search. It works on read-only files too.

On a narrow panel the three buttons shrink to their icons (names on hover).

Leaving a file with unsaved changes asks whether to save them, discard them or stay.

## Binary metadata and text tables

`.ymap`, `.ytyp`, `.ymt` and `.pso` files (resource, PSO and RBF metadata) open as editable XML, in the same format as CodeWalker. Saving converts the XML back to the binary format.

![Binary metadata as XML](../screenshots/xml-editor.png)

Text tables (`.gxt2`) open as `0xHASH = text` lines, one per string, and are converted back the same way.

## Textures

Selecting a texture dictionary (`.ytd`) shows its textures, with their format, size and mipmaps. Textures in a format Paleto Modkit cannot read are left out, and the header says how many; such a dictionary cannot be changed. **Alpha** shows their transparency over a checkerboard. ↑/↓ move through the list, Home/End jump to the first or last texture.

![Texture dictionary viewer](../screenshots/texture-viewer.png)

- **⬇ Export**: the selected texture or all of them, as DDS or PNG.
- **✏ Edit**:
  - **Replace with DDS or PNG…**: a DDS file is used as it is, with its own format and mipmaps; a PNG picture is compressed to the texture's format (BCn), with mipmaps generated when the size is a power of two.
  - **Replace from several files…** and **Replace from a folder…**: many mods ship a folder of DDS files named after the textures they replace (for example for `fxdecal.ytd`). Each file replaces the texture it is named after; files that match no texture can be added as new ones. Dropping the folder on an open dictionary does the same.
  - **Add texture…**, **Rename…**, **Remove**.

New textures from PNG pictures become BC1, or BC3 when they have transparency. The command line can pick another format; see [cli.md](cli.md#textures).

## 3D models

Selecting a model (`.ydr`, `.yft`, `.ydd`) shows it in 3D, with its textures: its own, the ones vehicles share (`vehshare.ytd`) and the parent dictionaries the game links them to, DLC packs and tuning parts included. Drag to turn it, scroll to zoom, and **Reset view** to start over. Vehicle parts and wheels are shown in place.

![A model in the preview panel](../screenshots/archive-browser.png)

## Audio

Selecting an audio container (`.awc`) lists its sounds.

![Audio preview](../screenshots/audio-preview.png)

- **▶ Play** and **⏹ Stop**.
- **⬇ Export**: the selected sound or all of them, as WAV.
- **🔄 Replace…**: replaces the selected sound with a WAV or MP3 file, converted to the sound's format and rate. Radio songs are streams of several channels: a stereo file replaces both channels at once. On the Enhanced edition streams are MP3, one mono MP3 per channel.

## Other files

Any other binary file is shown read-only as hex: the offset, 16 bytes per row (8 while the panel is too narrow for 16), and the same bytes as text (`.` for those that are not printable). Only its first 8 MB are shown; the header says so for a larger file.

## Keyboard and mouse

| Keys | Action |
| :--- | :--- |
| Typing | Filter the open folder |
| ↑ / ↓ | Move the selection |
| Enter, double-click | Open a folder or archive, preview a file (with several selected, the one last clicked without Shift) |
| Backspace | Go up one level |
| Alt+← / Alt+→, mouse side buttons | Back / forward |
| Ctrl+click, Shift+click | Select several items |
| Ctrl+Shift+click | Add a range to the selection |
| Delete | Delete the selection |
| F2 | Rename the selected item |
| Ctrl+Shift+N | New folder |
| F5 | Read the folders again (the list also refreshes by itself after every change) |
| Ctrl+S | Save the file being edited |
| Ctrl+F | Search in the text being previewed |
| Esc | Clear the filter, close dialogs |
| Right click | Actions for the item |

**Help → Keyboard and mouse shortcuts** shows the same list in the app.

Dropping on the window:

- files and folders: added to the open archive folder;
- DDS or PNG files on an open texture dictionary: they replace the textures they are named after;
- an `.oiv` package: opens the installer;
- a `dlc.rpf`, or a mod's folder holding one: installs it as an add-on pack (see [mods.md](mods.md#add-on-packs)).

## Appearance

The **View** menu:

- **Theme**: light, dark, or the same as the desktop (followed live, also on Wayland; light when the desktop states no preference).
- **Text and icon size** and **Density** (row spacing).
- **Folders and files**: folders first, grouped by type, or mixed.
- **Side tree** shows or hides the tree of the game folder; **Coloured icons**; **Show all files in the side tree** (by default it shows folders and archives only); **Remember open folders** (the side tree opens as you left it).

## Menus at a glance

| Menu | Entries |
| :--- | :--- |
| **File** | Open archive… · Export mods as an OIV package… · Quit |
| **Game** | The installed games (pick one) · Choose the game folder… |
| **Tools** | Mods in game… · Snapshots of the mods folder… · Crash detection… · Script Hook V… |
| **View** | Theme · Text and icon size · Density · Folders and files (grouping) · Coloured icons · Side tree · Show all files in the side tree · Remember open folders · Show original files · Expand all / Collapse all folders · Refresh (F5) |
| **Help** | Getting started · Keyboard and mouse shortcuts · Online documentation · Report a problem · About |

What each one does: [mods.md](mods.md) for installing, converting and exporting; [troubleshooting.md](troubleshooting.md) for the Tools menu.
