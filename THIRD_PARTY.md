# Third-party material

## gta-toolkit — MIT licence

`crates/rpf/src/hashes.rs` contains SHA-1 hashes taken from
[gta-toolkit](https://github.com/Neodymium146/gta-toolkit)
(`RageLib.GTA5/Cryptography/GTA5Constants.cs`).

    Copyright (c) 2015 Neodymium

    Permission is hereby granted, free of charge, to any person obtaining a copy
    of this software and associated documentation files (the "Software"), to deal
    in the Software without restriction, including without limitation the rights
    to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    copies of the Software, and to permit persons to whom the Software is
    furnished to do so, subject to the following conditions:

    The above copyright notice and this permission notice shall be included in
    all copies or substantial portions of the Software.

    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
    THE SOFTWARE.

## CodeWalker — MIT licence, Copyright (c) 2017-2019 dexyfex

The RPF7 layout and the `magic.dat` decoding scheme follow
[CodeWalker](https://github.com/dexyfex/CodeWalker). `magic.dat` itself is
**not** distributed with this project: the first time the keys are needed,
the program downloads it from CodeWalker's repository (a pinned commit, checked
against a fixed SHA-256) and keeps it in the user's data folder.
The texture dictionary layouts (Legacy and Enhanced) in `crates/formats/src/ytd.rs`
follow CodeWalker's `Texture.cs`, and the page layout used when rebuilding
resources (`crates/formats/src/pages.rs`) follows its `ResourceBuilder.cs`.
The default header of new textures (`crates/formats/src/ytd_write.rs`) uses
its Enhanced defaults (`EnsureGen9`, `ShaderResourceViewG9`). The metadata
formats and their XML (`crates/formats/src/meta/`) follow `Meta.cs`, `Pso.cs`
and `MetaXml.cs` (`Pso.cs` is Copyright (c) 2015 Neodymium, MIT), and
`crates/formats/data/meta_names.txt` lists the names of
its `MetaNames.cs`. Text tables, audio containers and model meshes follow
`Gxt2File.cs`, `AwcFile.cs` and `Drawable.cs`/`Frag.cs` (with the placement
of fragment parts and wheels from its `Renderer.cs`); RBF metadata follows
`Rbf.cs` (Copyright (c) 2015 Neodymium, MIT).

## CodeWalker.Core in paleto-cwconv — MIT licence, Copyright (c) 2017-2019 dexyfex

`paleto-cwconv` (`tools/cwconv`), shipped with Paleto Modkit in
`lib/paleto-modkit/cwconv`, is built from CodeWalker's `CodeWalker.Core`
library (the `third_party/codewalker` submodule, unmodified) and runs its
Legacy → Enhanced converter (`Gen9Converter.TryConvert`, with
`ShadersGen9Conversion.xml`). The MIT licence text is the one quoted above
(CodeWalker's `Notice.txt`); its `Pso.cs` and other files credited to
Neodymium are Copyright (c) 2015 Neodymium, MIT.

`paleto-cwconv` bundles the [.NET](https://github.com/dotnet/runtime) runtime
(MIT licence, Copyright (c) .NET Foundation and Contributors) and
[SharpDX](https://github.com/sharpdx/SharpDX) 4.2.0 (`SharpDX.dll`,
`SharpDX.Mathematics.dll`; MIT licence, Copyright (c) 2010-2014 SharpDX -
Alexandre Mutel), which CodeWalker.Core uses for its math types.

## texture2ddecoder — MIT or Apache-2.0 licence

[texture2ddecoder](https://github.com/UniversalGameExtraction/texture2ddecoder)
decodes block-compressed (BCn) textures for previews and PNG export. It is a
dependency; the BC7 partition and anchor tables in `crates/formats/src/bc7.rs`
are copied from it (they come from the BC7 specification).

## texpresso — MIT licence

[texpresso](https://github.com/jansol/texpresso) compresses imported pictures
to BC1–BC5. It is a dependency, not copied code.

## pulseaudio — MIT licence

[pulseaudio](https://github.com/colinmarc/pulseaudio-rs), a native Rust client
of the PulseAudio protocol (also served by PipeWire), plays audio previews. It
is a dependency, not copied code.

## nanomp3 — MIT or Apache-2.0 licence

[nanomp3](https://crates.io/crates/nanomp3), a Rust port of minimp3, decodes
the MP3 audio of the Enhanced edition. It is a dependency, not copied code.

## Downloaded by Prefix tweaks — not distributed

Only when the user asks it to install .NET Framework 4.8 in the game's Wine
prefix (**Game > Prefix tweaks**, `paleto prefix dotnet`), Paleto Modkit
downloads, once and checked against fixed SHA-256 sums:

- [winetricks](https://github.com/Winetricks/winetricks) (LGPL-2.1-or-later),
  from a pinned commit, which it runs;
- Microsoft's .NET Framework 4.0 and 4.8 installers, from Microsoft's download
  servers, which winetricks runs. They are Microsoft's software under
  Microsoft's licence terms; none of it is part of this project.

## Fonts in the binary (egui's defaults) — mixed licences

egui draws its text and icons with the fonts of
[epaint_default_fonts](https://crates.io/crates/epaint_default_fonts) (0.37),
compiled into `paleto-modkit`. Each font keeps its own licence; the texts are
in that crate's `fonts/` folder:

- Ubuntu-Light (text): [Ubuntu Font Licence 1.0](https://ubuntu.com/legal/font-licence),
  Copyright Canonical Ltd.
- NotoEmoji-Regular (symbols): SIL Open Font License 1.1, Copyright Google Inc.
- Hack-Regular (monospace): MIT licence, Copyright 2018 Source Foundry Authors;
  its Bitstream Vera and DejaVu parts are under the Bitstream Vera licence and
  the public domain.
- emoji-icon-font (symbols), and egui-icons, a subset of it with five logos: MIT licence,
  Copyright (c) 2014 John Slegers.

The crate's own code is MIT OR Apache-2.0.

## webpki-roots — CDLA-Permissive-2.0

[webpki-roots](https://crates.io/crates/webpki-roots) carries Mozilla's list of
root certificates, used to check the HTTPS connections the app makes (see
"Downloaded by Prefix tweaks" and the magic.dat download above). The data is
under the Community Data License Agreement, Permissive, version 2.0.

## Unicode data — Unicode-3.0

The ICU and Unicode crates (`icu_*`, `zerovec`, `yoke`, `litemap`, `tinystr`,
`unicode-ident`, and others) carry Unicode data under the Unicode licence,
version 3.0 (SPDX `Unicode-3.0`).

## Other Rust crates

The other crates in the build are under MIT, Apache-2.0, BSD-2/3-Clause, ISC,
Zlib, BSL-1.0, 0BSD, Unlicense or LGPL-2.1-or-later (where a crate offers it
as an option next to MIT or Apache-2.0). Each crate's licence is in its
`Cargo.toml` (`license` field) and in the `cargo-sources.json` of the Flatpak
build; the full list is in `Cargo.lock`.

## .NET runtime and NuGet packages of paleto-cwconv — MIT

Besides SharpDX (see above), `paleto-cwconv` bundles the self-contained .NET
10 runtime: Microsoft.NETCore.App.Runtime.linux-x64, Microsoft.AspNetCore.App.Runtime.linux-x64,
Microsoft.NET.ILLink.Tasks, Microsoft.NETCore.Platforms and NETStandard.Library.
All are Copyright (c) .NET Foundation and Contributors, under the MIT licence.
The packages are listed in `packaging/nuget-sources.json`.

## Generated material

- The application icon (`packaging/icons/`) was generated with Claude (Anthropic).
- The screenshots in `screenshots/` and `docs/images/` are generated by the
  application's own tests (`crates/gui/tests/screens.rs`), from its interface.
- `crates/gui/tests/data/probe.ytd` is a 91-byte test file made for this project.
