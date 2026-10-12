//! `terrain-scan` command: study a terrain streaming tree res.
//! Finds every known chunk id inside the file and prints the spacing between
//! them, so the node record layout can be worked out.

use crate::Log;
use anyhow::{Context, Result};
use fb_vfs::GameFs;
use std::collections::{BTreeMap, HashMap};

pub fn cmd_terrain_scan(log: &mut Log, root: &std::path::Path, superbundle: &str, file: &str) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let data = std::fs::read(file).with_context(|| format!("read {file}"))?;
    log.line(format!("{file}: {} bytes", data.len()));

    let mut hits: Vec<(usize, [u8; 16], &str)> = Vec::new();
    for at in 0..data.len().saturating_sub(16) {
        let id: [u8; 16] = data[at..at + 16].try_into().unwrap();
        if id == [0; 16] {
            continue;
        }
        if world.chunks.contains_key(&id) {
            hits.push((at, id, "bundle"));
        } else if fs.toc_chunks.contains_key(&id) {
            hits.push((at, id, "toc"));
        }
    }
    log.line(format!("known chunk ids found: {}", hits.len()));
    let mut gaps: BTreeMap<usize, usize> = BTreeMap::new();
    for w in hits.windows(2) {
        *gaps.entry(w[1].0 - w[0].0).or_default() += 1;
    }
    let mut top: Vec<_> = gaps.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    log.line(format!("most common gaps between ids (bytes, count): {:?}", &top[..top.len().min(8)]));
    let out = std::path::PathBuf::from("extracted").join("terrain");
    std::fs::create_dir_all(&out)?;
    for (i, (at, id, _)) in hits.iter().enumerate().take(8) {
        if let Ok(d) = world.read_chunk(&fs, id) {
            std::fs::write(out.join(format!("chunk_{i}_at_{at:x}_{}.bin", d.len())), &d)?;
        }
    }
    for (at, id, src) in hits.iter().take(12) {
        let size = world
            .read_chunk(&fs, id)
            .map(|d| d.len().to_string())
            .unwrap_or_else(|e| format!("read failed: {e:#}"));
        log.line(format!("  0x{at:06x} {} ({src}) size {size}", fb_ebx::format_guid(id)));
    }
    if let (Some(first), Some(last)) = (hits.first(), hits.last()) {
        log.line(format!("first id at 0x{:x}, last at 0x{:x}", first.0, last.0));
    }
    Ok(())
}

/// `terrain-fit`: read the low-detail height tile inside the streaming tree
/// (261 x 261 u16 at 0x89, covering -2048..2048 on x and z) and test every
/// orientation against the heights of placed trees and props. For each
/// orientation, fit y = a * sample + b by least squares and report the median
/// error. The right orientation should have a small error.
pub fn cmd_terrain_fit(log: &mut Log, root: &std::path::Path, superbundle: &str, level: &str, file: &str, skip: &[String]) -> Result<()> {
    const N: usize = 261;
    const START: usize = 0x89;
    let data = std::fs::read(file)?;
    let tile: Vec<u16> = (0..N * N).map(|i| u16::from_le_bytes([data[START + i * 2], data[START + i * 2 + 1]])).collect();
    let (lo, hi) = (tile.iter().min().unwrap(), tile.iter().max().unwrap());
    log.line(format!("root tile: min sample {lo}, max sample {hi}"));

    let fs = GameFs::open(root)?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let mut walker = fb_level::Walker::new(&world, &fs);
    walker.skip_subworlds = skip.to_vec();
    walker.walk_level(level)?;
    // Ground objects: trees and havok-placed static props inside the map.
    let pts: Vec<[f32; 3]> = walker
        .meshes
        .iter()
        .filter(|m| m.source == "VegetationTreeEntityData" || m.source == "StaticModelGroup(havok)")
        .map(|m| m.transform.translation.to_array())
        .filter(|p| p[0].abs() < 2000.0 && p[2].abs() < 2000.0)
        .collect();
    log.line(format!("ground objects used: {}", pts.len()));

    let mut results = Vec::new();
    for border in 0..=2usize {
        let cells = (N - 1 - 2 * border) as f32; // cells across 4096 m
        for rows_along_z in [true, false] {
            for flip_u in [false, true] {
                for flip_v in [false, true] {
                    let sample = |x: f32, z: f32| -> Option<f32> {
                        let mut u = (x + 2048.0) / 4096.0;
                        let mut v = (z + 2048.0) / 4096.0;
                        if flip_u {
                            u = 1.0 - u;
                        }
                        if flip_v {
                            v = 1.0 - v;
                        }
                        let (col, row) = if rows_along_z { (u, v) } else { (v, u) };
                        let fc = col * cells + border as f32;
                        let fr = row * cells + border as f32;
                        let (c0, r0) = (fc.floor() as usize, fr.floor() as usize);
                        if c0 + 1 >= N || r0 + 1 >= N {
                            return None;
                        }
                        let (tc, tr) = (fc - c0 as f32, fr - r0 as f32);
                        let s = |r: usize, c: usize| tile[r * N + c] as f32;
                        Some(
                            (s(r0, c0) * (1.0 - tc) + s(r0, c0 + 1) * tc) * (1.0 - tr)
                                + (s(r0 + 1, c0) * (1.0 - tc) + s(r0 + 1, c0 + 1) * tc) * tr,
                        )
                    };
                    let pairs: Vec<(f64, f64)> =
                        pts.iter().filter_map(|p| sample(p[0], p[2]).map(|s| (s as f64, p[1] as f64))).collect();
                    let n = pairs.len() as f64;
                    let (sx, sy) = pairs.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
                    let (mx, my) = (sx / n, sy / n);
                    let cov: f64 = pairs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
                    let var: f64 = pairs.iter().map(|p| (p.0 - mx).powi(2)).sum();
                    let a = cov / var;
                    let b = my - a * mx;
                    let mut err: Vec<f64> = pairs.iter().map(|p| (a * p.0 + b - p.1).abs()).collect();
                    err.sort_by(|x, y| x.total_cmp(y));
                    let median = err[err.len() / 2];
                    let p90 = err[err.len() * 9 / 10];
                    results.push((median, p90, border, rows_along_z, flip_u, flip_v, a, b));
                }
            }
        }
    }
    results.sort_by(|x, y| x.0.total_cmp(&y.0));
    log.line("median error, 90% error, border, rows along z, flip x, flip z, scale a, offset b (best first)");
    for r in results.iter().take(8) {
        log.line(format!(
            "  {:>7.2} m {:>7.2} m  border {}  rows_z {:<5}  flip_x {:<5}  flip_z {:<5}  a {:.6}  b {:.2}",
            r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7
        ));
    }
    let worst = results.last().unwrap();
    log.line(format!("worst: median {:.2} m", worst.0));

    // Fixed formulas with the best orientation (rows along z, no flips, border 2).
    let header_range = f32::from_le_bytes(data[0x30..0x34].try_into().unwrap());
    log.line(format!("float at 0x30: {header_range}"));
    let sample = |x: f32, z: f32| -> f32 {
        let fc = (x + 2048.0) / 4096.0 * 256.0 + 2.0;
        let fr = (z + 2048.0) / 4096.0 * 256.0 + 2.0;
        let (c0, r0) = (fc.floor() as usize, fr.floor() as usize);
        let (tc, tr) = (fc - c0 as f32, fr - r0 as f32);
        let s = |r: usize, c: usize| tile[r * N + c] as f32;
        (s(r0, c0) * (1.0 - tc) + s(r0, c0 + 1) * tc) * (1.0 - tr) + (s(r0 + 1, c0) * (1.0 - tc) + s(r0 + 1, c0 + 1) * tc) * tr
    };
    for (label, scale) in [("512/65536", 512.0 / 65536.0), ("512/65535", 512.0 / 65535.0), ("header/65536", header_range / 65536.0)] {
        let mut signed: Vec<f32> = pts.iter().map(|p| p[1] - sample(p[0], p[2]) * scale).collect();
        signed.sort_by(|a, b| a.total_cmp(b));
        let mut abs: Vec<f32> = signed.iter().map(|e| e.abs()).collect();
        abs.sort_by(|a, b| a.total_cmp(b));
        log.line(format!(
            "  height = sample * {label}: median |error| {:.2} m, median signed error (object minus ground) {:+.2} m, 90% |error| {:.2} m",
            abs[abs.len() / 2],
            signed[signed.len() / 2],
            abs[abs.len() * 9 / 10]
        ));
    }
    Ok(())
}

/// `terrain-tiles`: read the chunk table at the end of the streaming tree
/// (records of 3 flag bytes, u32 size, 16-byte chunk id), find which chunks
/// start with a 261 x 261 height tile, and locate each one by matching it
/// against the low-detail root tile at every level and position.
pub fn cmd_terrain_tiles(log: &mut Log, root: &std::path::Path, superbundle: &str, file: &str) -> Result<()> {
    const N: usize = 261;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let data = std::fs::read(file)?;
    let root_tile: Vec<u16> = (0..N * N).map(|i| u16::from_le_bytes([data[0x89 + i * 2], data[0x8a + i * 2]])).collect();

    // Find the table: the first known chunk id, minus 7 bytes.
    let first = (0..data.len() - 16)
        .find(|&at| {
            let id: [u8; 16] = data[at..at + 16].try_into().unwrap();
            id != [0; 16] && (world.chunks.contains_key(&id) || fs.toc_chunks.contains_key(&id))
        })
        .context("no chunk table")?;
    let table = first - 7;
    let count = (data.len() - table) / 23;
    log.line(format!("chunk table at 0x{table:x}, {count} records, {} bytes after the last record", data.len() - table - count * 23));

    let mut by_level: BTreeMap<u32, Vec<(f64, u32, u32)>> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for i in 0..count {
        let r = &data[table + i * 23..table + i * 23 + 23];
        let flags = [r[0], r[1], r[2]];
        let size = u32::from_le_bytes(r[3..7].try_into().unwrap()) as usize;
        let id: [u8; 16] = r[7..23].try_into().unwrap();
        let chunk = match world.read_chunk(&fs, &id) {
            Ok(c) => c,
            Err(_) => {
                *kinds.entry("unreadable".into()).or_default() += 1;
                continue;
            }
        };
        let tile: Vec<u16> = if chunk.len() >= N * N * 2 {
            (0..N * N).map(|k| u16::from_le_bytes([chunk[k * 2], chunk[k * 2 + 1]])).collect()
        } else {
            vec![]
        };
        // A height tile is smooth: neighbouring samples differ little.
        let smooth = !tile.is_empty() && {
            let d: f64 = (0..N - 1).map(|k| (tile[k] as f64 - tile[k + 1] as f64).abs()).sum::<f64>() / (N - 1) as f64;
            d < 1500.0
        };
        let key = format!("flags {:02x} {:02x} {:02x} size {} height {}", flags[0], flags[1], flags[2], if size == chunk.len() { "=" } else { "!=" }, smooth);
        *kinds.entry(key).or_default() += 1;
        if !smooth {
            continue;
        }
        // Locate: level L tile spans 256 / 2^L root cells; compare every 2^L-th sample.
        let mut best = (f64::MAX, 0u32, 0u32, 0u32);
        for level in 1..=6u32 {
            let step = 1usize << level;
            let span = 256 / step;
            for iz in 0..(1u32 << level) {
                for ix in 0..(1u32 << level) {
                    let mut sum = 0.0;
                    let mut n = 0.0;
                    for k in (0..=span).step_by((span / 16).max(1)) {
                        for j in (0..=span).step_by((span / 16).max(1)) {
                            let t = tile[(2 + k * step) * N + 2 + j * step] as f64;
                            let rr = root_tile[(2 + iz as usize * span + k) * N + 2 + ix as usize * span + j] as f64;
                            sum += (t - rr).abs();
                            n += 1.0;
                        }
                    }
                    let err = sum / n * 512.0 / 65536.0;
                    if err < best.0 {
                        best = (err, level, ix, iz);
                    }
                }
            }
        }
        by_level.entry(best.1).or_default().push((best.0, best.2, best.3));
        if i < 6 {
            log.line(format!("  record {i}: level {} at ({}, {}), mean difference {:.3} m", best.1, best.2, best.3, best.0));
        }
    }
    log.line("--- record kinds");
    for (k, n) in &kinds {
        log.line(format!("  {n:>4}  {k}"));
    }
    log.line("--- height tiles located, per level: count, worst mean difference, distinct positions");
    for (level, v) in &by_level {
        let worst = v.iter().map(|x| x.0).fold(0.0, f64::max);
        let mut pos: Vec<(u32, u32)> = v.iter().map(|x| (x.1, x.2)).collect();
        pos.sort();
        pos.dedup();
        log.line(format!("  level {level}: {} tiles, worst {:.3} m, {} distinct positions", v.len(), worst, pos.len()));
    }
    Ok(())
}

/// Load the level's terrain with fb_terrain.
pub fn load_terrain(fs: &GameFs, world: &fb_level::World) -> Result<(fb_terrain::Terrain, fb_terrain::LoadStats)> {
    use fb_ebx::Value;
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData in the loaded partitions")?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("TerrainStreamingTreeResource") else {
        anyhow::bail!("TerrainData has no TerrainStreamingTreeResource")
    };
    let tree = world.read_res(fs, *rid)?;
    fb_terrain::Terrain::load(
        &tree,
        |id| world.chunks.contains_key(id) || fs.toc_chunks.contains_key(id),
        |id| world.read_chunk(fs, id),
    )
}

/// `terrain`: load the terrain, check it against placed objects, and export
/// a heightmap PNG and a terrain OBJ to extracted/export/.
pub fn cmd_terrain(log: &mut Log, root: &std::path::Path, superbundle: &str, level: &str, skip: &[String]) -> Result<()> {
    use std::fmt::Write as _;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, st) = load_terrain(&fs, &world)?;
    log.line(format!("terrain box {:?} .. {:?}, height scale {} m per step", terrain.min, terrain.max, terrain.height_scale));
    log.line(format!(
        "chunk records {}, height tiles {}, located {}, not located {}, unreadable {}",
        st.records,
        st.height_tiles,
        terrain.tiles.len() - 1,
        st.not_located,
        st.unreadable
    ));
    let mut per_level: BTreeMap<u32, usize> = BTreeMap::new();
    for t in &terrain.tiles {
        *per_level.entry(t.level).or_default() += 1;
    }
    log.line(format!("tiles per level (level: count): {per_level:?}"));
    log.line(format!("tree order check: {} height tiles agree, {} disagree", st.tree_position_agrees, st.tree_position_disagrees));
    log.line(format!("height nodes without a colour layer (use the parent's colour): {}", st.nodes_without_colour));
    // Material layer masks (STATUS.md update 9).
    match (&st.masks, &st.mask_error, &terrain.layers) {
        (Some(m), _, Some(lm)) => {
            log.line(format!(
                "layer masks: {} records (header says {}), {} with a stored mask, {} masks kept; nodes matched {}, mismatched {}, missing {}; odd gaps {}",
                m.records,
                m.header_count,
                m.stored,
                lm.masks.len() / fb_terrain::layers::MASK_BYTES,
                m.nodes_matched,
                m.nodes_mismatched,
                m.nodes_missing,
                m.odd_gaps
            ));
            for m in &m.mismatches {
                log.line(format!("  layer mask mismatch: {m}"));
            }
            log.line(format!(
                "layer mask edges: x equal {}, different {}; z equal {}, different {}",
                m.edge_x_equal, m.edge_x_different, m.edge_z_equal, m.edge_z_different
            ));
            // Strongest layer over the battle area (1 km around the centre, 4 m grid).
            let skip = [false; fb_terrain::layers::MAX_LAYERS];
            let mut w = [0f32; fb_terrain::layers::MAX_LAYERS];
            let mut top: BTreeMap<usize, usize> = BTreeMap::new();
            let mut levels: BTreeMap<u32, usize> = BTreeMap::new();
            for j in 0..250 {
                for i in 0..250 {
                    let (x, z) = (-80.0 - 500.0 + i as f32 * 4.0, -234.0 - 500.0 + j as f32 * 4.0);
                    if let Some(level) = lm.weights_at(x, z, &skip, &mut w) {
                        *levels.entry(level).or_default() += 1;
                        let best = (0..w.len()).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap();
                        *top.entry(best).or_default() += 1;
                    }
                }
            }
            log.line(format!("battle area: level of the patch used (level: samples) {levels:?}"));
            log.line(format!("battle area: strongest layer (layer: samples of 62,500) {top:?}"));
        }
        (_, Some(e), _) => log.line(format!("layer masks not read: {e}")),
        _ => log.line("layer masks: none"),
    }
    // Coverage: share of the battle area (1 km around the centre) by colour tile level.
    let mut cov: BTreeMap<String, usize> = BTreeMap::new();
    for j in 0..100 {
        for i in 0..100 {
            let (x, z) = (-80.0 - 500.0 + i as f32 * 10.0, -234.0 - 500.0 + j as f32 * 10.0);
            let k = terrain.colour_at(x, z).map(|(t, _)| format!("L{}", terrain.colours[t].level)).unwrap_or("none".into());
            *cov.entry(k).or_default() += 1;
        }
    }
    log.line(format!("battle area colour by tile level (percent): {cov:?}"));
    // Red marker pixels ("finer data here") in the battle area, by tile level.
    let decoded: Vec<Vec<[u8; 3]>> = terrain.colours.iter().map(|c| fb_terrain::decode_colour(&c.data)).collect();
    let mut red: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Wider classes (sRGB bytes): reddish = red at least twice green and blue;
    // purple or blue = blue above green by a third. Per level: total, reddish,
    // purple, and the sum of the other colours (for the mean).
    let mut classes: BTreeMap<String, (usize, usize, usize, [u64; 3])> = BTreeMap::new();
    let mut purple_tiles: BTreeMap<usize, usize> = BTreeMap::new();
    for j in 0..100 {
        for i in 0..100 {
            let (x, z) = (-80.0 - 500.0 + i as f32 * 10.0, -234.0 - 500.0 + j as f32 * 10.0);
            if let Some((t, uv)) = terrain.colour_at(x, z) {
                let px = decoded[t][((uv[1] * 260.0) as usize).min(259) * 260 + ((uv[0] * 260.0) as usize).min(259)];
                let key = format!("L{}", terrain.colours[t].level);
                let e = red.entry(key.clone()).or_default();
                e.0 += 1;
                if px[0] > 150 && px[1] < 40 && px[2] < 40 {
                    e.1 += 1;
                }
                let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
                let c = classes.entry(key).or_default();
                c.0 += 1;
                if r >= 2 * g && r >= 2 * b && r > 40 {
                    c.1 += 1;
                } else if 3 * b > 4 * g && b > 30 {
                    c.2 += 1;
                    *purple_tiles.entry(t).or_default() += 1;
                } else {
                    for k in 0..3 {
                        c.3[k] += px[k] as u64;
                    }
                }
            }
        }
    }
    log.line(format!("battle area samples (total, red marker) by tile level: {red:?}"));
    for (level, (n, reddish, purple, sum)) in &classes {
        let rest = (n - reddish - purple).max(1) as u64;
        log.line(format!(
            "  colour check {level}: {n} samples, reddish {reddish} ({:.0}%), purple or blue {purple} ({:.0}%), mean of the rest (sRGB) {:?}",
            100.0 * *reddish as f64 / *n as f64,
            100.0 * *purple as f64 / *n as f64,
            sum.map(|s| s / rest)
        ));
    }
    for (t, n) in &purple_tiles {
        let c = &terrain.colours[*t];
        log.line(format!("  purple samples {n} in colour tile record {} (L{} at {}, {})", c.record, c.level, c.ix, c.iz));
    }
    let mut colour_levels: BTreeMap<u32, usize> = BTreeMap::new();
    for c in &terrain.colours {
        *colour_levels.entry(c.level).or_default() += 1;
    }
    log.line(format!("colour tiles per level (level: count): {colour_levels:?}"));
    let not_ground: Vec<String> = terrain
        .colours
        .iter()
        .filter(|c| !fb_terrain::looks_like_ground(fb_terrain::bc3_mean_colour(&c.data)))
        .map(|c| { let m = fb_terrain::bc3_mean_colour(&c.data); format!("L{} ({},{}) rec {} size {} mean [{:.2} {:.2} {:.2}]", c.level, c.ix, c.iz, c.record, terrain.records[c.record].size, m[0], m[1], m[2]) })
        .collect();
    log.line(format!("colour tiles that do not look like ground: {}", not_ground.len()));
    for s in not_ground.iter().take(12) {
        log.line(format!("  {s}"));
    }
    // Every colour tile must decode to an image, not noise: average neighbour difference per tile.
    let mut diffs: Vec<(f64, u32, u32, u32, usize)> = terrain
        .colours
        .iter()
        .map(|c| {
            let mut px = vec![0u32; 260 * 260];
            let _ = texture2ddecoder::decode_bc3(&c.data, 260, 260, &mut px);
            let mut d = 0f64;
            for y in 0..260 {
                for x in 0..259 {
                    let (a, b) = (px[y * 260 + x].to_le_bytes(), px[y * 260 + x + 1].to_le_bytes());
                    d += (0..3).map(|k| (a[k] as f64 - b[k] as f64).abs()).sum::<f64>() / 3.0;
                }
            }
            (d / (260.0 * 259.0), c.level, c.ix, c.iz, c.record)
        })
        .collect();
    diffs.sort_by(|a, b| b.0.total_cmp(&a.0));
    log.line(format!(
        "colour tiles: noisiest {:.1} at L{} ({},{}); tiles above 15: {}; median {:.1}",
        diffs[0].0, diffs[0].1, diffs[0].2, diffs[0].3,
        diffs.iter().filter(|d| d.0 > 15.0).count(),
        diffs[diffs.len() / 2].0
    ));
    for d in diffs.iter().take(66) {
        let r = terrain.records[d.4];
        log.line(format!("  noisy {:.1}  L{} ({},{})  record {}  size {}  flags {:02x?}", d.0, d.1, d.2, d.3, d.4, r.size, r.flags));
    }

    // Check against ground objects, per finest level under the object.
    let mut walker = fb_level::Walker::new(&world, &fs);
    walker.skip_subworlds = skip.to_vec();
    walker.walk_level(level)?;
    let mut errs: BTreeMap<u32, Vec<f32>> = BTreeMap::new();
    for m in walker.meshes.iter().filter(|m| m.source == "VegetationTreeEntityData" || m.source == "StaticModelGroup(havok)") {
        let p = m.transform.translation;
        if let (Some(h), Some(lv)) = (terrain.height_at(p.x, p.z), terrain.level_at(p.x, p.z)) {
            errs.entry(lv).or_default().push(p.y - h);
        }
    }
    log.line("ground objects: object height minus ground height, by finest terrain level under the object");
    for (lv, e) in errs.iter_mut() {
        e.sort_by(|a, b| a.total_cmp(b));
        let mut abs: Vec<f32> = e.iter().map(|x| x.abs()).collect();
        abs.sort_by(|a, b| a.total_cmp(b));
        log.line(format!(
            "  level {lv} ({:.2} m per sample): {} objects, median |error| {:.2} m, median signed {:+.2} m, 90% |error| {:.2} m",
            (terrain.max[0] - terrain.min[0]) / 256.0 / (1u32 << lv) as f32,
            e.len(),
            abs[abs.len() / 2],
            e[e.len() / 2],
            abs[abs.len() * 9 / 10]
        ));
    }

    // Holes: where are they, and are they near bunker stairs?
    let holes = terrain.hole_positions();
    log.line(format!("hole samples (height 0) in finest tiles: {}", holes.len()));
    if !holes.is_empty() {
        let mut groups: Vec<([f32; 2], usize)> = Vec::new();
        for h in &holes {
            match groups.iter_mut().find(|g| (g.0[0] - h[0]).hypot(g.0[1] - h[1]) < 20.0) {
                Some(g) => g.1 += 1,
                None => groups.push((*h, 1)),
            }
        }
        log.line(format!("hole groups (within 20 m): {}", groups.len()));
        for (p, n) in groups.iter().take(12) {
            let near = walker
                .meshes
                .iter()
                .filter(|m| m.mesh.contains("stair") || m.mesh.contains("bunker"))
                .map(|m| (m.transform.translation.x - p[0]).hypot(m.transform.translation.z - p[1]))
                .fold(f32::MAX, f32::min);
            log.line(format!("  hole at x {:.1} z {:.1}: {n} samples, nearest bunker or stair mesh {:.1} m", p[0], p[1], near));
        }
    }

    // Trees only (they always stand on the ground), and the mesh names of the worst misses.
    let mut tree_err: Vec<f32> = Vec::new();
    let mut misses: BTreeMap<String, (usize, f32)> = BTreeMap::new();
    for m in &walker.meshes {
        let p = m.transform.translation;
        let Some(h) = terrain.height_at(p.x, p.z) else { continue };
        let e = p.y - h;
        if m.source == "VegetationTreeEntityData" {
            tree_err.push(e.abs());
        }
        if m.source == "StaticModelGroup(havok)" && e.abs() > 5.0 {
            let leaf = m.mesh.rsplit('/').next().unwrap_or(&m.mesh).to_string();
            let x = misses.entry(leaf).or_insert((0, 0.0));
            x.0 += 1;
            x.1 += e;
        }
    }
    tree_err.sort_by(|a, b| a.total_cmp(b));
    if !tree_err.is_empty() {
        log.line(format!(
            "trees: {} , median |error| {:.2} m, 90% |error| {:.2} m",
            tree_err.len(),
            tree_err[tree_err.len() / 2],
            tree_err[tree_err.len() * 9 / 10]
        ));
    }
    let mut top: Vec<_> = misses.into_iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    log.line("static props more than 5 m from the ground, most common meshes (count, average object minus ground):");
    for (name, (n, sum)) in top.iter().take(10) {
        log.line(format!("  {n:>4}  {:+.1} m  {name}", sum / *n as f32));
    }

    // Exports. Centre on the middle of the placed objects.
    let mut xs: Vec<f32> = walker.meshes.iter().map(|m| m.transform.translation.x).collect();
    let mut zs: Vec<f32> = walker.meshes.iter().map(|m| m.transform.translation.z).collect();
    xs.sort_by(|a, b| a.total_cmp(b));
    zs.sort_by(|a, b| a.total_cmp(b));
    let (cx, cz) = (xs[xs.len() / 2], zs[zs.len() / 2]);
    let dir = std::path::PathBuf::from("extracted").join("export");
    std::fs::create_dir_all(&dir)?;

    // Heightmap PNG of the whole terrain, 1024 x 1024 (4 m per pixel), dark = low.
    let (lo, hi) = (terrain.min[1], terrain.max[1]);
    let size = 1024u32;
    let mut px = Vec::with_capacity((size * size) as usize);
    for r in 0..size {
        for c in 0..size {
            let x = terrain.min[0] + (c as f32 + 0.5) / size as f32 * (terrain.max[0] - terrain.min[0]);
            let z = terrain.min[2] + (r as f32 + 0.5) / size as f32 * (terrain.max[2] - terrain.min[2]);
            let h = terrain.height_at(x, z).unwrap_or(lo);
            px.push((((h - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0) as u8);
        }
    }
    let path = dir.join("verdun_heightmap.png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), size, size);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&px)?;
    log.line(format!("wrote {} (whole terrain, 4 m per pixel; rows = z, columns = x)", path.display()));

    // Terrain OBJ: 1024 m square around the objects, 2 m grid (513 x 513 vertices).
    // Frostbite is left-handed; negate z for OBJ and reverse the winding.
    let (n, step) = (513usize, 2.0f32);
    let (x0, z0) = (cx - 512.0, cz - 512.0);
    let mut obj = String::from("# Verdun terrain around the battle area, exported by bf1_probe\n");
    for r in 0..n {
        for c in 0..n {
            let (x, z) = (x0 + c as f32 * step, z0 + r as f32 * step);
            let h = terrain.height_at(x, z).unwrap_or(0.0);
            let _ = writeln!(obj, "v {x} {h} {}", -z);
        }
    }
    for r in 0..n - 1 {
        for c in 0..n - 1 {
            let i = r * n + c + 1;
            let _ = writeln!(obj, "f {} {} {}\nf {} {} {}", i, i + n, i + 1, i + 1, i + n, i + n + 1);
        }
    }
    let path = dir.join("verdun_terrain.obj");
    std::fs::write(&path, obj)?;
    log.line(format!("wrote {} (1024 m square centred on x {cx:.0}, z {cz:.0}, 2 m grid)", path.display()));
    Ok(())
}

/// `terrain-table`: print every chunk table record with its flags, size, and
/// (for height tiles) level and position.
/// Ground layers around a point: the share of each layer (sum of mask
/// weights) in a square of `radius` metres, and a map of the strongest layer
/// per `step` metres (rows = z rising, columns = x rising; '.' = none).
/// `layer-combos <superbundle>`: do the layer numbers of the mask records
/// match the layer numbers in the terrain shader names? Each patch's set of
/// layers is compared with the 185 shader layer sets (exact match, or a
/// shader set that contains it), per quadtree level. Also counts, per layer,
/// the patches that list it and the shaders that name it.
pub fn cmd_layer_combos(log: &mut Log, root: &std::path::Path, superbundle: &str, swaps: &[(u8, u8)]) -> Result<()> {
    use fb_ebx::Value;
    use std::collections::{BTreeSet, HashSet};
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let lm = terrain.layers.as_ref().context("no layer masks")?;
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let Some(Value::ResourceRef(rid)) = world.partitions[part].primary().and_then(|p| p.value.get("TerrainLayerCombinationsResource")).cloned() else {
        anyhow::bail!("no TerrainLayerCombinationsResource")
    };
    let shaders = fb_res::shaderdb::terrain_colour_shaders(&world.read_res(&fs, rid)?);
    let sets: HashSet<BTreeSet<u8>> = shaders.iter().map(|(_, l)| l.iter().copied().collect()).collect();
    log.line(format!("{} shader names, {} different layer sets; record layers exchanged: {swaps:?}", shaders.len(), sets.len()));
    // Layers that no shader names (scatter-only layers) are left out of the patch sets.
    let named: HashSet<u8> = shaders.iter().flat_map(|(_, l)| l.iter().copied()).collect();
    // --dump <file>: shader sets ("S 0 2 11") and patch sets ("P <level> 0 4 ...", record numbers, all layers).
    if let Some(path) = std::env::args().skip_while(|a| a != "--dump").nth(1) {
        let mut out = String::new();
        for (_, l) in &shaders {
            out.push_str(&format!("S {}
", l.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")));
        }
        for (&(level, _, _), layers) in &lm.patches {
            out.push_str(&format!("P {level} {}
", layers.iter().map(|pl| pl.layer.to_string()).collect::<Vec<_>>().join(" ")));
        }
        std::fs::write(&path, out)?;
        log.line(format!("wrote {path}"));
    }
    let mut per_level: BTreeMap<u32, (usize, usize, usize)> = BTreeMap::new();
    let mut unmatched: BTreeMap<String, usize> = BTreeMap::new();
    let mut patches_with: BTreeMap<u8, usize> = BTreeMap::new();
    for (&(level, _, _), layers) in &lm.patches {
        // Test: --swap a,b exchanges two record layer numbers before the comparison.
        let swap = |l: u8| swaps.iter().fold(l, |l, &(a, b)| if l == a { b } else if l == b { a } else { l });
        let set: BTreeSet<u8> = layers.iter().map(|pl| swap(pl.layer)).filter(|l| named.contains(l)).collect();
        if set.is_empty() {
            continue;
        }
        for &l in &set {
            *patches_with.entry(l).or_default() += 1;
        }
        let e = per_level.entry(level).or_default();
        e.0 += 1;
        if sets.contains(&set) {
            e.1 += 1;
        } else if sets.iter().any(|s| set.is_subset(s)) {
            e.2 += 1;
        } else {
            *unmatched.entry(format!("{set:?}")).or_default() += 1;
        }
    }
    for (level, (n, exact, subset)) in &per_level {
        log.line(format!("level {level}: {n} patches, layer set equals a shader set {exact}, inside a shader set {subset}, neither {}", n - exact - subset));
    }
    let mut top: Vec<_> = unmatched.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    for (set, n) in top.iter().take(12) {
        log.line(format!("  no shader for {set}: {n} patches"));
    }
    let mut shaders_with: BTreeMap<u8, usize> = BTreeMap::new();
    for (_, l) in &shaders {
        for &x in l {
            *shaders_with.entry(x).or_default() += 1;
        }
    }
    let layers: BTreeSet<u8> = patches_with.keys().chain(shaders_with.keys()).copied().collect();
    let list: Vec<String> = layers.iter().map(|l| format!("{l}: {}/{}", patches_with.get(l).unwrap_or(&0), shaders_with.get(l).unwrap_or(&0))).collect();
    log.line(format!("per layer (patches listing it / shaders naming it): {}", list.join(", ")));
    Ok(())
}

/// `layer-colours <superbundle> <texture>...`: for each layer, the mean
/// colour of the terrain colour map (the game's baked far colour, sRGB) at
/// points where the layer's mixed weight is over 0.8 (8 m grid, whole map),
/// and the mean sRGB colour of each named texture. A layer's colour-map
/// colour should be close to its own texture's colour.
pub fn cmd_layer_colours(log: &mut Log, root: &std::path::Path, superbundle: &str, textures: &[String]) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let lm = terrain.layers.as_ref().context("no layer masks")?;
    let skip = [false; fb_terrain::layers::MAX_LAYERS];
    let mut w = [0f32; fb_terrain::layers::MAX_LAYERS];
    let mut sum = [[0f64; 3]; fb_terrain::layers::MAX_LAYERS];
    let mut count = [0usize; fb_terrain::layers::MAX_LAYERS];
    let mut decoded: HashMap<usize, Vec<[u8; 3]>> = HashMap::new();
    let step = 8.0f32;
    let n = (lm.width / step) as i32;
    for iz in 0..n {
        for ix in 0..n {
            let (x, z) = (lm.min[0] + (ix as f32 + 0.5) * step, lm.min[1] + (iz as f32 + 0.5) * step);
            if lm.weights_at(x, z, &skip, &mut w).is_none() {
                continue;
            }
            let Some(best) = (0..w.len()).find(|&l| w[l] > 0.8) else { continue };
            let Some((tile, uv)) = terrain.colour_at(x, z) else { continue };
            let px = decoded.entry(tile).or_insert_with(|| fb_terrain::decode_colour(&terrain.colours[tile].data));
            let s = fb_terrain::COLOUR_SIZE;
            let c = px[((uv[1] * s as f32) as usize).min(s - 1) * s + ((uv[0] * s as f32) as usize).min(s - 1)];
            for k in 0..3 {
                sum[best][k] += f64::from(c[k]);
            }
            count[best] += 1;
        }
    }
    for l in 0..fb_terrain::layers::MAX_LAYERS {
        if count[l] > 0 {
            let m = sum[l].map(|v| v / count[l] as f64);
            log.line(format!("layer {l:2}: colour map where dominant: sRGB [{:.0}, {:.0}, {:.0}] at {} points (8 m grid)", m[0], m[1], m[2], count[l]));
        }
    }
    for t in textures {
        match crate::meshes::texture_mean_srgb(&fs, &world, t) {
            Ok(m) => log.line(format!("texture {}: mean sRGB [{:.0}, {:.0}, {:.0}]", t.rsplit('/').next().unwrap_or(t), m[0], m[1], m[2])),
            Err(e) => log.line(format!("texture {t}: {e:#}")),
        }
    }
    Ok(())
}

pub fn cmd_layer_grid(log: &mut Log, root: &std::path::Path, superbundle: &str, x: f32, z: f32, radius: f32, step: f32) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let lm = terrain.layers.as_ref().context("no layer masks")?;
    let skip = [false; fb_terrain::layers::MAX_LAYERS];
    let mut w = [0f32; fb_terrain::layers::MAX_LAYERS];
    let mut share = [0f64; fb_terrain::layers::MAX_LAYERS];
    let mut levels: BTreeMap<u32, usize> = BTreeMap::new();
    let n = (2.0 * radius / step).round() as i32;
    let mut rows = Vec::new();
    for j in 0..=n {
        let mut row = String::new();
        for i in 0..=n {
            let (px, pz) = (x - radius + i as f32 * step, z - radius + j as f32 * step);
            match lm.weights_at(px, pz, &skip, &mut w) {
                Some(level) => {
                    *levels.entry(level).or_default() += 1;
                    for (s, v) in share.iter_mut().zip(&w) {
                        *s += f64::from(*v);
                    }
                    let best = (0..w.len()).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap();
                    row.push_str(&if w[best] > 0.0 { format!("{best:>3}") } else { "  .".into() });
                }
                None => row.push_str("  -"),
            }
        }
        rows.push(row);
    }
    let total: f64 = share.iter().sum();
    let list: Vec<String> = share.iter().enumerate().filter(|(_, s)| **s > 0.0).map(|(l, s)| format!("layer {l}: {:.1}%", 100.0 * s / total.max(1e-9))).collect();
    log.line(format!("terrain height at ({x}, {z}): {:?}", terrain.height_at(x, z)));
    // The terrain colour map (far ground colour) per grid row: mean sRGB and
    // the colour tile levels used.
    let mut decoded: HashMap<usize, Vec<[u8; 3]>> = HashMap::new();
    for j in 0..=n {
        let pz = z - radius + j as f32 * step;
        let (mut sum, mut count) = ([0u64; 3], 0u64);
        let mut levels_used: BTreeMap<u32, usize> = BTreeMap::new();
        for i in 0..=n {
            let px = x - radius + i as f32 * step;
            let Some((tile, uv)) = terrain.colour_at(px, pz) else { continue };
            let px_data = decoded.entry(tile).or_insert_with(|| fb_terrain::decode_colour(&terrain.colours[tile].data));
            let s = fb_terrain::COLOUR_SIZE;
            let (cx, cz) = (((uv[0] * s as f32) as usize).min(s - 1), ((uv[1] * s as f32) as usize).min(s - 1));
            let c = px_data[cz * s + cx];
            for k in 0..3 {
                sum[k] += c[k] as u64;
            }
            count += 1;
            *levels_used.entry(terrain.colours[tile].level).or_default() += 1;
        }
        if count > 0 && j % 4 == 0 {
            log.line(format!("  colour map z {pz:>7.1}: mean sRGB [{}, {}, {}], tile levels {levels_used:?}", sum[0] / count, sum[1] / count, sum[2] / count));
        }
    }
    log.line(format!("layers within {radius} m of ({x}, {z}), {step} m grid: {}; mask level used {levels:?}", list.join(", ")));
    // Each layer's own mask (not mixed): mean, and the share of points above
    // 0, 0.25 and 0.5; against the mixed weight.
    for layer in 0..fb_terrain::layers::MAX_LAYERS as u8 {
        let (mut sum, mut mixed, mut pts, mut a0, mut a25, mut a50) = (0f64, 0f64, 0usize, 0usize, 0usize, 0usize);
        for j in 0..=n {
            for i in 0..=n {
                let (px, pz) = (x - radius + i as f32 * step, z - radius + j as f32 * step);
                let Some(r) = lm.raw_weight_at(px, pz, layer) else { continue };
                if lm.weights_at(px, pz, &skip, &mut w).is_some() {
                    mixed += f64::from(w[layer as usize]);
                }
                pts += 1;
                sum += f64::from(r);
                a0 += usize::from(r > 0.0);
                a25 += usize::from(r > 0.25);
                a50 += usize::from(r > 0.5);
            }
        }
        if a0 > 0 || (12..=17).contains(&layer) {
            // Per quadtree level: mean own mask and share above 0.25.
            let mut per_level = Vec::new();
            for level in 0..=lm.finest {
                let (mut s, mut c, mut hi) = (0f64, 0usize, 0usize);
                for j in 0..=n {
                    for i in 0..=n {
                        let (px, pz) = (x - radius + i as f32 * step, z - radius + j as f32 * step);
                        if let Some(r) = lm.raw_weight_at_level(px, pz, layer, level) {
                            s += f64::from(r);
                            c += 1;
                            hi += usize::from(r > 0.25);
                        }
                    }
                }
                if c > 0 {
                    per_level.push(format!("L{level} {:.3}/{:.0}%", s / c as f64, 100.0 * hi as f64 / c as f64));
                }
            }
            log.line(format!("  layer {layer:>2} own mask by level (mean/above 0.25): {}", per_level.join(", ")));
        }
        if a0 > 0 {
            let pc = |k: usize| 100.0 * k as f64 / pts.max(1) as f64;
            log.line(format!(
                "  layer {layer:>2} own mask: mean {:.3}, above 0 {:.0}%, above 0.25 {:.0}%, above 0.5 {:.0}%; mixed weight mean {:.3}",
                sum / pts as f64, pc(a0), pc(a25), pc(a50), mixed / pts as f64
            ));
        }
    }
    for (j, r) in rows.iter().enumerate() {
        log.line(format!("  z {:>7.1}: {r}", z - radius + j as f32 * step));
    }
    Ok(())
}

/// Study of the 11 record bytes after `full` (DESIGN.md "layers carried from
/// coarser levels"): compare their 84 sub-cell bits (4 + 16 + 64 cells below
/// the record's patch) with (a) "a finer patch has its own record of this
/// layer there" and (b) "this record's own mask is not zero in that cell",
/// for several bit orders. Prints the share of bits that agree.
pub fn cmd_mask_bits(log: &mut Log, root: &std::path::Path, superbundle: &str) -> Result<()> {
    use fb_terrain::layers::{MASK_SIZE, MAX_LAYERS};
    use std::collections::HashSet;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let lm = terrain.layers.as_ref().context("no layer masks")?;
    let present: HashSet<(u32, u32, u32, u8)> = lm.records.iter().map(|r| {
        let (l, x, z) = lm.patch_of(r);
        (l, x, z, r.layer)
    }).collect();
    let _ = MAX_LAYERS;
    // Orders of the cells of one depth: 0 = row (z) major, 1 = column (x) major, 2 = Morton (x low bit first).
    let cell_index = |order: u32, n: u32, i: u32, j: u32| -> u32 {
        match order {
            0 => j * n + i,
            1 => i * n + j,
            _ => {
                let mut m = 0;
                for b in 0..8 {
                    m |= ((i >> b) & 1) << (2 * b);
                    m |= ((j >> b) & 1) << (2 * b + 1);
                }
                m
            }
        }
    };
    let bit = |tree: &[u8; 11], k: u32, msb: bool| -> bool {
        let byte = tree[(k / 8) as usize];
        if msb { (byte >> (7 - k % 8)) & 1 == 1 } else { (byte >> (k % 8)) & 1 == 1 }
    };
    // agree[meaning][order][msb]
    let mut agree = [[[0u64; 2]; 3]; 2];
    let mut total = 0u64;
    let mut ones = 0u64;
    let mut self_bit = [0u64; 2];
    let mut per_depth_true = [[0u64; 3]; 2];
    for r in &lm.records {
        let (l, px, pz) = lm.patch_of(r);
        // Own mask, if stored and usable.
        let mask = lm.patches.get(&(l, px, pz)).and_then(|v| v.iter().find(|pl| pl.layer == r.layer)).and_then(|pl| pl.mask).map(|i| lm.mask(i));
        self_bit[0] += u64::from(bit(&r.tree, 0, false));
        self_bit[1] += u64::from(bit(&r.tree, 0, true));
        for d in 1..=3u32 {
            let n = 1u32 << d;
            let base = (0..d).map(|k| 1u32 << (2 * k)).sum::<u32>(); // 1, 5, 21
            for j in 0..n {
                for i in 0..n {
                    let a = present.contains(&(l + d, px * n + i, pz * n + j, r.layer));
                    let b = match mask {
                        None => r.full,
                        Some(m) => {
                            let cells = 64 / n as usize;
                            let (x0, z0) = (i as usize * cells, j as usize * cells);
                            (z0..=z0 + cells).any(|zz| (x0..=x0 + cells).any(|xx| m[zz.min(MASK_SIZE - 1) * MASK_SIZE + xx.min(MASK_SIZE - 1)] > 0))
                        }
                    };
                    per_depth_true[0][d as usize - 1] += u64::from(a);
                    per_depth_true[1][d as usize - 1] += u64::from(b);
                    total += 1;
                    for order in 0..3 {
                        for msb in [false, true] {
                            let k = base + cell_index(order, n, i, j);
                            let v = bit(&r.tree, k, msb);
                            if order == 0 && !msb {
                                ones += u64::from(v);
                            }
                            agree[0][order as usize][usize::from(msb)] += u64::from(v == a);
                            agree[1][order as usize][usize::from(msb)] += u64::from(v == b);
                        }
                    }
                }
            }
        }
    }
    log.line(format!("records {}, sub-cell bits {total}, bits set (row order, low bit first) {:.1}%", lm.records.len(), 100.0 * ones as f64 / total.max(1) as f64));
    log.line(format!("bit 0 set: low bit first {:.1}%, high bit first {:.1}%", 100.0 * self_bit[0] as f64 / lm.records.len() as f64, 100.0 * self_bit[1] as f64 / lm.records.len() as f64));
    log.line(format!("true share by depth (1, 2, 3): (a) finer record {:?}, (b) own mask not zero {:?}", per_depth_true[0], per_depth_true[1]));
    for (mi, meaning) in ["(a) finer record of the layer", "(b) own mask not zero"].iter().enumerate() {
        for (oi, order) in ["row", "column", "Morton"].iter().enumerate() {
            for (bi, b) in ["low bit first", "high bit first"].iter().enumerate() {
                log.line(format!("  {meaning}, {order} order, {b}: {:.1}% agree", 100.0 * agree[mi][oi][bi] as f64 / total.max(1) as f64));
            }
        }
    }
    Ok(())
}

pub fn cmd_terrain_table(log: &mut Log, root: &std::path::Path, superbundle: &str) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let mut tile_of: BTreeMap<usize, (u32, u32, u32)> = BTreeMap::new();
    for t in &terrain.tiles {
        if let Some(r) = t.record {
            tile_of.insert(r, (t.level, t.ix, t.iz));
        }
    }
    for (i, r) in terrain.records.iter().enumerate() {
        let tile = tile_of.get(&i).map(|(l, x, z)| format!("height L{l} ({x},{z})")).unwrap_or_default();
        log.line(format!("{i:>4}  flags {:02x} {:02x} {:02x}  size {:>7}  {tile}", r.flags[0], r.flags[1], r.flags[2], r.size));
    }
    Ok(())
}

/// `terrain-layer <record>`: decode the 67,600-byte layer of a chunk record
/// (a whole leaf chunk, or bytes 136242..203842 of a height chunk) as each BC
/// format; print smoothness and channel averages; write PNGs to extracted/terrain/.
pub fn cmd_terrain_layer(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let r = terrain.records.get(record).context("no such record")?;
    let chunk = world.read_chunk(&fs, &r.chunk_id)?;
    let start = if chunk.len() == 67600 { 0 } else { 261 * 261 * 2 };
    let layer = chunk.get(start..start + 67600).context("chunk too small")?;
    log.line(format!("record {record}: chunk {} bytes, layer at {start}", chunk.len()));
    let (w, h) = (260usize, 260usize);
    let out = std::path::PathBuf::from("extracted").join("terrain");
    std::fs::create_dir_all(&out)?;
    // Also try the layer as two BC1 images of 33,800 bytes each.
    for half in 0..2 {
        let mut px = vec![0u32; w * h];
        if texture2ddecoder::decode_bc1(&layer[half * 33800..(half + 1) * 33800], w, h, &mut px).is_ok() {
            let ch = |p: u32, c: usize| p.to_le_bytes()[c] as f64;
            let (mut d, mut m) = (0f64, [0f64; 3]);
            for y in 0..h {
                for x in 0..w - 1 {
                    let p = px[y * w + x];
                    d += (ch(p, 2) - ch(px[y * w + x + 1], 2)).abs();
                    for c in 0..3 {
                        m[c] += ch(p, c);
                    }
                }
            }
            let n = (w * h) as f64;
            log.line(format!("  bc1 half {half}: mean r {:.0} g {:.0} b {:.0}  neighbour difference r {:.1}", m[2] / n, m[1] / n, m[0] / n, d / n));
        }
    }
    for (name, f) in [
        ("bc3", texture2ddecoder::decode_bc3 as fn(&[u8], usize, usize, &mut [u32]) -> std::result::Result<(), &'static str>),
        ("bc5", texture2ddecoder::decode_bc5),
        ("bc7", texture2ddecoder::decode_bc7),
    ] {
        let mut px = vec![0u32; w * h];
        if f(layer, w, h, &mut px).is_err() {
            log.line(format!("  {name}: decode error"));
            continue;
        }
        let ch = |p: u32, c: usize| p.to_le_bytes()[c] as f64;
        let mut diff = [0f64; 4];
        let mut mean = [0f64; 4];
        for y in 0..h {
            for x in 0..w {
                let p = px[y * w + x];
                for c in 0..4 {
                    mean[c] += ch(p, c);
                    if x + 1 < w {
                        diff[c] += (ch(p, c) - ch(px[y * w + x + 1], c)).abs();
                    }
                }
            }
        }
        let n = (w * h) as f64;
        // Bytes are [b, g, r, a].
        log.line(format!(
            "  {name}: mean r {:.0} g {:.0} b {:.0} a {:.0}   neighbour difference r {:.1} g {:.1} b {:.1} a {:.1}",
            mean[2] / n, mean[1] / n, mean[0] / n, mean[3] / n, diff[2] / n, diff[1] / n, diff[0] / n, diff[3] / n
        ));
        let rgba: Vec<u8> = px.iter().flat_map(|p| { let b = p.to_le_bytes(); [b[2], b[1], b[0], 255] }).collect();
        let path = out.join(format!("record{record}_{name}.png"));
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&rgba)?;
    }
    Ok(())
}

/// `terrain-extra <record>...`: hex dump of the bytes after heights + colour.
pub fn cmd_terrain_extra(log: &mut Log, root: &std::path::Path, superbundle: &str, records: &[usize]) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let out = std::path::PathBuf::from("extracted").join("terrain");
    std::fs::create_dir_all(&out)?;
    for &i in records {
        let r = terrain.records.get(i).context("no such record")?;
        let chunk = world.read_chunk(&fs, &r.chunk_id)?;
        let start = 261 * 261 * 2;
        let extra = chunk.get(start..start + 8847).unwrap_or(&[]);
        log.line(format!("record {i}: chunk {} bytes, block A (8847 bytes after heights); distinct byte values {}", chunk.len(), { let mut v = extra.to_vec(); v.sort(); v.dedup(); v.len() }));
        for row in extra.chunks(32).take(8) {
            log.line(format!("  {}", row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")));
        }
        std::fs::write(out.join(format!("record{i}_extra.bin")), extra)?;
    }
    Ok(())
}

/// `terrain-colour-offset <record>`: decode the colour layer at start offsets
/// 136242..136242+24 and print how smooth each result is (lower = more image-like).
pub fn cmd_terrain_colour_offset(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    // Candidates: offsets where 64 blocks in a row start with 8 zero bytes (alpha 0).
    let mut cands = Vec::new();
    let mut at = 261 * 261 * 2;
    while at + 67600 <= chunk.len() {
        if (0..64).all(|b| chunk[at + b * 16..at + b * 16 + 8].iter().all(|&x| x == 0)) {
            cands.push(at);
            at += 67600;
        } else {
            at += 1;
        }
    }
    log.line(format!("  zero-alpha runs start at offsets (minus 136242): {:?}", cands.iter().map(|c| c - 136242).collect::<Vec<_>>()));
    for k in [0usize, 8, 15, 8847] {
        let start = 261 * 261 * 2 + k;
        let Some(layer) = chunk.get(start..start + 67600) else { break };
        let mut px = vec![0u32; 260 * 260];
        if texture2ddecoder::decode_bc3(layer, 260, 260, &mut px).is_err() {
            continue;
        }
        let mut d = 0f64;
        for y in 0..260 {
            for x in 0..259 {
                let (a, b) = (px[y * 260 + x].to_le_bytes(), px[y * 260 + x + 1].to_le_bytes());
                d += (0..3).map(|c| (a[c] as f64 - b[c] as f64).abs()).sum::<f64>() / 3.0;
            }
        }
        log.line(format!("  start 136242 + {k:>2}: neighbour difference {:.1}", d / (260.0 * 259.0)));
        if k == 8847 {
            let rgba: Vec<u8> = px.iter().flat_map(|p| { let b = p.to_le_bytes(); [b[2], b[1], b[0], 255] }).collect();
            let path = std::path::PathBuf::from("extracted").join("terrain").join(format!("record{record}_start{k}.png"));
            let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), 260, 260);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&rgba)?;
        }
    }
    Ok(())
}

/// `terrain-node <record>`: count special height values and dump the start of
/// each 4 KB step of the bytes after heights + block A + colour.
pub fn cmd_terrain_node(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    let heights: Vec<u16> = chunk[..261 * 261 * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let (zeros, maxes) = (heights.iter().filter(|&&h| h == 0).count(), heights.iter().filter(|&&h| h == u16::MAX).count());
    let (lo, hi) = (heights.iter().min().unwrap(), heights.iter().max().unwrap());
    log.line(format!("record {record}: heights min {lo} max {hi}, samples == 0: {zeros}, == 65535: {maxes}"));
    let rest = &chunk[261 * 261 * 2 + 8847 + 67600..];
    log.line(format!("bytes after heights + block A + colour: {}", rest.len()));
    std::fs::write(std::path::PathBuf::from("extracted").join("terrain").join(format!("record{record}_rest.bin")), rest)?;
    for at in (0..rest.len()).step_by(33800).take(16) {
        let row: Vec<String> = rest[at..(at + 24).min(rest.len())].iter().map(|b| format!("{b:02x}")).collect();
        log.line(format!("  +{at:>7}: {}", row.join(" ")));
    }
    Ok(())
}


/// `terrain-scan-colour <record>`: test every byte offset after the heights for a
/// smooth, ground-coloured BC3 layer (quick check on sample blocks), and list the best.
pub fn cmd_terrain_scan_colour(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    let mut hits: Vec<(f32, usize, [f32; 3])> = Vec::new();
    let mut at = if chunk.len() < 136242 + 67600 || record == 0 { 0 } else { 136242 };
    while at + 67600 <= chunk.len() {
        let layer = &chunk[at..at + 67600];
        // Quick check: roughness over 8 sample rows.
        let c0 = |b: usize| -> [i32; 3] { let v = u16::from_le_bytes([layer[b * 16 + 8], layer[b * 16 + 9]]) as i32; [(v >> 11) & 31, (v >> 5) & 63, v & 31] };
        let mut s = 0i32;
        for row in (4..65).step_by(8) {
            for x in 0..64 {
                let (a, b) = (c0(row * 65 + x), c0(row * 65 + x + 1));
                s += (a[0] - b[0]).abs() * 2 + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() * 2;
            }
        }
        let quick = s as f32 / (8.0 * 64.0);
        if quick < 12.0 {
            let mean = fb_terrain::bc3_mean_colour(layer);
            hits.push((fb_terrain::bc3_roughness(layer), at, mean));
        }
        at += 1;
    }
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    log.line(format!("record {record}: {} smooth offsets", hits.len()));
    let mut shown = 0;
    let mut last = usize::MAX;
    for (rough, at, mean) in &hits {
        if last != usize::MAX && at.abs_diff(last) < 32 {
            continue;
        }
        last = *at;
        log.line(format!("  at {:>7} (chunk end - {:>7})  roughness {rough:.2}  mean [{:.2} {:.2} {:.2}] ground {}", at, chunk.len() - at, mean[0], mean[1], mean[2], fb_terrain::looks_like_ground(*mean)));
        shown += 1;
        if shown >= 12 {
            break;
        }
    }
    Ok(())
}

/// `terrain-first-layers`: for every height node, the roughness and mean colour
/// of the layer right after block A, and whether it is taken as colour.
pub fn cmd_terrain_first_layers(log: &mut Log, root: &std::path::Path, superbundle: &str) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    for t in terrain.tiles.iter().filter(|t| t.record.is_some()) {
        let r = t.record.unwrap();
        let chunk = world.read_chunk(&fs, &terrain.records[r].chunk_id)?;
        let at = fb_terrain::COLOUR_IN_HEIGHT_NODE;
        let layer = &chunk[at..at + fb_terrain::COLOUR_BYTES];
        let (rough, mean) = (fb_terrain::bc3_roughness(layer), fb_terrain::bc3_mean_colour(layer));
        let ratio = fb_terrain::pixel_structure_ratio(&fb_terrain::decode_colour(layer));
        log.line(format!("  {}: L{} ({},{}) rec {r}  roughness {rough:.1}  ratio {ratio:.2}  mean [{:.2} {:.2} {:.2}]", if fb_terrain::is_colour_layer(layer) { "colour   " } else { "no colour" }, t.level, t.ix, t.iz, mean[0], mean[1], mean[2]));
    }
    Ok(())
}

/// `bc3-at <file> <offset> <png>`: decode 260 x 260 BC3 at a byte offset of any
/// file, print roughness and mean colour, and write a PNG (for study).
pub fn cmd_bc3_at(log: &mut Log, file: &str, offset: usize, out: &str) -> Result<()> {
    let data = std::fs::read(file)?;
    let layer = data.get(offset..offset + 67600).context("offset past end of file")?;
    let px = fb_terrain::decode_colour(layer);
    let m = fb_terrain::bc3_mean_colour(layer);
    log.line(format!("offset {offset} (0x{offset:x}): roughness {:.1}, mean [{:.2} {:.2} {:.2}]", fb_terrain::bc3_roughness(layer), m[0], m[1], m[2]));
    let rgba: Vec<u8> = px.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(out)?), 260, 260);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

/// `terrain-minmax <record>`: compare the start of block A with min/max heights
/// of square cells of the node's height grid, for several cell sizes and offsets.
pub fn cmd_terrain_minmax(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    let n = 261usize;
    let h: Vec<u16> = chunk[..n * n * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let a: Vec<u16> = chunk[n * n * 2..n * n * 2 + 8847].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    log.line(format!("block A first 12 u16: {:?}", &a[..12]));
    for cell in [2usize, 4, 8, 16, 32] {
        for start in [0usize, 1, 2] {
            // First cells along the first row of cells.
            let mut pairs = Vec::new();
            for cx in 0..6 {
                let (x0, y0) = (start + cx * cell, start);
                let (mut lo, mut hi) = (u16::MAX, 0u16);
                for y in y0..(y0 + cell + 1).min(n) {
                    for x in x0..(x0 + cell + 1).min(n) {
                        let v = h[y * n + x];
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                pairs.push((lo, hi));
            }
            log.line(format!("  cell {cell:>2} start {start}: {:?}", pairs));
        }
    }
    Ok(())
}

/// `terrain-chunk-save <record>`: save the whole chunk of a record to extracted/terrain/.
pub fn cmd_terrain_chunk_save(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    let path = std::path::PathBuf::from("extracted").join("terrain").join(format!("record{record}_chunk.bin"));
    std::fs::write(&path, &chunk)?;
    log.line(format!("wrote {} ({} bytes)", path.display(), chunk.len()));
    Ok(())
}

/// `terrain-chunks-save-all`: write every record's chunk to
/// extracted/terrain/all/record<N>.bin and a list (record, flags, size, level,
/// ix, iz, kind) to extracted/terrain/all/records.csv, for offline study.
pub fn cmd_terrain_chunks_save_all(log: &mut Log, root: &std::path::Path, superbundle: &str) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let dir = std::path::PathBuf::from("extracted").join("terrain").join("all");
    std::fs::create_dir_all(&dir)?;
    let mut csv = String::from("record,flags,size,level,ix,iz,kind
");
    let (mut saved, mut failed) = (0usize, 0usize);
    for (i, r) in terrain.records.iter().enumerate() {
        let (level, ix, iz, kind) = if let Some(t) = terrain.tiles.iter().find(|t| t.record == Some(i)) {
            (t.level.to_string(), t.ix.to_string(), t.iz.to_string(), "height")
        } else if let Some(c) = terrain.colours.iter().find(|c| c.record == i) {
            (c.level.to_string(), c.ix.to_string(), c.iz.to_string(), "colour")
        } else {
            (String::new(), String::new(), String::new(), "other")
        };
        csv.push_str(&format!("{i},{:02x}{:02x}{:02x},{},{level},{ix},{iz},{kind}
", r.flags[0], r.flags[1], r.flags[2], r.size));
        match world.read_chunk(&fs, &r.chunk_id) {
            Ok(data) => {
                std::fs::write(dir.join(format!("record{i}.bin")), &data)?;
                saved += 1;
            }
            Err(_) => failed += 1,
        }
    }
    std::fs::write(dir.join("records.csv"), csv)?;
    log.line(format!("saved {saved} chunks, {failed} not readable, list in {}", dir.join("records.csv").display()));
    Ok(())
}

/// `terrain-masks <record>`: list the node's 66 x 66 masks (after heights, block A
/// and the 67,600-byte layer) with their mean, and test which runs of
/// consecutive masks add up to 255 at every pixel (one patch's blend weights).
pub fn cmd_terrain_masks(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    const M: usize = 66 * 66;
    // Layout: heights | block A | N masks | final 67,600-byte layer.
    let start = 136242 + 8847;
    let n = (chunk.len() - start - 67600) / M;
    log.line(format!("record {record}: {n} masks"));
    let masks: Vec<&[u8]> = (0..n).map(|i| &chunk[start + i * M..start + (i + 1) * M]).collect();
    for (i, m) in masks.iter().enumerate() {
        let mean = m.iter().map(|&v| v as f64).sum::<f64>() / M as f64;
        let full = m.iter().filter(|&&v| v == 255).count();
        let zero = m.iter().filter(|&&v| v == 0).count();
        log.line(format!("  mask {i:>3}: mean {mean:6.1}  zero {zero:>4}  full {full:>4}"));
    }
    // Greedy grouping: extend a run while the per-pixel sum stays <= 255 + slack; a run ends when it reaches ~255.
    let mut i = 0;
    let mut groups = Vec::new();
    while i < n {
        let mut sum = vec![0u32; M];
        let mut j = i;
        let mut found = None;
        while j < n && j < i + 12 {
            for (s, &v) in sum.iter_mut().zip(masks[j].iter()) {
                *s += v as u32;
            }
            let off = sum.iter().filter(|&&s| (s as i32 - 255).abs() > 3).count();
            if off * 100 < M {
                found = Some(j);
                break;
            }
            j += 1;
        }
        match found {
            Some(j) => {
                groups.push(j - i + 1);
                i = j + 1;
            }
            None => {
                log.line(format!("  no run from mask {i} adds up to 255"));
                break;
            }
        }
    }
    log.line(format!("groups (masks per patch) found: {} groups {:?}", groups.len(), groups));
    Ok(())
}

/// `terrain-mask-sheet <record>`: write all 66 x 66 masks of a node as one
/// grayscale image (8 per row, 2-pixel gaps) to extracted/terrain/record<N>_masks.png.
pub fn cmd_terrain_mask_sheet(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    const S: usize = 66;
    let start = 136242 + 8847;
    let n = (chunk.len() - start - 67600) / (S * S);
    let cols = 8usize;
    let rows = n.div_ceil(cols).max(1);
    let (w, h) = (cols * (S + 2), rows * (S + 2));
    let mut img = vec![40u8; w * h];
    for i in 0..n {
        let (ox, oy) = ((i % cols) * (S + 2), (i / cols) * (S + 2));
        for y in 0..S {
            for x in 0..S {
                img[(oy + y) * w + ox + x] = chunk[start + i * S * S + y * S + x];
            }
        }
    }
    let path = std::path::PathBuf::from("extracted").join("terrain").join(format!("record{record}_masks.png"));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img)?;
    log.line(format!("wrote {} ({n} masks)", path.display()));
    Ok(())
}

/// `bc3-scan <file> <from> <to>`: roughness of a 260 x 260 BC3 image starting at
/// every offset in from..to; prints the 8 smoothest.
pub fn cmd_bc3_scan(log: &mut Log, file: &str, from: usize, to: usize) -> Result<()> {
    let data = std::fs::read(file)?;
    let mut res: Vec<(f32, usize, [f32; 3])> = (from..to.min(data.len().saturating_sub(67600) + 1))
        .map(|o| {
            let l = &data[o..o + 67600];
            (fb_terrain::bc3_roughness(l), o, fb_terrain::bc3_mean_colour(l))
        })
        .collect();
    res.retain(|x| x.2[0] + x.2[1] + x.2[2] > 0.3);
    res.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (r, o, m) in res.iter().take(8) {
        log.line(format!("  offset {o} (end - {}) roughness {r:.2} mean [{:.2} {:.2} {:.2}]", data.len() - o, m[0], m[1], m[2]));
    }
    Ok(())
}

/// `terrain-mask-edges <record>`: for every pair of masks, test whether an edge
/// of one equals the opposite edge of the other (neighbouring patches of the
/// same layer), for several column/row offsets. Skips flat edges.
pub fn cmd_terrain_mask_edges(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    const S: usize = 66;
    let start = 136242 + 8847;
    let n = (chunk.len() - start - 67600) / (S * S);
    let m = |i: usize, x: usize, y: usize| chunk[start + i * S * S + y * S + x];
    let col = |i: usize, x: usize| -> Vec<u8> { (0..S).map(|y| m(i, x, y)).collect() };
    let row = |i: usize, y: usize| -> Vec<u8> { (0..S).map(|x| m(i, x, y)).collect() };
    let flat = |v: &Vec<u8>| v.iter().all(|&a| a == v[0]);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: Vec<String> = Vec::new();
    for (a, b) in [(65usize, 1usize), (64, 0), (65, 0), (64, 1), (63, 0), (65, 2)] {
        for i in 0..n {
            for j in 0..n {
                if i == j {
                    continue;
                }
                let (ci, cj) = (col(i, a), col(j, b));
                if !flat(&ci) && ci == cj {
                    *counts.entry(format!("right col {a} = left col {b}")).or_default() += 1;
                    if examples.len() < 12 { examples.push(format!("x: {i}->{j} ({a},{b})")); }
                }
                let (ri, rj) = (row(i, a), row(j, b));
                if !flat(&ri) && ri == rj {
                    *counts.entry(format!("bottom row {a} = top row {b}")).or_default() += 1;
                    if examples.len() < 24 { examples.push(format!("y: {i}->{j} ({a},{b})")); }
                }
            }
        }
    }
    log.line(format!("record {record}: {n} masks; matching edge pairs: {counts:?}"));
    log.line(format!("examples: {}", examples.join("  ")));
    Ok(())
}

/// `terrain-find-table`: search the tree file for a table that gives each
/// node's mask count. Tries entry sizes and two encodings (sum of 4-bit
/// values, and count of set bits) and counts how many consecutive height
/// nodes match from the best start offset.
pub fn cmd_terrain_find_table(log: &mut Log, root: &std::path::Path, superbundle: &str) -> Result<()> {
    use fb_ebx::Value;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let part = (0..world.partitions.len())
        .find(|&i| world.partitions[i].primary().is_some_and(|p| p.value.type_name == "TerrainData"))
        .context("no TerrainData")?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("TerrainStreamingTreeResource") else { anyhow::bail!("no tree") };
    let tree = world.read_res(&fs, *rid)?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    // Mask counts of height nodes, in record order.
    let mut counts: Vec<usize> = Vec::new();
    for t in terrain.tiles.iter().filter(|t| t.record.is_some()) {
        let _ = t;
    }
    let mut rec_tiles: Vec<usize> = terrain.tiles.iter().filter_map(|t| t.record).collect();
    rec_tiles.sort();
    for r in &rec_tiles {
        let size = terrain.records[*r].size as usize;
        counts.push((size - 136242 - 8847 - 67600) / 4356);
    }
    log.line(format!("height nodes: {}, first mask counts: {:?}", counts.len(), &counts[..20.min(counts.len())]));
    let nib_sum = |b: &[u8]| b.iter().map(|x| (x & 15) as usize + (x >> 4) as usize).sum::<usize>();
    let bit_sum = |b: &[u8]| b.iter().map(|x| x.count_ones() as usize).sum::<usize>();
    let mut best = (0usize, 0usize, 0usize, "");
    for entry in [4usize, 8, 16, 32, 64, 128] {
        for (name, f) in [("4-bit sum", &nib_sum as &dyn Fn(&[u8]) -> usize), ("bit count", &bit_sum)] {
            for s in 0..tree.len().saturating_sub(entry * counts.len()) {
                let mut k = 0;
                while k < counts.len() {
                    let e = &tree[s + k * entry..s + (k + 1) * entry];
                    if f(e) != counts[k] {
                        break;
                    }
                    k += 1;
                }
                if k > best.0 {
                    best = (k, s, entry, name);
                }
            }
        }
    }
    log.line(format!("best: {} consecutive nodes match at offset 0x{:x}, entry size {}, {}", best.0, best.1, best.2, best.3));
    Ok(())
}

/// `terrain-combos <file>`: read every "__2d__" entry of a layercombinations res:
/// after the name, u32 a, u32 b, u32 n and n bytes. Sum n over all entries and
/// show the distribution, to compare with the number of stored masks.
pub fn cmd_terrain_combos(log: &mut Log, file: &str) -> Result<()> {
    let d = std::fs::read(file)?;
    let needle = b"__2d__d__";
    let (mut entries, mut sum_n, mut sum_layers) = (0usize, 0usize, 0usize);
    let mut hist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut examples = Vec::new();
    let mut at = 0;
    while let Some(p) = d[at..].windows(needle.len()).position(|w| w == needle) {
        let name_end = at + p + needle.len() + 1; // the digit after __d__
        let start = d[..at + p].iter().rposition(|&c| c == 0).map(|x| x + 1).unwrap_or(0);
        let name = String::from_utf8_lossy(&d[start..name_end]).to_string();
        let q = name_end + 1; // skip the terminating zero
        let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
        let (a, b, n) = (u(q), u(q + 4), u(q + 8));
        let list = &d[q + 12..q + 12 + n.min(64)];
        let layers = name.split("__").nth(1).map(|s| s.split('_').count()).unwrap_or(0);
        entries += 1;
        sum_n += n;
        sum_layers += layers;
        *hist.entry(n).or_default() += 1;
        if examples.len() < 12 {
            examples.push(format!("{} a {a} b {b} n {n} {:?}", name.rsplit('/').next().unwrap_or(""), list));
        }
        at = name_end;
    }
    log.line(format!("2d entries {entries}; sum of n {sum_n}; layers in names {sum_layers}; n histogram {hist:?}"));
    for e in examples {
        log.line(format!("  {e}"));
    }
    Ok(())
}

/// `terrain-blocka-map <record>`: write block A's bytes after the min/max tree
/// (offset 5,460) as a grayscale image 67 pixels wide (values scaled x16 for contrast).
pub fn cmd_terrain_blocka_map(log: &mut Log, root: &std::path::Path, superbundle: &str, record: usize) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (terrain, _) = load_terrain(&fs, &world)?;
    let chunk = world.read_chunk(&fs, &terrain.records.get(record).context("no such record")?.chunk_id)?;
    let a = &chunk[136242 + 5460..136242 + 8847];
    let (w, h) = (67usize, a.len() / 67);
    let img: Vec<u8> = a[..w * h].iter().map(|&v| if v == 255 { 255 } else { (v as u32 * 16).min(254) as u8 }).collect();
    let mut hist: BTreeMap<u8, usize> = BTreeMap::new();
    for &v in a {
        *hist.entry(v).or_default() += 1;
    }
    let path = std::path::PathBuf::from("extracted").join("terrain").join(format!("record{record}_blocka.png"));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&path)?), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img)?;
    let top: Vec<String> = { let mut v: Vec<_> = hist.into_iter().collect(); v.sort_by(|x, y| y.1.cmp(&x.1)); v.into_iter().take(12).map(|(k, c)| format!("{k}:{c}")).collect() };
    log.line(format!("wrote {} ({w}x{h}); most common values {}", path.display(), top.join(" ")));
    Ok(())
}
