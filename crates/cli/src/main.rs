use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use paleto_core::{ArchivePath, Game, Package, vfs};
use paleto_formats::xml_export;
use paleto_formats::{Edit, Format, MetaFile, TextureDictionary, edit_textures};
use paleto_rpf::{Archive, ArchiveEditor, EntryKind, Keys};

/// Work with GTA V archives, the mods folder and OIV packages.
///
/// Paths can point inside archives, including nested ones:
/// "x64e.rpf/levels/gta5/vehicles.rpf/adder.yft".
/// Editing commands never touch the original game files: archives under the
/// game folder are copied to its mods folder first and edited there.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Game folder. Defaults to $PALETO_GAME, or the game folder containing the
    /// current directory or the given path.
    #[arg(long, global = true, env = "PALETO_GAME")]
    game: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum HuntAction {
    /// Where the hunt is (the default).
    Status,
    /// Start: half of the plugins, add-on packs and modded archives are turned off.
    Start,
    /// The game still crashed with the current half off.
    Crashed,
    /// The game worked with the current half off.
    Works,
    /// End the hunt and turn everything back on (the culprit too).
    Stop,
    /// Look inside the archive found by the last hunt (or this one, e.g.
    /// "mods/update/update.rpf"): a hunt among its changed files.
    Inside { archive: Option<String> },
}

#[derive(Subcommand)]
enum PrefixAction {
    /// Show the prefix and what is in it (the default).
    Status,
    /// Install .NET Framework 4.8, which ScriptHookVDotNet mods need. It
    /// downloads winetricks and Microsoft's installers once (about 120 MB).
    Dotnet {
        /// Do not ask before downloading.
        #[arg(short, long)]
        yes: bool,
        /// Do not back the prefix up first.
        #[arg(long)]
        no_backup: bool,
    },
    /// List the backups of the prefix.
    Backups,
    /// Put a backup in place of the prefix ("last" for the newest).
    Restore { id: String },
    /// Delete a backup of the prefix.
    DeleteBackup { id: String },
    /// Use this folder as the game's prefix (the game's compatdata/<app id>
    /// folder, for a game Steam does not know by its own app id), or with no
    /// folder go back to looking in Steam's library.
    Use { path: Option<PathBuf> },
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// List the snapshots (the default).
    List,
    /// Save the mods folder as it is now.
    Save {
        /// A note to tell it apart, e.g. "before VisualV".
        label: Vec<String>,
    },
    /// Put a snapshot in place of the mods folder ("last" for the newest).
    Restore { id: String },
    /// Delete a snapshot.
    Delete { id: String },
}

#[derive(Subcommand)]
enum Command {
    /// Prepare the encryption keys (downloads CodeWalker's magic.dat once).
    Setup,
    /// Check that the game will load the mods folder, and which of its files the last game session read.
    Check,
    /// Show archive header and statistics.
    Info { path: String },
    /// List an archive or a folder inside it.
    Ls {
        path: String,
        /// List subfolders recursively.
        #[arg(short, long)]
        recursive: bool,
    },
    /// Extract a file or folder from an archive.
    Extract {
        path: String,
        /// Destination folder.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Add or replace files in an archive folder. For the Enhanced edition,
    /// files made for Legacy are converted on the way (when paleto-cwconv is
    /// installed); the files on disk are not changed.
    Add {
        /// Folder inside an archive, e.g. "update/update.rpf/common/data".
        dest: String,
        /// Files or folders to add.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Add files made for Legacy as they are.
        #[arg(long)]
        no_convert: bool,
    },
    /// Delete a file or folder inside an archive.
    Rm { path: String },
    /// Rename a file or folder inside an archive (it stays in the same folder).
    Mv {
        /// The file or folder, e.g. "update/update.rpf/common/data/old.meta".
        path: String,
        /// The new name, without any folder.
        new_name: String,
    },
    /// Create a folder (and any missing parents) inside an archive.
    Mkdir { path: String },
    /// Create a new, empty archive.
    New { path: PathBuf },
    /// Install an OIV package into the mods folder, or an add-on pack: a
    /// dlc.rpf (or a folder holding them) goes to update/x64/dlcpacks/<name>/
    /// and into the DLC list. For the Enhanced edition, files made for Legacy
    /// are converted first (when paleto-cwconv is installed).
    Install {
        package: PathBuf,
        /// Install files made for Legacy as they are.
        #[arg(long)]
        no_convert: bool,
        /// The add-on pack's name (by default its folder's, or the one in its setup2.xml).
        #[arg(long)]
        name: Option<String>,
    },
    /// Convert Legacy models, textures and particles to the Enhanced format
    /// (with CodeWalker's converter, paleto-cwconv). Without files: every such
    /// file in the mods folder's archives, keeping the originals in the game
    /// folder's mods_legacy/<time>/.
    Convert {
        /// Loose .ytd .ydr .ydd .yft .ypt files to convert into --output instead.
        files: Vec<PathBuf>,
        /// Folder for the converted loose files.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Do not keep the Legacy originals of the mods folder.
        #[arg(long)]
        no_backup: bool,
        /// Undo a conversion of the mods folder: put back the Legacy files kept
        /// in mods_legacy/<TIME>/ (the last conversion without TIME).
        #[arg(long, value_name = "TIME", num_args = 0..=1, default_missing_value = "last", conflicts_with_all = ["files", "no_backup"])]
        undo: Option<String>,
    },
    /// Make an OIV package of the mods folder's changes (files changed, added or
    /// deleted inside archives, add-on packs with their DLC list entry), to
    /// rebuild them on a clean mods folder. Without OUTPUT, lists the changes.
    Export {
        /// The .oiv file to write.
        output: Option<PathBuf>,
        /// Only changes whose path contains one of these (case-insensitive), e.g. "timecycle".
        #[arg(long, value_name = "TEXT")]
        only: Vec<String>,
        #[arg(long, default_value = "My mods")]
        name: String,
        #[arg(long, default_value = "1.0")]
        version: String,
        #[arg(long, default_value = "")]
        author: String,
    },
    /// Read the game's last crash (its minidump and crashcontext.log): the
    /// exception, the module it happened in, the plugins on the stack.
    Crash {
        /// A crash dump to read instead of the newest.
        dump: Option<PathBuf>,
    },
    /// List the files of the mods folder identical to the game's own (copies
    /// that change nothing), and delete them with --delete.
    Clean {
        #[arg(long)]
        delete: bool,
    },
    /// Find the mod that crashes the game, by halves: turn off half of the
    /// plugins, add-on packs and modded archives, start the game, answer
    /// whether it crashed, repeat.
    Hunt {
        #[command(subcommand)]
        action: Option<HuntAction>,
    },
    /// Save, list, restore or delete snapshots of the mods folder (kept in
    /// the game folder's mods_snapshots/; instant and free on btrfs and XFS).
    Snapshot {
        #[command(subcommand)]
        action: Option<SnapshotAction>,
    },
    /// The game's Wine prefix (Proton): .NET for ScriptHookVDotNet mods, and
    /// backups of the prefix.
    Prefix {
        #[command(subcommand)]
        action: Option<PrefixAction>,
    },
    /// Show metadata (.ymt, .ytyp, .ymap, .pso) as XML, or replace it with XML.
    Xml {
        /// A metadata file, on disk or inside an archive.
        path: String,
        /// Write the XML to this file instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Convert this XML file and save it in place of the original (archives
        /// are edited in their mods folder copy).
        #[arg(long, conflicts_with = "output")]
        import: Option<PathBuf>,
    },
    /// Show a text table (.gxt2) as "0xHASH = text" lines, or replace it.
    Text {
        /// A text table, on disk or inside an archive.
        path: String,
        /// Write the text to this file instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Convert this text file and save it in place of the original.
        #[arg(long, conflicts_with = "output")]
        import: Option<PathBuf>,
    },
    /// List the sounds of an audio container (.awc), optionally exporting them as WAV.
    Audio {
        /// An audio container, on disk or inside an archive.
        path: String,
        /// Export every sound into this folder.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Replace a sound, named by its hexadecimal id, with a WAV or MP3 file. In streams (radio songs),
        /// a stereo file replaces the sound and the next channel. MP3 sounds need a mono MP3 (one per channel)
        /// (repeatable). Archives are edited in their mods folder copy.
        #[arg(long, value_name = "ID=FILE", conflicts_with = "output")]
        replace: Vec<String>,
    },
    /// Export a model (.ydr or .ydd) as CodeWalker XML, with its textures as DDS files.
    Model {
        /// A .ydr or .ydd file, on disk or inside an archive.
        path: String,
        /// The folder to write into: the XML as <name>.<ext>.xml, and the DDS files.
        #[arg(short, long)]
        output: PathBuf,
        /// A texture dictionary the model's textures may be in (repeatable).
        #[arg(long, value_name = "YTD")]
        dictionary: Vec<String>,
    },
    /// List the textures of a texture dictionary (.ytd), optionally exporting them.
    Textures {
        /// A .ytd file, on disk or inside an archive.
        path: String,
        /// Export every texture into this folder (DDS, or PNG with --png).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Export as PNG instead of DDS.
        #[arg(long, requires = "output")]
        png: bool,
        /// Replace a texture with a DDS or PNG file (repeatable). A PNG keeps
        /// the texture's format unless --format is given.
        #[arg(long, value_name = "NAME=FILE", conflicts_with = "output")]
        replace: Vec<String>,
        /// Add a texture from a DDS or PNG file, named after the file unless a
        /// name is given (repeatable). A PNG becomes BC1, or BC3 if it has
        /// transparency, unless --format is given.
        #[arg(long, value_name = "[NAME=]FILE", conflicts_with = "output")]
        add: Vec<String>,
        /// Remove a texture (repeatable).
        #[arg(long, value_name = "NAME", conflicts_with = "output")]
        remove: Vec<String>,
        /// Rename a texture (repeatable).
        #[arg(long, value_name = "OLD=NEW", conflicts_with = "output")]
        rename: Vec<String>,
        /// Replace every texture named like a DDS or PNG file here: a folder
        /// (searched through) or files, as mods ship them (repeatable).
        #[arg(long, value_name = "FOLDER|FILE", conflicts_with = "output")]
        replace_matching: Vec<PathBuf>,
        /// With --replace-matching: add the files named like no texture as new textures.
        #[arg(long, requires = "replace_matching")]
        add_unmatched: bool,
        /// Format for PNG pictures: bc1, bc2, bc3, bc4, bc5, bc7, a8r8g8b8,
        /// x8r8g8b8, a8b8g8r8, a1r5g5b5, a8 or l8. Mipmaps are generated when
        /// the size is a power of two.
        #[arg(long)]
        format: Option<Format>,
    },
}

fn main() -> Result<()> {
    // Exit quietly when piped into `head` and the like.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    match cli.command {
        Command::Setup => {
            let game = find_game(cli.game.as_deref(), None)?;
            game.keys()?;
            println!("{:?} edition at {}: keys ready", game.edition, game.root.display());
        }
        Command::Check => {
            use paleto_core::loader;
            let game = find_game(cli.game.as_deref(), None)?;
            let setup = loader::setup(&game);
            println!("{:?} edition at {}", game.edition, game.root.display());
            println!("ASI loader:  {}", setup.asi_loader.as_deref().unwrap_or("missing"));
            println!("{:<12} {}", format!("{}:", setup.plugin), if setup.plugin_installed { "installed" } else { "missing" });
            if let Some(problem) = setup.problem() {
                println!("! {problem}");
            }
            if let Some(hook) = loader::script_hook(&game) {
                let supports = hook.supports.as_deref().unwrap_or("?");
                match &hook.status {
                    _ if hook.stale => println!("! Script Hook V {supports} has not run since the game was updated: start the game to see whether it supports it"),
                    loader::HookStatus::Ok(version) => println!("Script Hook V {supports}: works with {version}"),
                    loader::HookStatus::Failed(why) => println!("! Script Hook V {supports} did not start ({why}): script mods do not run, a newer Script Hook V is needed"),
                    loader::HookStatus::NotRun => println!("Script Hook V {supports}: has not run yet"),
                }
            }
            if let Some(report) = paleto_core::prefix::report(&game) {
                match (report.problem(), report.ok_line()) {
                    (Some(problem), _) => println!("! {problem}"),
                    (None, Some(ok)) => println!("{ok}"),
                    (None, None) => {}
                }
            }
            let plugins = loader::asi_plugins(&game);
            let old: Vec<&str> = plugins.iter().filter(|p| p.older_than_game).map(|p| p.name.as_str()).collect();
            if !plugins.is_empty() {
                println!("ASI plugins: {}", plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));
            }
            if !old.is_empty() {
                println!("  made before the last game update (if the game crashes at start, update or remove these first): {}", old.join(", "));
            }
            let keys = game.keys().ok();
            match loader::missing_dlc_packs(&game, keys.as_ref()) {
                Ok(missing) if !missing.is_empty() => println!(
                    "! mods/update/update.rpf predates the last game update: its dlclist.xml lacks {}, which the game will not load",
                    missing.join(", ")
                ),
                Ok(_) => {}
                Err(e) => println!("! could not compare the DLC lists: {e}"),
            }
            match loader::unlisted_dlc_packs(&game, keys.as_ref()) {
                Ok(packs) if !packs.is_empty() => println!(
                    "! the DLC list does not name {} of the mods folder, so the game never loads them (add them with the GUI's Mods in game)",
                    packs.join(", ")
                ),
                Ok(_) => {}
                Err(e) => println!("! could not read the DLC list: {e}"),
            }
            match loader::foreign_files_cached_at(&game, keys.as_ref(), &loader::ForeignCache::default_path()) {
                Ok(files) if !files.is_empty() => {
                    println!("! {} files in the mods folder are not in the Enhanced format, so the game may crash on them:", files.len());
                    for f in files.iter().take(30) {
                        println!("    {}: {}", f.path, f.problem);
                    }
                    if files.len() > 30 {
                        println!("    … and {} more", files.len() - 30);
                    }
                }
                Ok(_) => {}
                Err(e) if keys.is_some() => println!("! could not check the file formats: {e}"),
                Err(_) => {}
            }
            let session = loader::last_session(&game);
            match &session {
                Some(s) => {
                    let age = s.time.and_then(|t| t.elapsed().ok()).map(|d| format!(", {} ago", age(d.as_secs()))).unwrap_or_default();
                    println!("last game session: {}{age}", s.log.display());
                }
                None => println!("last game session: no {} log yet (start the game once)", setup.plugin.trim_end_matches(".asi")),
            }
            let files = loader::modded_files(&game, session.as_ref());
            let unknown = files.iter().filter(|f| f.loaded.is_none()).count();
            for f in files.iter().filter(|f| f.loaded.is_some()) {
                println!("  {} {}", if f.loaded == Some(true) { "loaded    " } else { "NOT LOADED" }, f.path);
            }
            if unknown > 0 {
                println!("  {unknown} other files in the mods folder (the log lists archives only)");
            }
            if let Some(s) = &session {
                for e in &s.errors {
                    println!("  log: {e}");
                }
            }
        }
        Command::Info { path } => {
            let (archive, keys, idx) = resolve(&path, cli.game.as_deref())?;
            let archive = descend(archive, idx, keys.as_ref())?;
            let (mut dirs, mut bins, mut res) = (0, 0, 0);
            for e in &archive.entries {
                match e.kind {
                    EntryKind::Directory { .. } => dirs += 1,
                    EntryKind::Binary { .. } => bins += 1,
                    EntryKind::Resource { .. } => res += 1,
                }
            }
            println!("name:       {}", archive.name);
            println!("size:       {} bytes", archive.size);
            println!("encryption: {:?}", archive.encryption);
            println!("entries:    {} ({dirs} folders, {bins} binary files, {res} resources)", archive.entries.len());
        }
        Command::Ls { path, recursive } => {
            let (archive, keys, idx) = resolve(&path, cli.game.as_deref())?;
            let (archive, dir) = if archive.entries[idx].is_archive() {
                (descend(archive, idx, keys.as_ref())?, 0)
            } else {
                (archive, idx)
            };
            if !archive.entries[dir].is_dir() {
                print_entry(&archive, dir, &archive.entries[dir].name);
            } else if recursive {
                for (p, i) in archive.walk(dir) {
                    print_entry(&archive, i, &p);
                }
            } else {
                for i in archive.children(dir) {
                    print_entry(&archive, i, &archive.entries[i].name);
                }
            }
        }
        Command::Extract { path, output } => {
            let (archive, keys, idx) = resolve(&path, cli.game.as_deref())?;
            let entry = &archive.entries[idx];
            if entry.is_dir() {
                let root = output.join(if idx == 0 { archive.name.trim_end_matches(".rpf") } else { &entry.name });
                let mut count = 0;
                for (p, i) in archive.walk(idx) {
                    if !archive.entries[i].is_dir() {
                        write_file(&root.join(&p), &archive.read(i, keys.as_ref())?)?;
                        count += 1;
                    }
                }
                println!("{count} files extracted to {}", root.display());
            } else {
                let dest = output.join(&entry.name);
                write_file(&dest, &archive.read(idx, keys.as_ref())?)?;
                println!("extracted {}", dest.display());
            }
        }
        Command::Textures { path, replace, add, remove, rename, format, replace_matching, add_unmatched, .. }
            if !(replace.is_empty() && add.is_empty() && remove.is_empty() && rename.is_empty() && replace_matching.is_empty()) =>
        {
            let pair = |arg: &str, value: &str| {
                value.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())).with_context(|| format!("--{arg} {value}: expected two values separated by ="))
            };
            let read = |file: &str| std::fs::read(file).with_context(|| format!("reading {file}"));
            let renames = rename.iter().map(|r| pair("rename", r)).collect::<Result<Vec<_>>>()?;
            let mut replaces = Vec::new();
            for r in &replace {
                let (name, file) = pair("replace", r)?;
                replaces.push((name, read(&file)?));
            }
            let mut adds = Vec::new();
            for a in &add {
                let (name, file) = match a.split_once('=') {
                    Some((name, file)) => (name.to_string(), file),
                    None => (Path::new(a).file_stem().context("--add: no file name")?.to_string_lossy().into_owned(), a.as_str()),
                };
                adds.push((name, read(file)?));
            }
            if !replace_matching.is_empty() {
                use paleto_core::texture_batch;
                let data = read_any(&path, cli.game.as_deref())?;
                let ytd = TextureDictionary::parse(&data).with_context(|| format!("reading {path}"))?;
                let names: Vec<&str> = ytd.textures.iter().map(|t| t.name.as_str()).collect();
                let files = texture_batch::texture_files(&replace_matching);
                let batch = texture_batch::match_files(&names, &files);
                for file in &batch.duplicates {
                    println!("skipped {} (another file has the same name)", file.display());
                }
                for (name, file) in &batch.replace {
                    replaces.push((name.clone(), read(&file.to_string_lossy())?));
                }
                for file in &batch.unmatched {
                    if add_unmatched {
                        adds.push((file.file_stem().unwrap_or_default().to_string_lossy().into_owned(), read(&file.to_string_lossy())?));
                    } else {
                        println!("no texture named like {} (--add-unmatched adds it)", file.display());
                    }
                }
                if replaces.is_empty() && adds.is_empty() && renames.is_empty() && remove.is_empty() {
                    bail!("no DDS or PNG file there is named after a texture of {path}");
                }
            }
            // Renames first, so the other options can use the new names.
            let mut edits: Vec<Edit> = renames.iter().map(|(from, to)| Edit::Rename { from, to }).collect();
            edits.extend(remove.iter().map(|name| Edit::Remove(name)));
            edits.extend(replaces.iter().map(|(name, file)| Edit::Replace { name, file, format }));
            edits.extend(adds.iter().map(|(name, file)| Edit::Add { name, file, format }));

            if Path::new(&path).is_file() {
                let file = editable_file(&path, cli.game.as_deref())?;
                std::fs::write(&file, edit_textures(&std::fs::read(&file)?, &edits)?)?;
                println!("saved {}", file.display());
            } else {
                let target = editable(&path, cli.game.as_deref())?;
                let keys = load_keys(cli.game.as_deref(), &target.disk)?;
                let name = target.inner.last().context("not a file inside an archive")?.clone();
                let folder = target.parent().context("not a file inside an archive")?;
                folder.edit(keys.as_ref(), |editor, folder| {
                    let inner = vfs::join_inner(folder, &name);
                    let rebuilt = edit_textures(&editor.read(&inner, keys.as_ref())?, &edits).map_err(std::io::Error::other)?;
                    editor.add_file(&inner, rebuilt)?;
                    Ok(())
                })?;
                println!("saved {}", target.disk.display());
            }
            for edit in &edits {
                match edit {
                    Edit::Rename { from, to } => println!("renamed {from} to {to}"),
                    Edit::Remove(name) => println!("removed {name}"),
                    Edit::Replace { name, .. } => println!("replaced {name}"),
                    Edit::Add { name, .. } => println!("added {name}"),
                }
            }
        }
        Command::Model { path, output, dictionary } => {
            let load = |p: &str| -> Result<Vec<u8>> {
                if Path::new(p).is_file() {
                    return Ok(std::fs::read(p)?);
                }
                let (archive, keys, idx) = resolve(p, cli.game.as_deref())?;
                Ok(archive.read(idx, keys.as_ref())?)
            };
            let data = load(&path)?;
            let dictionaries = dictionary.iter().map(|d| TextureDictionary::parse(&load(d)?).with_context(|| format!("reading {d}"))).collect::<Result<Vec<_>>>()?;
            let file = Path::new(&path).file_name().and_then(|n| n.to_str()).with_context(|| format!("no file name in {path}"))?;
            let (stem, ext) = file.rsplit_once('.').with_context(|| format!("{path}: not a model"))?;
            let export = xml_export::export(&data, ext, stem, &dictionaries).with_context(|| format!("reading {path}"))?;
            write_file(&output.join(format!("{file}.xml")), export.xml.as_bytes())?;
            println!("wrote {}", output.join(format!("{file}.xml")).display());
            for (name, dds) in &export.textures {
                write_file(&output.join(name), dds)?;
                println!("wrote {}", output.join(name).display());
            }
        }
        Command::Textures { path, output, png, .. } => {
            let data = if Path::new(&path).is_file() {
                std::fs::read(&path)?
            } else {
                let (archive, keys, idx) = resolve(&path, cli.game.as_deref())?;
                archive.read(idx, keys.as_ref())?
            };
            let ytd = TextureDictionary::parse(&data).with_context(|| format!("reading {path}"))?;
            for tex in &ytd.textures {
                let srgb = if tex.srgb { " sRGB" } else { "" };
                let cube = if tex.faces > 1 { " cube map" } else { "" };
                println!("{:>5}x{:<5} {:>2} mips  {:<11} {}", tex.width, tex.height, tex.levels, format!("{}{srgb}{cube}", tex.format.name()), tex.name);
                if let Some(dir) = &output {
                    let data = if png { tex.to_png()? } else { tex.to_dds() };
                    write_file(&dir.join(tex.file_name(png)), &data)?;
                }
            }
            let edition = if ytd.enhanced { "Enhanced" } else { "Legacy" };
            println!("{} textures ({edition} format)", ytd.textures.len());
            if ytd.unsupported > 0 {
                println!("{} skipped (format not supported)", ytd.unsupported);
            }
        }
        Command::Xml { path, output, import } => {
            let data = read_any(&path, cli.game.as_deref())?;
            let meta = MetaFile::parse(&data).with_context(|| format!("reading {path}"))?;
            match import {
                Some(file) => {
                    let xml = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
                    let rebuilt = meta.rebuild_from_xml(&xml).with_context(|| format!("converting {}", file.display()))?;
                    write_any(&path, cli.game.as_deref(), rebuilt)?;
                }
                None => print_or_save(&meta.to_xml(), output.as_deref())?,
            }
        }
        Command::Text { path, output, import } => {
            let data = read_any(&path, cli.game.as_deref())?;
            let entries = paleto_formats::gxt2::parse(&data).with_context(|| format!("reading {path}"))?;
            match import {
                Some(file) => {
                    let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
                    let rebuilt = paleto_formats::gxt2::from_text(&text).with_context(|| format!("converting {}", file.display()))?;
                    write_any(&path, cli.game.as_deref(), rebuilt)?;
                }
                None => print_or_save(&paleto_formats::gxt2::to_text(&entries), output.as_deref())?,
            }
        }
        Command::Audio { path, replace, .. } if !replace.is_empty() => {
            let mut data = read_any(&path, cli.game.as_deref())?;
            let key = audio_key(&path, cli.game.as_deref());
            for r in &replace {
                let (id, file) = r.split_once('=').with_context(|| format!("--replace {r}: expected ID=FILE"))?;
                let id = u32::from_str_radix(id.trim_start_matches("0x"), 16).with_context(|| format!("--replace {r}: the id is hexadecimal"))?;
                let audio = std::fs::read(file).with_context(|| format!("reading {file}"))?;
                data = paleto_formats::awc::replace(&data, key.as_ref(), id, &audio).with_context(|| format!("replacing {id:08X}"))?;
                println!("replaced {id:08X}");
            }
            write_any(&path, cli.game.as_deref(), data)?;
        }
        Command::Audio { path, output, .. } => {
            let data = read_any(&path, cli.game.as_deref())?;
            let key = audio_key(&path, cli.game.as_deref());
            let sounds = paleto_formats::awc::parse(&data, key.as_ref()).with_context(|| format!("reading {path}"))?;
            for sound in &sounds {
                println!("{:08X}  {:>6} Hz  {:>7.2} s", sound.id, sound.sample_rate, sound.duration_secs());
                if let Some(dir) = &output {
                    write_file(&dir.join(format!("{:08X}.wav", sound.id)), &sound.to_wav())?;
                }
            }
            println!("{} sounds", sounds.len());
        }
        Command::Add { dest, files, no_convert } => {
            use paleto_core::convert;
            let target = editable(&dest, cli.game.as_deref())?;
            let keys = load_keys(cli.game.as_deref(), &target.disk)?;
            let local: Vec<_> = files.iter().map(|f| vfs::local_files(f)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect();
            let enhanced = find_game(cli.game.as_deref(), Some(&target.disk)).is_ok_and(|g| g.edition == paleto_core::Edition::Enhanced);
            let legacy = if enhanced { convert::legacy_in_local(&local, keys.as_ref()) } else { Vec::new() };
            let data = match convert::converter().filter(|_| !legacy.is_empty() && !no_convert) {
                Some(tool) => {
                    let (data, log) = convert::read_converting(&local, &tool, keys.as_ref())?;
                    for line in log {
                        println!("{line}");
                    }
                    data
                }
                None => {
                    if !legacy.is_empty() {
                        println!("! {} files are made for Legacy and are added as they are:", legacy.len());
                        for f in &legacy {
                            println!("    {f}");
                        }
                    }
                    local.iter().map(|(p, _)| std::fs::read(p)).collect::<Result<_, _>>()?
                }
            };
            target.edit(keys.as_ref(), |editor, folder| {
                for ((_, rel), data) in local.iter().zip(data) {
                    let inner = vfs::join_inner(folder, rel);
                    editor.add_file(&inner, data)?;
                    println!("add {inner}");
                }
                Ok(())
            })?;
            println!("saved {}", target.disk.display());
        }
        Command::Rm { path } => {
            let target = editable(&path, cli.game.as_deref())?;
            let keys = load_keys(cli.game.as_deref(), &target.disk)?;
            let parent = target.parent().context("cannot delete a whole archive")?;
            let name = target.inner.last().unwrap().clone();
            let deleted = parent.edit(keys.as_ref(), |editor, folder| Ok(editor.delete(&vfs::join_inner(folder, &name))?))?;
            if !deleted {
                bail!("{name} not found");
            }
            println!("deleted {path} (in {})", target.disk.display());
        }
        Command::Mv { path, new_name } => {
            let target = editable(&path, cli.game.as_deref())?;
            let keys = load_keys(cli.game.as_deref(), &target.disk)?;
            let parent = target.parent().context("cannot rename a whole archive")?;
            let name = target.inner.last().unwrap().clone();
            parent.edit(keys.as_ref(), |editor, folder| {
                Ok(editor.rename(&vfs::join_inner(folder, &name), &new_name, keys.as_ref())?)
            })?;
            println!("renamed {name} to {} (in {})", new_name.to_ascii_lowercase(), target.disk.display());
        }
        Command::Mkdir { path } => {
            let target = editable(&path, cli.game.as_deref())?;
            let keys = load_keys(cli.game.as_deref(), &target.disk)?;
            let parent = target.parent().context("not a folder inside an archive")?;
            let name = target.inner.last().unwrap().clone();
            parent.edit(keys.as_ref(), |editor, folder| Ok(editor.create_dir(&vfs::join_inner(folder, &name))?))?;
            println!("created {path} (in {})", target.disk.display());
        }
        Command::New { path } => {
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            let name = path.file_name().context("missing file name")?.to_string_lossy();
            ArchiveEditor::new(&name).save(&path)?;
            println!("created {}", path.display());
        }
        Command::Install { package, no_convert, name } if !package.extension().is_some_and(|e| e.eq_ignore_ascii_case("oiv")) => {
            use paleto_core::addon;
            let game = find_game(cli.game.as_deref(), None)?;
            let keys = game.keys()?;
            let packs = addon::find_packs(std::slice::from_ref(&package), Some(&keys));
            anyhow::ensure!(!packs.is_empty(), "{} is not an .oiv package and holds no dlc.rpf", package.display());
            anyhow::ensure!(name.is_none() || packs.len() == 1, "--name needs a single pack, {} found", packs.len());
            let tool = paleto_core::convert::converter().filter(|_| !no_convert);
            for pack in packs {
                let name = name.clone().unwrap_or(pack.name);
                println!("{} as {name}", pack.dlc.display());
                for line in addon::install_pack(&game, Some(&keys), &pack.dlc, &name, tool.as_deref())? {
                    println!("  {line}");
                }
            }
        }
        Command::Install { package, no_convert, .. } => {
            let game = find_game(cli.game.as_deref(), None)?;
            let keys = game.keys()?;
            let mut pkg = Package::open(&package).with_context(|| format!("opening {}", package.display()))?;
            let m = &pkg.metadata;
            println!("{} {} by {} (OIV {})", m.name, m.version, m.author, m.format_version);
            if game.edition == paleto_core::Edition::Enhanced {
                let legacy = pkg.legacy_files(Some(&keys))?;
                if !legacy.is_empty() {
                    match paleto_core::convert::converter().filter(|_| !no_convert) {
                        Some(tool) => {
                            for line in pkg.convert_legacy(&tool, Some(&keys))? {
                                println!("  {line}");
                            }
                        }
                        None => {
                            println!("! {} files in the package were made for Legacy and are installed as they are:", legacy.len());
                            for f in &legacy {
                                println!("    {f}");
                            }
                        }
                    }
                }
            }
            for line in pkg.install(&game, &keys)? {
                println!("  {line}");
            }
            println!("installed into {}", game.mods_dir().display());
        }
        Command::Export { output, only, name, version, author } => {
            use paleto_core::export;
            let game = find_game(cli.game.as_deref(), None)?;
            let keys = game.keys()?;
            let mut changes = export::changed_files_cached_at(&game, Some(&keys), &export::ChangesCache::default_path(), &|| false)?;
            if !only.is_empty() {
                let only: Vec<String> = only.iter().map(|o| o.to_ascii_lowercase()).collect();
                changes.retain(|c| only.iter().any(|o| c.path.to_ascii_lowercase().contains(o)));
            }
            match output {
                None => {
                    for c in &changes {
                        println!("{:<9} {}", format!("{:?}", c.kind).to_ascii_lowercase(), c.path);
                    }
                    println!("{} changes", changes.len());
                }
                Some(out) => {
                    anyhow::ensure!(!changes.is_empty(), "no changes to export");
                    let description = format!(
                        "Made with Paleto Modkit from the mods folder of {} ({} changes).",
                        game.root.file_name().unwrap_or_default().to_string_lossy(),
                        changes.len()
                    );
                    let meta = export::Metadata { name, version, author, description };
                    let n = export::write_oiv(&game, Some(&keys), &changes, &meta, &out)?;
                    println!("wrote {} ({n} changes)", out.display());
                }
            }
        }
        Command::Crash { dump } => {
            use paleto_core::crash_report;
            let game = find_game(cli.game.as_deref(), None)?;
            let report = match dump {
                Some(path) => crash_report::read_dump(&path)?,
                None => match crash_report::latest_with_keys(&game, cached_keys().as_ref())? {
                    Some(r) => r,
                    None => {
                        println!("no crash dump found (the game writes them in its CrashLogs folder)");
                        return Ok(());
                    }
                },
            };
            let ago = report.time.and_then(|t| t.elapsed().ok()).map(|d| format!(" ({} ago)", age(d.as_secs()))).unwrap_or_default();
            println!("{}{ago}", report.dump.display());
            println!("{} (0x{:08X}) at 0x{:X}", report.code_name(), report.code, report.address);
            match &report.module {
                Some((name, offset)) => println!("in {name} + 0x{offset:X}"),
                None => println!("outside every module"),
            }
            if let Some((kind, addr)) = report.access {
                println!("trying to {kind} 0x{addr:X}");
            }
            if !report.stack.is_empty() {
                println!("on the stack: {}", report.stack.join(", "));
            }
            for line in &report.context {
                println!("  {line}");
            }
            for store in report.tight_stores() {
                let state = if store.full() { "FULL" } else { "nearly full" };
                println!("  store {}: {} of {} ({state})", store.name, store.used, store.size);
            }
            if let Some(archive) = &report.last_archive {
                println!("  last archive mounted: {}", archive.describe());
            }
            if let Some((mounted, limit)) = report.archives {
                println!("  archives mounted: {mounted} of {limit}");
            }
            if report.same_place > 0 {
                let times = if report.same_place == 1 { "1 more time".to_string() } else { format!("{} more times", report.same_place) };
                println!("the game crashed at the same place {times} (older dumps)");
            }
            println!("{}", report.verdict(game.edition.exe_name()));
        }
        Command::Clean { delete } => {
            use paleto_core::cleanup;
            let game = find_game(cli.game.as_deref(), None)?;
            let copies = cleanup::identical_copies(&game);
            let gb = copies.iter().map(|c| c.size).sum::<u64>() as f64 / 1e9;
            for c in &copies {
                println!("{}", c.path);
            }
            if copies.is_empty() {
                println!("no file of the mods folder is identical to the game's");
            } else if delete {
                let n = cleanup::delete(&game, &copies)?;
                println!("deleted {n} files ({gb:.1} GB) identical to the game's");
            } else {
                println!("{} files ({gb:.1} GB) are identical to the game's: paleto clean --delete removes them", copies.len());
            }
        }
        Command::Hunt { action } => {
            use paleto_core::crash_hunt::{self, Outcome};
            let game = find_game(cli.game.as_deref(), None)?;
            let show = |outcome: Outcome| match outcome {
                Outcome::Test(hunt) => {
                    println!("{} suspects left after {} answers. Turned off now:", hunt.suspects.len(), hunt.steps);
                    for p in &hunt.disabled {
                        println!("    {p}");
                    }
                    println!("Start the game and play until where it crashed, then: paleto hunt crashed | paleto hunt works");
                }
                Outcome::Found(part) if part.to_ascii_lowercase().ends_with(".rpf") && part.starts_with("mods/") => println!(
                    "found: {part}\nIt is left turned off (in mods_disabled/); play to confirm. It may hold several mods: paleto hunt inside looks for the file. paleto hunt stop turns it back on."
                ),
                Outcome::Found(part) => println!(
                    "found: {part}\nIt is left turned off (in mods_disabled/, or the game's own put back); play to confirm. paleto hunt stop turns it back on."
                ),
            };
            match action.unwrap_or(HuntAction::Status) {
                HuntAction::Status => match crash_hunt::load(&game) {
                    Some(hunt) => show(Outcome::Test(hunt)),
                    None => {
                        let off = crash_hunt::turned_off(&game);
                        if off.is_empty() {
                            println!("no hunt going on; up to {} parts can be turned off (paleto hunt start)", crash_hunt::parts(&game).len());
                        } else {
                            println!("no hunt going on; still turned off: {} (paleto hunt stop)", off.join(", "));
                        }
                    }
                },
                HuntAction::Start => show(crash_hunt::start(&game)?),
                HuntAction::Crashed => show(crash_hunt::answer(&game, game.keys().ok().as_ref(), true)?),
                HuntAction::Works => show(crash_hunt::answer(&game, game.keys().ok().as_ref(), false)?),
                HuntAction::Stop => {
                    crash_hunt::stop(&game, game.keys().ok().as_ref())?;
                    println!("everything is turned back on");
                }
                HuntAction::Inside { archive } => {
                    let archive = match archive {
                        Some(a) => a,
                        None => match crash_hunt::turned_off(&game).as_slice() {
                            [one] if one.to_ascii_lowercase().ends_with(".rpf") && one.starts_with("mods/") => one.clone(),
                            _ => bail!("name the archive, e.g. paleto hunt inside mods/update/update.rpf"),
                        },
                    };
                    let keys = game.keys()?;
                    println!("comparing the mods folder with the game's files…");
                    show(crash_hunt::start_inside(&game, Some(&keys), &archive, &paleto_core::export::ChangesCache::default_path())?);
                }
            }
        }
        Command::Snapshot { action } => {
            use paleto_core::snapshots;
            let game = find_game(cli.game.as_deref(), None)?;
            let pick = |id: &str| -> Result<String> {
                if id == "last" {
                    snapshots::list(&game).first().map(|s| s.id.clone()).context("no snapshots yet")
                } else {
                    Ok(id.to_string())
                }
            };
            match action.unwrap_or(SnapshotAction::List) {
                SnapshotAction::List => {
                    let list = snapshots::list(&game);
                    if list.is_empty() {
                        println!("no snapshots yet (paleto snapshot save LABEL)");
                    }
                    for s in list {
                        let ago = s.time().and_then(|t| t.elapsed().ok()).map(|d| format!("{} ago", age(d.as_secs()))).unwrap_or_default();
                        println!("{}  {:>10}  {}", s.id, ago, s.label);
                    }
                }
                SnapshotAction::Save { label } => {
                    if !snapshots::shares_copies(&game.root) {
                        let gb = snapshots::size(&game.mods_dir()) as f64 / 1e9;
                        println!("note: this filesystem copies data, so the snapshot takes {gb:.1} GB");
                    }
                    let s = snapshots::create(&game, &label.join(" "))?;
                    println!("saved {} ({})", s.id, s.dir.display());
                }
                SnapshotAction::Restore { id } => {
                    let id = pick(&id)?;
                    snapshots::restore(&game, &id)?;
                    println!("mods folder restored from snapshot {id}");
                }
                SnapshotAction::Delete { id } => {
                    let id = pick(&id)?;
                    snapshots::delete(&game, &id)?;
                    println!("deleted snapshot {id}");
                }
            }
        }
        Command::Prefix { action } => {
            use paleto_core::prefix;
            let game = find_game(cli.game.as_deref(), None)?;
            if let Some(PrefixAction::Use { path }) = &action {
                prefix::choose(&game, path.as_deref())?;
                match path {
                    Some(path) => println!("prefix set to {}", path.display()),
                    None => println!("prefix back to Steam's library"),
                }
                return Ok(());
            }
            let prefix = prefix::find(&game)?;
            let pick = |id: &str| -> Result<String> {
                if id == "last" {
                    prefix::backups(&prefix).first().map(|b| b.id.clone()).context("no backups yet")
                } else {
                    Ok(id.to_string())
                }
            };
            match action.unwrap_or(PrefixAction::Status) {
                PrefixAction::Status => {
                    println!("prefix: {}", prefix.path.display());
                    println!("proton: {}", prefix.proton.name);
                    println!("{}", prefix::dotnet_state(&prefix).describe());
                    println!("{}", if prefix::is_running(&prefix) { "the game (or Wine) is using the prefix now" } else { "not in use" });
                    if prefix::is_own_pfx(&prefix) {
                        println!("warning: {}", prefix::OWN_PFX_NOTE);
                    }
                }
                PrefixAction::Dotnet { yes, no_backup } => {
                    if prefix::is_own_pfx(&prefix) {
                        eprintln!("warning: {}", prefix::OWN_PFX_NOTE);
                    }
                    let needed = prefix::needed_downloads();
                    let missing: Vec<_> = needed.iter().filter(|n| !n.cached).collect();
                    if !missing.is_empty() && !yes {
                        let mb = missing.iter().map(|n| n.size).sum::<u64>().div_ceil(1 << 20);
                        println!("this downloads {} ({mb} MB) and changes the prefix {}", missing.iter().map(|n| n.name).collect::<Vec<_>>().join(", "), prefix.path.display());
                        print!("continue? [y/N] ");
                        std::io::Write::flush(&mut std::io::stdout())?;
                        let mut answer = String::new();
                        std::io::stdin().read_line(&mut answer)?;
                        if !answer.trim().eq_ignore_ascii_case("y") {
                            bail!("cancelled");
                        }
                    }
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    let mut log = |line: &str| println!("{line}");
                    let mut run = prefix::Run { log: &mut log, cancel: &cancel };
                    prefix::install_dotnet(&prefix, &prefix::Options { backup: !no_backup }, &mut run)?;
                }
                PrefixAction::Backups => {
                    let list = prefix::backups(&prefix);
                    if list.is_empty() {
                        println!("no backups yet (paleto prefix dotnet makes one)");
                    }
                    for b in list {
                        let ago = b.time().and_then(|t| t.elapsed().ok()).map(|d| format!("{} ago", age(d.as_secs()))).unwrap_or_default();
                        println!("{}  {:>10}  {}", b.id, ago, b.label);
                    }
                }
                PrefixAction::Restore { id } => {
                    let id = pick(&id)?;
                    prefix::restore(&prefix, &id)?;
                    println!("prefix restored from backup {id}");
                }
                PrefixAction::DeleteBackup { id } => {
                    let id = pick(&id)?;
                    prefix::delete_backup(&prefix, &id)?;
                    println!("deleted backup {id}");
                }
                PrefixAction::Use { .. } => {}
            }
        }
        Command::Convert { undo: Some(time), .. } => {
            use paleto_core::convert;
            let game = find_game(cli.game.as_deref(), None)?;
            let time = if time == "last" {
                convert::backups(&game).first().map(|(t, _)| t.clone()).context("no conversion to undo")?
            } else {
                time
            };
            let keys = game.keys()?;
            let report = convert::undo(&game, Some(&keys), &time)?;
            for r in &report {
                match &r.error {
                    None => println!("put back {}", r.path),
                    Some(e) => println!("! {}: {e}", r.path),
                }
            }
            let done = report.iter().filter(|r| r.error.is_none()).count();
            println!("{done} of {} Legacy files put back", report.len());
        }
        Command::Convert { files, output, no_backup, undo: None } => {
            use paleto_core::convert;
            let tool = convert::converter().with_context(|| {
                format!("{} not found: build it with packaging/build-cwconv.sh, or set PALETO_CWCONV", convert::TOOL)
            })?;
            if files.is_empty() {
                let game = find_game(cli.game.as_deref(), None)?;
                anyhow::ensure!(game.edition == paleto_core::Edition::Enhanced, "only the Enhanced edition needs converted files");
                let keys = game.keys()?;
                let foreign = paleto_core::loader::foreign_files_cached_at(&game, Some(&keys), &paleto_core::loader::ForeignCache::default_path())?;
                if foreign.is_empty() {
                    println!("every file in the mods folder is already in the Enhanced format");
                    return Ok(());
                }
                let backup = (!no_backup).then(|| convert::backup_dir(&game));
                let paths: Vec<String> = foreign.into_iter().map(|f| f.path).collect();
                let report = convert::convert_in_mods(&game, Some(&keys), &tool, &paths, backup.as_deref())?;
                let done = report.iter().filter(|r| r.error.is_none()).count();
                for r in &report {
                    match &r.error {
                        None => println!("converted {}", r.path),
                        Some(e) => println!("! {}: {e}", r.path),
                    }
                }
                println!("{done} of {} files converted", report.len());
                if let Some(dir) = backup.filter(|_| done > 0) {
                    println!("Legacy originals kept in {}", dir.display());
                }
            } else {
                let output = output.context("--output is needed with files")?;
                std::fs::create_dir_all(&output)?;
                let data: Vec<Vec<u8>> = files.iter().map(std::fs::read).collect::<Result<_, _>>()?;
                let names: Vec<String> = files.iter().map(|f| f.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
                let inputs: Vec<(&str, &[u8])> = names.iter().zip(&data).map(|(n, d)| (n.as_str(), d.as_slice())).collect();
                for ((name, file), outcome) in names.iter().zip(&files).zip(convert::convert(&tool, &inputs)?) {
                    match outcome {
                        convert::Outcome::Converted(new) => {
                            write_file(&output.join(name), &new)?;
                            println!("converted {}", file.display());
                        }
                        convert::Outcome::Unchanged => println!("{}: already in the Enhanced format", file.display()),
                        convert::Outcome::Failed(e) => println!("! {}: {e}", file.display()),
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reads a file on disk or inside an archive.
fn read_any(path: &str, game: Option<&Path>) -> Result<Vec<u8>> {
    if Path::new(path).is_file() {
        return Ok(std::fs::read(path)?);
    }
    let (archive, keys, idx) = resolve(path, game)?;
    Ok(archive.read(idx, keys.as_ref())?)
}

/// Saves a file on disk, or into the mods folder copy of its archive.
fn write_any(path: &str, game: Option<&Path>, data: Vec<u8>) -> Result<()> {
    if Path::new(path).is_file() {
        let file = editable_file(path, game)?;
        std::fs::write(&file, data)?;
        println!("saved {}", file.display());
        return Ok(());
    }
    let target = editable(path, game)?;
    let keys = load_keys(game, &target.disk)?;
    let name = target.inner.last().context("not a file inside an archive")?.clone();
    let folder = target.parent().context("not a file inside an archive")?;
    folder.edit(keys.as_ref(), |editor, folder| {
        editor.add_file(&vfs::join_inner(folder, &name), data)?;
        Ok(())
    })?;
    println!("saved {}", target.disk.display());
    Ok(())
}

fn print_or_save(text: &str, output: Option<&Path>) -> Result<()> {
    match output {
        Some(file) => {
            write_file(file, text.as_bytes())?;
            println!("saved {}", file.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

/// The audio key, from the cached keys or derived again from the game.
fn audio_key(path: &str, game: Option<&Path>) -> Option<[u32; 4]> {
    let disk = split_disk(path).map(|(d, _)| d).unwrap_or_else(|_| PathBuf::from(path));
    let keys = load_keys(game, &disk).ok().flatten();
    keys.and_then(|k| k.awc).or_else(|| {
        let abs = std::path::absolute(&disk).ok()?;
        find_game(game, Some(&abs)).ok()?.keys().ok()?.awc
    })
}

fn print_entry(archive: &Archive, idx: usize, path: &str) {
    let e = &archive.entries[idx];
    match e.kind {
        EntryKind::Directory { .. } => println!("{:>12}  {path}/", "<dir>"),
        EntryKind::Binary { .. } if e.is_archive() => println!("{:>12}  {path}  [rpf]", e.size()),
        EntryKind::Binary { .. } => println!("{:>12}  {path}", e.size()),
        EntryKind::Resource { .. } => println!("{:>12}  {path}  [rsc v{}]", e.size(), e.resource_version().unwrap()),
    }
}

fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))
}

/// Splits "dir/archive.rpf/inside/path" into the archive on disk and the rest.
fn split_disk(path: &str) -> Result<(PathBuf, String)> {
    let mut disk = PathBuf::new();
    if path.starts_with('/') {
        disk.push("/");
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        disk.push(part);
        if disk.is_file() {
            return Ok((disk, parts[i + 1..].join("/")));
        }
    }
    bail!("{path}: no archive in this path")
}

/// Opens the archive containing `path` and returns it with the target entry.
fn resolve(path: &str, game: Option<&Path>) -> Result<(Archive, Option<Keys>, usize)> {
    let (disk, inner) = split_disk(path)?;
    let keys = load_keys(game, &disk)?;
    let mut archive = Archive::open(&disk, keys.as_ref()).with_context(|| format!("opening {}", disk.display()))?;
    let mut cur = 0;
    for part in inner.split('/').filter(|p| !p.is_empty()) {
        if archive.entries[cur].is_archive() {
            archive = descend(archive, cur, keys.as_ref())?;
            cur = 0;
        }
        cur = archive
            .children(cur)
            .find(|&i| archive.entries[i].name.eq_ignore_ascii_case(part))
            .with_context(|| format!("{part} not found in {}", archive.name))?;
    }
    Ok((archive, keys, cur))
}

fn descend(archive: Archive, idx: usize, keys: Option<&Keys>) -> Result<Archive> {
    if archive.entries[idx].is_archive() {
        Ok(archive.open_child(idx, keys)?)
    } else {
        Ok(archive)
    }
}

/// The archive path, redirected to the mods folder for original game files.
fn editable(path: &str, game: Option<&Path>) -> Result<ArchivePath> {
    let target = ArchivePath::parse(path).with_context(|| format!("{path}: no archive in this path"))?;
    let owner = owner(&target.disk, game)?.map(|(game, _)| game);
    let editable = target.editable(owner.as_ref())?;
    if editable.disk != target.disk {
        println!("editing mods copy {}", editable.disk.display());
    }
    Ok(editable)
}

/// A loose file to write: an original game data file is copied to the mods
/// folder first, and the copy is written.
fn editable_file(path: &str, game: Option<&Path>) -> Result<PathBuf> {
    if let Some((game, rel)) = owner(Path::new(path), game)?
        && !rel.starts_with("mods")
        && game.target(&rel.to_string_lossy()).starts_with(game.mods_dir())
    {
        let copy = game.mods_copy(&rel.to_string_lossy())?.with_context(|| format!("{path} not found"))?;
        println!("editing mods copy {}", copy.display());
        return Ok(copy);
    }
    Ok(PathBuf::from(path))
}

/// The game folder `path` is in, with the path inside it: the chosen one, one
/// that holds it (as written or through links), or a Steam install. An
/// original of another game folder than the one `--game` names is refused.
fn owner(path: &Path, game: Option<&Path>) -> Result<Option<(Game, PathBuf)>> {
    // As written, so a game folder reached through links is still found.
    let abs = std::path::absolute(path)?;
    let chosen = find_game(game, Some(&abs)).ok();
    let above = |p: &Path| p.ancestors().find_map(|d| Game::open(d).ok());
    let real = std::fs::canonicalize(&abs).ok();
    let candidates = chosen.clone().into_iter().chain(above(&abs)).chain(real.as_deref().and_then(above)).chain(paleto_core::game::detect_installs());
    let found = candidates.into_iter().find_map(|g| Some((vfs::in_game(&abs, &g)?, g)));
    if let (Some(dir), Some((rel, owner))) = (game, &found)
        && !rel.starts_with("mods")
        && chosen.as_ref().is_none_or(|c| c.root != owner.root)
    {
        bail!("{} is in the game folder {}, not in {}", path.display(), owner.root.display(), dir.display());
    }
    Ok(found.map(|(rel, game)| (game, rel)))
}

/// "3 min", "2 h", "5 days".
fn age(secs: u64) -> String {
    match secs {
        0..120 => format!("{secs} s"),
        120..7200 => format!("{} min", secs / 60),
        7200..172_800 => format!("{} h", secs / 3600),
        _ => format!("{} days", secs / 86400),
    }
}

/// The game folder: explicit, or found above `near` or the current directory.
fn find_game(explicit: Option<&Path>, near: Option<&Path>) -> Result<Game> {
    if let Some(dir) = explicit {
        return Ok(Game::open(dir)?);
    }
    let cwd = std::env::current_dir()?;
    let starts = [near, Some(cwd.as_path())].into_iter().flatten();
    for start in starts {
        if let Some(game) = start.ancestors().find_map(|d| Game::open(d).ok()) {
            return Ok(game);
        }
    }
    bail!("game folder not found: use --game or set PALETO_GAME")
}

/// The keys cached by an earlier run, if there are any.
fn cached_keys() -> Option<Keys> {
    Keys::from_bytes(&std::fs::read(paleto_core::game::data_dir().join("keys.bin")).ok()?).ok()
}

/// Cached keys, or keys prepared from the game the archive belongs to.
fn load_keys(game: Option<&Path>, archive: &Path) -> Result<Option<Keys>> {
    let cached = paleto_core::game::data_dir().join("keys.bin");
    if let Ok(data) = std::fs::read(cached)
        && let Ok(keys) = Keys::from_bytes(&data) {
            return Ok(Some(keys));
        }
    let abs = std::path::absolute(archive)?;
    match find_game(game, Some(&abs)) {
        Ok(game) => Ok(Some(game.keys()?)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An archive of one install is never edited in place because another
    /// install was chosen with --game. (The current directory, the other way
    /// to choose one, cannot be changed safely in a test.)
    #[test]
    fn originals_of_another_install() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("GTA5.exe"), b"").unwrap();
        }
        let mut ed = ArchiveEditor::new("x.rpf");
        ed.add_file("f.txt", b"x".to_vec()).unwrap();
        ed.save(&b.join("x.rpf")).unwrap();
        let original = std::fs::read(b.join("x.rpf")).unwrap();
        let path = format!("{}/f.txt", b.join("x.rpf").display());

        assert!(editable(&path, Some(&a)).is_err());
        assert_eq!(editable(&path, Some(&b)).unwrap().disk, b.join("mods/x.rpf"));
        std::fs::remove_dir_all(b.join("mods")).unwrap();
        assert_eq!(editable(&path, None).unwrap().disk, b.join("mods/x.rpf"));
        assert_eq!(find_game(None, Some(&b.join("x.rpf"))).unwrap().root, b);
        // Its mods copy is not an original: editing it is fine whatever --game says.
        assert_eq!(editable(&format!("{}/mods/x.rpf/f.txt", b.display()), Some(&a)).unwrap().disk, b.join("mods/x.rpf"));
        // Through a link, from outside every game folder.
        std::os::unix::fs::symlink(b.join("x.rpf"), dir.path().join("link.rpf")).unwrap();
        let linked = format!("{}/f.txt", dir.path().join("link.rpf").display());
        assert!(editable(&linked, Some(&a)).is_err());
        assert_eq!(editable(&linked, None).unwrap().disk, b.join("mods/x.rpf"));
        assert_eq!(std::fs::read(b.join("x.rpf")).unwrap(), original);

        // Loose game data files go to the mods folder too; others stay where they are.
        std::fs::create_dir_all(b.join("x64")).unwrap();
        std::fs::write(b.join("x64/t.ytd"), b"original").unwrap();
        std::fs::write(b.join("plugin.ini"), b"original").unwrap();
        let loose = b.join("x64/t.ytd").display().to_string();
        assert!(write_any(&loose, Some(&a), b"new".to_vec()).is_err());
        write_any(&loose, None, b"new".to_vec()).unwrap();
        assert_eq!(std::fs::read(b.join("x64/t.ytd")).unwrap(), b"original");
        assert_eq!(std::fs::read(b.join("mods/x64/t.ytd")).unwrap(), b"new");
        write_any(&b.join("plugin.ini").display().to_string(), None, b"new".to_vec()).unwrap();
        assert_eq!(std::fs::read(b.join("plugin.ini")).unwrap(), b"new");
        assert!(!a.join("mods").exists());
    }
}
