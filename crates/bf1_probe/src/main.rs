//! bf1_probe: prints and logs what fb_vfs finds in the Battlefield 1 install.
//! Every run writes a copy of its output to logs/probe-<unix time>.log.

mod meshes;
mod terrain;

use anyhow::{bail, Context, Result};
use fb_vfs::{dbobject, read_toc, GameFs};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

struct Log {
    file: std::fs::File,
    pub path: PathBuf,
}

impl Log {
    fn new(command: &str) -> Result<Log> {
        std::fs::create_dir_all("logs")?;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let path = PathBuf::from(format!("logs/probe-{secs}-{command}.log"));
        Ok(Log { file: std::fs::File::create(&path)?, path })
    }

    fn line(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref();
        println!("{text}");
        let _ = writeln!(self.file, "{text}");
    }
}

const USAGE: &str = "usage:
  bf1_probe cats                              index every cas.cat and print counts
  bf1_probe schema-layout                     print the shape of layout.toc
  bf1_probe schema-toc <superbundle>          print the shape of a .toc (Data and Patch)
  bf1_probe schema-sb <superbundle> <data|patch> <offset>
                                              print the shape of the DbObject at an .sb offset
  bf1_probe read-sb <superbundle> [--no-chunks]
                                              read and decompress every file in every bundle
  bf1_probe dump-ebx <superbundle> [--write] [--filter <text>]
                                              decode every EBX; --write puts text in extracted/ebx/
  bf1_probe find-res <superbundle> <text>    list res entries whose name contains <text>
                                              (--save writes them to extracted/res/)
  bf1_probe level <superbundle> <level>       walk a level and list every placed mesh
                                              [--skip rush0,breakthrough0,...] leaves out sub-worlds
  bf1_probe meshes <superbundle> <level> [--skip ...]
                                              decode every placed mesh and read its texture headers
  bf1_probe export-mesh <superbundle> <mesh>  write a mesh as OBJ + PNG to extracted/export/
  bf1_probe terrain <superbundle> <level> [--skip ...]
                                              load the terrain, check it, export heightmap + OBJ
  bf1_probe havok <file>                      print the structure of a saved Havok res
example superbundle: Win32/xpack1/levels/mp_verdun/mp_verdun";

/// The comma-separated list after `--skip`, lower case.
fn skip_list(args: &[String]) -> Vec<String> {
    args.iter()
        .position(|a| a == "--skip")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.split(',').map(|x| x.trim().to_lowercase()).collect())
        .unwrap_or_default()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let mut log = Log::new(command)?;
    let root = fb_vfs::find_game_dir()?;
    log.line(format!("game dir: {}", root.display()));
    let started = Instant::now();

    match command.as_str() {
        "cats" => cmd_cats(&mut log, &root)?,
        "find-res" => {
            let name = args.get(1).context("missing superbundle name")?;
            let text = args.get(2).context("missing search text")?.to_lowercase();
            let save = args.iter().any(|a| a == "--save");
            cmd_find_res(&mut log, &root, name, &text, save)?
        }
        "schema-layout" => {
            let path = root.join("Data").join("layout.toc");
            let (kind, v) = read_toc(&path)?;
            log.line(format!("{} header={kind:?}", path.display()));
            log.line(dbobject::describe(&v, 3));
            let ppath = root.join("Patch").join("layout.toc");
            let (kind, v) = read_toc(&ppath)?;
            log.line(format!("{} header={kind:?}", ppath.display()));
            log.line(dbobject::describe(&v, 3));
        }
        "schema-toc" => {
            let name = args.get(1).context("missing superbundle name")?;
            for patch in [false, true] {
                let fs = GameFs::without_index(&root);
                let path = fs.toc_path(name, patch);
                if !path.is_file() {
                    log.line(format!("{}: not present", path.display()));
                    continue;
                }
                let (kind, v) = read_toc(&path)?;
                log.line(format!("{} header={kind:?}", path.display()));
                log.line(dbobject::describe(&v, 4));
            }
        }
        "schema-sb" => {
            let name = args.get(1).context("missing superbundle name")?;
            let patch = match args.get(2).map(String::as_str) {
                Some("data") => false,
                Some("patch") => true,
                _ => bail!("third argument must be data or patch"),
            };
            let offset: usize = args.get(3).context("missing offset")?.parse()?;
            let fs = GameFs::without_index(&root);
            let sb_path = fs.toc_path(name, patch).with_extension("sb");
            let sb = std::fs::read(&sb_path)?;
            log.line(format!("{} ({} bytes) offset {offset}", sb_path.display(), sb.len()));
            let v = fb_vfs::read_sb_object(&sb, offset)?;
            log.line(dbobject::describe(&v, 3));
        }
        "dump-ebx" => {
            let name = args.get(1).context("missing superbundle name")?;
            let write = args.iter().any(|a| a == "--write");
            let filter = args.iter().position(|a| a == "--filter").and_then(|i| args.get(i + 1)).map(String::as_str);
            cmd_dump_ebx(&mut log, &root, name, write, filter)?
        }
        "level" => {
            let name = args.get(1).context("missing superbundle name")?;
            let level = args.get(2).context("missing level name")?;
            let skip: Vec<String> = args
                .iter()
                .position(|a| a == "--skip")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.split(',').map(|x| x.trim().to_lowercase()).collect())
                .unwrap_or_default();
            cmd_level(&mut log, &root, name, level, &skip)?
        }
        "havok" => cmd_havok(&mut log, args.get(1).context("missing file path")?)?,
        "export-mesh" => {
            let name = args.get(1).context("missing superbundle name")?;
            let mesh = args.get(2).context("missing mesh asset name")?;
            meshes::cmd_export_mesh(&mut log, &root, name, mesh)?
        }
        "terrain-scan" => {
            let name = args.get(1).context("missing superbundle name")?;
            let file = args.get(2).context("missing file path")?;
            terrain::cmd_terrain_scan(&mut log, &root, name, file)?
        }
        "terrain-fit" => {
            let name = args.get(1).context("missing superbundle name")?;
            let level = args.get(2).context("missing level name")?;
            let file = args.get(3).context("missing streaming tree file")?;
            terrain::cmd_terrain_fit(&mut log, &root, name, level, file, &skip_list(&args))?
        }
        "terrain-tiles" => {
            let name = args.get(1).context("missing superbundle name")?;
            let file = args.get(2).context("missing streaming tree file")?;
            terrain::cmd_terrain_tiles(&mut log, &root, name, file)?
        }
        "terrain-table" => terrain::cmd_terrain_table(&mut log, &root, args.get(1).context("missing superbundle name")?)?,
        "terrain-layer" => terrain::cmd_terrain_layer(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-extra" => {
            let recs: Vec<usize> = args.iter().skip(2).filter_map(|a| a.parse().ok()).collect();
            terrain::cmd_terrain_extra(&mut log, &root, args.get(1).context("missing superbundle name")?, &recs)?
        }
        "terrain-colour-offset" => terrain::cmd_terrain_colour_offset(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-node" => terrain::cmd_terrain_node(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-scan-colour" => terrain::cmd_terrain_scan_colour(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-first-layers" => terrain::cmd_terrain_first_layers(&mut log, &root, args.get(1).context("missing superbundle name")?)?,
        "bc3-at" => terrain::cmd_bc3_at(&mut log, args.get(1).context("missing file")?, args.get(2).context("missing offset")?.parse()?, args.get(3).context("missing png path")?)?,
        "terrain-minmax" => terrain::cmd_terrain_minmax(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "image-stats" => cmd_image_stats(&mut log, &args[1..])?,
        "shader-textures" => {
            // shader-textures <superbundle> <level> <shader name>...: texture lists from the level shader database.
            use fb_res::shaderdb::{fb_hash, ShaderDb};
            let sb = args.get(1).context("missing superbundle name")?;
            let level = args.get(2).context("missing level name")?;
            let mut fs = fb_vfs::GameFs::open(&root)?;
            fs.index_toc_chunks()?;
            let world = fb_level::World::load(&fs, &[sb.as_str()])?;
            let db = world.read_res_named(&fs, &format!("{level}/shaderdb"))?;
            for name in &args[3..] {
                match ShaderDb::parse(&db, fb_hash(&name.to_lowercase())) {
                    Ok(sdb) => {
                        let blocks = sdb.blocks_of(name);
                        log.line(format!("{name}: {} permutation blocks", blocks.len()));
                        let mut seen = std::collections::BTreeSet::new();
                        for b in blocks {
                            for (slot, t) in sdb.block_texture_slots(b) {
                                if seen.insert((slot, t.clone())) {
                                    log.line(format!("  slot {slot:3}: {t}"));
                                }
                            }
                        }
                    }
                    Err(e) => log.line(format!("{name}: {e:#}")),
                }
            }
        }
        "mesh-elements" => {
            // Optional: --sample 0x21=<texture> (texture colour at the faces collapsed in that set).
            let mut samples = Vec::new();
            let mut alpha = None;
            let mut it = args.iter().skip(3);
            while let Some(a) = it.next() {
                if a == "--alpha" {
                    alpha = Some(it.next().context("--alpha needs a texture")?.clone());
                } else if a == "--sample" {
                    let v = it.next().context("--sample needs 0xNN=<texture>")?;
                    let (u, t) = v.split_once('=').context("--sample needs 0xNN=<texture>")?;
                    samples.push((u8::from_str_radix(u.trim_start_matches("0x"), 16)?, t.to_string()));
                }
            }
            meshes::cmd_mesh_elements(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing mesh name")?, &samples, alpha.as_deref())?
        }
        "texture-compare" => {
            let arg = |i: usize| args.get(i).map(|s| s.as_str()).context("usage: texture-compare <superbundle> <texture A> <r|g|b|a> <texture B> <r|g|b|a>");
            meshes::cmd_texture_compare(&mut log, &root, arg(1)?, arg(2)?, arg(3)?, arg(4)?, arg(5)?)?
        }
        "terrain-chunks-save-all" => terrain::cmd_terrain_chunks_save_all(&mut log, &root, args.get(1).context("missing superbundle name")?)?,
        "terrain-chunk-save" => terrain::cmd_terrain_chunk_save(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-masks" => terrain::cmd_terrain_masks(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-mask-sheet" => terrain::cmd_terrain_mask_sheet(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "bc3-scan" => terrain::cmd_bc3_scan(&mut log, args.get(1).context("missing file")?, args.get(2).context("missing from")?.parse()?, args.get(3).context("missing to")?.parse()?)?,
        "terrain-mask-edges" => terrain::cmd_terrain_mask_edges(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain-find-table" => terrain::cmd_terrain_find_table(&mut log, &root, args.get(1).context("missing superbundle name")?)?,
        "terrain-combos" => terrain::cmd_terrain_combos(&mut log, args.get(1).context("missing file")?)?,
        "terrain-blocka-map" => terrain::cmd_terrain_blocka_map(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing record")?.parse()?)?,
        "terrain" => {
            let name = args.get(1).context("missing superbundle name")?;
            let level = args.get(2).context("missing level name")?;
            terrain::cmd_terrain(&mut log, &root, name, level, &skip_list(&args))?
        }
        "texture-formats" => meshes::cmd_texture_formats(&mut log, &root, args.get(1).context("missing superbundle name")?, args.get(2).context("missing texture name")?)?,
        "meshes" => {
            let name = args.get(1).context("missing superbundle name")?;
            let level = args.get(2).context("missing level name")?;
            meshes::cmd_meshes(&mut log, &root, name, level, &skip_list(&args))?
        }
        "read-sb" => {
            let name = args.get(1).context("missing superbundle name")?;
            let with_chunks = !args.iter().any(|a| a == "--no-chunks");
            cmd_read_sb(&mut log, &root, name, with_chunks)?
        }
        _ => {
            println!("{USAGE}");
            bail!("unknown command {command}");
        }
    }

    log.line(format!("done in {:.2} s, log: {}", started.elapsed().as_secs_f64(), log.path.display()));
    Ok(())
}

fn cmd_cats(log: &mut Log, root: &std::path::Path) -> Result<()> {
    let fs = GameFs::open(root)?;
    for c in &fs.cats {
        let rel = c.path.strip_prefix(root).unwrap_or(&c.path);
        log.line(format!("cat {:>7} entries {:>5} patches  {}{}", c.entries, c.patch_entries, rel.display(), if c.from_patch { "  [patch]" } else { "" }));
    }
    let total: usize = fs.cats.iter().map(|c| c.entries).sum();
    log.line(format!(
        "cat files: {}  entries: {total}  unique sha1: {}  patch entries: {}  cas files: {}",
        fs.cats.len(),
        fs.index.len(),
        fs.patches.len(),
        fs.cas_files.len()
    ));
    let missing = fs.cas_files.iter().filter(|p| !p.is_file()).count();
    log.line(format!("cas files referenced but missing on disk: {missing}"));
    Ok(())
}

fn cmd_read_sb(log: &mut Log, root: &std::path::Path, name: &str, with_chunks: bool) -> Result<()> {
    use fb_vfs::dbobject::DbValue;
    use fb_vfs::payload::BlockStats;
    use fb_vfs::superbundle::{read_entry, EntryKind, Superbundle};
    use std::collections::{BTreeMap, HashSet};

    let fs = GameFs::open(root)?;
    log.line(format!("indexed {} sha1 from {} cat files", fs.index.len(), fs.cats.len()));
    let sb = Superbundle::open(&fs, name)?;
    log.line(format!("superbundle {name}: {} bundles", sb.bundles.len()));

    let mut bundle_fields: BTreeMap<String, usize> = BTreeMap::new();
    let mut entry_fields: BTreeMap<(EntryKind, String), usize> = BTreeMap::new();
    let mut seen: HashSet<[u8; 20]> = HashSet::new();
    let mut ok: BTreeMap<EntryKind, (usize, u64)> = BTreeMap::new();
    let mut skipped_dupes = 0usize;
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut magics: BTreeMap<(EntryKind, String), usize> = BTreeMap::new();
    let mut idata_checked = (0usize, 0usize);
    let mut stats = BlockStats::default();

    for b in &sb.bundles {
        let bundle = sb.read_bundle(b)?;
        let mut counts = Vec::new();
        if let DbValue::Object(fields) = &bundle {
            for (n, _) in fields {
                *bundle_fields.entry(n.clone()).or_default() += 1;
            }
        }
        for (key, kind) in [("ebx", EntryKind::Ebx), ("dbx", EntryKind::Dbx), ("res", EntryKind::Res), ("chunks", EntryKind::Chunk)] {
            let list = bundle.get(key).and_then(DbValue::as_list).unwrap_or(&[]);
            counts.push(format!("{key}={}", list.len()));
            for entry in list {
                if let DbValue::Object(fields) = entry {
                    for (n, _) in fields {
                        *entry_fields.entry((kind, n.clone())).or_default() += 1;
                    }
                }
                if kind == EntryKind::Chunk && !with_chunks {
                    continue;
                }
                let key_sha = entry.get("sha1").and_then(DbValue::as_sha1).copied().unwrap_or([0; 20]);
                if !seen.insert(key_sha) {
                    skipped_dupes += 1;
                    continue;
                }
                match read_entry(&fs, kind, entry, &mut stats) {
                    Ok(data) => {
                        let e = ok.entry(kind).or_default();
                        e.0 += 1;
                        e.1 += data.len() as u64;
                        let magic = fb_vfs::dbobject::hex(&data[..data.len().min(4)]);
                        *magics.entry((kind, magic)).or_default() += 1;
                        if let Some(DbValue::Blob(idata)) = entry.get("idata") {
                            let mut s = BlockStats::default();
                            let same = fb_vfs::payload::decompress(idata, None, &mut s).map(|d| d == data).unwrap_or(false);
                            if same { idata_checked.0 += 1 } else { idata_checked.1 += 1 }
                        }
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        let short: String = msg.chars().take(70).collect();
                        let name = entry.get("name").and_then(DbValue::as_str).unwrap_or("?").to_string();
                        let f = failures.entry(format!("{kind:?}: {short}")).or_insert((0, name));
                        f.0 += 1;
                    }
                }
            }
        }
        log.line(format!(
            "bundle {:<55} {:<5} off={:<9} {}{}",
            b.id,
            if b.from_patch { "patch" } else { "data" },
            b.offset,
            counts.join(" "),
            if b.extra_flags.is_empty() { String::new() } else { format!(" flags={:?}", b.extra_flags) }
        ));
    }

    log.line("--- bundle fields (how many bundles have each)");
    for (n, c) in &bundle_fields {
        log.line(format!("  {n}: {c}"));
    }
    log.line("--- entry fields");
    for ((k, n), c) in &entry_fields {
        log.line(format!("  {k:?}.{n}: {c}"));
    }
    log.line("--- read results (unique sha1)");
    for (k, (n, bytes)) in &ok {
        log.line(format!("  ok {k:?}: {n} files, {:.1} MB", *bytes as f64 / 1e6));
    }
    log.line(format!("  duplicates skipped: {skipped_dupes}"));
    log.line(format!("  res idata matches cas data: {} yes, {} no", idata_checked.0, idata_checked.1));
    for (msg, (n, example)) in &failures {
        log.line(format!("  FAIL x{n}: {msg}  (e.g. {example})"));
    }
    log.line("--- compression types (blocks)");
    for (t, n) in &stats.by_type {
        log.line(format!("  type 0x{t:02x}: {n}"));
    }
    log.line(format!("  dictionary-flag blocks: {}", stats.dictionary_blocks));
    log.line(format!("  files rebuilt from base + delta: {}", stats.delta_rebuilds));
    log.line("--- first 4 bytes of decompressed files (top 12)");
    let mut m: Vec<_> = magics.into_iter().collect();
    m.sort_by(|a, b| b.1.cmp(&a.1));
    for ((k, magic), n) in m.into_iter().take(12) {
        log.line(format!("  {k:?} {magic}: {n}"));
    }
    Ok(())
}

struct ProbeNames {
    partitions: std::collections::HashMap<[u8; 16], String>,
    res: std::collections::HashMap<u64, String>,
}

impl fb_ebx::text::Names for ProbeNames {
    fn partition_name(&self, g: &[u8; 16]) -> Option<String> {
        self.partitions.get(g).cloned()
    }
    fn res_name(&self, rid: u64) -> Option<String> {
        self.res.get(&rid).cloned()
    }
}

/// Decode every EBX in a superbundle. With --write, also write text files to extracted/ebx/.
fn cmd_dump_ebx(log: &mut Log, root: &std::path::Path, name: &str, write: bool, filter: Option<&str>) -> Result<()> {
    use fb_vfs::dbobject::DbValue;
    use fb_vfs::payload::BlockStats;
    use fb_vfs::superbundle::{read_entry, EntryKind, Superbundle};
    use std::collections::{BTreeMap, HashMap};

    let fs = GameFs::open(root)?;
    let sb = Superbundle::open(&fs, name)?;
    let mut ebx_entries: BTreeMap<String, DbValue> = BTreeMap::new();
    let mut res_names: HashMap<u64, String> = HashMap::new();
    for b in &sb.bundles {
        let bundle = sb.read_bundle(b)?;
        for e in bundle.get("ebx").and_then(DbValue::as_list).unwrap_or(&[]) {
            if let Some(n) = e.get("name").and_then(DbValue::as_str) {
                ebx_entries.entry(n.to_lowercase()).or_insert_with(|| e.clone());
            }
        }
        for e in bundle.get("res").and_then(DbValue::as_list).unwrap_or(&[]) {
            if let (Some(rid), Some(n)) = (e.get("resRid").and_then(DbValue::as_i64), e.get("name").and_then(DbValue::as_str)) {
                res_names.insert(rid as u64, n.to_string());
            }
        }
    }
    log.line(format!("unique EBX names in {name}: {}", ebx_entries.len()));

    let mut stats = BlockStats::default();
    let mut parsed: Vec<(String, fb_ebx::Partition)> = Vec::new();
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let started = Instant::now();
    for (n, e) in &ebx_entries {
        let result = read_entry(&fs, EntryKind::Ebx, e, &mut stats).and_then(|d| fb_ebx::parse(&d));
        match result {
            Ok(p) => parsed.push((n.clone(), p)),
            Err(err) => {
                let msg: String = format!("{err:#}").chars().take(90).collect();
                let f = failures.entry(msg).or_insert((0, n.clone()));
                f.0 += 1;
            }
        }
    }
    log.line(format!("parsed {} ok, {} failed, in {:.2} s", parsed.len(), ebx_entries.len() - parsed.len(), started.elapsed().as_secs_f64()));
    for (msg, (count, example)) in &failures {
        log.line(format!("  FAIL x{count}: {msg}  (e.g. {example})"));
    }

    // Check: the Name field inside each file must match the bundle's name for it.
    let (mut name_match, mut name_mismatch, mut name_missing) = (0, 0, 0);
    for (n, p) in &parsed {
        match p.name() {
            Some(inner) if inner.to_lowercase() == *n => name_match += 1,
            Some("") => name_missing += 1,
            Some(inner) => {
                if name_mismatch < 5 {
                    log.line(format!("  name mismatch: bundle {n:?} file {inner:?}"));
                }
                name_mismatch += 1
            }
            None => name_missing += 1,
        }
    }
    log.line(format!("Name field check: {name_match} match, {name_mismatch} differ, {name_missing} have an empty or no Name"));

    let mut primary_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut all_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut instances = 0usize;
    for (_, p) in &parsed {
        if let Some(i) = p.primary() {
            *primary_types.entry(i.value.type_name.clone()).or_default() += 1;
        }
        for i in &p.instances {
            *all_types.entry(i.value.type_name.clone()).or_default() += 1;
            instances += 1;
        }
    }
    log.line(format!("instances: {instances}, instance types: {}, asset types: {}", all_types.len(), primary_types.len()));
    let mut top: Vec<_> = primary_types.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    log.line("--- asset types (primary instance), top 40");
    for (t, c) in top.iter().take(40) {
        log.line(format!("  {c:>6}  {t}"));
    }
    let mut top: Vec<_> = all_types.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    log.line("--- all instance types, top 25");
    for (t, c) in top.iter().take(25) {
        log.line(format!("  {c:>6}  {t}"));
    }

    if write {
        let names = ProbeNames {
            partitions: parsed.iter().map(|(n, p)| (p.file_guid, p.name().map(str::to_string).unwrap_or_else(|| n.clone()))).collect(),
            res: res_names,
        };
        let out_dir = PathBuf::from("extracted").join("ebx");
        let (mut files, mut bytes) = (0usize, 0usize);
        for (n, p) in &parsed {
            if filter.is_some_and(|f| !n.contains(&f.to_lowercase())) {
                continue;
            }
            let text = fb_ebx::text::to_text(p, &names);
            let path = out_dir.join(format!("{n}.txt"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &text)?;
            files += 1;
            bytes += text.len();
        }
        log.line(format!("wrote {files} text files ({:.1} MB) to {}", bytes as f64 / 1e6, out_dir.display()));
    }
    Ok(())
}

/// List res entries by name: type, size, meta, and the first bytes of the data.
fn cmd_find_res(log: &mut Log, root: &std::path::Path, name: &str, text: &str, save: bool) -> Result<()> {
    use fb_vfs::dbobject::{hex, DbValue};
    use fb_vfs::payload::BlockStats;
    use fb_vfs::superbundle::{read_entry, EntryKind, Superbundle};
    use std::collections::BTreeMap;

    let fs = GameFs::open(root)?;
    let sb = Superbundle::open(&fs, name)?;
    let mut found: BTreeMap<String, DbValue> = BTreeMap::new();
    for b in &sb.bundles {
        let bundle = sb.read_bundle(b)?;
        for e in bundle.get("res").and_then(DbValue::as_list).unwrap_or(&[]) {
            if let Some(n) = e.get("name").and_then(DbValue::as_str) {
                if n.to_lowercase().contains(text) {
                    found.entry(n.to_string()).or_insert_with(|| e.clone());
                }
            }
        }
    }
    let mut stats = BlockStats::default();
    for (n, e) in &found {
        let res_type = e.get("resType").and_then(DbValue::as_i64).unwrap_or(0) as u32;
        let meta = match e.get("resMeta") {
            Some(DbValue::Blob(b)) => hex(b),
            _ => String::new(),
        };
        let head = match read_entry(&fs, EntryKind::Res, e, &mut stats) {
            Ok(d) => {
                if save {
                    let path = PathBuf::from("extracted").join("res").join(format!("{n}.bin"));
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    std::fs::write(&path, &d)?;
                }
                format!("{} bytes, starts {}", d.len(), hex(&d[..d.len().min(16)]))
            }
            Err(err) => format!("READ FAILED: {err:#}"),
        };
        log.line(format!("{n}\n    type 0x{res_type:08x} meta {meta}\n    {head}"));
    }
    log.line(format!("{} res entries matched", found.len()));
    Ok(())
}

/// Walk a level and log every placed mesh. Writes extracted/<level>_placements.csv.
fn cmd_level(log: &mut Log, root: &std::path::Path, superbundle: &str, level: &str, skip: &[String]) -> Result<()> {
    use std::collections::{BTreeMap, HashSet};
    use std::fmt::Write as _;

    let fs = GameFs::open(root)?;
    let t = Instant::now();
    let world = fb_level::World::load(&fs, &[superbundle])?;
    log.line(format!("loaded {} partitions ({} parse failures) in {:.2} s", world.partitions.len(), world.parse_failures, t.elapsed().as_secs_f64()));

    let mut walker = fb_level::Walker::new(&world, &fs);
    walker.skip_subworlds = skip.to_vec();
    walker.walk_level(level)?;
    let meshes = &walker.meshes;
    let st = &walker.stats;
    log.line(format!("sub-worlds loaded by name: {}", st.subworlds.join(", ")));
    if !st.subworlds_skipped.is_empty() {
        log.line(format!("sub-worlds skipped: {}", st.subworlds_skipped.join(", ")));
    }
    log.line(format!(
        "references followed: {}  excluded: {}  cycles skipped: {}  depth limit hits: {}",
        st.references_followed, st.excluded, st.cycles_skipped, st.depth_limit_hits
    ));
    let unique: HashSet<&str> = meshes.iter().map(|m| m.mesh.as_str()).collect();
    let resolved = meshes.iter().filter(|m| m.mesh_at.is_some()).count();
    log.line(format!(
        "static model groups from havok: {}  member count mismatches: {}  members without transforms: {}",
        st.groups_from_havok, st.group_count_mismatches, st.group_members_without_transforms
    ));
    for (why, n) in &st.group_physics_failures {
        log.line(format!("  group physics FAIL x{n}: {why}"));
    }
    log.line(format!("placed meshes: {}  unique meshes: {}  mesh asset loaded for {} of {}", meshes.len(), unique.len(), resolved, meshes.len()));

    let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
    for m in meshes {
        *by_source.entry(m.source.as_str()).or_default() += 1;
    }
    log.line("--- placed meshes by source type");
    for (s, c) in &by_source {
        log.line(format!("  {c:>7}  {s}"));
    }

    // Where are things? Translation range and middle 90%.
    let mut xs: Vec<f32> = meshes.iter().map(|m| m.transform.translation.x).collect();
    let mut ys: Vec<f32> = meshes.iter().map(|m| m.transform.translation.y).collect();
    let mut zs: Vec<f32> = meshes.iter().map(|m| m.transform.translation.z).collect();
    for v in [&mut xs, &mut ys, &mut zs] {
        v.sort_by(|a, b| a.total_cmp(b));
    }
    let pct = |v: &Vec<f32>, p: f32| v.get(((v.len() as f32 - 1.0) * p) as usize).copied().unwrap_or(0.0);
    for (axis, v) in [("x", &xs), ("y (height)", &ys), ("z", &zs)] {
        log.line(format!(
            "  {axis}: min {:.1}  5% {:.1}  median {:.1}  95% {:.1}  max {:.1}",
            pct(v, 0.0), pct(v, 0.05), pct(v, 0.5), pct(v, 0.95), pct(v, 1.0)
        ));
    }

    // Rotation check: after removing scale, the three axes must be unit length and at right angles.
    let mut bad_rotation: BTreeMap<&str, usize> = BTreeMap::new();
    for m in meshes {
        let a = m.transform.matrix3;
        let (x, y, z) = (a.x_axis.normalize_or_zero(), a.y_axis.normalize_or_zero(), a.z_axis.normalize_or_zero());
        let ok = x.dot(y).abs() < 0.01 && y.dot(z).abs() < 0.01 && x.dot(z).abs() < 0.01 && (x.cross(y).dot(z) - 1.0).abs() < 0.01;
        if !ok {
            *bad_rotation.entry(m.source.as_str()).or_default() += 1;
        }
    }
    log.line(format!("rotation check: {} of {} placements are not a clean rotation {:?}", bad_rotation.values().sum::<usize>(), meshes.len(), bad_rotation));
    let mut per_world: BTreeMap<String, usize> = BTreeMap::new();
    for m in meshes {
        *per_world.entry(m.layer.clone()).or_default() += 1;
    }
    log.line("--- placed meshes per sub-world");
    for (w, c) in &per_world {
        log.line(format!("  {c:>6}  {w}"));
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for m in meshes {
        *counts.entry(m.mesh.as_str()).or_default() += 1;
    }
    let mut top: Vec<_> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    log.line("--- most placed meshes (top 15)");
    for (n, c) in top.iter().take(15) {
        log.line(format!("  {c:>6}  {n}"));
    }

    let mut unresolved: Vec<_> = st.unresolved_links.iter().collect();
    unresolved.sort_by(|a, b| b.1.cmp(a.1));
    log.line(format!("--- unresolved links: {} targets, {} links (top 15)", unresolved.len(), unresolved.iter().map(|u| *u.1).sum::<usize>()));
    for (n, c) in unresolved.iter().take(15) {
        log.line(format!("  {c:>6}  {n}"));
    }
    let mut other: Vec<_> = st.other_entities.iter().collect();
    other.sort_by(|a, b| b.1.cmp(a.1));
    log.line(format!("--- entities without a mesh: {} types (top 15)", other.len()));
    for (n, c) in other.iter().take(15) {
        log.line(format!("  {c:>6}  {n}"));
    }

    let mut csv = String::from("mesh,source,layer,x,y,z,scale_x,scale_y,scale_z\n");
    for m in meshes {
        let t = m.transform.translation;
        let s = glam::Vec3::new(m.transform.matrix3.x_axis.length(), m.transform.matrix3.y_axis.length(), m.transform.matrix3.z_axis.length());
        let _ = writeln!(csv, "{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}", m.mesh, m.source, m.layer, t.x, t.y, t.z, s.x, s.y, s.z);
    }
    std::fs::create_dir_all("extracted")?;
    let leaf = level.rsplit('/').next().unwrap_or(level).to_lowercase();
    let path = PathBuf::from("extracted").join(format!("{leaf}_placements.csv"));
    std::fs::write(&path, csv)?;
    log.line(format!("wrote {} rows to {}", meshes.len(), path.display()));
    Ok(())
}

/// Print the structure of a Havok packfile saved with find-res --save.
fn cmd_havok(log: &mut Log, path: &str) -> Result<()> {
    use fb_level::havok::{find_packfile, vec4, Packfile};
    let buf = std::fs::read(path)?;
    let at = find_packfile(&buf).context("no Havok packfile in file")?;
    log.line(format!("packfile at 0x{at:x} of {} bytes", buf.len()));
    let pf = Packfile::parse(&buf[at..])?;
    log.line(format!("version {} pointer size {} contents {:?}", pf.version, pf.pointer_size, pf.contents_version));
    for s in &pf.sections {
        log.line(format!("  section {:<14} start 0x{:x} end 0x{:x}", s.tag, s.start, s.end));
    }
    log.line(format!("objects: {}  local pointers: {}", pf.objects.len(), pf.pointers.len()));
    for (i, o) in pf.objects.iter().enumerate() {
        log.line(format!("  [{i}] 0x{:06x} {:>7} bytes  {}", o.offset, pf.object_len(i), o.class));
        for (field, target, count) in pf.arrays_in(i) {
            log.line(format!("        +0x{field:02x} -> 0x{target:06x} count {count}"));
        }
    }
    // Show the first vectors of each array in the compound shape(s).
    let d = pf.data_bytes();
    for i in pf.find_class("hknpStaticCompoundShape") {
        for (field, target, count) in pf.arrays_in(i) {
            log.line(format!("compound +0x{field:02x}: {count} items at 0x{target:x}; first 160 bytes as float4:"));
            for k in 0..10 {
                if let Ok(v) = vec4(d, target + k * 16) {
                    log.line(format!("    +0x{:03x}  {:>12.4} {:>12.4} {:>12.4} {:>12.4}", k * 16, v[0], v[1], v[2], v[3]));
                }
            }
        }
    }
    Ok(())
}

/// `image-stats <files...> [--rect x0,y0,x1,y1]...`: mean colour (sRGB bytes) of
/// whole frame, top third, middle third and bottom third of WebP or PNG images
/// (reference screenshots). Each `--rect` (fractions of width and height) also
/// gives that region's mean colour, contrast and fine detail (see `region_stats`).
fn cmd_image_stats(log: &mut Log, args: &[String]) -> Result<()> {
    let mut files = Vec::new();
    let mut rects: Vec<[f64; 4]> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--rect" {
            let v: Vec<f64> = it.next().context("--rect needs x0,y0,x1,y1")?.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            anyhow::ensure!(v.len() == 4, "--rect needs 4 numbers");
            rects.push([v[0], v[1], v[2], v[3]]);
        } else {
            files.push(a.clone());
        }
    }
    for f in &files {
        let bytes = std::fs::read(f).with_context(|| format!("reading {f}"))?;
        let (w, h, rgba) = if f.to_lowercase().ends_with(".png") {
            let dec = png::Decoder::new(std::io::Cursor::new(&bytes));
            let mut r = dec.read_info()?;
            let mut buf = vec![0; r.output_buffer_size()];
            let info = r.next_frame(&mut buf)?;
            let ch = info.line_size / info.width as usize;
            let mut out = Vec::with_capacity(info.width as usize * info.height as usize * 4);
            for px in buf[..info.buffer_size()].chunks(ch) {
                out.extend_from_slice(&[px[0], px[1 % ch], px[2 % ch], 255]);
            }
            (info.width as usize, info.height as usize, out)
        } else {
            let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(&bytes))?;
            let (w, h) = d.dimensions();
            let ch = if d.has_alpha() { 4 } else { 3 };
            let mut buf = vec![0u8; w as usize * h as usize * ch];
            d.read_image(&mut buf)?;
            let mut out = Vec::with_capacity(w as usize * h as usize * 4);
            for px in buf.chunks(ch) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            (w as usize, h as usize, out)
        };
        let mean = |y0: usize, y1: usize| {
            let mut s = [0f64; 3];
            for y in y0..y1 {
                for x in 0..w {
                    for c in 0..3 {
                        s[c] += rgba[(y * w + x) * 4 + c] as f64;
                    }
                }
            }
            let n = ((y1 - y0) * w).max(1) as f64;
            s.map(|v| (v / n).round() as u32)
        };
        let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        log.line(format!(
            "{name:16} {w}x{h}  whole {:?}  top {:?}  middle {:?}  bottom {:?}",
            mean(0, h),
            mean(0, h / 3),
            mean(h / 3, 2 * h / 3),
            mean(2 * h / 3, h)
        ));
        for r in &rects {
            log.line(format!("    rect {:?}: {}", r, region_stats(w, h, &rgba, *r)));
        }
    }
    Ok(())
}

/// Statistics of one region (fractions of the frame), measured on a copy scaled
/// to 1280 pixels wide so screenshots of different sizes compare:
/// mean sRGB, mean saturation (max - min channel), luminance standard deviation
/// (contrast), and detail: the mean luminance difference between neighbouring
/// pixels at 1, 4 and 16 pixel distance (small = blurry or flat).
fn region_stats(w: usize, h: usize, rgba: &[u8], r: [f64; 4]) -> String {
    let scale = w as f64 / 1280.0;
    let (x0, y0) = ((r[0] * 1280.0) as usize, (r[1] * h as f64 / scale) as usize);
    let (x1, y1) = ((r[2] * 1280.0) as usize, (r[3] * h as f64 / scale) as usize);
    let (rw, rh) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
    // Box filter each target pixel from the source.
    let mut px = vec![[0f64; 3]; rw * rh];
    for ty in 0..rh {
        for tx in 0..rw {
            let sx0 = (((x0 + tx) as f64 * scale) as usize).min(w - 1);
            let sy0 = (((y0 + ty) as f64 * scale) as usize).min(h - 1);
            let sx1 = (((x0 + tx + 1) as f64 * scale) as usize).max(sx0 + 1).min(w);
            let sy1 = (((y0 + ty + 1) as f64 * scale) as usize).max(sy0 + 1).min(h);
            let mut s = [0f64; 3];
            let mut n = 0.0;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    for c in 0..3 {
                        s[c] += rgba[(sy * w + sx) * 4 + c] as f64;
                    }
                    n += 1.0;
                }
            }
            px[ty * rw + tx] = s.map(|v| v / f64::max(n, 1.0));
        }
    }
    let lum: Vec<f64> = px.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let n = px.len() as f64;
    let mut mean = [0f64; 3];
    let mut sat = 0.0;
    for p in &px {
        for c in 0..3 {
            mean[c] += p[c] / n;
        }
        sat += (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) / n;
    }
    let lm = lum.iter().sum::<f64>() / n;
    let sd = (lum.iter().map(|l| (l - lm) * (l - lm)).sum::<f64>() / n).sqrt();
    let detail = |d: usize| {
        let (mut s, mut k) = (0.0, 0.0);
        for y in 0..rh.saturating_sub(d) {
            for x in 0..rw.saturating_sub(d) {
                let l = lum[y * rw + x];
                s += (lum[y * rw + x + d] - l).abs() + (lum[(y + d) * rw + x] - l).abs();
                k += 2.0;
            }
        }
        s / f64::max(k, 1.0)
    };
    // Mean colour of the brightest and the darkest quarter of the pixels
    // (by luminance): sunlit and shaded parts, compared separately.
    let mut order: Vec<usize> = (0..px.len()).collect();
    order.sort_by(|&a, &b| lum[a].total_cmp(&lum[b]));
    let q = (order.len() / 4).max(1);
    let avg = |ids: &[usize]| {
        let mut m = [0f64; 3];
        for &i in ids {
            for c in 0..3 {
                m[c] += px[i][c] / ids.len() as f64;
            }
        }
        format!("[{:.0}, {:.0}, {:.0}]", m[0], m[1], m[2])
    };
    // Luminance percentiles (sRGB values): the tone curve of the picture.
    let pct = |f: f64| lum[order[((order.len() - 1) as f64 * f) as usize]];
    format!(
        "{}x{} px  mean [{:.0}, {:.0}, {:.0}]  luminance p10 {:.0} p25 {:.0} p50 {:.0} p75 {:.0} p90 {:.0}  bright quarter {}  dark quarter {}  saturation {:.1}  contrast {:.1}  detail 1px {:.1} 4px {:.1} 16px {:.1}",
        rw,
        rh,
        mean[0],
        mean[1],
        mean[2],
        pct(0.1),
        pct(0.25),
        pct(0.5),
        pct(0.75),
        pct(0.9),
        avg(&order[order.len() - q..]),
        avg(&order[..q]),
        sat,
        sd,
        detail(1),
        detail(4),
        detail(16)
    )
}
