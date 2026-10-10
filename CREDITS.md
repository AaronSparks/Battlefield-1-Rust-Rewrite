# Credits

This project contains no game assets and no decompiled code. It reads the
user's own Battlefield 1 install at runtime. Battlefield 1 and Frostbite are
trademarks of Electronic Arts Inc. This project is not affiliated with EA or DICE.

We learned file formats from the projects below. We write our own code; we do
not copy theirs. Their licenses stay with them.

## File formats

- **Frostbite-Scripts** by NicknineTheEagle, with portions by Frankelstner and
  NoFaTe. MIT License.
  https://github.com/NicknineTheEagle/Frostbite-Scripts
  Used to learn: TOC signature header, DbObject, layout.toc install manifest,
  cas.cat entries, compressed block headers, delta patch instructions, and the
  legacy EBX layout (header, keyword hash, field/complex descriptors, repeaters).
  Its `misc/resnames.txt` (res type names) told us that the level shader
  database is an `IShaderDatabase` resource.
- **FrostyToolsuite** (.NET 8 rewrite). GPL-3.0. Read to learn only; no code copied.
  https://github.com/FrostyToolsuite/FrostyToolsuite
  Used to learn: the Battlefield 1 profile values (EBX version, compression type).
- **FrostyToolsuite v1** by CadeEvs and contributors. CC BY-NC-ND 4.0. Read to
  learn only; no code copied.
  https://github.com/CadeEvs/FrostyToolsuite
  Used to learn: the MeshSet, LOD and section layout, the geometry
  declaration, subset categories, and the texture header layout. Vertex
  usage/format codes and texture pixel format codes are not in Frosty (it
  reads them from the running game); we worked them out from the data.
  Its `ShaderBlockDepot` reader was also read; it is for a different resource
  type and did not apply. The shader database layout we use (key table,
  permutation records, description blocks) was worked out from the data.

## Havok packfiles

The Havok 2013 binary packfile layout (header, section headers, fix-up tables)
is public knowledge used by many open tools. We used no Havok SDK code and
copied no code. The `hknpStaticCompoundShape` instance layout in Battlefield 1
data (128-byte records) and the Frostbite header in front of it were worked
out from the user's own files (see MODLOG.md, step 3).

## Terrain

No public specification of the Battlefield 1 terrain streaming tree was
found. The ZenHAX thread by id-daemon (closed-source tools) told us that the
data is a tree of square height tiles. The layout we use was worked out from
the user's own files (see MODLOG.md, step 5).

## Engine background

- Johan Andersson, "Terrain Rendering in Frostbite Using Procedural Shader
  Splatting", SIGGRAPH 2007.
- Mattias Widmark, "Terrain in Battlefield 3", GDC 2012.

## Project structure

- **gang-beasts-rust** by muffinmxn. MIT. https://github.com/muffinmxn/gang-beasts-rust
- **benilla** by samwhosung. MIT / Apache-2.0. https://github.com/samwhosung/benilla
- **2010-rust-rewrite-mashup** by chasmlol. Apache-2.0.
  https://github.com/chasmlol/2010-rust-rewrite-mashup

## Rust libraries

See each crate's `Cargo.toml`. Their licenses are in their own source.
Used by the probe tool for checks and exports: `texture2ddecoder` (MIT OR
Apache-2.0, BCn decoding) and `png` (MIT OR Apache-2.0).
