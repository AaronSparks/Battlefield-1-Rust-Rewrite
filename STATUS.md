# Status: terrain colour layers and materials

**2026-10-07: the material layers are decoded and drawn** (updates 8 and 9;
MODLOG steps 3a to 3c). Open: the exact blend rule, ground normal maps,
secondary textures per layer, texture repeat sizes. The older sections below
are kept as a record of the attempts.

Date: 2026-10-03. Written because AGENTS.md says to stop after two real
attempts that fail, instead of trying variations at random.

## The problem

Each terrain tree node chunk holds several layers after its heights. We
need the colour layer (260 x 260 BC3, 67,600 bytes) of every node. In some
nodes we take the wrong layer, so the ground shows blue, purple or red
patches (user screenshots, 2026-10-03).

## What is known (checked on the data)

- Leaf chunks of exactly 67,600 bytes are colour. Decoded and checked by eye.
- Height node chunk = heights (136,242 bytes) + a block of 8,847 bytes
  ("block A") + layers. In some nodes the layer right after block A is a
  real colour image (checked by eye on record 25).
- In other nodes the layer right after block A decodes, as BC3, to an
  almost pure red image (mean 0.48, 0.01, 0.01). This fits a BC5 normal map
  (two channels near 128). A full scan of every byte offset of records 27
  and 113 found no ground-coloured image at all: those nodes have **no
  colour layer**, and the game must take the colour from a parent node.
- Block A's first part is pairs of u16 (probably a min/max height table).
  Its last ~135 bytes look like a small mask image, not a layer table.

## What was tried and failed

1. **Colour right after the heights** (byte 136,242): noise for most nodes.
2. **Smoothest candidate** (after block A, last 67,600 bytes, zero-alpha
   runs): picked smooth normal maps and masks (the red and blue patches).
3. **Smoothest ground-coloured candidate**, then **structure ratio** of
   endpoint colours and of decoded pixels (neighbour difference / far
   difference): accepted and rejected layers overlap (0.1 to 0.9 in both),
   so no limit separates "coarse real image" from "other layer".

## What is in the code now (safe fallback)

A node gets a colour tile only if the layer after block A passes a strict
test (smooth, ground-coloured, not almost black). Other nodes use the
nearest parent's colour. Areas with no colour at any level are plain brown.
This avoids wrong colours, at the cost of resolution in some places.

## Ideas for the next attempt

1. Find the layer list: compare node chunk sizes with layer counts. Masks
   may be 8-bit 260 x 260 (67,600 bytes) or compressed; sizes like 212,689
   = 136,242 + 8,847 + 67,600 fit one layer, but 278,029 does not fit whole
   layers, so some layers are not 67,600 bytes.
2. Look at the tree file between the root tile and the chunk table
   (about 1.4 MB). It may hold per-node descriptors (layer counts, formats).
3. Look at `terrain_4km.layercombinations_win32` (392 KB res, starts with a
   level name string): it may say which layers each node combination has.
4. Decode candidate layers as BC5 and as 8-bit masks to name each layer
   type, then find the rule for their order.

## Update: why so many nodes have no colour (likely)

With the strict test, 67% of the battle area (1 km around the centre) has
no colour tile at any level. The real game shows colour there, so it must
make it another way. `TerrainData` lists 21 terrain layers; the level
`shaderdb` names tiling terrain textures (for example
`T_Gen_MudPile_05_NRH`, `T_WF_GrassField_Distance02_NRH`); and the node
chunks hold 8-bit masks. Most likely BF1 **blends tiling layer textures by
masks** near the player ("procedural shader splatting", Andersson 2007) and
uses colour tiles only for distant or simple areas. Next step: decode the
mask layers and `terrain_4km.layercombinations_win32`, map masks to the 21
layers, and blend their textures in a terrain shader.

## Update 2 (ground textures attempt)

- The strict colour test is right: the rejected first layers of records 1,
  24 and 118 (roughness 26, 40, 56) decode, as BC3, to structured noise
  (checked by eye on PNGs). They are another layer type (probably packed or
  compressed masks), not colour.
- The terrain surface shader EBX (`DefaultTerrainSurfaceShader`) is only a
  stub. `layercombinations_win32` names compiled shader variants by layer
  number (for example `Terrain_4km__0MV_2MV_11MV__2d__d__0` = layers 0, 2,
  11); these names are not in the level `shaderdb` as text.
- The `shaderdb` names about 17 terrain colour textures (`*_CS`) and their
  normal/roughness/height textures (`*_NRH`), for example
  `T_Gen_MudPile_05_CS`, `T_WF_CountrySide_03_CS`,
  `T_DM_VAL_GrassBigTile_01_CS`, `T_WF_Verdun_Forestfloor_02_CS`.
- Not known: which layer number uses which texture, and where each node's
  masks are. Both are needed for real mask blending.

## Update 3 (option A started; user chose real reverse engineering)

- `bf1.exe` cannot be analysed with Ghidra or similar: its code sections
  have entropy 8.00 (encrypted) and it has an `.ooa` section (EA online
  activation DRM). Reading the code would need decryption or a memory dump
  of the running game: DRM / anti-cheat bypass, forbidden by AGENTS.md
  rule 5. Data files only.
- No public tool decodes BF1 terrain layers (known tools export heights only).
- Block A is now partly understood: it starts with a **min/max height
  quadtree**, 6 levels (1 + 4 + 16 + 64 + 256 + 1,024 cells, a (min, max)
  u16 pair each = 5,460 bytes). Checked on record 25: the root pair equals
  the smallest min and largest max of its four children. After it come
  3,387 bytes of 8-bit values with a row width of 67 (65 blocks of 4 x 4
  pixels + 2 border): a low-resolution map of unknown meaning.
- Record 27 has exactly the same size as record 25 (heights + 8,847 +
  67,600), but its one layer is a BC5 normal map, not colour. So the layer
  type is a per-node property. On record 27's whole chain of parents
  (L4 rec 24, L3 rec 23, L2 rec 12, L1 rec 1) no node has a colour layer.
- Conclusion: in the middle of the battle area BF1 stores **no ground colour
  image**; the ground look is built at runtime from masks and the layer
  textures. The real ground needs (1) the mask layers of each node and
  (2) which layer number uses which texture. (2) is not in the data as text.
- The tree file area between the root tile and the chunk table has
  sections of 4-bit packed values (0x1111, 0x2222, 0x3333): possibly
  per-patch layer combination indices. Not decoded.

## Update 9 (2026-10-07): which mask belongs to which patch and layer SOLVED

The streaming tree file (`terrain_4km.streamingtree_win32`) after the root
heights holds: root block A (8,847 bytes), 1 byte, 120 node records of 30
bytes (bounding box as 6 floats + 6 flag bytes, one per height node in tree
order), then the **mask section**:

- Header: u8 1, u32 section size (138,097), u32 66 (mask size), u32 4, u32 2,
  ..., the terrain rectangle (-2048, -2048, 2048, 2048), u32 109, u32 4,569
  (= the number of records).
- Records of 30 bytes: **u8 layer**, rect (4 f32: x0, z0, x1, z1), u8 ?,
  **u8 full**, 11 bytes (an 85-bit quadtree, 1+4+16+64, not needed yet).
  After some records come 9 extra bytes (103 times).
- A record is one layer in one patch. The rect size gives the level: 1024 m
  patches belong to the root (level 0, 4 x 4 patches), ... 32 m patches to
  level 5. `full` = 1: the layer covers the patch, no mask stored.
- `full` = 0: a 66 x 66 mask is stored in the node's chunk (after heights and
  block A), **in the same order as the records** (layers ascending, patches
  in quadtree order). The root's masks are in chunk record 0 (69 masks +
  a 67,600-byte layer = exactly 368,164 bytes).

Checks (all on the data): not-full records per level = stored masks per
level: 69/69, 98/98, 243/243, 392/392, 879/879, 1,340/1,340; per node: 76 of
76 nodes match. Edge check: masks of the same layer in x-neighbour patches
share their edge column: **1,569 equal, 0 different**. Numbering: at level 5,
242 of 302 patches with 3+ layers fit inside a shader name's layer list;
with randomly mixed numbers at most 165 fit. So the record layer numbers are
the shader layer numbers of update 8. Layers 12 to 17 never appear in colour
shaders: they are the layers with mesh scattering (grass, debris) and have no
ground texture.

Not known: the exact blend rule of the compiled shader. Our assumption:
start with the patch's first full layer, then paint each masked layer on top
in ascending order (`colour = mix(colour, layer, mask)`), which fits "masks
do not add up to 255" (update 4).

## Update 8 (2026-10-06): layer number -> textures SOLVED

How the shader database (`.../mp_verdun/shaderdb`, res type `IShaderDatabase`)
links a terrain shader name to its textures (checked on the data):

1. Key = Frostbite hash (h = h * 33 ^ c, start 5381) of the **lower-case**
   full name, e.g. `xpack1/levels/mp_verdun/terrain_4km/terrain_4km__0mv_2mv_11mv__2d__d__0`.
2. The key table starts at byte 157,004,712: 35,757 (0x8BAD) records of 32
   bytes, the name hash first. 0x8BAD is also written in both table headers.
3. Key k belongs to permutation record k (136 bytes each, in the same order;
   record k's 16-byte hash is at 152,141,876 + 136 (k - 1)). After the hash:
   u64 fields; field 2 = vertex program, field 3 = pixel program, fields 4..6
   = -1 (unused stages), field 7 = 684 (same for all terrain), **field 8 =
   description block number**.
4. Description blocks start at byte 0x1C (a small first block of 0x210
   bytes, then the terrain blocks from 0x22C) and follow each other; each
   starts with its u64 size, then u64 offsets of its parts. The texture list is at
   +0x50: records of 152 bytes (u32 slot, u32 0, name). 3,171 blocks, they
   end where the program code (20,009 DXBC blocks) starts.

Check: all 740 keys of colour ("2d") terrain names land on blocks with
terrain colour textures (the next best guess: 232 of 740). Then, for each
texture, the layers that always come with it explain every use (0 left over):

| Layer | Main colour texture | Also uses |
|---|---|---|
| 0 | T_Gen_MudPile_02 | Side_Flows (NRH) |
| 1 | T_Gen_MudPile_05 | Side_Flows |
| 2 | T_WF_CountrySide_03 | RockGravelWadi_Distance (NRH) |
| 3 | T_WF_Dirt_01 | MudPile_02, CountrySide_03, RockGravelWadi, Side_Flows |
| 4 | T_DM_VAL_GrassField02_ | GrassBigTile_01, CountrySide_03, GrassField distance and slope (NRH) |
| 5 | T_DM_Grass_Yellow_01 | GrassDryField distance, GrassField slope (NRH) |
| 6 | T_WF_Verdun_Forestfloor_02 | AG_Forestfloor_02 (NRH) |
| 7 | T_WF_Shoveltown_Forestfloor_03 | AG_Forestfloor_03 (NRH) |
| 8 | T_Gen_GroundBurntSoot01 | Side_Flows |
| 9 | T_V_FootPrints_01 | NoMansland_FootPrints (NRH), Side_Flows |
| 10 | T_Larger_Rocks_01 | CliffSide_Detail (NRH), MudPile_05, GroundBurntSoot01 |
| 11 | T_DM_VAL_GrassBigTile_01 | GravelGrassMix and GrassField distance (N) |
| 18 | MudPile_05 + GroundBurntSoot01 (shared) | |
| 19 | MudPile_05 + GroundBurntSoot01 (shared, rare) | |

`T_Variation_03_RGB` is in every shader (a variation mask, not a layer).
Format facts only; no code from other tools. FrostyToolsuite's
`ShaderBlockDepot` reader was read and does **not** fit this file (different
resource type); Frostbite-Scripts' `resnames.txt` (MIT) gave the type name.

Still open: which combination each 64 m patch uses, and which stored mask
belongs to which layer (step 3, part 2).

## Update 7 (2026-10-06): the shader database lists the layer textures

- The level `shaderdb` holds 1,123 compiled terrain shader entries. Each has
  a list of ground textures (records every 152 bytes) and then its constants:
  `worldToMask0UvTransform` .. `worldToMaskNUvTransform`, `maskMap`,
  `globalColorMap`, `heightfieldAtlas`, `dynamicMask`.
- 24 distinct ground textures, for example `T_Gen_MudPile_05`,
  `T_Gen_MudPile_02`, `T_Gen_GroundBurntSoot01`, `T_Gen_Side_Flows_01`,
  `T_WF_CountrySide_03`, `T_DM_Grass_Yellow_01`, `T_Larger_Rocks_01`,
  `T_V_FootPrints_01`, `T_WF_AG_Forestfloor_02/03`. Most are in the Verdun
  bundles as TextureAssets (10 of 12 checked).
- **Mask slots = layers - 1.** The "2d" combination names have mostly 5 or 8
  layers; the entries with colour textures have mostly 4 or 7 mask slots.
  So the first layer is the base, and each further layer is painted on top
  with its own mask. This agrees with update 4 (masks do not sum to 255).
- The empty `ShaderGraph` EBX files (`Terrain_4km__0MF_10MF_18MF__...`)
  name the layer numbers of each combination (suffixes MF, MV, MS). Their
  GUIDs are not in the `shaderdb`, so entry and name are not linked yet.
- Still not known: the link between an entry and its name (gives layer
  number -> textures), and which combination each patch uses (gives
  mask -> layer).

## Update 6 (2026-10-06)

- **Correction to update 5:** the "0.5% red marker" number was wrong. The
  test counted only bright red. With a wider test, about half of the battle
  area shows marker colour (L2 tiles: 81% reddish, 6% purple). The user saw
  it as red ground. The game now replaces marker pixels with the mean ground
  colour of the finest tiles (see MODLOG.md). The real ground there comes
  from the material layers (masks), which are still not decoded.

## Update 5

- Colour is taken from the final layer now; see MODLOG.md (100% of the battle
  area has a colour tile; the "0.5% red marker" number is wrong, see update 6).
- `layercombinations_win32` grammar: see MODLOG.md; not decoded yet.

## Update 4 (masks, part 1)

Node chunk layout, checked on all 120 height nodes (sizes fit exactly):

    heights 136,242 | block A 8,847 | N masks of 66 x 66 bytes | final layer 67,600

- The **final layer** is BC3 colour in some nodes and a BC5 normal map in
  others. (Before, we read the layer right after block A; for nodes with
  masks that was mask data, not colour.) Record 1 (L1, a quarter of the map)
  has a real colour map at its end, with a red area exactly where the battle
  area is: a marker where finer data takes over.
- **Masks:** N = 0 to 168 per node (2,952 in total). Each mask is 8-bit,
  66 x 66, for one 64 x 64 patch (a node has 4 x 4 = 16 patches); column 64
  of a mask equals column 0 of its right neighbour, row 64 equals row 0 of
  the one below (checked: 141 matching edge pairs in record 37). Masks of one
  layer are stored in quadtree order of their patches ((0,0), (1,0), (1,1),
  (0,1), then the next quarter), only for patches that need them.
- Masks do not add up to 255 per patch: layers are probably painted on top
  of each other in order.
- Not found yet: which layer each mask belongs to and which patches are
  present. Not a fixed-size table in the tree file (searched: 4-bit sums and
  bit counts, entry sizes 4 to 128). `layercombinations_win32` has 3,455
  entries (1,461 "2d", 1,426 "Displacement2d", 498 "MaskScale2d", 70 "3d")
  with small numbers after each name (levels and layer ids), but the "2d"
  entries list 9,422 layers against 2,952 stored masks, so the link is not
  one-to-one.
