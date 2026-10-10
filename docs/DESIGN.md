# Design

## What this project is

A new program, written in Rust, that opens the Battlefield 1 files on your own
computer, reads the Verdun Heights map from them, and lets one player walk
around it offline. It does not run or change the real game. It contains no game
files and no decompiled code. It only knows how to *read* the file formats.

## Where the game data comes from

The program looks for the game folder in this order:

1. The `BF1_GAME_DIR` environment variable.
2. `Battlefield 1 Files` next to the project (the current setup).

It opens every game file read-only. It never writes into the game folder.

Anything we dump for study (text versions of assets, logs) goes into folders
that git ignores: `logs/` and `extracted/`.

## How Battlefield 1 stores its data (short version)

Think of it as a library:

- **`layout.toc`** is the front desk. It lists the "superbundles" and the
  install groups.
- **Superbundle** (`name.toc` + `name.sb`): a shelf. The `.toc` lists the
  bundles on the shelf and where each one starts in the `.sb` file.
- **Bundle**: a box. It lists the files that a part of the game needs: `ebx`
  (asset descriptions), `res` (resource headers) and `chunks` (big raw data like
  vertices and pixels). Each file is named by its SHA1 (a 20-byte fingerprint).
- **`cas.cat`**: the card index. For each SHA1 it says which `cas_NN.cas` file
  holds the data, at which offset, and how many bytes.
- **`cas_NN.cas`**: the warehouse. The data is cut into blocks of up to 64 KB,
  and each block may be compressed (LZ4, zlib or zstd).
- **`Patch` folder**: newer copies. When a bundle is listed in both `Patch` and
  `Data`, the `Patch` copy wins. A Patch `cas.cat` also has "patch entries":
  a file that is not stored whole, but as "take this old file (base) and
  apply these changes (delta)".

Most of these files (`.toc`, `.cat`) start with a 556-byte signature header
(`00 D1 CE 01`) that we skip. The data after it is not encrypted.

The `.toc` files and bundles use **DbObject**, a small binary format similar to
JSON (named fields with numbers, strings, lists and fingerprints).

## How a level is put together

- `LevelData` (the level) lists objects. Some are **sub-worlds**, named by
  bundle (for example `CONTENT_Zone_04`), and some are **layers**.
- Sub-worlds and layers list more objects. A **reference object** says "put
  this blueprint here, with this transform". A blueprint can be a prefab (a
  list of objects) or one entity.
- An entity with a `Mesh` link is something you can see.
- A **static model group** stores many copies of a few meshes. Most of their
  positions are not in the EBX file but in the group's physics file (Havok
  format), one record per copy.
- Every game mode (Conquest, Rush and others) is its own sub-world. A match
  loads only one. We use Conquest.

## Meshes and textures

- A mesh asset (EBX) points to a **MeshSet** res file. The MeshSet has up to
  6 levels of detail (LODs). Each LOD has sections (one per material) and
  points to a chunk that holds the vertex buffer, then the index buffer.
- The LOD's "subset categories" say which sections to draw normally
  (opaque, transparent) and which are only for shadows or depth.
- A material lists textures by role ("BaseColor", "Normal", and many other
  names). A texture asset points to a texture res (size, format, mip sizes),
  which points to a chunk with the compressed pixels, largest mip first.
- Shared chunks are listed in `.toc` files, mostly `Win32/chunks0`.

## Terrain

- `TerrainData` points to a "streaming tree" res. It holds one low-detail
  height tile for the whole 4 km map (16 m per sample) and a table of chunks.
- Some chunks are detailed height tiles: the same 261 x 261 grid for a
  smaller square (level 1 = half the width, level 5 = 1/32, 0.5 m per sample).
- To get the height at a point, we use the most detailed tile that covers it.
- Each sample is a 16-bit number; height in metres = sample x 512 / 65536.
  A sample of 0 is a hole (the bunker entrances).
- The chunk table lists the tree depth-first: each node, then its four
  children (0,0), (1,0), (1,1), (0,1). Every node has a 260 x 260 colour
  tile (BC3, 256 pixels + 2 border); the ground is coloured with the finest
  tile at each place.

## Code layout

```
Cargo.toml            workspace (one build for all crates)
crates/
  fb_vfs/             library: reads layout.toc, .toc, .sb, cas.cat, .cas
                      ("vfs" = virtual file system)
  fb_ebx/             library: reads EBX asset files (types, fields, values)
  fb_level/           library: walks a level, collects placed meshes; reads Havok packfiles
  fb_res/             library: mesh (MeshSet) and texture resources, material texture choice
  fb_terrain/         library: terrain heights from the streaming tree
  bf1_probe/          command-line tool: prints and logs what the libraries find
  (later) fb_res/     library: meshes and textures
  (later) bf1_game/   the Bevy program you walk around in
docs/                 this file and other notes
logs/                 log files (ignored by git)
extracted/            study dumps (ignored by git)
```

The format libraries do not depend on Bevy. This keeps them fast to build and
easy to test.

## Steps

Each step ends with a command you run and numbers to check in a log.

1. **fb_vfs + bf1_probe** (done 2026-10-03): read `layout.toc`, index every
   `cas.cat`, open the Verdun superbundle, and read every file in every
   bundle. Result: 0 failures (see MODLOG.md).
2. **fb_ebx** (done 2026-10-03): read EBX files. All 14,570 Verdun EBX
   files decode, and the dump goes to `extracted/ebx/` as text.
3. **Level layout** (done 2026-10-03): `fb_level` walks the level and
   collects 8,530 placed meshes (Conquest layout), including static model
   group positions read from Havok physics data. See MODLOG.md.
4. **fb_res** (done 2026-10-03): meshes and textures. 520 of 523 Verdun
   meshes decode and pass the bounding-box check; all 532 textures read.
   Bevy can take the BC1/BC3/BC5/BC7 data as it is (no decoding needed).
5. **Terrain** (done 2026-10-03, heights only): `fb_terrain` gives the
   ground height anywhere on the 4 km map, down to 0.5 m per sample in the
   battle area. Colour and material layers are not decoded yet.
6. **bf1_game** (Bevy 0.19, Rust 1.99): in three parts.
   - 6a: load terrain + all placed meshes with their colour textures; free
     fly camera (WASD, mouse, Shift = fast). Log load counts and timings.
   - 6b: walk mode: a player that stands on the terrain and has gravity.
   - 6c: collision with buildings and objects.

### How the game program works (step 6)

- At start it does what the probe does: index the archives, load the
  Verdun EBX, walk the level (Conquest layout), load the terrain.
- **Coordinates.** Frostbite world positions go into Bevy unchanged
  (x, y, z). Bevy is right-handed with y up. Triangle winding stays as
  stored (counter-clockwise faces the viewer).
  - Until 2026-10-06 we assumed Frostbite was left-handed and negated z.
    That drew the map mirrored. Proof: the 5 Conquest capture zones fit the
    A–E flags on the game's deploy screen with an average error of 33 pixels
    when drawn with x right and -z up (as Bevy shows them unchanged), and 96
    pixels with z up. The French base also lands next to the French HQ icon
    only in the unchanged form.
  - All conversion goes through `bf1_game/src/convert.rs` (now the identity),
    so a future change is in one place.
  - Checks in the log: the share of mesh triangles whose winding agrees
    with their stored normals, and the angle between the sun direction and
    the brightest point of the sky texture.
- **Meshes.** Each unique mesh (LOD 0) becomes one Bevy mesh per material.
  Every placement becomes an entity with a transform that shares that mesh,
  so the GPU can draw copies in batches.
- **Textures.** The BC1/BC3/BC5/BC7 data goes to the GPU as it is (no
  decoding). To save video memory we start at the second-largest mip.
- **Terrain.** Built as 64 m square patches of triangles from `height_at`:
  fine (1 m) in the 1 km around the battle area, 0.5 m where there are
  holes, coarse (8 m) for the far hills. Edges between different grids get
  skirts (strips hanging down) to fill cracks. Colour: the finest colour
  tile at each point; the game's marker pixels (red "finer data here",
  purple) are replaced with the mean ground colour of the finest tiles.
- **Logs.** `logs/game-<time>.log` gets load counts, timings, the camera
  position every 5 seconds and the frame rate.
- **Environment.** The level's visual environment EBX gives the sun, sky
  texture, fog curve, exposure and bloom. `environment.rs` reads it;
  `scene.rs` turns it into a Bevy camera (fog, bloom, tone mapping, ambient
  occlusion, SMAA), a sun with cascaded shadows, and a sky dome.

### Plan: ground textures (2026-10-07: steps 1, 2, 3a, 3b, 3c done)

BF1 draws the ground near the player from tiling layer textures (mud,
grass, rock, ...), blended by 8-bit masks: the base layer covers all, each
further layer is painted on top with its own mask (see STATUS.md, update 7).

1. **Load the ground textures.** Read the ~24 layer textures (`_CS` colour,
   `_NRH` normal/roughness/height) from the Verdun bundles. Log which load.
2. **Terrain shader.** A Bevy material with our own WGSL shader: per pixel,
   blend up to N layer textures (tiling, a few metres per repeat) by weights,
   and multiply by the colour map where one exists. Weights first come from a
   simple rule: the layer whose mean colour is closest to the colour map,
   steep slopes get rock; where there is no colour map, the main mud layer.
   This gives real BF1 textures, but their placement is a guess.
3. **Real masks.** Research done (STATUS.md updates 8 and 9). Implementation:
   - 3a. `fb_terrain`: parse the mask section of the tree file and keep each
     node's masks; a function that gives, for a world point, the weight of
     each layer (finest patch with records; base layer, then masked layers
     painted on top in ascending order). Checks in the probe and the log:
     records = stored masks per level, edge continuity in x and z.
   - 3b. Layer -> colour texture at runtime from the shader database (key
     table -> permutation record -> description block), checked by counts.
   - 3c. Game: two weight atlases (texture arrays, one 8-bit plane per
     layer with a texture): the 1 km battle area at 0.5 m per texel and the
     whole 4 km map at 4 m per texel. The terrain shader blends the layer
     textures by these weights near the camera and fades to the colour map
     far away. Layers 12 to 17 (mesh scattering only) are left out.

Each step is a separate change with its own test.

### Plan: match the reference look (2026-10-07: step 1 done)

Targets come from the user's screenshots, measured with `bf1_probe
image-stats` (MODLOG 2026-10-07). One step at a time, each measured again:

1. **Colour grading.** Done: each weather's two tables are combined into
   Bevy's tone mapping table (`grading.rs`); no extra pass.
2. **Indoor sky light.** Dim the ambient (sky) light where something is
   above: a ceiling map baked from down-facing mesh triangles, and the
   terrain height for underground rooms (every mesh material gets a small
   extension shader).
3. **Lamps.** Place the level's spot and sphere lights (warm bunker and wall
   lamps), most without shadows.
4. **Interior exposure** (`ve_mp_verdun_interior`), if still needed.
5. **Effects** (embers, fire, smoke): later, separate plan.

### Plan: ground detail (2026-10-07, from the user's 1:1 comparison; steps 1 and 2 done)

Measured with `bf1_probe image-stats --rect` on the user's screenshot 42
(real game) and ours at the same place: the mean ground colours are close,
but the near ground has 2.5 times less fine detail in ours (neighbour
difference 4.0 against 10.1). Facts found in the data:

- The ground textures are 2,048 pixels wide; we load them from the second
  mip (1,024), like all textures, to save video memory.
- Each layer has a second texture, `_NRH`, in the shader slot after its
  `_CS` (pairing consistent in every Verdun terrain shader). Red and green
  are a normal map; alpha is height. The normal points away from the uphill
  side of the alpha height in all 8 layers checked, which also gives the
  green direction: +green = towards the top of the image = world -Z in our
  shader (texture v runs along +Z). The meaning of blue is not known
  (constant 1.0 on grass); not used.
- `_CS` alpha is smoothness (mesh materials name the same textures
  `basecolor_smoothness`); means 0.16 to 0.26 on the 12 layers.

Steps, each measured again with the same regions:

1. Load the ground textures at full size (2,048). Tried: no measurable
   gain at the user's view (MODLOG); kept at 1,024 (`BF1_GROUND_MIP_SKIP`).
2. Normal maps and smoothness: a second array with the 12 `_NRH` textures,
   decoded to RGBA8 at the colour textures' size (1,024; about 67 MB). The
   `_NRH` of each layer comes from the shader database (slot after the
   `_CS`; most common pair). The shader blends normals and smoothness by
   the same weights as the colours and bends the terrain normal with them
   (tangent = world +X, +green = world -Z); roughness = 1 - smoothness.
   Both fade out with the colour detail (150 to 450 m). Test switch:
   `BF1_GROUND_NORMAL=<strength>` (0 = off).
3. Repeat size: the real value is compiled into the game's shader code.
   Measure the detail numbers at a few sizes and keep the closest one
   (a fit to the screenshot, not a value from the data).

### Plan: decal variants (object variations) (2026-10-10; done except step 6, which the data rejected; see MODLOG)

The 117 generic decals (`decal_128x128_mesh`) all show their default dirt
picture. The level picks a variant for most of them. How the data links it
(read from the Verdun EBX):
- A static model group member has `InstanceObjectVariation`: one value
  per instance, the variant's `NameHash` (0 = default). For groups whose
  positions come from Havok, we assume the same instance order.
- A reference object has `ObjectVariation`: a link to an `ObjectVariation`
  asset (with `NameHash`); it applies to the meshes in its blueprint.
- Each level part has a `MeshVariationDatabase`. Its entries join (mesh,
  `VariationAssetNameHash`) to one `MeshVariationDatabaseMaterial` per mesh
  material: `Material` (the mesh's own material) and `MaterialVariation`
  (a `MeshMaterialVariation`, null = unchanged). A `MeshMaterialVariation`
  has the same `Shader` block as a mesh material (shader, vectors,
  textures), for example `T_DecalGroundCracks_01_CA`.
- In Verdun (Conquest): 1,896 group instances in 52 meshes use a variant
  (decals 107, earth mounds 438, sandbags, bunker parts, lamps), and 343
  reference objects (charred trees 172, plastered walls 54, ruin houses 36).

Steps:
1. Walker: each placed mesh gets its variant hash (group value, else the
   nearest reference object's variant).
2. Loader: read the variation databases. For a placement whose variant
   changes at least one material, load the mesh once more for that
   (mesh, variant) with the variant's textures (and tint), falling back to
   the mesh material for anything the variant does not set.
3. `BF1_VARIATIONS=decals` (default: meshes under `artassets/decals`),
   `all`, or `off`. Log: placements per (mesh, variant), entries not found.
4. Check: counts in the log; the decal textures used; the user's view.

Found while doing steps 1 to 3 (2026-10-10): no decal was ever drawn. The
decal mesh's only section is in subset category 2 ("transparent decal"),
and `fb_res::mesh::decode_lod` decodes only categories 0 and 1. Placed
meshes with decal-only sections: 38 meshes, 368 placements, 482 sections
(the decal quads and bunker concrete walls and pillars). So:

5. Decode category 2 sections too, marked as decals.
6. Atlas: the decal materials have `TileCountX/Y` and `TileIndexX/Y` (base
   dirt decal: 2 x 2, index 0, 0; `dirt_01`: index y 0.5). We read the
   index as a UV offset: uv' = uv / count + index. A variant that sets none
   of these uses the whole texture (count 1, index 0); our reading.
7. Draw decal parts alpha-blended with a small depth bias, casting no
   shadow. `Opacity` scales the alpha; `DecalColorMultiply` multiplies the
   colour (the `_dark` variants set 0: dark stains). The `_Add`,
   `_RedTint` and `_LerpBaseColor` kinds are drawn the same way (not
   known). The puddle variant (`SS_PuddleSmall_01`) has no texture: its
   colour texture from the shader database if found, else left out.
8. Log: decal parts per kind, alpha coverage of the atlas tile used.

### Plan: detail in the mud (2026-10-10; steps 1 and 2 done, then undone at the user's request, see MODLOG)

The user's game screenshot of a mud path shows ruts, deep dark cracks and
wet shine; ours is flat. At that spot (`BF1_CAMERA="-257.6,70.0,14.4,408,
-50"`) the ground is 100% layer 0, `T_Gen_MudPile_02`, so the blend between
layers is not the cause. What the layer has that we do not use:
- `_NRH` blue (unused so far): mean 0.26, from 0 to 1. Correlation with the
  height (alpha) -0.36, with the smoothness (`_CS` alpha) -0.55, with the
  colour -0.30. We read it as cavity (1 - ambient occlusion): high in low,
  dry cracks. Not confirmed.
- `_NRH` alpha = height: not used yet (could give relief).

Steps (each with a test switch, each measured on a 2000 x 1125 screenshot
of the user's view: contrast and detail in the mud, against the game
screenshot):
1. Cavity: the layer colour times (1 - k * blue), blended by layer weight
   like the colour. `BF1_GROUND_CAVITY=<k>` (0 = off).
2. Relief: parallax occlusion mapping from the height near the camera
   (fades out by 40 m). `BF1_GROUND_PARALLAX=<depth in metres>` (0 = off).
   The real game probably moves the ground mesh (tessellation), which wgpu
   cannot do; parallax only shifts the texture.

Not in this plan: footprint and tyre decals, the texture repeat size.

### Plan: backdrop trees on the far hills (2026-10-10; steps 1 and 2 done, step 3 waits for the user's view)

The user's game screenshots show trees on the hills around the map; ours
are bare. Found (log "scatter layer .. cover" and "scatter map"):
- Scatter layer 16 (`ms_dm_treebackdrop_01`, 3 kinds, 0.021 per m2 in all)
  covers about 26 ha, almost all 1 to 2 km from the level centre, in the
  same map cells as the backdrop houses (`dm_backdrophouse_01_*`). Layer 17
  (`ms_wf_ashtreebackdrop_01`) covers about 4 ha, 0.5 to 1 km out.
- We read their draw distances as 500 m and 30 m, so they never show from
  the playable area. A backdrop tree with a 500 m range cannot be what the
  game does there; the meaning of the 4 stored values for these kinds is
  not known. (The value 5000 near them is the wind `Mass`, from the EBX.)
- The backdrop tree mesh is one quad (2 triangles), so it must face the
  camera, or it shows edge-on.

Steps:
1. Scatter kinds whose mesh name contains `backdrop` use a draw distance
   that covers the whole map (our rule, `BF1_BACKDROP_DISTANCE` overrides),
   with tiles up to 256 m (fewer tiles at long range).
2. Single-quad scatter meshes turn about the vertical axis to face the
   camera (updated when the camera moves 10 m).
3. Check: instance counts per kind in the log, frame rate (not with the
   real game open), and the user's view of the far hills.

Not in this step: the quad uses about a third of its texture (u 0 to
0.34); the other tree pictures may be chosen per instance by the shader.

### Plan: scattered grass and debris (2026-10-10; done, see MODLOG)

Found in the data:
- `TerrainData.TerrainLayers[n].MeshScatteringTypes` lists, per ground
  layer, 17 `TerrainMeshScatteringType`s with `Identifier` 0..16: density
  per quality (Low/Medium/High/Ultra), `MinScale`/`MaxScale` (x =
  horizontal, y = height), wind values and `DissolveRangeRatio`. Layers 4,
  5, 6 (grass, yellow grass, forest floor) and 12 to 17 (no texture, only
  scattering) have types.
- The terrain's `VisualResource` (4.7 KB) holds 17 records in identifier
  order, each with the mesh name (`artassets/meshscatter/...`). After each
  name there are 4 rising values per quality level, read as view distances
  in metres (grass 15/20/30/50, backdrop trees 100/400/500/500). The
  records are byte-packed; the reader finds the first 4 rising values
  after the name. Density reads as instances per square metre (grass 6,
  backdrop trees 0.01).

Steps:
1. `fb_terrain::scatter`: read the visual resource (mesh names, view
   distances), with a unit test.
2. Loader: join it with the EBX types per layer; load the 11 scatter meshes
   like other meshes; log the table (layer, mesh, density, scales,
   distance).
3. Game (`scatter.rs`): 8 m tiles around the camera. Per tile and type,
   random points (fixed by the tile position, so a tile looks the same each
   time): density x tile area candidates, each kept with probability = the
   layer's mask weight there; height from the terrain; random turn; scale
   in the given range. Tiles appear within the type's High view distance
   and go away beyond it (hard edge; the game fades them). Test switches:
   `BF1_SCATTER=off`, `BF1_SCATTER_DENSITY=<factor>`.
4. Check: instance counts and frame rate in the log; inspect key on the
   grass.

Not in this step: wind animation, the colour variation from the terrain
colour map (the scatter shader name says "ColorVariation"), and fading.

### Plan: position overlay and inspect key (2026-10-10; done)

For the user's map check (everything there, everything right), and an idea
from hl2-rs (F1 overlay, `getpos`):

1. Overlay (`overlay.rs`): text at the top left with the camera position,
   yaw and pitch in degrees, frame rate and weather, plus a ready-to-copy
   `BF1_CAMERA=...` line for coming back to the same view. F1 shows or
   hides it. It starts hidden when `BF1_SCREENSHOT_AT` is set (test
   screenshots must stay clean) or with `BF1_OVERLAY=off`.
2. Inspect key I (`inspect.rs`): a ray from the screen centre. It reports
   the first mesh part hit (mesh name, part number, texture, cut-out, tint
   and detail texture) or the terrain (height and the ground layers there,
   with their textures and weights). If the first hit is a leaf card, it
   also reports the first solid hit behind it. The result goes to the log
   with the `BF1_CAMERA` line, and to the overlay for 15 s.
   Data: the unique meshes' parts in mesh space (no world copies) and per
   placement its matrix, inverse and world box; the ray is tested against
   the boxes first, then the triangles in mesh space.
3. Check: a test run that presses nothing, but calls the same inspect code
   at a known camera (the rural house stove view) and compares with the
   view probe's result there.

### Plan: indoor light with a light probe grid (2026-10-08; done, see MODLOG)

Problem (user screenshot 48, MODLOG): inside buildings every surface is
lit by the full blue sky light, so rooms look pale blue-white. The user
asked for the approach closest to the game. BF1 uses precomputed global
illumination (Enlighten): light stored on surfaces (lightmaps) and in
light probes in space. We cannot read Enlighten's data (MODLOG dead end),
so we compute our own probes.

Bevy facts (bevy_pbr 0.19 source): an `IrradianceVolume` is a box of cells.
Each cell stores light from the 6 axis directions ("ambient cube"), 16-bit
float, so negative values work. Its light is added to the plain sky light
(`AmbientLight`), and both are scaled by the exposure. Overlapping boxes
are averaged, so boxes must not overlap.

Design: the plain sky light (sky + bounce) stays for the open world. Probe
boxes cover only areas with buildings or bunkers. A cell stores, per
direction, (visible share of the sky - 1) x the plain sky light, so it
removes the blocked part. Outdoor cells hold 0, so box edges cannot be
seen.

1. Covered areas: a 2D map of 1 m columns over the battle area where solid
   mesh geometry (the collision meshes, which skip foliage) lies 2 to 30 m
   above the terrain, plus terrain holes (bunkers). Group them into boxes.
   Log boxes and cell counts.
2. Bake, at load, on all CPU cores: solid meshes into a 0.5 m occupancy
   grid, plus terrain heights. For each 1 m cell and each of the 6
   directions, cast 12 rays spread around that direction (cosine-weighted):
   the share that reaches open sky (leaves the box top, or travels 40 m
   without a hit) is the visibility. Cells inside walls or below the
   terrain take the mean of their valid neighbours. Log time and the
   distribution of visibility.
3. Spawn the boxes as Bevy irradiance volumes; rebuild the values when the
   weather changes (they scale with the plain sky light).
4. Measure: view probe colours inside the rural house and in a bunker,
   against the user's interior screenshots (fire rain: (19, 9, 6) to
   (65, 45, 34)); outdoor views must not change.

As built (MODLOG 2026-10-08): a mesh hit blocks a ray only where the hit
point is covered or underground. Per direction the light is vis x A +
(1 - vis) x 0.3 x W, where W is the warm bounce colour at A's brightness.
The correction is scaled by 0.452, the factor Bevy applies to its plain sky
light on diffuse surfaces.

Known limits: 1 m cells blur light across walls (about half a cell); the
indoor fill is a fitted share; lamps are a later, separate step.

### Plan: flat grey walls (2026-10-08; step 3 done)

Problem (user screenshots 46, 47): large parts of ruin walls are flat grey.
Found (probe `mesh-elements`): these meshes have four texture coordinate
sets (vertex usages 0x21, 0x22, 0x23, 0x2a); we read only 0x21. On the
most common ruin piece (`ruinhouseend_01_chunk_a_mesh`, 23 copies), set
0x21 is collapsed (almost no texture area) on 87% of the main section's
surface, while 0x22 repeats normally (5.9 m per repeat). The shaders
(`FacadePreset_DetailsOverlay`, `SS_FortDetailBlend2UV_01_DestVol`) take a
repeating wall texture plus a unique detail texture: 0x21 looks like the
detail layout, 0x22 like the repeating one.

1. Tried and dropped: use set 0x22 when 0x21 is mostly collapsed. On the
   placed ruin piece (`xpack1/.../wf_houseruinset_01/`) the two sets
   complement each other instead: 18% of the main section's faces are
   collapsed in 0x21 and mapped in 0x22 (100%), 33% the other way round.
2. Detail material (`detail.rs`, `detail.wgsl`): StandardMaterial with the
   base texture on set 0x21, plus the detail texture on set 0x22 (Bevy's
   second UV attribute). Two blends:
   - `..._DetailsOverlay` presets (`_BaseColor` + `_BaseColorDetail`, no
     detail alpha: alpha is smoothness, mean 0.14): colour = base x detail /
     N. N is the detail texture's colour where 0x22 is collapsed (measured
     per part in the loader; texture mean if no face is collapsed). Measured
     on the ruin piece: wall faces then keep the wall colour exactly; detail
     faces get the detail colour x 1.07 (the wall's collapse colour / N).
   - `FortDetailBlend2UV` (`Base_Texture_CS` + `Detail_Texture_CA`, alpha is
     an on/off mask, mean 0.77): colour = mix(base, detail, detail alpha).
   Which set each texture uses is from the shader name ("2UV") and the
   measurements above; not confirmed for the archway shader.
   Check: view probe colours and image detail on ruin walls, before/after.
3. Corrections after the user's screenshots 49 to 51 (stone atlas pieces in
   the wrong places, brick patches like stickers). Measured with the probe
   (`mesh-elements --alpha`, alpha change inside a triangle; small = the
   atlas is sampled with its own coordinates):
   - `FortDetailBlend2UV`: the archway's `Arch_01_B_CA` changes 0.010 with
     0x21 and 0.211 with 0x22. So the detail is on 0x21 and the base brick
     on 0x22: swap the sets.
   - `..._DetailsOverlay`: the detail normal texture `_NormalDetail` (`_NAA`)
     has an on/off mask in alpha (spread 0.48), 0.004 on the faces whose
     detail coordinates are collapsed. So the blend is mix(base, detail,
     mask), not a multiply. The normaliser is removed.
   Both families: colour = mix(base, detail, mask); the mask is the
   `_NormalDetail` alpha or, for `Detail_Texture_CA`, the detail's own alpha.

### Plan: colour grading fix (2026-10-07; done: `pq-only` at 100 nits, plus bounce light)

Problem: shaded ground is almost black-blue: (3, 10, 24) in ours, about
(26, 31, 37) in the user's screenshots. Sunlit ground is too bright. Cause
(measured, MODLOG): both grading tables are S-curves around 0.5. We apply
them to finished screen colours, and together they push dark tones down by
about 3 stops.

- Weather table (`HdrColorGradingLut`, 33^3, 10 bit): near identity in the
  middle, slope about 1.27 around 0.47, deep shadows bluer.
- Plain cube (`ColorGradingTexture`, 32^3, 8 bit): S-curve around 0.5
  (index 0.26 -> 0.15, 0.77 -> 0.84).

Hypothesis (not confirmed): the game applies the weather table to scene
light before tone mapping, in the PQ encoding (ST 2084, as I remember a
public Frostbite HDR talk). The PQ scale (scene value 1.0 = S nits) is not
known.

Steps:
1. Grading modes in `grading.rs` (test switch `BF1_GRADING`):
   - `display`: the current way (both tables on screen colours);
   - `pq`: weather table on scene light in PQ (`BF1_GRADING_NITS` = S),
     then Bevy's tone mapping, then the plain cube on screen colours;
   - `pq-only`: the same without the plain cube;
   - `off`.
   Each cell of Bevy's 48^3 table stands for a known scene light value
   (cell / (cell + 1) encoding), so this stays one table and one pass.
2. Measure each mode at the gun view against the user's screenshot 44: the
   brightness percentiles of the whole frame (new `image-stats` output) and
   the bright and dark quarters of the ground. Keep the best mode and S, and
   record all results.
3. Then check the sun strength and sky light again (they were calibrated
   with the old grading). Result: deep shade was still too dark; more sky
   light alone made it too blue, so the ambient light got a bounce term
   (sunlight x mean ground colour / pi x 2, `scene::bounce_light`).

### Plan: material tints, part 1 (2026-10-07)

Some materials get their colour from a shader value, not from the texture.
Their textures are neutral grey, so we draw them grey. About 30 kinds of
tint exist on Verdun; part 1 covers the two on the howitzer the user
compared (screenshot 42):

- Paint: shader `45812999` (no name), value `Color` and a `Masks` texture
  (20 materials: howitzers, mortars, Vickers guns). Masks green = painted
  (89% on the howitzer); red and blue are 0, alpha is 1.
- Fabric: preset `ObjectPreset_CS+N_Fabric`, value `TintColor`, and
  `BaseColorTiling` (texture repeats; 3 materials).

Rules (fits to the user's screenshots, not known from the data; tint values
read as linear colours; `load::tint_base_colour`):
- paint: the tint is the part's mean colour, the texture keeps its detail
  (base colour = tint / texture mean);
- fabric: texture x tint x 2.
Measure: sunlit part / sunlit ground in the same picture (view probe
"bright quarter"), so that errors in our light cancel. Tried and dropped:
"overlay" blend (too bright, user's screenshot 45), "x 2" for paint (too
dark), "tint = mean" for fabric (too bright). Not in part 1: the paint mask
(the unpainted 11% are tinted too), the other tint kinds.

Part 1b, paint mask: painted parts get their own material (`paint.rs`: Bevy's
StandardMaterial plus a small shader, `paint.wgsl`). The shader reads the
`Masks` texture's green channel at the part's texture coordinates and
multiplies the colour texture by the paint factor (tint / texture mean)
only where the mask is set; elsewhere the plain texture shows. Check: the
log gives the painted share of each mask; the view probe colours of the
howitzer before and after.

## Decisions

- **Engine:** Bevy (approved 2026-10-03).
- **Rust:** the installed toolchain is 1.89. A recent Bevy may need a newer
  Rust. We update Rust (`rustup update`) when we reach step 6, not before.
- **Pure-Rust decompression** (`lz4_flex`, `flate2`, `ruzstd`) so the build
  needs no C compiler.
- **Delta patches:** step 1 detects and counts them. It decodes them only if
  Verdun needs them.
