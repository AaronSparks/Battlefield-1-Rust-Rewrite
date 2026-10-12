# Tools (for developers)

Test switches are environment variables. In PowerShell, set one for the
current window with `$env:NAME = "value"`, run the program, and remove it
again with `Remove-Item Env:NAME`. See also the [README](../README.md).

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
