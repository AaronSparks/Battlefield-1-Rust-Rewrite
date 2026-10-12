# Known issues

What does not work yet, or does not look like the game yet. Tested on the
Steam version, build 5142699. The details of each item (measurements,
attempts and failures) are in [MODLOG.md](../MODLOG.md); problems that two
attempts did not solve are written up in [STATUS.md](../STATUS.md).

To report a new problem: stand in front of the object, press I, and include
the text that appears at the top left (see "Controls" in the
[README](../README.md#controls)).

- Walking uses the visible meshes for collision, not the game's own
  collision meshes. Stairs and building interiors work (tested by the user);
  bunker entry through a hole is not tested.
- Terrain materials are close but not exact. The ground shows the level's 12
  ground textures with their normal maps and smoothness where the game's own
  layer masks put them, but the blend rule is our assumption, the texture
  repeat size is a guess, and each layer's other textures (variation,
  distance) are not used. The terrain shaders also use textures we do not:
  a variation mask (soft colour patches, in every shader), a "side flows"
  normal map (small water channels, with the mud layers), a cliff detail
  map and 6 distance normal maps. Ground layers 11 and 18 probably have
  each other's textures (found in two checks, not applied yet; mostly near
  the map edges). Tyre tracks: the level has no static ones; the game makes
  them while vehicles drive (not done). Far away (over about 450 m) the
  ground uses its colour map, which is a plain grey-brown in about half of
  the battle area. 9 small mesh parts are still grey (4 helmets, 2 stoves,
  2 plane wreck parts, 1 wallpaper part). Wires use a guessed dark colour.
- Footprints: in the real game most trench floors show the footprint
  texture; in our reading of the layer masks it covers only 2 to 3% of the
  battle area. Not explained yet (STATUS.md, update 10).
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
  own light probe grid (computed at the first start, about 12 s, then kept
  in the `cache` folder); lamps are not placed yet.
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
- Ivy on the ash trees (47 trees) is placed but drawn darker than in the
  game. The leaves now take their smoothness from their own texture, which
  helps only a little; the user's reading is that the ivy, the ground
  foliage and the wet spots on the ground should reflect the sky and sun
  more. Sky reflections (an environment map) are not done yet.
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
- Some objects may start damaged in the real game on purpose (29
  `PredestructionEntityData` in the level, not read yet).
- The dirt texture of the window glass is not used (its channels are not
  known).
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
- No sound: the level's 1,703 sound entities are not read.
- Chunks stored as a range (`rangeStart`/`rangeEnd`, 2,896 entries): they
  decompress, but their size is not checked yet. Not tested beyond "no error".
