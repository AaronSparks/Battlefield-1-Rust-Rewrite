# Put your game files here

Copy the `Data` and `Patch` folders from your own Battlefield 1 install into
this folder, so that it looks like this:

```
Battlefield 1 Files
├── Data
├── Patch
└── PUT_DATA_AND_PATCH_HERE.md
```

Only these two folders are needed; the program never opens the other files
of the install, and it never writes into the game files. Instead of copying,
you can also link the two folders or set `BF1_GAME_DIR` (see the README,
step 3).

Git ignores everything in this folder except this note, so your game files
are never uploaded.
