# Mod log

Newest entry at the top. One entry per change.

## 2026-10-10: Beginner setup guide; which game files are read; empty game folder

**Request (user):** the public README is not clear enough for someone new.
Which game files are needed? Add an empty `Battlefield 1 Files` folder for
users to paste into.

**Measured (new log line "game files read", tested by Claude):**
- The program opens only files inside `Data` and `Patch`. The code builds
  only those two paths from the game folder.
- In those two folders it reads every `.toc` and `cas.cat` (indexes) and
  the level's `.sb`.
- Of the `.cas` archives it read 110 of 486 (48.9 of 86.0 GB), in 51 of
  the 78 archive folders, in both `Data` and `Patch`. Verdun's shared art
  is in many other maps' and the campaign's archives, so both folders are
  needed in full.
- Sizes as Windows shows them: `Data` about 65 GB, `Patch` about 16 GB; all
  other files of the install (`bf1.exe`, `.dll`, `Core`, `Support`,
  `EAAntiCheat`, `__Installer`, `__overlay`) about 0.5 GB, never opened.

**What changed:**
- `fb_vfs`: `GameFs::cas_used` records the archives read. The game logs
  them per folder at the end of loading.
- `fb_vfs::find_game_dir`: the error messages say what to copy where.
- README: new sections "What you need" and steps 1 to 4:
  - install Rust;
  - get the project;
  - add the game files: copy `Data` and `Patch`, or link them, or set
    `BF1_GAME_DIR`; with a folder diagram;
  - run it.
  Plus "Controls" and "Tools".
- `.gitignore` step 6: `Battlefield 1 Files` is tracked only for its note
  file `PUT_DATA_AND_PATCH_HERE.md`; git ignores everything else in it and
  does not look into `Data` or `Patch`. Checked with `git check-ignore`:
  `Data\layout.toc`, `Patch\x.cas` and `bf1.exe` in that folder are
  ignored.
- The note file exists only in `publish/`. I did not write it into this
  machine's `Battlefield 1 Files`: that folder holds the game copy, and
  AGENTS.md rule 4 says game installs are read, never written.

**Mistake (fixed):** one test command made an empty folder `bf1-msg-test`
in the Windows Temp folder, outside the project. I removed it (it was
empty).

**Not checked:** the README steps on a new PC; the Steam menu path ("Manage
> Browse local files") and rustup's C++ build tools question are from
common knowledge, not tested here.

**How to test:** run `bf1_probe` from a folder without game files: the error
should say to copy `Data` and `Patch` into `Battlefield 1 Files`. A normal
run logs "game files read: ...".

## 2026-10-10: Scattered grass and debris (terrain mesh scattering)

**Request (user):** add the scattered grass and debris meshes.

**Found in the data (DESIGN.md "scattered grass and debris"):**
- `TerrainData.TerrainLayers[n].MeshScatteringTypes`: 17
  `TerrainMeshScatteringType`s (identifiers 0 to 16). Each has a density
  per quality level, a scale range (x horizontal, y height), wind values
  and `DissolveRangeRatio`. They sit on layers 4, 5 and 6 (grass, yellow
  grass, forest floor) and on layers 12 to 17 (scattering only).
- The terrain's `VisualResource` holds 17 records in identifier order, each
  with the mesh name and a group of 4 distance values. 11 different meshes:
  grass blades, dead tall grass, straw, ditch foliage 1 and 2, ground ivy,
  branch piles, brick and plank debris, backdrop tree billboards.
- Reading the distance group took 3 tries:
  - attempt 1 took the first plausible numbers (wrong, 1 to 60 m);
  - attempt 2 required zero bytes around the group (none found; I had
    misread small integers as zeros);
  - attempt 3, kept: the last plausible group of 4 in each record. It
    matches the values I read by hand for all 17.
  The groups are not always rising (for example 15, 20, 7, 15); we use the
  largest as the draw distance: grass 50 and 20 m, debris 20 to 25 m,
  backdrop trees 500 and 30 m.
- Textures:
  - the scatter materials name their colour texture `BaseColorMap` or
    `BillboardColor` (now in our name list);
  - the grass blade shader (`SS_MS_DM_MeshGrass_01`) has no colour texture
    at all (only `T_GrassBlade_01_NRT`; new probe command
    `shader-textures`). It has `TerrainBlend 1` and the tint `Color1`
    (0.64, 1.0, 0.70), so the blades take the ground colour. We use the
    mean colour of the ground layer textures at the tile, by layer weight,
    times the tint (our choice);
  - the backdrop tree texture has a soft alpha mask (59% clear pixels)
    that the leaf-card rule missed. For scatter textures that are not
    `_CS`, 20% clear pixels now means cut-out.

**What changed:**
- `fb_terrain::scatter`: reads the visual resource; with a unit test.
- Loader:
  - `scatter_types` joins the EBX types with the records;
  - the scatter meshes load like other meshes;
  - a shader-database fallback finds colour textures for shader-graph
    materials;
  - `Part::material_id` is new;
  - the log lists each type.
- New `scatter.rs`:
  - tiles around the camera (8 m, larger for long ranges);
  - deterministic random points per tile, kept by the layer's mask
    weight, with the terrain height, a random turn and scale;
  - tiles appear within the draw distance and go away beyond it, at most
    48 new tiles per frame;
  - work only when the camera moves 1 m or tiles are missing;
  - empty tiles get no entity.
- Test switches: `BF1_SCATTER=off`, `BF1_SCATTER_DENSITY`,
  `BF1_SCATTER_DISTANCE`.
- New tests: the record reader and the tile random numbers. 20 game tests,
  4 `fb_terrain` tests and 5 `fb_res` tests pass.

**Results (tested by Claude, numbers only), after the tile-size fix:**

| view | instances | fps with scatter | fps without |
|---|---|---|---|
| howitzer | 7,434 (grass 5,078, ivy 1,901) | 147 to 154 | 168 to 174 |
| battle-area start | 182 | 166 to 188 | 163 to 177 |
| village from the hill | 5,812 | 135 to 167 | 171 to 176 |

The first version kept about 26,000 mostly empty tiles (backdrop trees,
500 m range, with 8 m tiles), and the start view fell to 60 fps. Now
there are about 1,900 tiles.

**Not done:** wind, the fade at the edge of the draw distance (instances
pop in and out), the game's own grass colour source, and the inspect key
does not see scatter instances. No backdrop trees appeared in the tested
views. Not checked: the picture.

**How to test:** `cargo run --release -p bf1_game`. Grass, ivy, foliage and
debris should cover the ground near you on grass, forest floor and rubble
areas. `BF1_SCATTER=off` shows the ground without them.

## 2026-10-10: Position overlay (F1) and inspect key (I)

**Request (user):** add the position overlay and the inspect key, for
checking the map.

**What changed:**
- New `overlay.rs`: text at the top left with the camera position, yaw and
  pitch, frame rate, weather, and a ready-to-copy `BF1_CAMERA` line. F1
  shows or hides it. Hidden in screenshot tests (`BF1_SCREENSHOT_AT`)
  unless `BF1_OVERLAY=on`; `BF1_OVERLAY=off` hides it always.
- New `inspect.rs`: key I casts a ray from the screen centre. It reports the
  first mesh part (mesh, part number, texture, cut-out, tint, detail
  texture) or the terrain (height, ground layers with their textures and
  weights). Behind a leaf card it also reports the first solid hit. The
  result goes to the log (`inspect: ...`) and to the overlay for 15 s.
  `BF1_INSPECT_AT=<seconds>` runs it once without a key, for tests.
- Data: the unique meshes' parts in mesh space plus each placement's
  matrix, inverse and world box (built in 0.05 s); boxes first, then
  triangles.
- `view_probe::hit_terrain` takes a range.
- README: controls and how to report a problem.
- 2 new tests (triangle and box hits; mesh-space ray keeps world
  distances); 19 game tests pass.

**Results (tested by Claude):**
- Stove view (`-111.5,56.4,40.5,-90,-5`): inspect says
  `wf_houserural_01_mesh part 0 (t_brickred_01_cs; detail ...)` at 4.9 m;
  the view probe says the same part at 4.8 m.
- Howitzer view: its cloth (part 1, fabric tint) at 6.2 m.
- Ground at the attacker spawn: terrain, layer 6 forest floor 88%, layer 12
  (mesh scattering only) 12%.
- Overlay drawn: top-left corner p10 29 / p90 149 with it, 45 / 53
  without.
- One test run logged three inspects. The automatic trigger runs once and
  two repeat runs logged one each, so the extra two were I key presses
  that reached the test window.

**How to test:** `cargo run --release -p bf1_game`. The text is at the top
left; F1 hides it. Look at an object and press I: the text shows what it
is, and the log has an `inspect:` line.

## 2026-10-10: First public release on GitHub (one commit)

**Request (user):** publish everything as one commit on GitHub without
personal data.

**What changed:**
- `AGENTS.md`, `MODDING_PLAN.md`: the local install path is replaced by "the
  `Battlefield 1 Files` folder".
- `README.md`:
  - the outdated colour grading sentence is removed;
  - the intro no longer says "(later)";
  - new section "Set up the game folder": `BF1_GAME_DIR`, or a copy or
    junction named `Battlefield 1 Files`;
  - the user's title from GitHub is kept.
- The release is built in the git-ignored `publish/` folder. It holds only
  the files that git would publish (57) and a fresh history: one commit,
  author `AaronSparks` with the GitHub private address, UTC time, with the
  Claude co-author line. The main folder keeps its own history.

**Checks (tested by Claude):**
- Scan of the committed content: no email, Windows user name, local path,
  LAN address or keys.
- A fresh build of `publish/` passes all 34 tests.
- After the user's force push, GitHub's `main` is the same commit as local
  (`6e66a59`), 1 commit.

**Not checked:** the Steam default path in the README
(`steamapps\common\Battlefield 1`); this machine's game folder is a copy.

**For later updates:** copy the published files to `publish/` again, run the
same personal-data scan, then commit there and push. Edits on the GitHub
website make commits with the local time zone.

## 2026-10-08: Detail textures, corrected (sets and blend mask)

**Problem (user screenshots 49 to 51):**
- Church walls still partly flat grey, with faint arch, window and
  quatrefoil shapes in wrong places.
- Brick patches on a ruined house wall looked like stickers.

**Found (probe `mesh-elements --alpha <texture>`: how much a texture's
alpha changes inside one triangle; small = sampled with its own
coordinates):**
- `FortDetailBlend2UV` (church pieces): the archway's `Arch_01_B_CA` changes
  0.010 with set 0x21 and 0.211 with 0x22. So the detail atlas is on 0x21
  and the brick base on 0x22, the reverse of my first reading (attempt 2
  of the previous entry was wrong for this family). The detail atlas on
  the repeating set gave the scattered shapes; the brick base on the
  detail set gave the flat grey.
- `..._DetailsOverlay` (ruined houses): the detail normal texture
  `_NormalDetail` (`_NAA`) holds an on/off mask in alpha (spread 0.48). On
  the faces whose detail coordinates are collapsed, it is 0.004. With 0x22
  it changes 0.137 inside a triangle, with 0x21 0.355. So the blend is
  mix(wall, detail, mask) on 0x22, not my normalised multiply.

**What changed:**
- `detail.wgsl`: colour = mix(base, detail, mask alpha). It has no mode and
  no normaliser now; the mask is a second texture (the detail itself for
  `Detail_Texture_CA`).
- Loader: for `Detail_Texture_CA` the two sets are swapped (base on 0x22,
  detail on 0x21). For `_BaseColorDetail` the mask is `_NormalDetail`;
  without one the part is drawn without detail. The normaliser code is
  removed.
- Probe: `mesh-elements --alpha <texture>`; `--sample` also prints alpha.

**Results (tested by Claude, numbers only; probes off for comparison):**
- Church from the east (camera `-80,69,-150,90,-5`), pillars and supports
  region, fine detail 1 px / 4 px:

  | state | detail |
  |---|---|
  | before the detail work | 1.9 / 5.0 |
  | previous entry | 4.3 / 10.2 |
  | now | 4.3 / 10.1 |

  The brick base now covers the walls on its own: 4.3 / 10.2 with the
  detail turned off. The detail darkens the pillars a little:
  (64, 67, 69) -> (58, 61, 63).
- Ruined house wallpaper wall: most wallpaper faces are unchanged (mask
  near 0); one part shows its damage detail: (44, 58, 70) -> (29, 42, 56).
- 17 game tests pass. No shader errors.

**Not checked:** the tall church facade of screenshot 50 (I did not find
its mesh by name; my test camera missed it), and the picture itself.

**How to test:** close the running game, then `cargo run --release -p
bf1_game`. Fly to the church ruin at about (-98, 66, -152): walls should
show bricks everywhere and stone arch pieces only on arches and edges.
`BF1_DETAIL=off` shows the walls without the detail.

## 2026-10-08: Indoor light with a light probe grid

**Request (user):** fix the white-blue wood inside buildings, with the
approach closest to the game. BF1 stores precomputed light in light probes
(and lightmaps); a probe grid is the closest Bevy feature (`IrradianceVolume`).

**What changed (new `probes.rs`, DESIGN.md "indoor light with a light
probe grid"):**
- Step 1: a coverage map of 1 m columns over the battle area: roof-like
  solid geometry 2.2 to 30 m above the terrain, or geometry more than 2.5 m
  underground, or terrain holes. Solid = the visible meshes without leaf
  cards, decals, vegetation and backdrops. Covered 16 m tiles -> one box
  per 64 m chunk (Bevy uses the 8 boxes nearest to the camera and averages
  overlapping ones, so chunks do not overlap). Result: 62 boxes, 3.13
  million 1 m cells (150 MB of video memory).
- Step 2, at load, on all cores: solid triangles into 0.5 m occupancy
  voxels (sparse 8 m blocks) and a 1 m terrain height grid. For each cell
  within 2 m of a covered column, 12 cosine-spread rays per direction (6
  directions) give the visible share of the sky. A mesh hit blocks only
  where the hit point is covered or underground; outdoor objects and the
  terrain count as open, because the plain sky light already stands for
  them. Cells inside solid geometry take their neighbours' mean.
- Step 3: one Bevy irradiance volume per box, 16-bit float ambient cubes.
  Per direction: light = vis x A + (1 - vis) x 0.3 x W - A, scaled by
  0.452. A = the plain sky light (sky + bounce). W = the bounce colour
  (warm) at A's brightness. 0.452 = the factor Bevy applies to its plain
  sky light on diffuse surfaces, `F_AB(1, NdotV).x`, but not to volume
  light. The volumes are rebuilt when the weather changes.
- Test switches: `BF1_PROBES=off`, `BF1_INDOOR_FILL=<share>`.
- 3 new tests: half floats, ray directions, voxel marking. 17 game tests
  pass.

**Failed on the way (recorded):**
1. Every mesh hit counted as blocked. Indoors that gave black rooms
   (1, 1, 3). Outdoors near sandbags and boards the shadows went darker
   (gun view p10 20 -> 2).
2. Fixed the outdoor rule; the rooms stayed black for every fill value.
   Cause: Bevy scales its plain sky light by 0.452 on diffuse surfaces but
   not the volume light, so the volume removed about 2.2 times too much.
3. Correct scale, sky-coloured fill: the rooms were dark but blue
   ((8, 17, 32) at fill 0.2). The fill now has the warm bounce colour.

**Results (tested by Claude, numbers only):**
- Bake: step 1 2.6 s, voxels and heights 2.3 s, rays 7.8 s (32 threads);
  847,674 cells computed. The level now takes about 12 s longer to load.
- Rural house next to the stove (camera `-111.5,56.4,40.5,-90,-5`), whole
  frame:
  - sunny: (33, 45, 60) -> (14, 21, 29);
  - fire rain: (29, 12, 5), p50 16. The user's fire rain interiors:
    (19, 8, 5) p50 9 and (24, 15, 10) p50 15.
  - Fill 0.2 and 0.35 were both measured; 0.3 is between them.
- Outdoor gun view: (52, 59, 58) without probes, (50, 57, 56) with; p10 20
  -> 17. Frame rate unchanged (59 fps in this heavy view; 174 to 179 at the
  start view).

**Not sure / limits:**
- The fill 0.3 and the bounce colour are fits.
- 1 m cells blur light across walls (about half a cell): outside walls get
  slightly darker, inside walls slightly brighter.
- Only the 8 nearest boxes are used, so distant interiors stay bright.
- Lamps are not placed yet.
- The sunny interior is still a little blue (window light is sky light).

**How to test:** `cargo run --release -p bf1_game` (loading takes about
12 s longer). Fly into the rural house at about (-112, 56, 41): the rooms
should be dark, not pale blue-white. Press G for fire rain: warm, dark
rooms. `BF1_PROBES=off` shows the old look.

## 2026-10-08: White-blue wood inside houses (found, not fixed)

**Problem (user screenshot 48):** the wood inside some buildings is white.

**Found (view probe, camera `-111.5,56.4,40.5,-90,-5`, inside the rural
house at (-112.5, 54.5, 41.0), next to its stove):**
- Every surface is strongly blue: plaster (37, 50, 66), interior planks
  (51, 66, 84), stove (10, 20, 35); whole frame (33, 45, 60).
- The walls and roof block the sun, so the only light inside is the sky
  light, at full strength, plus the bounce term; nothing dims it indoors.
- The user's fire rain screenshots show warm, dark interiors: (19, 9, 6) to
  (65, 45, 34).
- The plank texture itself is brown, not white: `T_WoodPlanks_01_CS` mean
  (0.44, 0.40, 0.35) sRGB.
- Many house materials also have tints that we do not apply ("Tintmask"
  presets, `_Tintcolor1` 0.22 to 0.56, some also `_Tintcolor2`). Where
  their mask is stored is not known.

**Next:** indoor light (DESIGN.md "match the reference look", step 2). This
touches every material, so it waits for the user's choice of approach.

## 2026-10-08: Detail textures on buildings (flat grey ruin walls)

**Problem (user screenshots 46, 47):** large parts of the ruined church
walls were flat grey.

**Found (new probe command `mesh-elements`):** building meshes have four
texture coordinate sets (vertex usages 0x21, 0x22, 0x23, 0x2a); we read only
0x21. Their shaders take a repeating base texture plus a detail texture:
- `..._DetailsOverlay` presets: `_BaseColor` + `_BaseColorDetail`; the
  detail's alpha is smoothness (mean 0.14).
- `FortDetailBlend2UV`: `Base_Texture_CS` + `Detail_Texture_CA`; the
  detail's alpha is an on/off mask (mean 0.77).

On the placed ruin piece `ruinhouseend_01_chunk_a_mesh`, sets 0x21 and 0x22
complement each other: 18% of the main section's faces are collapsed in
0x21 (one texture point) and mapped in 0x22 (100%); 33% the other way
round. Each texture at its collapse points is a plain light grey: wall
(0.615, 0.603, 0.583), detail (0.596, 0.579, 0.552) sRGB.

**Failed attempt 1:** use set 0x22 for the base texture when 0x21 is mostly
collapsed. My probe had read a different asset with the same file name
(`ruinset_01/...` instead of `xpack1/.../wf_houseruinset_01/...`). On the
real piece, 0x22 is worse (32.6% collapsed against 17.9%). Removed.

**What changed (attempt 2):**
- `fb_res`: decodes set 0x22 (`Geometry::uvs1`).
- Loader: for materials with a detail texture, it loads the detail texture
  and keeps the part's second set (`Part::detail`, `Part::uvs1`). For the
  multiply blend it measures the normaliser: the detail texture's colour at
  the faces collapsed in 0x22 (texture mean if under 1% are collapsed).
- New `detail.rs` and `detail.wgsl`: base texture on set 0x21 plus detail on
  set 0x22.
  - Multiply blend: base x detail / normaliser. Wall faces keep the wall
    colour; detail faces get the detail colour x about 1.07.
  - Alpha blend: mix(base, detail, detail alpha).
- `BF1_DETAIL=off` turns the detail off, for comparison;
  `BF1_UV_DEBUG=<mesh name part>` logs the per-part numbers.
- The load log counts the parts with detail textures and the surface that
  still has collapsed coordinates, and lists the meshes with the most
  untextured surface.
- Probe: `mesh-elements` also samples textures at collapse points
  (`--sample 0x21=<texture>`); `texture-compare` was added earlier.

**Results (tested by Claude, numbers only):**
- Load: 73 of 760 textured parts have a detail texture (9.1% of the
  textured surface): 37 `housepreset_detailsoverlay`, 9
  `ss_fortdetailblend2uv_01`, 8 `..._destvol`, 7 `...tilebreaker`, 6
  `...tintmask`, 6 `facadepreset_...`. No shader errors. 14 game tests pass.
- Church from the east (camera `-80,69,-150,90,-5`), regions that the view
  probe confirmed as ruin pieces. Fine detail (1 px / 4 px), detail off ->
  on:

  | region | detail off | detail on |
  |---|---|---|
  | pillars and supports (83%) | 1.9 / 5.0 | 4.3 / 10.2 |
  | left (supports 41%) | 5.6 / 10.6 | 7.7 / 15.1 |
  | right | 1.8 / 3.7 | 2.5 / 5.1 |

  The colours get a little darker; the stone detail is darker than the brick
  base. Example: support mean (71, 74, 77) -> (60, 62, 65).
- Ruin house piece (camera `-76.7,55,54.1,90,0`): wall region 2.7 / 6.7 ->
  3.3 / 8.1; mean colour unchanged.

**Not sure:** which set the archway shader (`FortDetailBlend2UV`) uses for
which texture. I followed the name ("2UV": detail on the second set) and
the ruin house measurements. Not confirmed.

**Not fixed:** surface whose coordinates are collapsed in every set we
read: 4.5% of the textured surface. The largest are the observation turret
base (380 m2), the barn wall `housebarn_01` material 4 (311 m2) and the far
backdrop. Not checked: the picture.

**How to test:** `cargo run --release -p bf1_game` (close the running game
first), then fly to the ruined church at about (-98, 66, -152). Walls should
show stone blocks and edges instead of flat grey areas. `BF1_DETAIL=off`
shows the old look.

## 2026-10-07: Colour grading on scene light (PQ); bounce light

**Request (user):** the colour grading fix (shadows almost black).

**What changed:**
- `grading.rs`: new modes. The default is now `pq-only`: the weather table
  (`HdrColorGradingLut`) works on scene light in the PQ encoding (ST 2084;
  scene value 1.0 = 100 nits), before Bevy's tone mapping. The plain cube
  (`ColorGradingTexture`) is not applied.
  - It is still one table and one pass. Each cell of Bevy's 48^3 tone map
    table stands for a known scene light value (x / (x + 1)).
  - Test switches: `BF1_GRADING=display|weather|plain|pq|pq-only|off` and
    `BF1_GRADING_NITS`. `display` is the old way.
  - 2 new tests: PQ known values and round trip; tone map cell round trip.
- `scene.rs`: the ambient light is the sky light from the data plus a new
  bounce term: sunlight on level ground x the map's mean ground colour
  (from the terrain colour map, linear (0.144, 0.136, 0.082)) / pi x
  `BOUNCE` (2). The game's global illumination gives this light; we do not
  compute it. Base weather: sky (1,496, 2,462, 3,470) plus bounce
  (1,455, 1,236, 576) cd/m2. Test switches: `BF1_BOUNCE`, `BF1_SKY_SCALE`.
- Probe: `image-stats` regions also give luminance percentiles (p10, p25,
  p50, p75, p90).

**Results (tested by Claude, numbers only; regions that the view probe
confirmed as ground):**

| view | old (`display`) | `pq-only` | `pq-only` + bounce 2 (kept) | user's screenshot 44 |
|---|---|---|---|---|
| open ground (yaw 129), mean | (52, 45, 44) | (55, 54, 55) | (61, 59, 58) | (57, 55, 54) |
| open ground, p10 / p50 / p90 | 24 / 44 / 74 | 35 / 54 / 75 | 41 / 58 / 79 | 42 / 54 / 70 |
| deep shade (crater), mean | (3, 10, 24) | (5, 15, 31) | (13, 23, 35) | about (26, 31, 37) |
| gun view frame, p10 / p25 / p50 | 9 / 17 / 71 (fire rain) | 14 / 20 / 51 | 22 / 28 / 57 | 22 / 38 / 55 |

Other modes measured:
- `pq` (with the plain cube after): everything too dark (open ground
  (31, 31, 36)).
- `pq-only` at 300 or 1,000 nits: too bright.
- Sky light x 2 instead of bounce: right brightness in deep shade, but too
  blue (14, 29, 48).
- Bounce 0.5, 1, 2 and 3 were all measured; 3 fits deep shade best but
  makes open ground too bright.

Fire rain (3 views): p10 20 to 33, p50 66 to 72, p90 91 to 98, mean
(82..92, 56..62, 41..49). The user's 7 outdoor fire rain screenshots: p10
21 to 39, p50 38 to 77, p90 90 to 105, mean (71..94, 50..67, 32..56). The
old mode gave p10 9 to 20, darker than all of them. The earlier choice of
"both tables" for fire rain was made while the sky dome still blocked the
sun, so it was not reliable.

Tests: 13 game, 5 `fb_res`, all pass.

**Not sure:**
- That the game uses PQ here, and the scale (100 nits per unit). It fits
  the numbers, and 100 nits is the usual reference white.
- Where the game uses the plain cube.
- Bounce share 2 is a fit, not data.

**Not fixed:** deep shade is still darker and bluer than in the game
((13, 23, 35) against about (26, 31, 37)).

**How to test:** `cargo run --release -p bf1_game`. Shaded ground should be
dark grey-brown with visible detail, not black-blue; sunlit ground a little
less bright. Press G for the other weathers. `BF1_GRADING=display` shows
the old look.

## 2026-10-07: Ground normal maps and smoothness (ground detail, step 2)

**Request (user):** normal maps for the ground; the terrain still looks very
different from the game.

**What changed:**
- `fb_res::shaderdb`: `block_texture_slots`, `blocks_of`,
  `pair_next_slot_nrh` and `terrain_layer_normals`. They pair each ground
  colour texture (`_CS`) with the `_NRH` texture in the next shader slot
  (most common pair wins). New unit test.
- Loader: each ground texture loads its `_NRH` texture
  (`GroundTexture::nrh`). The log shows the pair per layer.
- `ground.rs`: the colour array keeps `_CS` alpha (smoothness; it was set to
  255 before). A second array holds the 12 `_NRH` textures, decoded to RGBA8
  at 1,024 pixels (about 67 MB of video memory). A layer without one gets a
  flat normal.
- `terrain.wgsl`: it blends normal and smoothness by the layer weights, like
  the colours. It bends the terrain normal (tangent = world +X, +green =
  world -Z, as measured) and sets roughness = 1 - smoothness. Both fade out
  with the colour detail (150 to 450 m). `BF1_GROUND_NORMAL=<strength>`
  (0 = off) is for tests.

**Results (tested by Claude, numbers only):**
- 185 of 185 terrain shaders found; 12 of 12 layers have a normal map; no
  shader errors. Tests: 11 game, 5 `fb_res`, all pass.
- Fine detail (neighbour difference, 1 px / 4 px), regions that the view
  probe confirms are 98 to 100% terrain, camera at (-259.2, 64.3, 3.2),
  pitch -35:

  | yaw, region | normal maps off | normal maps on |
  |---|---|---|
  | 129, left (sun and shade) | 4.8 / 9.3 | 9.6 / 17.8 |
  | 129, right | 3.7 / 8.4 | 6.5 / 14.4 |
  | -51, left (sunlit) | 12.2 / 23.8 | 15.4 / 30.0 |
  | -51, right | 5.7 / 11.7 | 8.9 / 17.4 |
  | 39, left (mostly shade) | 3.1 / 8.0 | 5.0 / 12.6 |

  User's screenshot 42, near ground: 10.1 / 18.3.

**Found:** normal maps change only the sunlight. In shade our ground is
almost black-blue: (3, 10, 24) in the crater at the start point, against
about (26, 31, 37) in the user's screenshot. So shaded ground shows almost
no detail. Sunlit ground is too bright and too red: (105, 89, 81) mean at
yaw -51, against about (58, 54, 54) to (74, 70, 68) bright in the user's
screenshot. Both point to the colour grading (MODLOG entry "colour grading
crushes shadows") and to weak sky light.

**Not checked:** the picture (wet mud should now shine a little: smoothness
up to 0.96 in `T_Gen_MudPile_02_CS`).

**How to test:** `cargo run --release -p bf1_game`. Look at sunlit ground
near you: it should show bumps, clods and leaf relief that change with the
sun, not a flat print. Compare with `BF1_GROUND_NORMAL=0`.

## 2026-10-07: Paint mask (bare metal parts of guns no longer painted)

**Request (user):** finish the paint mask, before the colour grading change.

**What changed:**
- New `paint.rs` and `paint.wgsl`: a material for painted parts (Bevy's
  StandardMaterial plus a small shader). The shader reads the `Masks`
  texture's green channel and multiplies the colour texture by the paint
  factor only where it is set; elsewhere the plain texture shows.
- Loader: the paint tint carries its `Masks` texture (loaded at full size;
  512 pixels).
- Game: painted parts with a mask use the new material; the log shows each
  mask and its painted share. `BF1_PAINT_MASK=off` paints the whole part,
  as before.
- Probe: `texture-compare <superbundle> <A> <channel> <B> <channel>` gives
  the correlation between two texture channels.

**Results (tested by Claude, numbers only):**
- Painted share: howitzer body mask 91%, barrel mask 93%.
- The mask matches the texture layout. Where the mask says "not painted",
  the `_NAM` metal channel is high:

  | texture | correlation | metal, unpainted | metal, painted |
  |---|---|---|---|
  | body | -0.97 | 0.86 | 0.10 |
  | barrel | -0.73 | 0.58 | 0.04 |

- Gun view (camera `-259.2,64.3,3.2,-141,-20`, 60 x 60 rays), howitzer
  body:

  | | mean | bright quarter |
  |---|---|---|
  | mask off | (28, 44, 36) | (82, 105, 73) |
  | mask on | (35, 49, 46) | (103, 117, 99) |

  Sunlit body / sunlit ground (green channel): 1.33, user's screenshot about
  1.35. Barrel: 1.69 (no change, as expected; little bare metal there).
- No shader errors. 11 game tests pass.

**Not checked:** the picture. **Not done:** the bare metal parts are not
drawn as metal (the `_NAM` metal channel is not used yet). Colour grading
still crushes the shadows (previous entry).

**How to test:** `cargo run --release -p bf1_game`, then fly to the howitzer
at about (-255, 61, 9). The rails, bolts and other bare metal should be
grey; the rest stays olive green. Compare with `BF1_PAINT_MASK=off`.

## 2026-10-07: Tints too bright: rule per kind; colour grading crushes shadows (found)

**Problem (user, screenshots 44 real and 45 ours):** the howitzer paint is
pale mint green and the cloth almost white; too bright.

**Measured (`image-stats` now gives the mean of the brightest and darkest
quarter of a region; the view probe gives the same per mesh part):** in the
real game the sunlit gun parts are about 1.35 times as bright as sunlit
ground (green channel: box 1.37, ring 1.34, cloth 1.33). Ours (camera
`-259.2,64.3,3.2,-141,-20`, 60 x 60 rays):

| rule | gun body | barrel | cloth |
|---|---|---|---|
| overlay (previous entry) | 1.50 | 2.01 | 2.05 |
| tint = mean colour | 1.19 | 1.68 | 2.03 |
| texture x tint x 2 | 0.75 | 1.09 | 1.22 |
| **kept: paint "mean", fabric "x 2"** | 1.21 | 1.70 | 1.23 |

**What changed:** `load::tint_base_colour` uses the rule per kind (paint:
the tint is the mean colour; fabric: x 2). The overlay rule is removed. Test
`tint_rules` replaces the overlay test; 11 game tests pass.

**Found, not fixed (affects the whole picture):** our shadows are almost
black. Shaded ground: ours (3, 8, 19), real about (43, 41, 42). Sunlit
ground: ours (100, 87, 75), real (74, 70, 68). The colour grading does most
of this. With `BF1_GRADING=off`, the shade is (30, 35, 38).

| tables applied | grey 43 becomes |
|---|---|
| weather table only | (15, 24, 36) |
| plain table only | (21, 21, 24) |
| both (current) | (4, 12, 25) |

The weather table (`HdrColorGradingLut`, 33^3) is an S-curve: dark 0.125
becomes 0.03 to 0.11. We apply it to finished screen colours. I think the
game applies it to light values before tone mapping, in another encoding
(possibly the PQ curve), but this is not confirmed. Also, with grading off
our shade is 0.31 of the sun (sRGB). In the real game it is 0.58, so our
sky and bounce light are too weak compared with the sun.

**Not fixed:**
- The barrel is still a bit too bright and too green.
- The mortar paint gives base colours up to 3.6, so its brightest pixels
  may be brighter than white (no reference screenshot).
- The paint mask is not used.

**How to test:** `cargo run --release -p bf1_game`, then fly to the howitzer
at about (-255, 61, 9). The paint should be darker olive and the cloth
khaki-grey, not white. Shadows are still too dark (see above).

## 2026-10-07: Paint and cloth tints (howitzer green); view probe colours

**Problem (user):** the howitzer in our game is white-grey; in the real game
it is olive green (user's screenshots 42 and 43).

**Cause:** its paint colour is a shader value (`Color` (0.12, 0.18, 0.08),
with a `Masks` texture), and the cloth cover's is `TintColor`
(0.34, 0.33, 0.24). The textures themselves are neutral grey. We ignored
both values.

**What changed:**
- Loader: `material_tint` reads the paint tint (`Color` with a `Masks`
  texture: 20 materials, on howitzers, mortars and Vickers guns) and the
  fabric tint (`TintColor` in `..._Fabric` presets, 3 materials, with
  `BaseColorTiling`). Parts carry it as `Part::tint`.
- Game: one material per texture and tint. The base colour comes from
  `load::tint_base_colour` (overlay blend at the texture's mean, see
  DESIGN.md). Fabric repeats come from `BaseColorTiling` (2).
  `BF1_TINTS=off` turns the tints off for comparison. The log lists each
  tinted material.
- View probe: faster (box test per part), `BF1_PROBE_GRID=<n>`, labels per
  mesh part, and the screenshot's mean colour per thing ("view colour").

**Results (tested by Claude, view probe colours; camera
`-259.2,64.3,3.2,-141,-20`, 40 x 40 rays):**

| part | before | "tint = mean" (first try) | overlay (kept) | user's screenshot 42 |
|---|---|---|---|---|
| gun body | (56, 64, 78) | (28, 44, 36) | (41, 57, 50) | about (52, 58, 51) |
| barrel and cradle | (103, 101, 107) | (57, 73, 54) | (79, 91, 74) | not measured |
| cloth | (55, 65, 82) | (73, 83, 88) | (73, 84, 89) | about (50, 54, 51) |

The reference values are hand-picked regions with mixed sun and shade. 11
game tests pass (one new: overlay bounds).

**Not fixed:**
- The cloth is still too bright and too blue.
- Objects in shade look too blue in general (sandbags (16, 23, 34)).
- The paint mask is not used.
- The mortar tints give base colours up to 2.4, so their brightest pixels
  may be brighter than white.
- About 30 other tint kinds are not applied (tree, house and wall tints).

**How to test:** `cargo run --release -p bf1_game`, then fly to the howitzer
at about (-255, 61, 9), north-west of the village. It should be olive
green, with a grey-khaki cloth. `BF1_TINTS=off` shows the old grey look.

## 2026-10-07: Ground detail checks; view probe test tool (work in progress)

**What changed:** probe `image-stats --rect x0,y0,x1,y1` (region colour,
contrast, detail); probe `texture-formats` logs channel statistics and the
normal/height slope check; game test options `BF1_WINDOW=WxH`, `BF1_FOV=<deg>`,
`BF1_GROUND_MIP_SKIP=<n>`, and `BF1_PROBE_RECTS` (new `view_probe.rs`: logs
which mesh or the terrain each screen region shows). Plan in DESIGN.md
"ground detail".

**Found:** `_NRH` ground textures: red/green = normal, alpha = height, +green
= world -Z in our shader (8 of 8 layers); `_CS` alpha = smoothness.
**Failed:** full-size (2,048) ground textures gave no measurable change
(detail 4.7 against 4.9 at 2000x1125); default kept at the second mip.
**Found:** the "1:1" camera position is inside a dry hedge
(`wf_hedge_bushdry_01_mesh`, 0.5 m away): 3 of 4 measured regions show hedge
leaves, not ground. So the earlier ground numbers compare hedge against
ground. Why the real game shows no hedge there is not known (the hedge's
shadow-only material is not drawn by us; checked).

**How to test:** `cargo run --release -p bf1_game` (no visible change).
**Not tested:** normal maps (not written yet).

## 2026-10-07: The sky dome blocked the sun; light levels from the data

**Problem (user's 1:1 screenshots, real game against ours, same place):**
ours was about 3 times too dark and blue; the real game shows tree shadows
on sunlit ground, ours had no sunlight at all.

**Cause 1 (bug since step 6):** the 45 km sky dome cast a shadow over the
whole map. Fixed: the dome has `NotShadowCaster` and `NotShadowReceiver`.
This also explains an old open point: sunlit ground measured about 60 of
255 where about 170 was expected (MODLOG exposure entry).

**Cause 2 (my own constant):** the sky (ambient) light was SkyColor times
12,000, a value set by eye. Now it comes from the data: the upper half of
the weather's sky texture times its LuminanceScale, weighted by the cosine
from straight up (`Environment::sky_ambient`): (1,496, 2,462, 3,470) cd/m2
for the base weather, about half the old value and bluer.

**Calibration:** with the sun as 100,000 lux, the sunlit ground was still
about 2.5 times brighter than the game's at the level's own exposure
(EV 12.5). With the sun / pi (`SUN_UNITS`; a common factor between lighting
conventions, but we do not know Frostbite's), the view of the user's
screenshot 42 matches: whole frame (56, 53, 56) against (57, 62, 65),
middle (49, 49, 57) against (45, 47, 48). An exposure of EV 14 (outside the
level's 11..12.5) matched the brightness too, but stayed warm (55, 49, 47).
Fire rain with the weaker sun stays within the user's fire rain
screenshots (whole 47..89 against 42..77, sky (86..114, 60..78, 48..76)
against about (100, 70, 58)). Test option: `BF1_SUN_SCALE=<factor>`.

**How to test:** `cargo run --release -p bf1_game`. The ground should be
sunlit with clear tree shadows, about as bright as the real game (not dark
blue). Walk to the howitzer north-west of the village (the user's position
-261, 65, 1): compare with the real game screenshot.

**Results (tested by Claude, screenshot numbers):** as above; 10 tests pass.
**Not checked:** the picture. **Found, not fixed:** the frame rate drops to
7 fps when looking through the hedge next to that position (cut-out leaf
cards very close to the camera); the howitzer and its cloth have tint and
paint colours (`Color`, `TintColor`) that we ignore, so they look
white/grey instead of olive green.

## 2026-10-07: Colour grading with the level's own tables

**What changed:** new `grading.rs`; `environment.rs` reads each weather's
two grading tables from its ColorCorrection component (variants replace them
only when their override list names them):
- `HdrColorGradingLut` (`MP_Verdun_HdrCube_Sun`, `_Rain`, `_OC`): 33^3,
  format 27 = 10-bit RGB + 2 bits, **stored with the green axis upside down**
  (found from the data: with that layout the corners map to red, green, blue,
  white and black; without it the values make no sense);
- `ColorGradingTexture` (`T_MP_Verdun_ColorCube_01`, `_02`): 32^3 RGBA8.
Bevy's TonyMcMapface tone mapping reads a 48^3 table (RGB9E5) that Bevy
keeps in main memory. `grading.rs` keeps a copy and writes into it: tone map,
then the weather table, then the plain cube. So the existing tone mapping
pass also grades; no new render pass. Rebuilt when the weather changes (G).
Test options: `BF1_GRADING=off|weather|plain|both` (default both),
`BF1_EV=<value>` (exposure override).

**How to test:** `$env:BF1_WEATHER="firerain"; cargo run --release -p bf1_game`.
The fire rain scene should look darker and warmer: dark red-brown ground,
orange-brown haze, like the user's screenshots. Press G to see the other
weathers; the sunny one now has darker, slightly blue shadows.

**Results (tested by Claude, screenshot numbers):** fire rain, ground
(bottom third) 55..64 grey -> (21, 8, 6) and (28, 14, 13); sky (top third)
(91, 65, 61) and (111, 76, 75); whole frame (44, 27, 25) to (75, 51, 49).
User's fire rain screenshots: ground about (25, 14, 12), sky about
(100, 70, 58), whole 45..77. Table order chosen by applying all orders to
our screenshots offline: "weather table, then plain cube" fits the user's
fire rain screenshots best. EV test (12.5, 11.75, 11.0): 12.5 fits best;
lower EVs make the sky and the whole frame too bright. 2 new unit tests.

**Not sure:** for the **sunny** weather, the earlier sunny images (from
other sources, one a video thumbnail) fit the weather table alone better:
our sunny ground is now (16, 21, 33) against about (36..51, 37..52, 39..57)
in those images. Another possible explanation: the game uses only the
weather table, and the fire rain smoke dims the sun (the user's fire rain
screenshots show no hard shadows; ours keep full sun). A sunny or overcast
screenshot from the user's own game would decide it.

## 2026-10-07: Reference screenshots measured; start weather option

**What changed:**
- Probe: `image-stats <files>` prints the mean colour (sRGB bytes) of the
  whole frame and its top, middle and bottom thirds, for WebP or PNG images.
  New probe dependency `image-webp` (MIT OR Apache-2.0), probe only.
- Probe: `texture-formats` also decodes single-channel BC4 textures and
  writes them to the git-ignored `extracted/export/`.
- Game: `BF1_WEATHER=<part of a variant name>` (for example `firerain`)
  starts in that weather variant.

**Results (tested by Claude, numbers only):** the user's 16 reference
screenshots (real game, fire rain weather): outdoor sky about (100, 70, 58),
outdoor ground about (25, 14, 12), interiors (19, 9, 6) to (65, 45, 34); a
strong warm cast everywhere. Ours with `BF1_WEATHER=firerain`: sky (83..130,
68..101, 61..90), ground (55..64, 49..56, 47..54): ground 2 to 2.5 times too
bright and grey instead of red-brown.

**Dead end:** the level's Enlighten sky visibility texture
(`mp_verdun_enlighten_01_win32__skyvisibilitytexture`, 1024 x 1024 BC4) is a
lightmap atlas (terrain tiles and small object charts), not a world map. Using
it needs the per-object atlas placement from the Enlighten database, which we
cannot read.

**Found:** the fire rain variant also sets colour correction with its own
colour-grading cube (`MP_Verdun_HdrCube_Rain`), which we do not apply; the
level has 454 spot lights and 85 sphere lights (lamps) that we do not place,
and an interior exposure setting (`ve_mp_verdun_interior`).

## 2026-10-07: Colour texture from shader presets (plane wreck)

**Problem (user screenshot):** the Fokker wreck was grey.

**What changed:** `load.rs`: when a material lists no colour texture, the
loader follows its shader link; if that is a `SurfaceShaderPreset`, it uses
the preset's own texture list (presets can point to presets; up to 4 steps).
The Fokker's preset names `CS_Texture` =
`Gameplay/Vehicles/GER_FokkerDR1/Textures/T_GER_FokkerDR1_CS`. New log lines:
the 12 brightest textures and all bunker textures with their mean colour.

**How to test:** `cargo run --release -p bf1_game`. Go to the plane wreck: the
body should show its texture (panels, wear), no longer flat grey.

**Results (tested by Claude, from the log):** grey parts 10 -> 9; textures 241
-> 242. **Not done:** the plane's paint (`UsePaintMask` true, paint index
values): its texture is a light grey base (mean 0.71) that the game's shader
paints; we show the base. Its 2 small parts with shaders outside the Verdun
data stay grey. **Found for the bunker:** the wall texture
`T_BunkerWallConcrete_01_CS` is light grey (mean sRGB 0.66), its
`Wall_Paint_Color` is white in all 17 uses, and its `Dirt_Color` (about 0.42)
is applied by a mask we cannot read. Our sky (ambient) light is the same
indoors and outdoors, which likely makes it look much brighter than in the
game.

## 2026-10-07: Leaves and branches cut out by their alpha

**Problem (user screenshots):** leaf and branch cards on trees and bushes
were drawn as solid dark squares and shapes.

**What changed:** `load.rs` measures each texture's alpha on a small mip
(share of clear, middle and solid pixels) and marks it as a cut-out when at
least 5% is clear, at least 2% fully solid and at most 25% in between
(`is_cutout`). `main.rs` draws materials with a cut-out colour texture with
alpha test at 50% (`AlphaMode::Mask`), from both sides. `_CS` textures keep
smoothness in their alpha (many low values, almost no fully solid pixels,
or many middle values), so they are not affected. The log lists the cut-out
textures and an alpha summary by name ending.

**How to test:** `cargo run --release -p bf1_game`. Look at the dead trees,
the bare ash trees, the hedges and the ivy: branches and leaves should show
their shapes, with the sky visible between them, and no dark squares.

**Results (tested by Claude, from the log):** 13 cut-out textures: 12
vegetation (hedge, larch, yew and ash branches, dead-tree branches, ivy,
tree billboards) and 1 ground dirt decal; no `_CS` texture selected; no
errors; 180 to 232 fps at the start pose after 10 s. **Not checked:** the
picture. **Not done:** wind movement, and the leaves' own lighting
(translucency).

## 2026-10-07: Ground textures, step 3c: draw with the game's own masks

**What changed:** `ground.rs` and `terrain.wgsl` rewritten. The colour guess
is gone. At load, the game bakes two weight atlases from the real layer
masks (step 3a) and the layer -> texture table (step 3b): the 1 km battle
area at 0.5 m per texel (2048 x 2048) and the whole 4 km map at 4 m per
texel (1024 x 1024); 4 texture weights per RGBA texel, with mips. Layers
without a colour texture (12 to 17, mesh scattering) are not painted.
The shader blends the 12 ground textures by these weights up to 150 m from
the camera, fading to the colour map by 450 m. Where no textured layer has
weight (1.8% of the battle area), the colour map shows.

**How to test:** `cargo run --release -p bf1_game`. Walk (F) or fly low.
The ground near you should change between mud, green grass, yellow grass,
burnt soot, forest floor, footprint mud and rock, in patches that follow
the map (for example grass fields at the edges, mud in the middle).
`BF1_GROUND_OFF=1` shows the old ground for comparison; `BF1_GROUND_TILE`
changes the texture size (default 6 m per repeat, a guess).

**Results (tested by Claude, from the log and screenshot numbers):** weight
atlases built in 0.10 s and 0.03 s; battle area share: mud pile 02 33%,
big-tile grass 33%, grass field 19%, yellow grass 4%, burnt soot 4%, mud
pile 05 3%, the rest about 1% each; outside the battle area 95% big-tile
grass. Fine detail of the ground part of the frame: 0.93 -> 1.66 and
0.37 -> 1.57 (textures off -> on); frame rate 162 -> 163 and 242 -> 225 fps.
7 unit tests pass. **Not checked:** the picture.

**Not sure:** the blend rule (base layer, then each mask painted on top in
ascending layer order). Against the colour maps, greenness correlates only
weakly (+0.15 painted, +0.18 plain sum, 0 painted in reverse), so the
colour maps neither prove nor reject it. Also not done: ground normal maps
(`_NRH`), the second textures some layers use (for example rock layer 10
also uses mud and soot), the real texture repeat sizes.

## 2026-10-07: Ground textures, step 3b: layer -> texture from the shader database

**What changed:** new `fb_res::shaderdb` (`ShaderDb`, `terrain_colour_shaders`,
`terrain_layer_textures`). The game reads the terrain's layer combinations
(shader names with layer numbers) and the level shader database, finds each
colour shader's texture list (name hash -> key -> permutation -> description
block), and picks each layer's main colour texture: the most specific
colour texture that every shader with that layer uses. Each ground texture
now knows its layers. Still drawn with the colour guess (step 3c next).

**How to test:** `cargo run --release -p bf1_game`, then open the newest log.
Expect `185 colour shader names, 185 found; layers without a texture: []`
and 14 lines `layer N: T_...` (0 MudPile_02, 4 GrassField02, 8 burnt soot,
10 rocks, 11 big-tile grass, ...).

**Results (tested by Claude, from the log):** as expected; the table equals
the Python analysis in STATUS.md update 8; 0.07 s. **Dead end on the way:**
the block chain starts one small block earlier (byte 0x1C) than the one I
used in Python, so the permutation field is the block number itself (my
first Rust version subtracted 1 and found no textures).

## 2026-10-07: Ground textures, step 3a: real layer masks (reading only)

**What changed:** new `fb_terrain::layers`. It parses the mask section of the
terrain tree file (STATUS.md update 9) and links every stored 66 x 66 mask
to its patch and layer. `LayerMasks::weights_at(x, z)` gives the weight of
each layer at a point: the finest patch that has records, its full layer
first, then each masked layer painted on top in order (our assumption of
the blend rule). The root's masks come from chunk record 0. Probe: the
`terrain` command logs the counts, both edge checks and the strongest
layer over the battle area. New probe command `terrain-chunks-save-all`
(writes every terrain chunk to the git-ignored `extracted/terrain/all/`).
Nothing new is drawn yet (step 3c).

**How to test:**
`cargo run --release -p bf1_probe -- terrain Win32/xpack1/levels/mp_verdun/mp_verdun Xpack1/Levels/MP_Verdun/MP_Verdun`
Expect `layer masks: 4569 records (header says 4569), 3021 with a stored
mask, 3021 masks kept; nodes matched 77, mismatched 0` and edges
`different 0` in x and z.

**Results (tested by Claude, from the log):** as above; edges x 2,130 equal
/ 0 different, z 2,034 / 0; battle area patch level 5: 51%, 4: 17%, 3: 32%;
strongest layer: 0 (mud pile 02) 32%, 11 (big-tile grass) 30%, 4 (grass
field) 18%, 8 (burnt soot) 4.5%, 5 (yellow grass) 3.5%; no layer at 1.6% of
the points (all weights 0). 2 new unit tests.

**Dead ends on the way:** the root chunk's first bytes look like smooth
heights (masks are smooth images), so "smooth" was not enough to tell a
height node; now only located height nodes use the height layout. The last
record ends 2 bytes after the stated section size.

## 2026-10-06: Exposure from the level (world was too dark)

**Problem (found from screenshot numbers, and the user's dark screenshots):**
the camera used a fixed EV100 of 15 ("tuned by eye"), but the sky texture
was scaled with the level's own exposure (EV 11.75, the middle of its
11..12.5 range). So the lit world was about 10 times too dark against the
sky. Sunlit ground measured about 30 of 255.

**What changed:** `Environment::exposure_ev` (the level's MaxEV: in daylight
the game's auto exposure stays at the top of its range). The camera and the
sky both use it, and the G key updates it with the weather. Sky brightness
factor 5.87 -> 3.49 (the sky is 1.7 times dimmer); the world is 5.7 times
brighter in linear light.

**How to test:** `cargo run --release -p bf1_game`. The ground and buildings
should be clearly brighter than before, and no longer dark against the sky.

**Results (tested by Claude, screenshot numbers, 1280 x 720):** ground part
of the frame 18..23 -> 49..59 (start pose), 30 -> 56..61 (30 m up, sun
behind); whole frame 39 -> 65. **Not sure:** the real game may still be
brighter (its tone mapping differs from Bevy's TonyMcMapface).

## 2026-10-06: Ground textures, step 2: terrain shader (guessed placement)

**What changed:** new `ground.rs` and `terrain.wgsl`. The terrain uses a
Bevy `ExtendedMaterial` (StandardMaterial + our fragment shader). The 12
ground textures are decoded to one RGBA8 texture array (they mix BC7 and
BC3, and an array needs one format). Per pixel, up to 120 m from the camera
(fading out by 400 m), the shader blends the textures. Weights: textures
whose mean colour is near the colour map count more; steep slopes add rock
(`T_Larger_Rocks_01`). The colour map then tints the result, so the large
scale colour stays. **The placement is a guess**, not the game's masks
(step 3). Colour only: no normal maps yet.
Test options: `BF1_GROUND_OFF=1` (old look), `BF1_GROUND_TILE=<metres per
repeat>` (default 6, a guess), `BF1_GROUND_SHARPNESS` (default 250),
`BF1_CAMERA=x,y,z,yaw,pitch` (start pose), `BF1_SCREENSHOT_AT=<seconds>`
(saves `logs/screenshot-*.png` and logs brightness and fine detail of the
lower 40% of the frame).

**How to test:** `cargo run --release -p bf1_game`. Walk on the ground (F).
Close to you, the ground should show mud, grass, rock and dirt texture
instead of smooth colour. Far away it looks as before.

**Results (tested by Claude, from the log and screenshot numbers):** shader
compiles, no errors; 12 layers, 0.07 s to decode; fine detail of the ground
part of the frame 0.55 -> 0.69 (start pose) and 0.56 -> 0.66 (30 m up,
looking down); frame rate about 214 fps (was about 250 before; not measured
under the same conditions). **Found:** the frame is very dark (ground about
30 of 255): see the next entry. **Not checked:** the picture.

## 2026-10-06: Ground textures, step 1: load the layer textures

**What changed:** the game reads the level's shader database
(`xpack1/levels/mp_verdun/mp_verdun/shaderdb`, 159 MB) and lists the
colour textures (`..._CS`) of its terrain shaders: texture lists that are
followed by a `maskMap` slot. It loads each one like a mesh texture and logs
its size, format and mean colour. `fb_level::World` now also keeps res
entries by name (`read_res_named`): the shader database has no usable rid,
so the old lookup by rid dropped it. Nothing new is drawn yet (step 2).

**How to test:** `cargo run --release -p bf1_game`, then open the newest
file in `logs/`. Expect `terrain colour textures listed: 12`, twelve
`ground texture ...` lines and `ground textures: 12 loaded`.

**Results (tested by Claude, from the log):** 12 of 12 loaded, all 1024 x
1024 (second mip), 11 BC7 and 1 BC3 (`T_WF_Shoveltown_Forestfloor_03`);
mean colours from (0.28, 0.26, 0.25) (burnt soot) to (0.62, 0.54, 0.46)
(dirt); extra load time 0.65 s (0.51 s of it reading the shader database).

## 2026-10-06: Cracks between terrain patches (skirts)

**Problem (user screenshots):** thin bright lines and light gaps in the
ground. Where a 1 m patch meets an 8 m patch (edge of the 1 km fine square)
or a 0.5 m hole patch meets a 1 m patch, the finer edge follows the true
height between the coarse points, so a crack opens and the sky shows.

**What changed:** `terrain_mesh.rs`. The grid size of every patch is chosen
first, so each patch knows its neighbours. An edge next to a different grid
gets a skirt: a strip hanging down from the edge, as deep as the measured
gap on that edge plus 0.25 m, drawn from both sides. Edges between equal
grids share their points and get no skirt (so no deep strips appear inside
the bunkers). New log line `terrain seams`; 3 new unit tests for the gap
measurement.

**How to test:** `cargo run --release -p bf1_game`. Fly along the edge of the
detailed area (about 500 m from the middle) and look along the ground: no
bright lines. Look at the bunker entrances: no light gaps around them.

**Results (tested by Claude, from the log):** 168 edges next to a different
grid; largest gap 2.91 m (x -640, z 118, the edge of the fine square);
31,872 skirt triangles, deepest 3.16 m; 6 unit tests pass. **Not checked:**
the picture. **Not sure:** the second screenshot (large light triangles next
to a rock) may be a terrain hole that no mesh covers, not a crack. If it is
still there, please send me the camera position from the log
(`camera ... frostbite (x, y, z)`).

## 2026-10-06: Red and purple ground in the battle area

**Problem (user screenshot):** most of the battle area ground was red, with
purple patches. **Failure on our side:** the 2026-10-03 entry said "red
marker pixels 0.5%". That test counted only bright red (red > 150, green
and blue < 40) and missed the darker marker red. A wider test (red at least
twice green and blue) gives: L2 tiles (54% of the battle area) 81% reddish
and 6% purple; L4 (10%) 24% reddish; L5 (34%) 7% reddish and 1% purple.

**Cause:** where no finer colour tile is found, the ground uses a coarse
tile, and coarse tiles hold the game's marker colour ("finer data here")
over the battle area. The game draws that ground from the terrain material
layers, which we have not decoded (paused work, see STATUS.md).

**What changed:** `fb_terrain::is_marker`, `fill_markers` and
`Terrain::ground_colour`. When the game decodes a colour tile, marker pixels
get the mean colour of the finest tiles (the battle area ground, from the
data), blended with their neighbours over 8 pixels. The probe's `terrain`
command logs the wider colour check.

**How to test:** `cargo run --release -p bf1_game`. Fly up over the battle
area. There should be no red or purple ground. About half of the battle area
is a plain olive grey-brown with no detail; the rest keeps its colour map.

**Results (tested by Claude, from the log):** 297,827 marker pixels of
10,545,600 (2.8%) replaced with sRGB (106, 103, 81). **Not checked:** the
picture.

## 2026-10-06: Map was mirrored; sky turned the wrong way

**Problem (reported by the user with a deploy-screen screenshot):** the map
was drawn mirrored. We had assumed Frostbite is left-handed and negated z.

**Proof (from the data):** the 5 Conquest capture zones (layer
`conquest0/layer0`, 25 to 50 m shapes) were fitted to the A–E flag icons on
the deploy screen. Fit error: 96 px with our old drawing, 33 px with z not
negated. With z not negated, the village houses (mean x -97, z 24) land 40
px from flag A, and the French base lands next to the French HQ icon.

**What changed:**
- `convert.rs`: Frostbite positions and transforms go into Bevy unchanged.
  New `to_frostbite` for lookups (terrain height, terrain colour, the
  camera log), so every conversion is in this one file.
- `load.rs`: triangle winding is kept as stored (no reversal).
- `scene.rs`: the sky dome now turns the other way round the horizon
  (`sky_direction`), so the sky is not mirrored from inside.
- New log checks: winding (share of triangles that face the way of their
  normals) and sun (angle between the sun direction and the brightest point
  of the sky texture). Landmark log line for houses and balloons.

**How to test:** `cargo run --release -p bf1_game`. Fly up high and compare
with the deploy screen: village (A) and the French side should be in the
same places relative to each other. The bright glow in the sky should be in
the same direction the shadows come from.

**Results (tested by Claude, from the log):** winding 99.4% agree (1.58
million triangles); sun check 43 deg -> 9 deg (base) and 45 -> 12 deg
(other variants); walking test: feet stay on the terrain; 3 unit tests pass.
**Not checked:** the picture.

## 2026-10-06: Remaining grey objects

**What changed:** `bf1_game/src/load.rs` and `main.rs`.
- Cards on wooden props (trench planks, fences, furniture) that have no
  texture and only one colour value, "AlphaColor", are not drawn. They are
  sets of small squares (about 20 cm) lying on the wood surface, facing both
  ways, and 182 of 202 cast no shadow. Our guess: they show damage (bullet
  holes, splinters) after a hit. **Not sure**: their shader is in another
  superbundle and we cannot read it.
- Tram wires (shader `ss_wire_01`, no textures) are drawn dark grey (a guess).
- Gameplay helper meshes are not drawn and give no collision: collision
  spheres (`gameplay/collisions/`), vehicle spawner markers, and the
  `characters/test/` ramp.
- The effect test now matches all shaders with "smoke" in the name (it
  catches the volcano smoke mesh; we checked that only effect shaders match).
- The grey log line names the meshes, and marks transparent parts.

**How to test:** `cargo run --release -p bf1_game`. Walk or fly along the
trenches (planked walls, parapets, wooden stairs). They should have no grey
squares. The tram wires should be thin dark lines, not grey.

**Results (tested by Claude, from the log only):** grey placed parts 414 ->
10; parts left out: 1,443 damage cards, 293 effect parts; 19 wire parts
dark grey; materials with a guessed folder texture 176 -> 107 (69 card
materials were drawn before with a guessed wood texture); collision
placements 7,949 -> 7,935. **Not checked:** the picture.

**Still grey (10 parts):** a French helmet (4; its textures are in another
superbundle), a stove (2, transparent), the Fokker wreck (3), one wallpaper
part. Small, left for later.

## 2026-10-03: Weather variants (G key)

**What changed:** the game loads the level's weather variants (overcast,
heavy fog `fog_02`, fog event `fog_01`, fire rain) on top of the base
setup. A variant replaces only the fields named in its PropertyOverrides
list. G switches to the next variant (fog, sun, ambient colour, sky). Fog
thickness scales with the height fog visibility range (base 3,000 m).
Test mode `BF1_WEATHER_CYCLE=<seconds>` switches automatically.

**How to test:** `cargo run --release -p bf1_game`, press G several times.
Order: sun, overcast (grey sky, light haze), heavy fog, fog event (you see
only about 30 to 50 m), fire rain (overcast, light haze), then sun again.

**Results (tested by Claude, from the log):** fog at 500 m: sun 16%,
overcast 41%, fog_02 93%, fog_01 100%, fire rain 41%; sun 100,000 lux and
sky brightness 5.87 in every variant; about 160 fps; no warnings.
**Not checked:** the picture.

**Dead ends:** copying every field of a variant gave a sun of 1 lux and a
black sky (fields not in PropertyOverrides only hold defaults). The fog
curve barely differs between variants; the height fog visibility range does.

**Not done:** rain and fire effects of the fire rain variant (effects are
not implemented); the overcast sun direction is used but not checked.

## 2026-10-03: Terrain colour from the final node layer; combinations study

**What changed:** `fb_terrain` takes a height node's colour from its final
67,600-byte layer (the layout is heights | block A | masks | final layer),
not from right after block A. New probe study commands `terrain-combos`,
`terrain-blocka-map`; the `terrain` command now logs red marker pixels.

**How to test:** `cargo run --release -p bf1_game`. The ground in the
battle area should now be coloured almost everywhere (grass, mud, roads),
sharp in some places and softer (4 m per pixel) in others. A few small red
spots can remain (the game's "finer data here" marker).

**Results (tested by Claude, numbers only):** height nodes without colour
79 -> 48; colour tiles 125 -> 156; battle area with a colour tile 33% ->
100% (L2 54%, L5 34%, L4 10%, L3 2%); red marker pixels 48 of 10,000
samples (0.5%). Game starts with no warnings. **Not checked:** the picture;
the frame rate showed 60 in one run (about 140 before), probably because the
window was in the background; not investigated.

**Combinations study (no result yet):** `layercombinations_win32` is a
serialized tree with variable records: before each "2d" name a count and a
list of layer numbers (layer 0 often left out); after it three u32 values
and a list, but this fixed reading fails for about 180 of 1,461 entries.
Block A's tail (67 x 50 bytes) is a small blotchy image, not a table.

## 2026-10-03: Option A, masks part 1

**What changed:** probe study commands `terrain-masks`, `terrain-mask-sheet`,
`terrain-mask-edges`, `terrain-find-table`, `bc3-scan`. No game change.

**Results (checked on the data):** node chunk layout found (heights, block A,
N 66 x 66 masks, final 67,600-byte colour or normal layer); fits all 120
height nodes. Masks are per 64 x 64 patch, in quadtree order per layer;
neighbouring masks share edges. Layer ids and patch lists not found yet.
Details in STATUS.md (update 4).

**Dead ends:** masks read from the wrong start showed diagonal stripes (they
were the colour layer); per-patch masks do not sum to 255; no fixed-size
mask table in the tree file.

## 2026-10-03: Option A (terrain reverse engineering), part 1

**What changed:** probe study commands `terrain-minmax`, `bc3-at`,
`terrain-chunk-save`, `terrain-scan-colour`, `terrain-first-layers`. No
game change. Findings in STATUS.md (update 3): block A starts with a 6-level
min/max height quadtree; the battle area has no stored ground colour image;
`bf1.exe` is DRM-encrypted, so code analysis is not allowed.

## 2026-10-03: Ground textures: research only, stopped

**What changed:** no code change. Findings added to STATUS.md (update 2).

**Result:** the strict colour test is correct (rejected layers are not
colour images, checked by eye). The layer-to-texture mapping and the mask
layout are not decoded. Stopped as AGENTS.md asks; options given to the user.

## 2026-10-03: Step 6b/6c, walking with collision; terrain colour fallback

**What changed:**
- Walking (`player.rs`): F switches between flying and walking. Capsule
  1.8 m x 0.35 m, walk 4 m/s, sprint (Left Shift) 7 m/s, jump (Space),
  gravity. Stands on the terrain and on meshes; sticks to the ground when
  walking downhill; climbs small ledges; falls into terrain holes; ignores
  the terrain when more than 1.5 m below it (inside bunkers).
- Collision (`collision.rs`): the visible meshes (LOD 0) near the player
  are baked into world triangles on a 4 m grid within 32 m, rebuilt after
  10 m of movement. Three spheres along the capsule are pushed out of the
  triangles. Decals, scattered debris, cloth, grass and effects do not block.
  (The game has separate Havok collision meshes; we do not decode them yet.)
- Lighting and visibility helper meshes (light blockers over the bunkers,
  occluders, Enlighten proxies; 14 placements) are no longer drawn.
- Terrain colour: a node keeps a colour tile only if the layer after block
  A passes a strict test; other nodes use their parent's colour, and places
  with no colour at any level are plain brown. See STATUS.md.
- Fog: 50% at 2 km.
- Test mode `BF1_AUTOWALK=1`: starts walking forward on its own, turns every
  4 s, logs position and ground contact every second, and names the meshes
  it touches when it cannot move.

**How to test:** `cargo run --release -p bf1_game`, press F, walk with
W/A/S/D, jump with Space, look by holding the right mouse button. Walk into
a bunker entrance hole to go underground.

**Results (tested by Claude with BF1_AUTOWALK, from the log):** the player
walks at about 4 m/s over the terrain, stays grounded on slopes, and stops in
a trench against a trench log and an earth mound (correct: the wall is too
high without jumping). About 100 fps while walking. First tries bounced
(walking speed was turned into upward speed on slopes) and stuck on mesh
floors lying just above the terrain; both fixed.

**Tested by the user (2026-10-03):** walking down and up stairs works;
building interiors work and look the best in the level. **Not reported
yet:** going down into a bunker through a hole, jumping.

## 2026-10-03: Fixes after user screenshots (ground squares, sky, fog)

User report: ground showed blue/green/purple squares, fog far too thick,
sky had a seam and a large jagged grey shape.

- **Ground squares (my bug):** height nodes still took the colour right after
  the heights. Worse, the layer order differs between nodes (in some the
  colour is right after block A, in others it is the last layer). Now
  `find_colour_layer` tries the likely starts (after block A, the last
  67,600 bytes, starts of zero-alpha block runs) and keeps the smoothest
  (`bc3_roughness`). Probe check: before, 66 tiles looked like noise (up to
  62); after, the roughest tiles are coarse 2 m leaf tiles at about 27, the
  same as a tile checked by eye; median 5.9.
- **Sky seam:** the sky texture was clamped at its edges; it now repeats
  around the horizon.
- **Fog:** now 85% at the fog colour end distance (1,200 m on Verdun)
  instead of the fog curve end (300 m): 21% at 150 m, 55% at 500 m.
- **Grey jagged shape:** a ring of 8 backdrop landscape meshes, 32 km wide
  and up to 1 km high, around the map. The 15 km sky dome hid the parts
  beyond it, and grey fog made the rest a silhouette. Now: sky dome 45 km,
  view distance 60 km, fog colour = sky colour at the horizon.
- **Sky brightness:** the sky texture is now multiplied by the level's
  LuminanceScale and the exposure (factor 5.87), as Frostbite does.

**How to test:** `cargo run --release -p bf1_game`. The ground should look
like an aerial photo with no coloured squares; the sky should have no
seam; distant hills should fade into the sky. **Not checked by Claude.**
About 133 fps.

## 2026-10-03: Sky, sun, fog and post-processing

**What changed:** New `bf1_game` modules `environment.rs` (reads the level's
visual environment `VE_MP_Verdun_Base_Sun_01`: sun colour, direction and
strength, sky colour, fog start/end/curve, exposure range, bloom, panoramic
sky texture) and `scene.rs` (camera, sun, sky dome, post-processing).
- Sky: a dome with the panoramic sky texture (RGB9E5 HDR, 4096 x 1024).
- Sun: directional light with the level's colour and 100,000 lux, cascaded
  shadows up to 200 m.
- Fog: exponential distance fog fitted to the level's fog curve (85% at
  300 m), tinted by the sun.
- Exposure EV100 15, TonyMcMapface tone mapping, bloom, screen-space ambient
  occlusion, SMAA anti-aliasing.
- New probe command `texture-formats` (tests pixel layouts of 4-byte textures).

**How to test:** `cargo run --release -p bf1_game`. You should see a cloudy
sky instead of a flat colour, sunlight with shadows, mist that hides things
beyond a few hundred metres, and soft shadows in corners (ambient occlusion).

**Results (tested by Claude, from the log):** environment read; fog density
0.00639; sky 4096 x 1024; no warnings; about 153 fps (was 240 without
shadows and ambient occlusion). **Not checked by Claude:** the picture.

**Format facts:** texture pixel format 29 = RGB9E5 (HDR; the only reading
that gives sky values from 0.02 to 1.1, bluish on average); format 18 = RGBA8
(the 32 x 32 x 32 colour grading cube; alpha is exactly 1 everywhere).

**Not verified:** the sun direction convention. The brightest part of the sky
texture is at about 39 degrees elevation (data: 31) and its azimuth does not
match by any simple rotation rule we tried. Exposure, ambient brightness and
fog colour were chosen by reasoning, not checked against the real game.

**Not done:** the colour grading cube (Bevy has no 3D LUT grading built
in), vignette, cloud shadows, lens flare, rain/overcast variants.

## 2026-10-03: Terrain colour and holes

**What changed:**
- `fb_terrain` now rebuilds the node position of every chunk record from the
  depth-first order of the chunk table (children in the order (0,0), (1,0),
  (1,1), (0,1); records without heights are leaf children of the node before
  them). It keeps every node's colour tile.
- A height sample of 0 is a hole. `height_at` returns None there;
  `is_hole` and `hole_positions` are new.
- The game colours each terrain patch with the finest colour tile (decoded
  to RGBA with mips), cuts holes (patches with holes use a 0.5 m grid), and
  gives "virtual texture only" objects the terrain colour under them.

**How to test:**

```
cargo run --release -p bf1_probe -- terrain Win32/xpack1/levels/mp_verdun/mp_verdun Xpack1/Levels/MP_Verdun/MP_Verdun --skip breakthrough0,rush0,tugofwar0,dominiation0,possession0,teamdeathmatch0,autotests
cargo run --release -p bf1_game
```

The probe should print "tree order check: 120 height tiles agree, 0
disagree", "colour tiles per level ... {1: 4, 2: 16, 3: 64, 4: 56, 5: 64}" and
"hole groups (within 20 m): 13", each 3 to 12 m from a bunker or stair mesh.
In the game the ground should look like an aerial photo (grass, mud, roads),
with openings at the bunker entrances. **Not checked by Claude:** that the
colour lines up with the ground shape (same orientation as the heights is
assumed), and how the holes look.

**Results (tested by Claude on the install):** tree order matches all 120
matched height tiles. 204 colour tiles. 496 hole samples in 13 groups near
bunkers and stairs. Game: 7 patches with holes, 1,727 triangles left out;
403 terrain-coloured object copies; grey parts 422 of 12,690; about 240 fps.

**Layout of a height node chunk (worked out from the data):** heights
(261 x 261 u16 = 136,242 bytes), block A (8,847 bytes: pairs of u16, probably
a min/max height table), colour (260 x 260 BC3 = 67,600 bytes), then
optional 8-bit masks of 260 x 260 (probably ground material weights, not
used yet). Leaf chunks hold only the colour (67,600 bytes).

**Dead ends:** colour at byte 136,242 (right after the heights) decoded as
noise; scanning start offsets showed a 16-byte period and led to the 8,847
byte block. Decoding test tiles to PNG and looking at one was needed to tell
real ground detail from noise.

## 2026-10-03: Step 6a, the game program with a fly camera (bf1_game)

**What changed:** Rust updated from 1.89 to 1.99 (`rustup update stable`).
New crate `crates/bf1_game` (Bevy 0.19.1): loads the archives, the Verdun
EBX, the level layout (Conquest), the terrain, the meshes and their colour
textures, and shows them with a free camera. Logs go to `logs/game-*.log`.
`BF1_EXIT_AFTER=<seconds>` closes the program after that time (for tests).
The workspace builds dependencies with optimisation in debug builds too.

**How to test:**

```
cargo run --release -p bf1_game
```

After about 6 seconds a window opens at the middle of the battle area.
Hold the right mouse button and move the mouse to look around. W/A/S/D
move, Space goes up, Left Ctrl goes down, Left Shift moves 6 times faster.
You should see brown ground with hills, and buildings, trees, wire and props
with their textures. Some parts are plain grey (no colour texture yet).

**Results (tested by Claude, from the log only):** load in 5.5 s; 520 meshes,
8,509 placements, 12,982 mesh entities; terrain 4,096 patches, 2.85 million
triangles; 231 textures (187 MB); about 240 frames per second; no errors.
**Not checked by Claude:** what the picture looks like.

**Update (same day, after user feedback "missing almost all textures,
effects and post processing, no underground"):**
- The loader now logs why each placed part is grey. At first 3,168 of
  12,982 placed parts were grey. Causes: materials whose shader graph holds
  the textures (the material lists none), shaders in superbundles we do not
  load, `TrunkBaseColor`, terrain-coloured objects, and effect cards.
- Added `TrunkBaseColor` and `Texture` as colour parameter names.
- For materials without a texture list, the loader now guesses the colour
  texture from the mesh's or shader's folder (closest name). This is a
  guess, not the game's real link; the real links are probably in the level
  `shaderdb` res (159 MB of compiled shaders that name their textures).
- Effect materials (fire, smoke, glow) are left out for now: without their
  shaders they draw as grey sheets.
- Result: 653 of 12,690 placed parts grey (5%): 370 with shaders from other
  superbundles, 231 that take the terrain's colour, the rest small.
- The underground is hidden because the terrain is solid. The real terrain
  has holes there; the hole data is not decoded yet.

**Known problems:** some mesh parts have no colour texture (grey).
The 3 cloth flags are missing (inline mesh data). Terrain is one colour.
Small cracks can show where fine and coarse terrain patches meet.

## 2026-10-03: Step 5, terrain heights (fb_terrain)

**What changed:** New crate `crates/fb_terrain` reads the terrain streaming
tree res and its chunks, and gives `height_at(x, z)` from the finest tile.
New probe commands: `terrain` (load, check, export), plus the study tools
`terrain-scan`, `terrain-fit` and `terrain-tiles`.

**How to test:**

```
cargo test
cargo run --release -p bf1_probe -- terrain Win32/xpack1/levels/mp_verdun/mp_verdun Xpack1/Levels/MP_Verdun/MP_Verdun --skip breakthrough0,rush0,tugofwar0,dominiation0,possession0,teamdeathmatch0,autotests
```

You should see "height tiles 121, located 120, not located 1",
"tiles per level (level: count): {0: 1, 1: 4, 2: 16, 3: 16, 4: 40, 5: 44}",
"level 5 (0.50 m per sample): 6864 objects, median |error| 0.66 m" and
"trees: 337 , median |error| 0.34 m". It writes
`extracted/export/verdun_heightmap.png` (whole 4 km terrain, dark = low) and
`extracted/export/verdun_terrain.obj` (1 km square around the battle area).
**Not checked by Claude:** how the exports look.

**Results (tested by Claude on the install):** as above. Height formula
`sample * 512 / 65536` fits objects better than any fitted line (median
1.15 m with the root tile alone, 0.66 m with detail tiles). Objects far from
the ground are explained: underground bunker parts about 8 m below, telegraph
poles about 7 m above (their origin is probably at the top).

**How the format was worked out (no public spec found):**
- The tree file starts with a packed header (no alignment). Found: height
  range 512.0 at 0x2f, terrain box at 0x6c..0x84 (x, z: -2048..2048).
- At 0x89: root height tile, 261 x 261 u16 (256 cells + 2 border samples
  each side). Orientation found by testing all 24 variants against 6,866
  object heights: rows along z, no mirroring, border 2 (median error 1.54 m;
  the next best is 5.74 m).
- The file ends with 205 records of 23 bytes: 3 flag bytes, u32 size, chunk
  id. 121 chunks start with a smooth 261 x 261 grid (height tile, then other
  layers we do not use yet). 67,600-byte chunks are 260 x 260 BC-compressed
  texture tiles (not used yet).
- Tile positions are not decoded from the tree yet. Each detailed tile is
  located by matching it against the root tile: 120 of 121 match exactly
  (0 difference) at exactly one position. The one that does not match has
  flags `00 00 ff` and is probably another kind of data.

**Not done:** the terrain's colour and material layers (it will be one plain
colour for now), holes, and the tree's own node layout.

## 2026-10-03: Step 4, meshes and textures (fb_res)

**What changed:**
- New crate `crates/fb_res`: `mesh.rs` reads MeshSet res files and decodes a
  LOD's opaque and transparent sections (positions, normals, texture
  coordinates, indices). `texture.rs` reads texture headers and mip layout.
  `material.rs` picks the colour and normal textures from a material.
- `fb_vfs`: `index_toc_chunks` reads the chunk lists of all .toc files
  (124,194 chunks, mostly `Win32/chunks0`). The cat index now keeps the
  largest copy of each sha1.
- `fb_level`: `World` indexes chunk entries; `read_chunk` and `read_res`.
- New probe commands: `meshes` (decode and check everything) and
  `export-mesh` (write OBJ + MTL + PNG to `extracted/export/`).

**How to test:**

```
cargo test
cargo run --release -p bf1_probe -- meshes Win32/xpack1/levels/mp_verdun/mp_verdun Xpack1/Levels/MP_Verdun/MP_Verdun --skip breakthrough0,rush0,tugofwar0,dominiation0,possession0,teamdeathmatch0,autotests
cargo run --release -p bf1_probe -- export-mesh Win32/xpack1/levels/mp_verdun/mp_verdun artassets/architecture/wf_houserural_04/wf_houserural_04_mesh
```

The first command should show "meshes decoded: 520 of 523",
"meshes with vertices outside their bounding box: 0" and
"texture chunks read: 532 ... size mismatches: 0  decode failures: 0".
The second writes `extracted/export/wf_houserural_04_mesh.obj`. Open it in
Windows 3D Viewer: you should see a rural house with roof tiles, plaster,
brick and wooden doors. **Not checked by Claude** (Claude cannot see images).

**Results (tested by Claude on the install):** 520 of 523 meshes decode
(1,666,784 vertices, 1,629,525 triangles). 0 meshes have a vertex outside
their bounding box. 532 of 532 texture chunks read with the right size.
Normal maps (red and green average near 0.5): BC5 21 of 21, BC7 144 of 146,
BC1 3 of 3, BC3 2 of 2.

**Format facts worked out from the data:**
- Vertex usages: 0x01 position, 0x02 blend indices (part index on composite
  meshes), 0x06 normal, 0x07 tangent, 0x09 binormal sign, 0x21 texture coordinate 0.
- Vertex formats: 0x01-0x04 float x1-4, 0x05-0x08 half x1-4, 0x0c 4 bytes,
  0x0d 4 bytes normalised. Positions are half3 (with binormal sign as the
  4th half) or half4.
- Index format 0x21 = 16-bit indices (all 523 meshes).
- Texture pixel formats: 54/55 BC1 (linear/sRGB), 60/61 BC3, 63 BC5, 66/67 BC7.
- Section records are 304 bytes; geometry declarations hold 16 elements and
  8 streams (84 bytes). Frosty's notes suggested 4 streams; the data says 8.

**Dead ends and surprises:**
- 397 meshes first failed with "chunk not in loaded bundles". Shared vertex
  data is listed in `.toc` chunk lists, not in the Verdun bundles.
- 185 textures first had too few bytes. The same sha1 is in several cats,
  and some copies hold only the small mips (a tail range). Keeping the
  largest copy fixed all 185.
- A GitHub download hung with no time limit. Downloads now use a time limit.
- Not done: 3 cloth flag meshes store their data inline in the res file
  ("inline mesh data, not supported yet"). 13 material textures are in other
  superbundles that we do not load yet.

## 2026-10-03: Step 3, object positions (fb_level)

**What changed:** New crate `crates/fb_level`:
- `World` loads every EBX partition of a superbundle and indexes them by
  GUID and name, plus the res entries by resource id.
- `Walker` starts at `LevelData` and follows sub-worlds (by `BundleName`),
  layers, prefabs, object blueprints and static model groups. It multiplies
  transforms on the way down and collects one record per placed mesh.
- `havok.rs` reads Havok 2013 packfiles (sections, object list). Static model
  group positions come from the group's Havok physics res.
New probe commands: `level` and `havok`; `find-res --save`.

**How to test:**

```
cargo test
cargo run --release -p bf1_probe -- level Win32/xpack1/levels/mp_verdun/mp_verdun Xpack1/Levels/MP_Verdun/MP_Verdun --skip breakthrough0,rush0,tugofwar0,dominiation0,possession0,teamdeathmatch0,autotests
```

You should see:
- "static model groups from havok: 92  member count mismatches: 0  members without transforms: 0"
- "placed meshes: 8530  unique meshes: 523  mesh asset loaded for 8530 of 8530"
- "rotation check: 0 of 8530 placements are not a clean rotation"
- "wrote 8530 rows to extracted\mp_verdun_placements.csv"

Without `--skip` (all game modes): 9,293 placed meshes from 122 groups.

**Results (tested by Claude on the install):** as above. Positions: x from
about -222 to 75 and z from -361 to 41 for the middle 90%; height (y) mostly
54 to 117. A few backdrop meshes are far out (up to about 2,000 to 3,600 units).
Not tested yet: whether these positions look right. That needs the renderer (step 6).

**Dead ends and surprises:**
- First walk found only 2,324 meshes. 1,482 of 1,614 group members have an
  empty `InstanceTransforms` list, but their `InstanceCount` adds up to 7,262.
- The missing positions are in the group's Havok physics res
  (`staticmodelgroup_gridNNxNN_physics_win32`, res type 0x91043f65). It is a
  Frostbite header + a Havok 2013.3 packfile with 32-bit pointers. The first
  `hknpStaticCompoundShape` whose count (u32 at +0x64) equals the header count
  (u32 at 0x34) holds one 128-byte record per instance: rotation (3 x float4),
  translation (float4), scale (float4). The header's u16 table (offset at 0x38)
  gives the member of each instance.
- The packfile's local fix-up table is empty in this data, so pointers cannot
  be followed. We locate the records from the object's end instead.
- All 244 `GroupRigidBodyData` transforms are the identity, so we do not apply them.
- The level references all game modes. A real match loads one, so `--skip`
  leaves the others out.

## 2026-10-03: Step 2, read EBX asset files (fb_ebx)

**What changed:** New crate `crates/fb_ebx` decodes EBX files (legacy
little-endian layout, magic `CE D1 B2 0F`) into a tree of typed values, and
writes them as indented text. New probe commands: `dump-ebx` and `find-res`.

**How to test:**

```
cargo test
cargo run --release -p bf1_probe -- dump-ebx Win32/xpack1/levels/mp_verdun/mp_verdun --write
```

You should see "parsed 14570 ok, 0 failed" and
"Name field check: 13457 match, 70 differ, 1043 have an empty or no Name",
then "wrote 14570 text files (223.3 MB) to extracted\ebx". The text files are
for study only. Git ignores `extracted/`.

**Results (tested by Claude on the install):** as above. 188,339 instances,
1,330 instance types, 106 asset types. The 70 name differences are level
layers whose internal name is shorter than the bundle name (for example
`.../breakthrough0/layer0_breakthrough0_layer` is named `.../Breakthrough0_Layer`).

**What we found for the next steps:**
- Main level: `xpack1/levels/mp_verdun/mp_verdun` (type `LevelData`).
- Terrain: `xpack1/levels/mp_verdun/terrain_4km/terrain_4km` (`TerrainData`).
  It refers to three res files: `...streamingtree_win32` (1.6 MB, type
  0x22fe8ac8), `...layercombinations_win32` (392 KB) and the visual res
  `terrain_4km` (4.7 KB). The streaming tree is small, so the heights are
  probably in chunks. The terrain is placed by `TerrainEntityData` in
  `mp_verdun/layer0_default`.
- Placed objects: 146 `StaticModelGroupEntityData`, 867
  `ObjectReferenceObjectData`, 852 `SpatialPrefabReferenceObjectData`, each
  with a `Transform` (right/up/forward/trans vectors).

## 2026-10-03: License (Unlicense)

**What changed:** Added `LICENSE` (the Unlicense, a public-domain dedication),
allowed it in `.gitignore`, set `license = "Unlicense"` in `Cargo.toml`, and
added a License section to `README.md`.

**How to test:** `git status` shows `LICENSE` as tracked. `cargo build` still works.

## 2026-10-03: Step 1, read the archives (fb_vfs + bf1_probe)

**What changed:** New Rust workspace. `crates/fb_vfs` reads the signed
`.toc`/`.cat` header, DbObject, `cas.cat`, compressed blocks and delta patches.
`crates/bf1_probe` prints and logs what it finds. Added `docs/DESIGN.md`,
`CREDITS.md`, `README.md`.

**How to test:**

```
cargo test
cargo run --release -p bf1_probe -- read-sb Win32/xpack1/levels/mp_verdun/mp_verdun
```

You should see 22 `bundle` lines, then "ok Ebx: 14570", "ok Res: 3710",
"ok Chunk: 5320", "files rebuilt from base + delta: 752", and no `FAIL` lines.
It takes about 5 seconds after the first build.

**Results (tested by Claude on the install):** as above, 0 failures. Every EBX
starts with `CE D1 B2 0F`. All 1,765 inline `idata` res copies match the cas data.
Only compression types 0x00 (stored) and 0x09 (LZ4) appear. No zstd dictionary.

**Dead ends and surprises:**
- First cat reader used the "2017" header from Frostbite-Scripts. Wrong for BF1:
  it produced 211,079 fake cas file names. The BF1 cat header is 24 bytes, and
  Patch cats have a second section of 60-byte patch entries (sha1, base, delta).
- 752 files were "not in any cas.cat". They are those patch entries. Fixed by
  rebuilding them from base + delta.
- Verdun is listed only in `Patch\layout.toc`, under the name
  `Win32/xpack1/levels/mp_verdun/mp_verdun`. All 22 bundles come from the Patch `.sb`.
- A shell command hung on a stray `python -` (it waited for keyboard input).
  Stopped it. No harm done.
