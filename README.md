# Battlefield 1 Rust rewrite | 🚨 AI SLOP ALERT 🚨

A clean-room Rust program that reads your own Battlefield 1 install and lets
you fly or walk around the Verdun Heights map offline. This repository
contains no game files and no decompiled code. See [CREDITS.md](CREDITS.md).

Status: all 6 steps work (archives, asset files, object positions, meshes
and textures, terrain heights, walking around). Work now is on looks and
detail: see "What does not work yet". See [docs/DESIGN.md](docs/DESIGN.md)
for the plan.

## What you need

- A Windows 10 or 11 PC. Any desktop graphics card works (it must support
  BC texture compression, which all of them do).
- Your own copy of Battlefield 1 for PC, installed. Tested with the Steam
  version, build 5142699 (the build number is in `version.json` in the
  install). Other builds, for example the EA app version, are not tested.
- About 82 GB of free disk space if you copy the game files (step 3, way A).
  Ways B and C need no extra space.
- Rust, the programming tool kit (step 1).

You do not need to start Battlefield 1, and this program does not change it.

## Step 1: install Rust

1. Download and run `rustup-init.exe` from https://rustup.rs.
2. If it asks for the Visual Studio C++ build tools, let it install them
   (the "Quick install" choice). Rust needs them to build programs on
   Windows.
3. Keep the default options. When it is done, close and reopen any
   terminal window.

Tested with Rust 1.99.

## Step 2: get this project

On this GitHub page, click **Code**, then **Download ZIP**, and unzip it, for
example to `C:\BF1-Rewrite`. Or, with git:

```
git clone https://github.com/AaronSparks/Battlefield-1-Rust-Rewrite.git
```

The project folder has an empty folder named `Battlefield 1 Files` (with
only a note file in it). The game files go there in step 3.

## Step 3: give it the game files

The program reads only the `Data` and `Patch` folders of your install. It
never opens the other files (`bf1.exe`, the `.dll` files, `Core`,
`Support`, `EAAntiCheat`, `__Installer`, `__overlay`), and it never writes
into the game files.

To find your install in Steam: right-click Battlefield 1, then **Manage**,
then **Browse local files**. The folder that opens has `Data`, `Patch` and
`bf1.exe` in it. Both `Data` and `Patch` are needed: Verdun's data is spread
over many folders in both (a test run read archives in 51 of the 78 archive
folders).

**Way A (simplest): copy.** Copy the `Data` and `Patch` folders from your
install into `Battlefield 1 Files`, so that it looks like this:

```
your project folder
├── Battlefield 1 Files
│   ├── Data                         (copied from your install, about 65 GB)
│   ├── Patch                        (copied from your install, about 16 GB)
│   └── PUT_DATA_AND_PATCH_HERE.md
├── crates
├── README.md
└── ...
```

A copy keeps working when a game update changes your install.

**Way B: links (no extra space).** Open PowerShell in the project folder
(see step 4) and run these two lines. Change the path to your own install
if it is not the Steam default:

```
New-Item -ItemType Junction -Path "Battlefield 1 Files\Data" -Target "C:\Program Files (x86)\Steam\steamapps\common\Battlefield 1\Data"
New-Item -ItemType Junction -Path "Battlefield 1 Files\Patch" -Target "C:\Program Files (x86)\Steam\steamapps\common\Battlefield 1\Patch"
```

The links show your install as it is. After a game update the files can
differ from the tested build.

**Way C: point to your install.** Set `BF1_GAME_DIR` to your install folder
(the one that has `Data` in it). Then the program uses it instead of
`Battlefield 1 Files`. For the current PowerShell window only:

```
$env:BF1_GAME_DIR = "C:\Program Files (x86)\Steam\steamapps\common\Battlefield 1"
```

For all new windows (run it once, then open a new window):

```
setx BF1_GAME_DIR "C:\Program Files (x86)\Steam\steamapps\common\Battlefield 1"
```

Git never uploads anything in `Battlefield 1 Files` except the note file
(see `.gitignore`), so your game files stay on your PC.

## Step 4: run it

Open PowerShell in the project folder: in File Explorer, open the project
folder, click the address bar, type `powershell` and press Enter. Then run:

```
cargo run --release -p bf1_game
```

The first time, this downloads and builds the libraries the program needs.
That takes several minutes. Later starts take a few seconds to build. Then
the map loads and the window shows Verdun Heights. The first start takes
about 20 seconds, because it computes the indoor light; it saves that in
the `cache` folder (about 19 MB), so later starts take about 6 to 7
seconds (measured on a 32-thread PC with the game files on a local
disk).

If it says "game files not found", check that the file
`Battlefield 1 Files\Data\layout.toc` exists (or that `BF1_GAME_DIR` points
to the folder that has `Data` in it). Each run writes a log to the `logs`
folder; its first line shows which game folder the program used.

## Controls

The camera uses the game's field of view setting 100 (Hor+ 116);
`BF1_GAME_FOV=<setting>` uses another setting.

Hold the right mouse button to look around; W/A/S/D move, Space up, Left
Ctrl down, Left Shift fast. Press F to walk (Space jumps, Left Shift
sprints) and F again to fly. G switches the weather (sun, overcast, heavy
fog, fog event, fire rain).

The text at the top left shows your position, view direction, frame rate
and weather, and a `BF1_CAMERA=...` line: set that variable to start at the
same place and view next time. F1 hides or shows the text. Press I to
inspect the object in the centre of the screen: the mesh name, part and
texture (or, for the ground, its terrain layers) go to the log and to the
text for 15 seconds. This is the most useful thing to include when you
report a missing or wrong-looking object.

## Tools (for developers)

`BF1_PROFILE=on` logs the graphics card time of each render step every 5
seconds (in the `logs` folder), which shows where frame time goes.
`BF1_AUTOFLY=<m/s>` flies the camera straight ahead (for repeatable frame
rate tests). Switches to compare frame rate or look:
`BF1_SHADOW_CASCADES=<n>` (sun shadow steps, default 4),
`BF1_SCATTER_SHADOWS=off` (grass and debris cast no shadows),
`BF1_NORMAL_MAPS=off` (no surface bumps on models) and
`BF1_NORMAL_GREEN=keep` (bumps lit from the other side).
`BF1_DRAW_SHARE=<percent>` draws only that share of the placed objects
(to measure how the frame rate depends on the number of objects).
`BF1_TERRAIN_BLEND=off` draws mounds and dirt piles without the ground
layers on their tops. `BF1_GLASS=off` draws window glass with a guessed
texture, as before. `BF1_DAMAGED_PARTS=show` also draws the damage-state
pieces of destructible objects (broken glass, broken planks), as before.

The `bf1_probe` tool checks and exports the data, for example:

```
cargo run --release -p bf1_probe -- read-sb Win32/xpack1/levels/mp_verdun/mp_verdun
```

## What works

Tested on build 5142699 (Steam), 2026-10-03:

- Reads `layout.toc`, `.toc` and `.sb` files (signed header + DbObject).
- Indexes all 78 `cas.cat` files (348,802 unique files, 486 `.cas` archives).
- Reads all 22 Verdun bundles and decompresses every file in them:
  14,570 EBX, 3,710 RES and 5,320 chunks (about 4.1 GB), 0 failures.
- Rebuilds patched files from base + delta (752 files on Verdun).
- Decodes all 14,570 Verdun EBX asset files (0 failures) and writes them as
  text with `dump-ebx --write`.
- Walks the Verdun level and lists every placed mesh with its position:
  8,530 placements of 523 meshes in the Conquest layout (`level` command).
  Static model group positions come from their Havok physics data.

- Decodes the meshes placed on Verdun (520 of 523, about 1.6 million
  triangles) and reads all 532 of their textures (`meshes` command).
- Exports one mesh as OBJ + PNG to look at (`export-mesh` command).

- Reads the terrain heights (ground) of the whole 4 km map, 0.5 m per
  sample in the battle area, and exports a heightmap and terrain OBJ
  (`terrain` command).

- Shows Verdun Heights in a window (Bevy) with a free fly camera: terrain
  with its colour map (where decoded) and holes at the bunker entrances, buildings, trees and
  props with colour textures and normal maps (surface bumps); the level's
  sky, sun with shadows, fog, bloom and ambient occlusion; about 100 to 200
  frames per second on an RTX 3080 with the game's field of view.

## What does not work yet

- Walking uses the visible meshes for collision, not the game's own
  collision meshes. Stairs and building interiors work (tested by the user);
  bunker entry through a hole is not tested.
- Terrain materials are close but not exact. The ground shows the level's 12
  ground textures with their normal maps and smoothness where the game's own
  layer masks put them, but the blend rule is our assumption, the texture
  repeat size is a guess, and each layer's other textures (variation,
  distance) are not used. The terrain shaders also use textures we do not:
  a variation mask, a "side flows" normal map (probably for slopes), a
  cliff detail map and 6 distance normal maps. Tyre tracks: the level has
  no static ones; the game makes them while vehicles drive (not done). Far away
  (over about 450 m) the ground uses its colour map, which is a plain
  grey-brown in about half of the battle area. 10 small
  mesh parts are still grey (a helmet, a stove, a plane wreck). Wires use a
  guessed dark colour.
- Damage cards on wooden props are hidden (we think they show only after a
  hit; not confirmed).
- Decals (dirt, mud, stains, cracks, manhole covers, wall leaks) are drawn
  with their level variants, blended over the surface. Their blend kinds
  (`_Add`, `_RedTint`, `_LerpBaseColor`) are all drawn the same way; the
  19 puddles and 67 decal parts without an alpha texture are left out.
  Variants of other objects (earth mounds, sandbags, charred trees, 2,068
  placements) are off by default: `BF1_VARIATIONS=all` turns them on (not
  checked by eye).
- Effects (fire, smoke, dust, falling embers) and the vignette are not drawn
  yet. The sun strength is calibrated against the user's screenshots
  (sun / pi), not taken directly from the data. Interiors are darkened by our
  own light probe grid (computed at load, about 12 s); lamps are not placed
  yet.
- Building detail textures (a second texture set over the repeating
  base) are drawn for the two main building shader families; which set
  goes with which texture is our reading of the data. About 4.5% of the
  textured surface still has no usable texture coordinates (for example a
  barn wall and an observation turret).
- Scattered grass, ivy, foliage and debris come from the level's own
  scattering data, but without wind, without fading at the edge of their
  draw distance, and with our own choice of colour for the grass blades
  (the ground colour under them). Which of the 4 stored distances the game
  uses is our reading. Backdrop trees (flat cards on the far hills, about
  24,800) are drawn over the whole map and turn to face the camera: both are
  our rules, because their stored distances (500 and 30 m) cannot reach
  hills 0.7 to 2 km away. Their layer is mostly stored only in the coarse
  terrain levels; scatter-only layers take their mask from the finest level
  that has them (our reading; `BF1_SCATTER_CARRY=off` for the old way).
  Only one of the card's tree pictures is used.
- Ivy on the ash trees (47 trees) is placed but drawn about as dark as the
  bark. A lower cut-out threshold did not help, so the leaves are not cut
  away; we think the leaves need light through them (not done), but this is
  not confirmed.
- Material tints: only gun paint (with its paint mask) and cloth covers are
  applied (fitted to the user's screenshots). Bare metal is not drawn as
  metal yet. About 30 other tint kinds (trees, houses, walls) are
  not applied, so some objects look too grey.
- Colour grading: the level's weather table is applied to scene light in
  the PQ encoding, a fit to the user's screenshots (not confirmed from the
  game). Deep shade is still a bit too dark and blue; light bounced from the
  ground is a simple fitted term, not computed.
- The frame rate drops very low when leaf cards (hedges) are right in front
  of the camera.
- The wide field of view (Hor+ 116) costs about 25% frame rate: the
  processor prepares every object once per sun shadow step (4 steps), and
  the wide view makes these steps cover much more of the level. The
  graphics card needs only 2 to 3 ms per frame.
- Destructible objects show their start state: the pieces that only their
  damage states show (broken glass, broken planks, scattered hay) are left
  out, from the objects' own health states (903 placements of 85 meshes;
  checked by the user). Some objects may start damaged in the real game on
  purpose (`PredestructionEntityData`, not read yet).
- Window and lamp glass (231 placements) is drawn see-through with the
  material's grey colour and opacity; its dirt texture is not used yet.
- Mounds, dirt piles and ridges (619 placements) take the ground layers
  on faces that point up, by the threshold in their material. This is our
  reading of the game's terrain blend shader (not confirmed); the soft edge
  is a guess. Roots, embedded stones, tree stumps and small mounds (276
  placements) use other blend settings that are not drawn yet.
- Model normal maps: the green direction is our reading from the terrain
  (not confirmed on models); normal maps on painted, detail-textured and
  leaf parts are not used yet.
- 3 cloth flag meshes with inline data (the capture point flags, 21
  placements), and 13 textures from other superbundles.
- Objects that the game creates only while a round runs are not drawn: the
  weapons of the 11 vehicle spawn points in the Conquest layout (for example
  3 fixed machine-gun posts and 2 fixed guns), soldiers, and some weapon
  pickups. Their model is chosen at run time, so the level data does not
  name it. Effects, lamp light and lens flares are not drawn either.
- Chunks stored as a range (`rangeStart`/`rangeEnd`, 2,896 entries): they
  decompress, but their size is not checked yet. Not tested beyond "no error".

## License

The code in this repository is released into the public domain under the
[Unlicense](LICENSE). This covers only our own source code. It does not cover
Battlefield 1 or its data, which belong to Electronic Arts. Rust libraries we
depend on keep their own licenses.
