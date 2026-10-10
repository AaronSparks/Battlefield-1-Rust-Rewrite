# Battlefield 1 rewrite: recon and plan

Recon date: 2026-10-03. Nothing here is tested in code yet. Facts marked
"seen" were read from the install. Facts marked "reported" come from the
sources listed at the end.

## Goal

A clean-room Rust program that reads the user's own Battlefield 1 install at
runtime, loads the Verdun Heights map offline, and lets one player walk around.
No networking, no bots, no weapons.

## Install (seen)

- Path: the `Battlefield 1 Files` folder
  (inside this project folder, so the whitelist `.gitignore` must exclude it).
- Store: Steam app 1238840, EA sku "origin", build 5142699 (`version.json`).
- Size: 82.2 GB.
- `bf1.exe`: native 64-bit, about 345 MB.

## Engine (seen + reported)

- Frostbite 3, Frostbite version 2014.4.x, data version 20161012 (Frosty profile).
- Code is native C++. There is no managed (.NET) code and no mod loader.
- Data layout: `Data\layout.toc` + `Patch\layout.toc` (the patch overrides data),
  superbundles (`.toc` + `.sb`), and content archives (`cas.cat` + `cas_NN.cas`)
  in many `*_install` folders.
- `.toc`, `layout.toc`, `initfs_Win32` and `cas.cat` start with `00 D1 CE 01`.
  This is the signed/XOR-obfuscated TOC header. Frostbite-Scripts `unXor` reads it.
  Frosty says BF1 needs no InitFs key.
- `.sb` files start with a DbObject (binary JSON-like) "bundles" list (seen).
- Compression: type 9 (LZ4) for EBX, RES and chunks (Frosty BF1 profile).
  Zlib and Zstd may also occur. No zstd dictionary is expected for BF1. Verify this.
- EBX (asset/config files): "legacy" partition format, EbxVersion 2. Magic
  seen on all 14,570 Verdun EBX files: `CE D1 B2 0F` (corrected 2026-10-03;
  the first version of this plan said `B4`). Type and field names are stored inside each file
  (`HasStrippedTypeNames: false`). We do not need a type SDK dumped from game memory.

## Verdun Heights (seen)

- `Data\Win32\xpack1\levels\mp_verdun\mp_verdun.toc` / `.sb` (12.8 MB)
- `Patch\Win32\xpack1\levels\mp_verdun\mp_verdun.toc` / `.sb` (7.7 MB)
- Content is in `installation\xpack1_install\cas_01..04.cas` and other shared
  install groups.
- Same pack: `mp_fields` (Soissons), `mp_graveyard` (Rupture), `mp_underworld`
  (Fort de Vaux). These names are from memory and are not verified.

## Anti-cheat and online

- EA Javelin anticheat (EAAC, kernel-mode) is installed in the game folder.
  It has protected BF1 since October 2024.
- Verdict: **safe for this project.** We never start, inject into, read memory
  from, or change `bf1.exe` or any game file. We only open data files read-only.
- Do NOT use tools that dump the type SDK from the running game's memory
  (Frosty's SDK generator, FrostbiteGen, ReClass). We do not need them
  (see EBX above).
- Do NOT use cheat-forum material (for example UnknownCheats BF1 dumps) as a source.

## Saves / config / logs

Not needed. The rewrite keeps its own config and log in the project folder.

## Sources found

| Source | License | Language | Use for us |
|---|---|---|---|
| [NicknineTheEagle/Frostbite-Scripts](https://github.com/NicknineTheEagle/Frostbite-Scripts) (based on Frankelstner, NoFaTe) | MIT | Python | Main reference: TOC un-XOR, DbObject, layout/installManifest, cas.cat, block decompression, legacy EBX, RES list. |
| [FrostyToolsuite/FrostyToolsuite](https://github.com/FrostyToolsuite/FrostyToolsuite) (new .NET 8 rewrite) | GPL-3.0 | C# | BF1 profile facts, DbObject, legacy EBX reader, Cas/Cat streams. Read only to learn. Do not copy code (GPL). |
| [CadeEvs/FrostyToolsuite v1.0.6.3](https://github.com/CadeEvs/FrostyToolsuite) | CC BY-NC-ND 4.0 | C# | MeshSet and Texture resource layouts. Read only to learn. Never copy code. |
| [GDC 2012 "Terrain in Battlefield 3"](https://media.gdcvault.com/gdc2012/slides/Programming%20Track/Widmark_Mattias_Terrain_in_Battlefield3a.pdf), [SIGGRAPH 2007 Frostbite terrain](https://www.realtimerendering.com/advances/s2007/Andersson-TerrainRendering(Siggraph07).pdf) | DICE talks | - | Concepts of the terrain system (quadtree, 128x128 height tiles, streaming). |
| [ZenHAX id-daemon thread](http://zenhax.com/viewtopic.php@t=4864&start=100.html) | closed tools | - | Says terrain is in `TerrainStreamingTree` files, a tree of 128x128 heightmap squares. No spec, no code. |
| [mitsuhiko/frostbite2-stuff](https://github.com/mitsuhiko/frostbite2-stuff) | see repo | Python | Old Frostbite 2 format notes. Low priority. |
| [muffinmxn/gang-beasts-rust](https://github.com/muffinmxn/gang-beasts-rust) | MIT | Rust | Repo structure: `crates/`, `tools/`, `docs/`, whitelist `.gitignore`, Bevy. |
| [samwhosung/benilla](https://github.com/samwhosung/benilla) | MIT / Apache-2.0 | Rust | Structure: format-reader crates, runtime reads from the user's install, Bevy. |
| [chasmlol/2010-rust-rewrite-mashup](https://github.com/chasmlol/2010-rust-rewrite-mashup) | Apache-2.0 | Rust | Structure: `xtask/`, `NOTICE` file for credits. |

Not found: any Rust Frostbite reader, any public decompilation of BF1, any
open spec for BF1 terrain.

Avoid: GitHub repos named like "Hack-BF1-Mod" (they look like malware bait),
and live-game injection mods such as OpenBF1-BF1BotMod.

## Route

Reimplementation (clean-room). There is no loader route for an offline,
standalone program. Reading the user's own files at runtime is the only route
that meets the rules.

## Steps (each one is a test point)

1. `fb_vfs`: un-XOR TOC, parse DbObject, read both `layout.toc` files, index all
   `cas.cat` files, resolve a SHA1 to (cas file, offset, size), decompress
   LZ4/zlib/zstd blocks. Test: list the bundles in `mp_verdun.sb` and log the counts.
2. `fb_ebx`: legacy EBX reader. Test: dump the Verdun level EBX to text in a
   gitignored folder and log the instance type counts.
3. Find the level layout: the static model groups, object blueprints and their
   transforms. Test: log N placed objects with positions.
4. `fb_res`: MeshSet and texture headers, then vertex/index chunks. Test: one
   mesh renders, and its vertex count matches the header.
5. Terrain: reverse-engineer `TerrainStreamingTree` / heightfield chunks. This
   is the largest unknown. Fallback: render meshes only and walk on mesh collision.
6. `bf1_game` (Bevy): free camera, then a walking character with collision.

## Unknowns to resolve first

1. Do BF1 bundles in `.sb` use the "binary bundle" layout or a DbObject per
   bundle? (The header looks like DbObject.)
2. Exact compression types present (expect 9 = LZ4). Is a zstd dictionary
   needed anywhere?
3. Delta/patch rules between `Data` and `Patch` (BF1 may use delta bundles).
4. Terrain data format (no public spec).
5. Material/shader parameters: the game's shaders are compiled DX11 bytecode.
   We write our own simple shaders and only read texture references.
6. Which Rust stack: Bevy (as in the reference repos) is the default proposal.
