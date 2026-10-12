//! `meshes` command: decode LOD 0 of every mesh placed in a level, and read
//! every texture header its materials use.

use crate::Log;
use anyhow::Result;
use fb_ebx::{Pointer, Value};
use fb_res::mesh::{decode_lod, parse_meshset, DecodeStats};
use fb_res::texture::{block_format, parse_texture, BlockFormat};
use fb_vfs::GameFs;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Instant;

pub fn cmd_meshes(log: &mut Log, root: &std::path::Path, superbundle: &str, level: &str, skip: &[String]) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    let toc_chunks = fs.index_toc_chunks()?;
    log.line(format!("chunks listed in .toc files: {toc_chunks}"));
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    log.line(format!("loaded {} partitions, {} res, {} chunks", world.partitions.len(), world.res.len(), world.chunks.len()));
    let mut walker = fb_level::Walker::new(&world, &fs);
    walker.skip_subworlds = skip.to_vec();
    walker.walk_level(level)?;
    let assets: BTreeSet<(usize, usize)> = walker.meshes.iter().filter_map(|m| m.mesh_at).collect();
    log.line(format!("unique mesh assets placed: {}", assets.len()));

    let t = Instant::now();
    let mut fails: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut fail = |why: String, name: &str| {
        let short: String = why.chars().take(90).collect();
        fails.entry(short).or_insert((0, name.to_string())).0 += 1;
    };
    let mut stats = DecodeStats::default();
    let (mut ok, mut verts, mut tris, mut out_of_box, mut no_normals, mut no_uvs) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut layouts: BTreeMap<String, usize> = BTreeMap::new();
    let mut mesh_types: BTreeMap<u32, usize> = BTreeMap::new();
    let mut index_formats: BTreeMap<u32, usize> = BTreeMap::new();
    let mut textures: BTreeSet<(usize, usize)> = BTreeSet::new();

    for &at in &assets {
        let asset = world.get(at);
        let name = world.names[at.0].clone();
        // Textures used by this mesh's materials.
        if let Some(Value::Array { items, .. }) = asset.get("Materials") {
            for m in items {
                let Value::Pointer(p) = m else { continue };
                let Some(mat) = world.resolve(at.0, p) else { continue };
                let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { continue };
                let Some(Value::Array { items: tps, .. }) = sh.get("TextureParameters") else { continue };
                for tp in tps {
                    let Value::Struct(tp) = tp else { continue };
                    if let Some(Value::Pointer(tex)) = tp.get("Value") {
                        match world.resolve(mat.0, tex) {
                            Some(tat) => {
                                textures.insert(tat);
                            }
                            None if !matches!(tex, Pointer::Null) => fail("texture asset not loaded".into(), &name),
                            None => {}
                        }
                    }
                }
            }
        }
        let Some(Value::ResourceRef(rid)) = asset.get("MeshSetResource") else {
            fail("no MeshSetResource".into(), &name);
            continue;
        };
        let ms = match world.read_res(&fs, *rid).and_then(|d| parse_meshset(&d)) {
            Ok(m) => m,
            Err(e) => {
                fail(format!("meshset: {e:#}"), &name);
                continue;
            }
        };
        *mesh_types.entry(ms.mesh_type).or_default() += 1;
        let Some(lod) = ms.lods.first() else {
            fail("no LODs".into(), &name);
            continue;
        };
        *index_formats.entry(lod.index_format).or_default() += 1;
        for s in &lod.sections {
            let d: Vec<String> = s.declaration.elements.iter().map(|e| format!("{:02x}:{:02x}", e.usage, e.format)).collect();
            *layouts.entry(d.join(" ")).or_default() += 1;
        }
        let buffer = match (lod.chunk_id, lod.inline_offset) {
            (Some(id), _) => world.read_chunk(&fs, &id),
            (None, Some(_)) => {
                fail("inline mesh data (not supported yet)".into(), &name);
                continue;
            }
            (None, None) => {
                fail("no chunk and no inline data".into(), &name);
                continue;
            }
        };
        let buffer = match buffer {
            Ok(b) => b,
            Err(e) => {
                fail(format!("chunk: {e:#}"), &name);
                continue;
            }
        };
        match decode_lod(lod, &buffer, &mut stats) {
            Ok(g) => {
                ok += 1;
                verts += g.positions.len();
                tris += g.indices.len() / 3;
                no_normals += g.normals.is_empty() as usize;
                no_uvs += g.uvs.is_empty() as usize;
                // Every position must lie inside the mesh's bounding box (small tolerance).
                let size = (0..3).map(|i| ms.bbox_max[i] - ms.bbox_min[i]).fold(0.0f32, f32::max);
                let tol = 0.01 + 0.01 * size;
                let outside = g
                    .positions
                    .iter()
                    .filter(|p| (0..3).any(|i| !p[i].is_finite() || p[i] < ms.bbox_min[i] - tol || p[i] > ms.bbox_max[i] + tol))
                    .count();
                if outside > 0 {
                    out_of_box += 1;
                    if out_of_box <= 5 {
                        log.line(format!("  {outside} of {} vertices outside the box: {name}", g.positions.len()));
                    }
                }
            }
            Err(e) => fail(format!("decode: {e:#}"), &name),
        }
    }
    log.line(format!(
        "meshes decoded: {ok} of {}  in {:.2} s  vertices: {verts}  triangles: {tris}",
        assets.len(),
        t.elapsed().as_secs_f64()
    ));
    log.line(format!("meshes with vertices outside their bounding box: {out_of_box}"));
    log.line(format!("meshes without normals: {no_normals}  without texture coordinates: {no_uvs}"));
    log.line(format!("mesh types (0 rigid, 1 skinned, 2 composite): {mesh_types:?}"));
    log.line(format!("index formats (LOD 0): {index_formats:?}"));
    log.line(format!("sections skipped (not triangle lists or no position): {}", stats.sections_skipped));
    for ((u, f), n) in &stats.unknown_formats {
        log.line(format!("  unknown vertex format: usage 0x{u:02x} format 0x{f:02x} in {n} sections"));
    }
    let mut top: Vec<_> = layouts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    log.line("--- vertex layouts (usage:format), top 12");
    for (l, n) in top.iter().take(12) {
        log.line(format!("  {n:>5}  {l}"));
    }

    // Texture headers.
    let mut by_format: BTreeMap<i32, (usize, BTreeSet<String>, Vec<String>)> = BTreeMap::new();
    let (mut tex_ok, mut tex_chunk_missing) = (0usize, 0usize);
    let mut seen = HashSet::new();
    for &at in &textures {
        let name = world.names[at.0].clone();
        let Some(Value::ResourceRef(rid)) = world.get(at).get("Resource") else {
            fail("texture without Resource".into(), &name);
            continue;
        };
        if !seen.insert(*rid) {
            continue;
        }
        match world.read_res(&fs, *rid).and_then(|d| parse_texture(&d)) {
            Ok(h) => {
                tex_ok += 1;
                if !world.chunks.contains_key(&h.chunk_id) {
                    tex_chunk_missing += 1;
                }
                let e = by_format.entry(h.pixel_format).or_default();
                e.0 += 1;
                e.1.insert(format!("{:.2}", h.bytes_per_pixel()));
                if e.2.len() < 3 {
                    e.2.push(format!("{} {}x{} mips {}", name.rsplit('/').next().unwrap_or(&name), h.width, h.height, h.mip_count));
                }
            }
            Err(e) => fail(format!("texture header: {e:#}"), &name),
        }
    }
    log.line(format!("texture headers read: {tex_ok} of {}  chunk not in loaded bundles: {tex_chunk_missing}", textures.len()));
    check_texture_pixels(log, &fs, &world, &textures)?;
    log.line("--- texture pixel formats: count, bytes per pixel of mip 0, examples");
    for (f, (n, bpp, ex)) in &by_format {
        log.line(format!("  format {f:>3} (0x{f:02x}): {n:>4}  bpp {bpp:?}  {}", ex.join("; ")));
    }
    for (why, (n, ex)) in &fails {
        log.line(format!("  FAIL x{n}: {why}  (e.g. {ex})"));
    }
    Ok(())
}

/// Read each texture's chunk, check its size against the mip sizes, decode a
/// small mip, and test normal maps: their red and green must average near 0.5.
fn check_texture_pixels(log: &mut Log, fs: &GameFs, world: &fb_level::World, textures: &BTreeSet<(usize, usize)>) -> Result<()> {
    let mut chunk_ok = 0usize;
    let mut size_mismatch = 0usize;
    let mut read_fail = 0usize;
    let mut decode_fail = 0usize;
    // Per pixel format: (decoded count, normal-map count, normal maps passing, average R, G, B, A of all).
    let mut per_format: BTreeMap<i32, (usize, usize, usize, [f64; 4])> = BTreeMap::new();
    let mut seen = HashSet::new();
    for &at in textures {
        let name = world.names[at.0].clone();
        let Some(Value::ResourceRef(rid)) = world.get(at).get("Resource") else { continue };
        if !seen.insert(*rid) {
            continue;
        }
        let Ok(h) = world.read_res(fs, *rid).and_then(|d| parse_texture(&d)) else { continue };
        let data = match world.read_chunk(fs, &h.chunk_id) {
            Ok(d) => d,
            Err(_) => {
                read_fail += 1;
                continue;
            }
        };
        chunk_ok += 1;
        let mips = h.mips();
        let total: usize = mips.iter().map(|m| m.size).sum();
        if total != data.len() {
            size_mismatch += 1;
            if size_mismatch <= 2 {
                log.line(format!("  chunk {} bytes, mip sizes add up to {total}: {name}", data.len()));
                log.line(format!("    header: first mip {} mip offsets {:?} chunk size {}", h.first_mip, h.mip_offsets, h.chunk_size));
                if let Some(e) = world.chunks.get(&h.chunk_id) {
                    log.line(format!("    bundle entry: {}", fb_vfs::dbobject::describe(e, 2).replace('\n', " | ")));
                }
                if let Some(sha1) = fs.toc_chunks.get(&h.chunk_id) {
                    let loc = fs.index.get(sha1).map(|l| (l.size, fs.cas_files[l.cas_file as usize].display().to_string()));
                    let n = fs.read(sha1, None, &mut Default::default()).map(|d| d.len());
                    log.line(format!("    .toc sha1 {} cat entry {:?} decompressed {:?} patched: {}", fb_vfs::dbobject::hex(sha1), loc, n.map_err(|e| format!("{e:#}")), fs.patches.contains_key(sha1)));
                }
            }
        }
        let Some(fmt) = block_format(h.pixel_format) else { continue };
        // Decode the first mip that is 256 pixels wide or smaller.
        let Some(m) = mips.iter().find(|m| m.width <= 256 && m.width >= 4 && m.height >= 4) else { continue };
        let Some(src) = data.get(m.offset..m.offset + m.size) else {
            decode_fail += 1;
            continue;
        };
        let (w, hgt) = (m.width as usize, m.height as usize);
        let mut px = vec![0u32; w * hgt];
        let r = match fmt {
            BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(src, w, hgt, &mut px),
            BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(src, w, hgt, &mut px),
            BlockFormat::Bc5 => texture2ddecoder::decode_bc5(src, w, hgt, &mut px),
            BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(src, w, hgt, &mut px),
        };
        if r.is_err() {
            decode_fail += 1;
            continue;
        }
        // Pixels are stored as bytes [b, g, r, a].
        let mut sum = [0f64; 4];
        for p in &px {
            let b = p.to_le_bytes();
            sum[0] += b[2] as f64;
            sum[1] += b[1] as f64;
            sum[2] += b[0] as f64;
            sum[3] += b[3] as f64;
        }
        let avg = sum.map(|s| s / px.len() as f64 / 255.0);
        let e = per_format.entry(h.pixel_format).or_default();
        e.0 += 1;
        for i in 0..4 {
            e.3[i] += avg[i];
        }
        let leaf = name.rsplit('/').next().unwrap_or("");
        let is_normal = leaf.ends_with("_n") || leaf.ends_with("_nm") || leaf.ends_with("_nmo") || leaf.ends_with("_nrm");
        if is_normal {
            e.1 += 1;
            if (avg[0] - 0.5).abs() < 0.1 && (avg[1] - 0.5).abs() < 0.1 {
                e.2 += 1;
            } else if e.1 - e.2 <= 2 {
                log.line(format!("  normal map not centred: format {} avg rgba {:.2?} {name}", h.pixel_format, avg));
            }
        }
    }
    log.line(format!(
        "texture chunks read: {chunk_ok}  read failures: {read_fail}  size mismatches: {size_mismatch}  decode failures: {decode_fail}"
    ));
    log.line("--- decoded textures per pixel format: count, normal maps passing the centre test, average rgba");
    for (f, (n, normals, pass, sum)) in &per_format {
        let avg: Vec<String> = sum.iter().map(|s| format!("{:.2}", s / *n as f64)).collect();
        log.line(format!("  format {f}: {n} decoded, normal maps {pass} of {normals} centred, avg rgba [{}]", avg.join(", ")));
    }
    Ok(())
}

/// Write one mesh (LOD 0) as OBJ + MTL, with each material's BaseColor texture as PNG,
/// to extracted/export/. Frostbite is left-handed (x right, y up, z forward); OBJ is
/// right-handed, so z is negated and the triangle winding reversed.
pub fn cmd_export_mesh(log: &mut Log, root: &std::path::Path, superbundle: &str, mesh_name: &str) -> Result<()> {
    use anyhow::Context;
    use std::fmt::Write as _;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let part = world.partition_by_name(mesh_name).with_context(|| format!("mesh {mesh_name} not found"))?;
    let asset = world.get((part, 0));
    let Some(Value::ResourceRef(rid)) = asset.get("MeshSetResource") else { anyhow::bail!("not a mesh asset") };
    let ms = parse_meshset(&world.read_res(&fs, *rid)?)?;
    let lod = ms.lods.first().context("no LODs")?;
    let buf = world.read_chunk(&fs, &lod.chunk_id.context("inline mesh data not supported yet")?)?;
    let g = decode_lod(lod, &buf, &mut DecodeStats::default())?;

    let leaf = mesh_name.rsplit('/').next().unwrap_or(mesh_name).to_lowercase();
    let dir = std::path::PathBuf::from("extracted").join("export");
    std::fs::create_dir_all(&dir)?;

    // Materials: index -> BaseColor texture asset.
    let materials: Vec<Option<(usize, usize)>> = match asset.get("Materials") {
        Some(Value::Array { items, .. }) => items
            .iter()
            .map(|m| {
                let Value::Pointer(p) = m else { return None };
                let mat = world.resolve(part, p)?;
                let Some(Value::Struct(sh)) = world.get(mat).get("Shader") else { return None };
                let Some(Value::Array { items: tps, .. }) = sh.get("TextureParameters") else { return None };
                tps.iter()
                    .filter_map(|tp| {
                        let Value::Struct(tp) = tp else { return None };
                        let Some(Value::String(n)) = tp.get("ParameterName") else { return None };
                        let rank = fb_res::material::base_color_rank(n)?;
                        let Some(Value::Pointer(t)) = tp.get("Value") else { return None };
                        Some((rank, world.resolve(mat.0, t)?))
                    })
                    .min_by_key(|(rank, _)| *rank)
                    .map(|(_, at)| at)
            })
            .collect(),
        _ => vec![],
    };

    let mut mtl = String::new();
    for (i, tex) in materials.iter().enumerate() {
        let _ = writeln!(mtl, "newmtl mat{i}\nKd 1 1 1");
        if let Some(tex) = tex {
            match export_texture_png(&fs, &world, *tex, &dir.join(format!("{leaf}_mat{i}.png"))) {
                Ok((w, h)) => {
                    let _ = writeln!(mtl, "map_Kd {leaf}_mat{i}.png");
                    log.line(format!("  material {i}: BaseColor {} ({w}x{h})", world.names[tex.0]));
                }
                Err(e) => log.line(format!("  material {i}: texture FAIL {e:#}")),
            }
        }
    }
    std::fs::write(dir.join(format!("{leaf}.mtl")), mtl)?;

    let mut obj = format!("# exported by bf1_probe from {mesh_name}\nmtllib {leaf}.mtl\n");
    for p in &g.positions {
        let _ = writeln!(obj, "v {} {} {}", p[0], p[1], -p[2]);
    }
    for t in &g.uvs {
        let _ = writeln!(obj, "vt {} {}", t[0], 1.0 - t[1]);
    }
    for n in &g.normals {
        let _ = writeln!(obj, "vn {} {} {}", n[0], n[1], -n[2]);
    }
    let (has_uv, has_n) = (!g.uvs.is_empty(), !g.normals.is_empty());
    for s in &g.submeshes {
        let _ = writeln!(obj, "usemtl mat{}", s.material_id);
        for tri in g.indices[s.first_index..s.first_index + s.index_count].chunks(3) {
            let v = |i: u32| {
                let i = i + 1;
                match (has_uv, has_n) {
                    (true, true) => format!("{i}/{i}/{i}"),
                    (true, false) => format!("{i}/{i}"),
                    (false, true) => format!("{i}//{i}"),
                    (false, false) => format!("{i}"),
                }
            };
            // Reverse the winding because z is mirrored.
            let _ = writeln!(obj, "f {} {} {}", v(tri[0]), v(tri[2]), v(tri[1]));
        }
    }
    let path = dir.join(format!("{leaf}.obj"));
    std::fs::write(&path, obj)?;
    log.line(format!(
        "wrote {} ({} vertices, {} triangles, {} submeshes)",
        path.display(),
        g.positions.len(),
        g.indices.len() / 3,
        g.submeshes.len()
    ));
    Ok(())
}

/// Decode the largest mip of a texture asset to an RGBA PNG.
fn export_texture_png(fs: &GameFs, world: &fb_level::World, tex: (usize, usize), path: &std::path::Path) -> Result<(u32, u32)> {
    use anyhow::Context;
    let Some(Value::ResourceRef(rid)) = world.get(tex).get("Resource") else { anyhow::bail!("texture without Resource") };
    let h = parse_texture(&world.read_res(fs, *rid)?)?;
    let data = world.read_chunk(fs, &h.chunk_id)?;
    let m = *h.mips().first().context("no mips")?;
    let fmt = block_format(h.pixel_format).with_context(|| format!("unknown pixel format {}", h.pixel_format))?;
    let (w, hgt) = (m.width as usize, m.height as usize);
    let src = data.get(m.offset..m.offset + m.size).context("mip past end of chunk")?;
    let mut px = vec![0u32; w * hgt];
    let r = match fmt {
        BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(src, w, hgt, &mut px),
        BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(src, w, hgt, &mut px),
        BlockFormat::Bc5 => texture2ddecoder::decode_bc5(src, w, hgt, &mut px),
        BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(src, w, hgt, &mut px),
    };
    r.map_err(|e| anyhow::anyhow!("decode: {e}"))?;
    let rgba: Vec<u8> = px.iter().flat_map(|p| {
        let b = p.to_le_bytes();
        // Colour textures keep smoothness in alpha; write it opaque so the colour is visible.
        [b[2], b[1], b[0], 255]
    }).collect();
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), m.width, m.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok((m.width, m.height))
}

/// `texture-formats <superbundle> <texture asset>`: read a 4-byte-per-pixel
/// texture and print value ranges for several possible pixel layouts.
/// Per-channel statistics of a block-compressed texture (first mip of 256
/// pixels or less): to tell what each channel holds (normal X/Y centred on
/// 0.5, roughness, height, ...). Also: the normal-map check (R and G as X and
/// Y in -1..1: share of pixels with X^2 + Y^2 <= 1.05) and the correlation
/// between channels.
fn log_block_channels(log: &mut Log, h: &fb_res::texture::TextureHeader, fmt: BlockFormat, data: &[u8]) {
    let Some(m) = h.mips().into_iter().find(|m| m.width <= 256 && m.width >= 4 && m.height >= 4) else { return };
    let Some(src) = data.get(m.offset..m.offset + m.size) else { return };
    let (w, hgt) = (m.width as usize, m.height as usize);
    let mut px = vec![0u32; w * hgt];
    let ok = match fmt {
        BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(src, w, hgt, &mut px),
        BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(src, w, hgt, &mut px),
        BlockFormat::Bc5 => texture2ddecoder::decode_bc5(src, w, hgt, &mut px),
        BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(src, w, hgt, &mut px),
    };
    if ok.is_err() {
        log.line("  channel stats: decode failed");
        return;
    }
    // Decoder bytes are [b, g, r, a].
    let v: Vec<[f64; 4]> = px.iter().map(|p| { let b = p.to_le_bytes(); [b[2] as f64 / 255.0, b[1] as f64 / 255.0, b[0] as f64 / 255.0, b[3] as f64 / 255.0] }).collect();
    let n = v.len() as f64;
    let mean: [f64; 4] = std::array::from_fn(|c| v.iter().map(|p| p[c]).sum::<f64>() / n);
    let sd: [f64; 4] = std::array::from_fn(|c| (v.iter().map(|p| (p[c] - mean[c]).powi(2)).sum::<f64>() / n).sqrt());
    let lo: [f64; 4] = std::array::from_fn(|c| v.iter().map(|p| p[c]).fold(f64::MAX, f64::min));
    let hi: [f64; 4] = std::array::from_fn(|c| v.iter().map(|p| p[c]).fold(f64::MIN, f64::max));
    let unit = v.iter().filter(|p| { let (x, y) = (p[0] * 2.0 - 1.0, p[1] * 2.0 - 1.0); x * x + y * y <= 1.05 }).count() as f64 / n;
    let corr = |a: usize, b: usize| {
        let c = v.iter().map(|p| (p[a] - mean[a]) * (p[b] - mean[b])).sum::<f64>() / n;
        c / (sd[a] * sd[b]).max(1e-9)
    };
    let f = |x: [f64; 4]| format!("[{:.3}, {:.3}, {:.3}, {:.3}]", x[0], x[1], x[2], x[3]);
    log.line(format!("  {:?} mip {}x{}: mean {} sd {} min {} max {}", fmt, w, hgt, f(mean), f(sd), f(lo), f(hi)));
    log.line(format!(
        "  R,G as normal X,Y: {:.1}% inside the unit circle; correlation RG {:.2} RB {:.2} RA {:.2} GB {:.2} GA {:.2} BA {:.2}",
        unit * 100.0,
        corr(0, 1),
        corr(0, 2),
        corr(0, 3),
        corr(1, 2),
        corr(1, 3),
        corr(2, 3)
    ));
    // If alpha is height, the normal leans away from the uphill side: R should
    // follow -d(alpha)/dx. For G the sign tells the convention: G following
    // -d(alpha)/d(row) (rows go down the image) means +G points down the image
    // (+v in Direct3D and wgpu texture coordinates).
    let a = |x: usize, y: usize| v[(y % hgt) * w + (x % w)][3];
    let (mut sx, mut sy, mut sxx, mut syy, mut rr, mut gg) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for y in 0..hgt {
        for x in 0..w {
            let dx = -(a(x + 1, y) - a(x + w - 1, y));
            let dy = -(a(x, y + 1) - a(x, y + hgt - 1));
            let (r, g) = (v[y * w + x][0] - mean[0], v[y * w + x][1] - mean[1]);
            sx += dx * r;
            sy += dy * g;
            sxx += dx * dx;
            syy += dy * dy;
            rr += r * r;
            gg += g * g;
        }
    }
    let rx = sx / (sxx * rr).sqrt().max(1e-9);
    let gy = sy / (syy * gg).sqrt().max(1e-9);
    log.line(format!("  correlation of R with -d(alpha)/dx {rx:.2}, of G with -d(alpha)/d(row) {gy:.2}"));
}

/// Decode a block-compressed texture at the first mip of `size` pixels wide
/// or less: (width, height, pixels as [r, g, b, a] in 0..1).
fn decode_small(fs: &GameFs, world: &fb_level::World, name: &str, size: u32) -> Result<(usize, usize, Vec<[f64; 4]>)> {
    use anyhow::Context;
    let part = world.partition_by_name(name).with_context(|| format!("{name} not found"))?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("Resource") else { anyhow::bail!("{name}: no Resource") };
    let h = parse_texture(&world.read_res(fs, *rid)?)?;
    let fmt = block_format(h.pixel_format).with_context(|| format!("{name}: pixel format {}", h.pixel_format))?;
    let data = world.read_chunk(fs, &h.chunk_id)?;
    let m = h.mips().into_iter().find(|m| m.width <= size && m.width >= 4 && m.height >= 4).context("no small mip")?;
    let src = data.get(m.offset..m.offset + m.size).context("mip past end")?;
    let (w, hgt) = (m.width as usize, m.height as usize);
    let mut px = vec![0u32; w * hgt];
    match fmt {
        BlockFormat::Bc1 { .. } => texture2ddecoder::decode_bc1(src, w, hgt, &mut px),
        BlockFormat::Bc3 { .. } => texture2ddecoder::decode_bc3(src, w, hgt, &mut px),
        BlockFormat::Bc5 => texture2ddecoder::decode_bc5(src, w, hgt, &mut px),
        BlockFormat::Bc7 { .. } => texture2ddecoder::decode_bc7(src, w, hgt, &mut px),
    }
    .map_err(|e| anyhow::anyhow!("{name}: decode: {e}"))?;
    // Decoder bytes are [b, g, r, a].
    Ok((w, hgt, px.iter().map(|p| { let b = p.to_le_bytes(); [b[2] as f64 / 255.0, b[1] as f64 / 255.0, b[0] as f64 / 255.0, b[3] as f64 / 255.0] }).collect()))
}

/// Mean colour of a texture as sRGB bytes (from its 64-pixel mip; the
/// decoder returns the stored bytes, which are sRGB for colour textures).
pub fn texture_mean_srgb(fs: &GameFs, world: &fb_level::World, name: &str) -> Result<[f64; 3]> {
    let (_, _, px) = decode_small(fs, world, name, 64)?;
    let n = px.len() as f64;
    Ok(std::array::from_fn(|c| px.iter().map(|p| p[c]).sum::<f64>() / n * 255.0))
}

/// `texture-png <superbundle> <texture> [width]`: the first mip of `width`
/// pixels or less (default 512) as PNG files in `extracted/export` (git
/// ignores it): the colour picture and one grey picture per channel. Per
/// channel it logs the value spread and the patch size: the shift (in
/// texels) at which the channel's autocorrelation falls under 0.5.
pub fn cmd_texture_png(log: &mut Log, root: &std::path::Path, superbundle: &str, name: &str, width: u32) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let (w, h, px) = decode_small(&fs, &world, name, width)?;
    let leaf = name.rsplit('/').next().unwrap_or("texture");
    let dir = std::path::PathBuf::from("extracted").join("export");
    std::fs::create_dir_all(&dir)?;
    let write = |path: &std::path::Path, colour: png::ColorType, bytes: &[u8]| -> Result<()> {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
        enc.set_color(colour);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(bytes)?;
        Ok(())
    };
    let byte = |v: f64| (v * 255.0).round() as u8;
    let rgb: Vec<u8> = px.iter().flat_map(|p| [byte(p[0]), byte(p[1]), byte(p[2])]).collect();
    let path = dir.join(format!("{leaf}_{w}.png"));
    write(&path, png::ColorType::Rgb, &rgb)?;
    log.line(format!("{name}: mip {w}x{h}, wrote {}", path.display()));
    for (c, label) in ["red", "green", "blue", "alpha"].iter().enumerate() {
        let v: Vec<f64> = px.iter().map(|p| p[c]).collect();
        let path = dir.join(format!("{leaf}_{w}_{label}.png"));
        write(&path, png::ColorType::Grayscale, &v.iter().map(|&x| byte(x)).collect::<Vec<u8>>())?;
        let n = v.len() as f64;
        let mean = v.iter().sum::<f64>() / n;
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        if var < 1e-8 {
            log.line(format!("  {label}: constant {mean:.3}"));
            continue;
        }
        // Autocorrelation for a shift (dx, dy), wrapping at the edges (the texture repeats).
        let ac = |dx: usize, dy: usize| -> f64 {
            let mut s = 0.0;
            for y in 0..h {
                for x in 0..w {
                    s += (v[y * w + x] - mean) * (v[((y + dy) % h) * w + (x + dx) % w] - mean);
                }
            }
            s / (n * var)
        };
        let half_x = (1..w / 2).find(|&d| ac(d, 0) < 0.5).unwrap_or(w / 2);
        let half_y = (1..h / 2).find(|&d| ac(0, d) < 0.5).unwrap_or(h / 2);
        let step = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| (v[y * w + x] - v[y * w + (x + 1) % w]).abs()).sum::<f64>() / n;
        let share = |f: &dyn Fn(f64) -> bool| 100.0 * v.iter().filter(|&&x| f(x)).count() as f64 / n;
        log.line(format!(
            "  {label}: mean {mean:.3}, sd {:.3}, under 0.1 {:.1}%, over 0.9 {:.1}%, mean step to the next texel {step:.3}; patch size (autocorrelation under 0.5) x {half_x}, y {half_y} texels of {w} ({:.1}% of the width); wrote {}",
            var.sqrt(),
            share(&|x| x < 0.1),
            share(&|x| x > 0.9),
            100.0 * half_x as f64 / w as f64,
            path.display()
        ));
    }
    Ok(())
}

/// `texture-compare <superbundle> <texture A> <channel A> <texture B> <channel B>`:
/// correlation between one channel of each texture (channels r, g, b, a),
/// both decoded at 64 pixels wide (B is resampled to A's size). A strong
/// correlation means both textures use the same layout (texture coordinates).
pub fn cmd_texture_compare(log: &mut Log, root: &std::path::Path, superbundle: &str, a: &str, ca: &str, b: &str, cb: &str) -> Result<()> {
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let ch = |c: &str| "rgba".find(c).ok_or_else(|| anyhow::anyhow!("channel must be r, g, b or a"));
    let (ia, ib) = (ch(ca)?, ch(cb)?);
    let (wa, ha, pa) = decode_small(&fs, &world, a, 64)?;
    let (wb, hb, pb) = decode_small(&fs, &world, b, 64)?;
    let va: Vec<f64> = pa.iter().map(|p| p[ia]).collect();
    let vb: Vec<f64> = (0..ha).flat_map(|y| (0..wa).map(move |x| (x, y))).map(|(x, y)| pb[(y * hb / ha) * wb + x * wb / wa][ib]).collect();
    let n = va.len() as f64;
    let (ma, mb) = (va.iter().sum::<f64>() / n, vb.iter().sum::<f64>() / n);
    let cov = va.iter().zip(&vb).map(|(x, y)| (x - ma) * (y - mb)).sum::<f64>() / n;
    let sa = (va.iter().map(|x| (x - ma).powi(2)).sum::<f64>() / n).sqrt();
    let sb = (vb.iter().map(|y| (y - mb).powi(2)).sum::<f64>() / n).sqrt();
    // Also: the mean of B where A is low (under 0.5) and where A is high.
    let (mut lo, mut nlo, mut hi, mut nhi) = (0.0, 0.0, 0.0, 0.0);
    for (x, y) in va.iter().zip(&vb) {
        if *x < 0.5 {
            lo += y;
            nlo += 1.0;
        } else {
            hi += y;
            nhi += 1.0;
        }
    }
    log.line(format!(
        "{a} ({ca}, {wa}x{ha}) against {b} ({cb}, {wb}x{hb}): correlation {:.2}; mean of B where A < 0.5: {:.3} ({:.0}% of pixels), where A >= 0.5: {:.3}",
        cov / (sa * sb).max(1e-9),
        lo / f64::max(nlo, 1.0),
        100.0 * nlo / n,
        hi / f64::max(nhi, 1.0)
    ));
    Ok(())
}

/// `mesh-elements <superbundle> <mesh>`: the vertex elements of each section
/// of LOD 0 (usage, format, offset, stream), and for texture coordinate
/// elements (usage 0x20 to 0x2f) the range of u and v and the median metres
/// per texture repeat (mesh units per UV unit along triangle edges).
/// `mesh-dump <superbundle> <mesh> <file>`: every vertex element of LOD 0
/// as text, for study: lines "s <section> <material id> <vertices>
/// <triangles>", "v <section> <usage>=<values> ..." and "f <section> a b c"
/// (indices inside the section).
pub fn cmd_mesh_dump(log: &mut Log, root: &std::path::Path, superbundle: &str, mesh_name: &str, path: &str) -> Result<()> {
    use anyhow::Context;
    use fb_res::mesh::{format_size, read_element};
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let part = world.partition_by_name(mesh_name).with_context(|| format!("{mesh_name} not found"))?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("MeshSetResource") else { anyhow::bail!("no MeshSetResource") };
    let ms = parse_meshset(&world.read_res(&fs, *rid)?)?;
    let lod = ms.lods.first().context("no LODs")?;
    let buffer = world.read_chunk(&fs, &lod.chunk_id.context("inline mesh data")?)?;
    let index_size = if lod.index_format == fb_res::mesh::INDEX_FORMAT_U16 { 2 } else { 4 };
    let ib = &buffer[lod.vertex_buffer_size as usize..];
    let mut out = String::new();
    for (si, s) in lod.sections.iter().enumerate() {
        let d = &s.declaration;
        out.push_str(&format!("s {si} {} {} {}\n", s.material_id, s.vertex_count, s.primitive_count));
        let base = |stream: u8| -> usize { s.vertex_offset as usize + d.stream_strides.iter().take(stream as usize).map(|&st| st as usize * s.vertex_count as usize).sum::<usize>() };
        let stride = |stream: u8| -> usize { d.stream_strides.get(stream as usize).copied().unwrap_or(s.stride) as usize };
        for v in 0..s.vertex_count as usize {
            out.push_str(&format!("v {si}"));
            for e in &d.elements {
                let at = base(e.stream) + v * stride(e.stream) + e.offset as usize;
                let vals = format_size(e.format).and_then(|n| buffer.get(at..at + n)).and_then(|b| read_element(e.format, b));
                match vals {
                    Some(x) => out.push_str(&format!(" {:02x}={},{},{},{}", e.usage, x[0], x[1], x[2], x[3])),
                    None => out.push_str(&format!(" {:02x}=?", e.usage)),
                }
            }
            out.push('\n');
        }
        for t in 0..s.primitive_count as usize {
            let idx = |k: usize| -> Option<u32> {
                let at = (s.start_index as usize + t * 3 + k) * index_size;
                let b = ib.get(at..at + index_size)?;
                Some(if index_size == 2 { u16::from_le_bytes([b[0], b[1]]) as u32 } else { u32::from_le_bytes(b.try_into().ok()?) })
            };
            if let (Some(a), Some(b), Some(c)) = (idx(0), idx(1), idx(2)) {
                out.push_str(&format!("f {si} {a} {b} {c}\n"));
            }
        }
    }
    std::fs::write(path, out)?;
    log.line(format!("{mesh_name}: {} sections written to {path}", lod.sections.len()));
    Ok(())
}

pub fn cmd_mesh_elements(log: &mut Log, root: &std::path::Path, superbundle: &str, mesh_name: &str, samples: &[(u8, String)], alpha: Option<&str>) -> Result<()> {
    use anyhow::Context;
    use fb_res::mesh::{format_size, read_element, usage};
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let part = world.partition_by_name(mesh_name).with_context(|| format!("{mesh_name} not found"))?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("MeshSetResource") else { anyhow::bail!("no MeshSetResource") };
    let ms = parse_meshset(&world.read_res(&fs, *rid)?)?;
    let lod = ms.lods.first().context("no LODs")?;
    let buffer = world.read_chunk(&fs, &lod.chunk_id.context("inline mesh data")?)?;
    let index_size = if lod.index_format == fb_res::mesh::INDEX_FORMAT_U16 { 2 } else { 4 };
    let ib = &buffer[lod.vertex_buffer_size as usize..];
    log.line(format!("{mesh_name}: {} sections; categories {:?}", lod.sections.len(), lod.categories));
    let alpha_tex = match alpha.map(|t| (t, decode_small(&fs, &world, t, 256))) {
        Some((t, Ok((w, h, px)))) => Some((t.to_string(), w, h, px)),
        Some((t, Err(e))) => {
            log.line(format!("  alpha texture {t}: {e:#}"));
            None
        }
        None => None,
    };
    // Textures to sample at the collapse points of a set (`--sample 0x21=<texture>`).
    let sampled: Vec<(u8, String, usize, usize, Vec<[f64; 4]>)> = samples
        .iter()
        .filter_map(|(u, t)| match decode_small(&fs, &world, t, 256) {
            Ok((w, h, px)) => Some((*u, t.clone(), w, h, px)),
            Err(e) => {
                log.line(format!("  sample texture {t}: {e:#}"));
                None
            }
        })
        .collect();
    for (si, s) in lod.sections.iter().enumerate() {
        let d = &s.declaration;
        let list: Vec<String> = d.elements.iter().map(|e| format!("0x{:02x}/fmt 0x{:02x}/off {}/stream {}", e.usage, e.format, e.offset, e.stream)).collect();
        log.line(format!("  section {si}: material {} '{}', {} vertices, {} triangles, strides {:?}: {}", s.material_id, s.material_name, s.vertex_count, s.primitive_count, d.stream_strides, list.join(", ")));
        let stream_base = |stream: u8| -> usize {
            let before: usize = d.stream_strides.iter().take(stream as usize).map(|&st| st as usize * s.vertex_count as usize).sum();
            s.vertex_offset as usize + before
        };
        let stride_of = |stream: u8| -> usize { d.stream_strides.get(stream as usize).copied().unwrap_or(s.stride) as usize };
        let read = |e: &fb_res::mesh::Element, v: usize| -> Option<[f32; 4]> {
            let at = stream_base(e.stream) + v * stride_of(e.stream) + e.offset as usize;
            read_element(e.format, buffer.get(at..at + format_size(e.format)?)?)
        };
        let Some(pos) = d.find(usage::POSITION) else { continue };
        let tris: Vec<[usize; 3]> = (0..s.primitive_count as usize)
            .filter_map(|t| {
                let idx = |k: usize| -> Option<usize> {
                    let at = (s.start_index as usize + t * 3 + k) * index_size;
                    Some(if index_size == 2 { u16::from_le_bytes(ib.get(at..at + 2)?.try_into().ok()?) as usize } else { u32::from_le_bytes(ib.get(at..at + 4)?.try_into().ok()?) as usize })
                };
                Some([idx(0)?, idx(1)?, idx(2)?])
            })
            .collect();
        // Per texture coordinate set: which triangles are collapsed.
        let mut flags: Vec<(u8, Vec<bool>)> = Vec::new();
        let tri_area: Vec<f64> = tris
            .iter()
            .map(|t| {
                let (Some(a), Some(b), Some(c)) = (read(pos, t[0]), read(pos, t[1]), read(pos, t[2])) else { return 0.0 };
                let (ab, ac) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
                let cr = [ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0]];
                0.5 * ((cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]) as f64).sqrt()
            })
            .collect();
        for e in d.elements.iter().filter(|e| (0x20..0x30).contains(&e.usage)) {
            let uv: Vec<[f32; 4]> = (0..s.vertex_count as usize).map(|v| read(e, v).unwrap_or([f32::NAN; 4])).collect();
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for t in &uv {
                for c in 0..2 {
                    lo[c] = lo[c].min(t[c]);
                    hi[c] = hi[c].max(t[c]);
                }
            }
            let mut per: Vec<f32> = Vec::new();
            for t in &tris {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    let (Some(pa), Some(pb)) = (read(pos, a), read(pos, b)) else { continue };
                    let dw = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    let du = ((uv[a][0] - uv[b][0]).powi(2) + (uv[a][1] - uv[b][1]).powi(2)).sqrt();
                    if du > 1e-6 && dw > 1e-4 {
                        per.push(dw / du);
                    }
                }
            }
            per.sort_by(f32::total_cmp);
            let median = per.get(per.len() / 2).copied().unwrap_or(f32::NAN);
            // Share of the surface (by world area) whose UV area is collapsed:
            // under 1% of the UV area that the median scale would give.
            let (mut area, mut collapsed) = (0f64, 0f64);
            let mut flag = Vec::with_capacity(tris.len());
            for (k, t) in tris.iter().enumerate() {
                let wa = tri_area[k];
                let (ua, ub, uc) = (uv[t[0]], uv[t[1]], uv[t[2]]);
                let uva = 0.5 * (((ub[0] - ua[0]) * (uc[1] - ua[1]) - (uc[0] - ua[0]) * (ub[1] - ua[1])) as f64).abs();
                let expected = wa / (median as f64 * median as f64);
                area += wa;
                let c = uva < 0.01 * expected;
                if c {
                    collapsed += wa;
                }
                flag.push(c);
            }
            // Texture colour at the faces collapsed in this set (area-weighted mean
            // of the texture at each face's UV centre), and the texture's mean.
            for (u, name, w, h, px) in sampled.iter().filter(|(u, ..)| *u == e.usage) {
                let at = |c: [f32; 2]| {
                    let x = ((c[0].rem_euclid(1.0) * *w as f32) as usize).min(w - 1);
                    let y = ((c[1].rem_euclid(1.0) * *h as f32) as usize).min(h - 1);
                    px[y * w + x]
                };
                let (mut sum, mut n) = ([0f64; 4], 0f64);
                for (k, t) in tris.iter().enumerate().filter(|(k, _)| flag[*k]) {
                    let c = [(uv[t[0]][0] + uv[t[1]][0] + uv[t[2]][0]) / 3.0, (uv[t[0]][1] + uv[t[1]][1] + uv[t[2]][1]) / 3.0];
                    let p = at(c);
                    for i in 0..4 {
                        sum[i] += p[i] * tri_area[k];
                    }
                    n += tri_area[k];
                }
                let all: [f64; 3] = std::array::from_fn(|i| px.iter().map(|p| p[i]).sum::<f64>() / px.len() as f64);
                log.line(format!(
                    "      {} at the collapsed faces of 0x{u:02x} (0..1): [{:.3}, {:.3}, {:.3}] alpha {:.3}; texture mean [{:.3}, {:.3}, {:.3}]",
                    name.rsplit('/').next().unwrap_or(name),
                    sum[0] / n.max(1e-9),
                    sum[1] / n.max(1e-9),
                    sum[2] / n.max(1e-9),
                    sum[3] / n.max(1e-9),
                    all[0],
                    all[1],
                    all[2]
                ));
            }
            // Alpha check (`--alpha <texture>`): sampled with this set, how much
            // does the alpha change inside one triangle? An atlas sampled with
            // its own coordinates changes little; with other coordinates its
            // pieces land anywhere.
            if let Some((name, w, h, px)) = &alpha_tex {
                let at = |c: [f32; 2]| {
                    let x = ((c[0].rem_euclid(1.0) * *w as f32) as usize).min(w - 1);
                    let y = ((c[1].rem_euclid(1.0) * *h as f32) as usize).min(h - 1);
                    px[y * w + x][3]
                };
                let (mut spread, mut mean, mut n) = (0f64, 0f64, 0f64);
                for (k, t) in tris.iter().enumerate() {
                    let c = [(uv[t[0]][0] + uv[t[1]][0] + uv[t[2]][0]) / 3.0, (uv[t[0]][1] + uv[t[1]][1] + uv[t[2]][1]) / 3.0];
                    let mut vals = vec![at(c)];
                    for &v in t {
                        vals.push(at([c[0] + 0.8 * (uv[v][0] - c[0]), c[1] + 0.8 * (uv[v][1] - c[1])]));
                    }
                    let (lo, hi) = vals.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
                    spread += (hi - lo) * tri_area[k];
                    mean += vals[0] * tri_area[k];
                    n += tri_area[k];
                }
                log.line(format!(
                    "      {} alpha with 0x{:02x}: mean {:.2}, mean change inside a triangle {:.3}",
                    name.rsplit('/').next().unwrap_or(name),
                    e.usage,
                    mean / n.max(1e-9),
                    spread / n.max(1e-9)
                ));
            }
            flags.push((e.usage, flag));
            log.line(format!(
                "    uv 0x{:02x}: u {:.2}..{:.2}, v {:.2}..{:.2}, median {:.2} m per repeat, collapsed on {:.0}% of the surface",
                e.usage,
                lo[0],
                hi[0],
                lo[1],
                hi[1],
                median,
                100.0 * collapsed / area.max(1e-9)
            ));
        }
        // Faces collapsed in the first set (0x21): which other set maps them,
        // and their vertex colour (0x33) against the other faces.
        if let Some((_, first)) = flags.iter().find(|(u, _)| *u == 0x21) {
            let total: f64 = tris.iter().enumerate().filter(|(k, _)| first[*k]).map(|(k, _)| tri_area[k]).sum();
            if total > 0.0 {
                let mapped: Vec<String> = flags
                    .iter()
                    .filter(|(u, _)| *u != 0x21)
                    .map(|(u, f)| {
                        let ok: f64 = (0..tris.len()).filter(|&k| first[k] && !f[k]).map(|k| tri_area[k]).sum();
                        format!("0x{u:02x} {:.0}%", 100.0 * ok / total)
                    })
                    .collect();
                log.line(format!("    faces collapsed in 0x21: mapped normally in {}", mapped.join(", ")));
            }
            if let Some(vc) = d.find(0x33) {
                let mean = |want: bool| {
                    let (mut sum, mut n) = ([0f64; 4], 0f64);
                    for (k, t) in tris.iter().enumerate().filter(|(k, _)| first[*k] == want) {
                        for &v in t {
                            if let Some(c) = read(vc, v) {
                                for i in 0..4 {
                                    sum[i] += c[i] as f64 * tri_area[k];
                                }
                                n += tri_area[k];
                            }
                        }
                    }
                    sum.map(|x| (x / n.max(1e-9)).round())
                };
                log.line(format!("    vertex colour 0x33 (area-weighted mean, 0..255): faces collapsed in 0x21 {:?}, other faces {:?}", mean(true), mean(false)));
            }
        }
    }
    Ok(())
}

pub fn cmd_texture_formats(log: &mut Log, root: &std::path::Path, superbundle: &str, name: &str) -> Result<()> {
    use anyhow::Context;
    let mut fs = GameFs::open(root)?;
    fs.index_toc_chunks()?;
    let fs = fs;
    let world = fb_level::World::load(&fs, &[superbundle])?;
    let part = world.partition_by_name(name).with_context(|| format!("{name} not found"))?;
    let Some(Value::ResourceRef(rid)) = world.get((part, 0)).get("Resource") else { anyhow::bail!("no Resource") };
    let h = parse_texture(&world.read_res(&fs, *rid)?)?;
    let data = world.read_chunk(&fs, &h.chunk_id)?;
    log.line(format!("{name}: format {} type {} {}x{}x{} mips {} chunk {} bytes", h.pixel_format, h.kind, h.width, h.height, h.depth, h.mip_count, data.len()));
    if let Some(fmt) = block_format(h.pixel_format) {
        log_block_channels(log, &h, fmt, &data);
    }
    let words: Vec<u32> = data.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    let stats = |label: &str, v: &dyn Fn(u32) -> [f32; 4]| {
        let (mut lo, mut hi, mut sum) = ([f32::MAX; 4], [f32::MIN; 4], [0f64; 4]);
        for &w in &words {
            let p = v(w);
            for c in 0..4 {
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
                sum[c] += p[c] as f64;
            }
        }
        let n = words.len() as f64;
        format!("{label:<12} min {:?} max {:?} mean [{:.3}, {:.3}, {:.3}, {:.3}]", lo.map(|x| (x * 1000.0).round() / 1000.0), hi.map(|x| (x * 1000.0).round() / 1000.0), sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n)
    };
    log.line(stats("rgba8", &|w| { let b = w.to_le_bytes(); [b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0, b[3] as f32 / 255.0] }));
    log.line(stats("r11g11b10f", &|w| {
        let f11 = |m: u32| { let e = (m >> 6) & 0x1f; let f = m & 0x3f; if e == 0 { f as f32 / 64.0 * 2f32.powi(-14) } else { (1.0 + f as f32 / 64.0) * 2f32.powi(e as i32 - 15) } };
        let f10 = |m: u32| { let e = (m >> 5) & 0x1f; let f = m & 0x1f; if e == 0 { f as f32 / 32.0 * 2f32.powi(-14) } else { (1.0 + f as f32 / 32.0) * 2f32.powi(e as i32 - 15) } };
        [f11(w & 0x7ff), f11((w >> 11) & 0x7ff), f10(w >> 22), 0.0]
    }));
    log.line(stats("rgb9e5", &|w| {
        let e = (w >> 27) as i32;
        let s = 2f32.powi(e - 15 - 9);
        [(w & 0x1ff) as f32 * s, ((w >> 9) & 0x1ff) as f32 * s, ((w >> 18) & 0x1ff) as f32 * s, 0.0]
    }));
    log.line(stats("rgb10a2", &|w| [(w & 0x3ff) as f32 / 1023.0, ((w >> 10) & 0x3ff) as f32 / 1023.0, ((w >> 20) & 0x3ff) as f32 / 1023.0, (w >> 30) as f32 / 3.0]));
    // 3D colour tables (4 bytes per entry): the grey diagonal and the corners, as RGBA8,
    // assuming x = red is fastest, then green, then blue.
    if h.kind == 2 && h.depth > 1 && data.len() as u64 >= h.width as u64 * h.height as u64 * h.depth as u64 * 4 {
        let n = h.width as usize;
        let at = |r: usize, g: usize, b: usize| {
            let i = ((b * n + g) * n + r) * 4;
            [data[i], data[i + 1], data[i + 2], data[i + 3]]
        };
        let diag: Vec<String> = (0..n).step_by((n / 8).max(1)).chain(std::iter::once(n - 1)).map(|i| format!("{i}:{:?}", at(i, i, i))).collect();
        log.line(format!("3D table {n}^3, grey diagonal (input index: output RGBA bytes): {}", diag.join(" ")));
        let dir = std::path::PathBuf::from("extracted").join("export");
        std::fs::create_dir_all(&dir)?;
        let raw = dir.join(format!("{}.bin", name.rsplit('/').next().unwrap_or("table")));
        std::fs::write(&raw, &data)?;
        log.line(format!("wrote {}", raw.display()));
        log.line(format!(
            "corners: red {:?} green {:?} blue {:?} white {:?} black {:?}",
            at(n - 1, 0, 0),
            at(0, n - 1, 0),
            at(0, 0, n - 1),
            at(n - 1, n - 1, n - 1),
            at(0, 0, 0)
        ));
    }
    // Single-channel BC4 decode of the first mip (formats near 62 are single channel guesses).
    if h.depth <= 1 && (data.len() as u64) * 2 >= h.width as u64 * h.height as u64 {
        let (w, hgt) = (h.width as usize, h.height as usize);
        let mut px = vec![0u32; w * hgt];
        if texture2ddecoder::decode_bc4(&data[..w * hgt / 2], w, hgt, &mut px).is_ok() {
            // Decoder writes the value into the red channel (bytes b, g, r, a).
            let v: Vec<u8> = px.iter().map(|p| p.to_le_bytes()[2]).collect();
            let zeros = v.iter().filter(|&&x| x == 0).count();
            let full = v.iter().filter(|&&x| x == 255).count();
            let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
            // Neighbour jumps: share of horizontal neighbours that differ by more than 64.
            let jumps = (0..hgt).flat_map(|y| (0..w - 1).map(move |x| (y, x))).filter(|&(y, x)| (v[y * w + x] as i32 - v[y * w + x + 1] as i32).abs() > 64).count();
            log.line(format!(
                "as BC4: mean {:.1}, zero {:.1}%, 255 {:.1}%, big neighbour jumps {:.2}%",
                mean,
                100.0 * zeros as f64 / v.len() as f64,
                100.0 * full as f64 / v.len() as f64,
                100.0 * jumps as f64 / v.len() as f64
            ));
            let dir = std::path::PathBuf::from("extracted").join("export");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{}_bc4.png", name.rsplit('/').next().unwrap_or("texture")));
            let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
            let mut enc = png::Encoder::new(file, w as u32, hgt as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&v)?;
            log.line(format!("wrote {}", path.display()));
        }
    }
    Ok(())
}
