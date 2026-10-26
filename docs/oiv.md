# OIV packages

An `.oiv` file is a zip archive with an `assembly.xml` that describes the changes, a `content/` folder with the files it adds, and optionally an icon. This page describes what Paleto Modkit does with each part of `assembly.xml`. For installing packages from the app, see [mods.md](mods.md#oiv-packages).

## Where changes go

Paths in a package are relative to the game folder. Game data (archives, `update/`, `x64/`) goes to the `mods/` folder: an original archive is copied there the first time a package changes it. Everything else, such as `.asi` plugins, `.dll` files, `scripts/` and configuration files, goes to the game folder itself, where the game and its plugins look for it. Programs (`.exe` files, such as the game itself) are never replaced or deleted: the installation logs a warning and goes on. A package with a path that could reach outside the game folder (`..`, a drive letter such as `C:`) is refused before anything is installed.

Before touching any file, the installer checks that every file the package adds is in it, so a broken package fails before it changes anything.

## Supported actions

Paleto Modkit follows OpenIV's package format 2.x (`<package version="2.x" target="Five">`).

| Action | What it does |
| :--- | :--- |
| `<add source="…">target</add>` | Adds or replaces a file, from the package's `content/` folder. On the Enhanced edition, models, textures and particles made for Legacy can be converted on the way. |
| `<delete>target</delete>` | Deletes a file or folder. |
| `<archive path="…" createIfNotExist="…" type="RPF7">` | Runs the actions inside it in that archive; archives nest. With `createIfNotExist`, a missing archive is created. Only RPF7 archives exist in GTA V. |
| `<defragmentation archive="…"/>` | Rewrites an archive without the space left by replaced files. |
| `<text path="…" createIfNotExist="…">` | Line-based edits of a text file: `<add>` (at the end), `<insert where="Before\|After" line="…">`, `<replace line="…">`, `<delete>`. Lines are matched with `condition="Equal\|StartWith\|Mask"`. |
| `<xml path="…">` | XPath edits of an XML file: `<add xpath="…" append="First\|Last\|Before\|After">`, `<replace xpath="…">`, `<remove xpath="…"/>`. |
| `<pso path="…">` | The same XPath edits on binary metadata (PSO and resource META files such as `.ymt`), through the same conversion to XML as the app's editor; the file is converted back afterwards. |

A `<remove>` whose XPath matches nothing is not an error: the node was already gone. Other edits that find nothing to change are logged as warnings, and the installation goes on.

## The installation log

The installer ends with a log of every change (`add …`, `edit …`, `saved …`) and its warnings. Keep it in mind when a mod does not work: a warning usually means the package was made for another version of the game files.

## Making packages

**File → Export mods as an OIV package…** (or `paleto export`) writes a package of your own changes to the mods folder; see [mods.md](mods.md#exporting-your-mods-as-an-oiv-package). It uses `<archive>`, `<add>` and `<delete>`, plus an `<xml>` edit of the DLC list for each add-on pack.
