//! Read-only access to the Frostbite data of a Battlefield 1 install.
//!
//! Format notes were learned from Frostbite-Scripts (MIT, NicknineTheEagle,
//! Frankelstner, NoFaTe). See CREDITS.md. No code was copied.

pub mod cat;
pub mod dbobject;
pub mod payload;
pub mod signed;
pub mod superbundle;

use anyhow::{bail, Context, Result};
use dbobject::DbValue;
use payload::BlockStats;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const GAME_DIR_ENV: &str = "BF1_GAME_DIR";
pub const DEFAULT_GAME_DIR_NAME: &str = "Battlefield 1 Files";

/// Find the game folder: `BF1_GAME_DIR`, else `Battlefield 1 Files` in the
/// current folder or one of its parents.
pub fn find_game_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(GAME_DIR_ENV) {
        let dir = PathBuf::from(dir);
        if dir.join("Data").join("layout.toc").is_file() {
            return Ok(dir);
        }
        bail!("{GAME_DIR_ENV} is set to {} but Data\\layout.toc is not there. Set it to the folder that has Data and Patch in it (see README.md, step 3).", dir.display());
    }
    let mut cur = std::env::current_dir()?;
    loop {
        let candidate = cur.join(DEFAULT_GAME_DIR_NAME);
        if candidate.join("Data").join("layout.toc").is_file() {
            return Ok(candidate);
        }
        if !cur.pop() {
            bail!(
                "game files not found. Copy the Data and Patch folders of your Battlefield 1 install into the \"{DEFAULT_GAME_DIR_NAME}\" folder of this project, so that \"{DEFAULT_GAME_DIR_NAME}\\Data\\layout.toc\" exists, and run from the project folder; or set {GAME_DIR_ENV} to your install folder. See README.md, step 3."
            );
        }
    }
}

/// Where one SHA1 lives.
#[derive(Debug, Clone, Copy)]
pub struct CasLocation {
    pub cas_file: u32,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct CatSummary {
    pub path: PathBuf,
    pub entries: usize,
    pub patch_entries: usize,
    pub from_patch: bool,
}

pub struct GameFs {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub patch_dir: PathBuf,
    pub cas_files: Vec<PathBuf>,
    pub index: HashMap<[u8; 20], CasLocation>,
    pub cats: Vec<CatSummary>,
    /// Patch entries from the cats: sha1 -> (base sha1, delta sha1).
    pub patches: HashMap<[u8; 20], ([u8; 20], [u8; 20])>,
    /// Chunks listed in .toc files: chunk id -> sha1. Filled by `index_toc_chunks`.
    pub toc_chunks: HashMap<[u8; 16], [u8; 20]>,
    /// Indices into `cas_files` that were read (to report which game files
    /// a run needs).
    pub cas_used: std::sync::Mutex<std::collections::BTreeSet<u32>>,
}

impl GameFs {
    /// Open the install and index every cas.cat (Data first, then Patch, so Patch wins).
    pub fn open(root: &Path) -> Result<GameFs> {
        let mut fs = GameFs::without_index(root);
        let mut cas_ids: HashMap<PathBuf, u32> = HashMap::new();
        for (dir, from_patch) in [(fs.data_dir.clone(), false), (fs.patch_dir.clone(), true)] {
            let mut cat_paths = Vec::new();
            find_files(&dir, "cas.cat", &mut cat_paths)?;
            cat_paths.sort();
            for path in cat_paths {
                let catf = cat::read_cat(&path)?;
                for e in &catf.entries {
                    let cas_path = catf.cas_path(e.cas_number);
                    let next = cas_ids.len() as u32;
                    let id = *cas_ids.entry(cas_path.clone()).or_insert_with(|| {
                        fs.cas_files.push(cas_path);
                        next
                    });
                    // The same sha1 can be in several cats, and some copies hold only a
                    // tail range of the data (seen for streamed textures). Keep the largest.
                    let loc = CasLocation { cas_file: id, offset: e.offset, size: e.size };
                    match fs.index.get(&e.sha1) {
                        Some(old) if old.size >= e.size => {}
                        _ => {
                            fs.index.insert(e.sha1, loc);
                        }
                    }
                }
                for p in &catf.patches {
                    fs.patches.insert(p.sha1, (p.base_sha1, p.delta_sha1));
                }
                fs.cats.push(CatSummary { path, entries: catf.entries.len(), patch_entries: catf.patches.len(), from_patch });
            }
        }
        Ok(fs)
    }

    /// Read the `chunks` list of every .toc file (Data first, then Patch, so
    /// Patch wins) into `toc_chunks`. Shared chunks, for example mesh vertex
    /// data used by many levels, are listed here (mostly in `Win32/chunks0`).
    pub fn index_toc_chunks(&mut self) -> Result<usize> {
        for dir in [self.data_dir.clone(), self.patch_dir.clone()] {
            let mut tocs = Vec::new();
            find_files_ext(&dir, "toc", &mut tocs)?;
            tocs.sort();
            for path in tocs {
                if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("layout.toc")) {
                    continue;
                }
                let Ok((_, toc)) = read_toc(&path) else { continue };
                for c in toc.get("chunks").and_then(DbValue::as_list).unwrap_or(&[]) {
                    if let (Some(id), Some(sha1)) = (c.get("id").and_then(DbValue::as_guid), c.get("sha1").and_then(DbValue::as_sha1)) {
                        self.toc_chunks.insert(*id, *sha1);
                    }
                }
            }
        }
        Ok(self.toc_chunks.len())
    }

    /// Paths only, no cas.cat index. Enough to read .toc and .sb files.
    pub fn without_index(root: &Path) -> GameFs {
        GameFs {
            root: root.to_path_buf(),
            data_dir: root.join("Data"),
            patch_dir: root.join("Patch"),
            cas_files: Vec::new(),
            index: HashMap::new(),
            cats: Vec::new(),
            patches: HashMap::new(),
            toc_chunks: HashMap::new(),
            cas_used: Default::default(),
        }
    }

    /// Read the raw (still compressed) bytes stored for a SHA1.
    pub fn read_raw(&self, sha1: &[u8; 20]) -> Result<Vec<u8>> {
        let loc = *self.index.get(sha1).with_context(|| format!("sha1 {} not in any cas.cat", dbobject::hex(sha1)))?;
        let path = &self.cas_files[loc.cas_file as usize];
        if let Ok(mut used) = self.cas_used.lock() {
            used.insert(loc.cas_file);
        }
        let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
        f.seek(SeekFrom::Start(loc.offset as u64))?;
        let mut buf = vec![0u8; loc.size as usize];
        f.read_exact(&mut buf).with_context(|| format!("read {} bytes at 0x{:x} in {}", loc.size, loc.offset, path.display()))?;
        Ok(buf)
    }

    /// Read and decompress the data stored for a SHA1. If the SHA1 is a patch
    /// entry of a Patch cat, rebuild it from its base and delta.
    pub fn read(&self, sha1: &[u8; 20], expected_size: Option<usize>, stats: &mut BlockStats) -> Result<Vec<u8>> {
        if !self.index.contains_key(sha1) {
            if let Some((base, delta)) = self.patches.get(sha1) {
                let expected = expected_size.context("patched entry without a known size")?;
                let base = self.read_raw(base).context("base of patched entry")?;
                let delta = self.read_raw(delta).context("delta of patched entry")?;
                stats.delta_rebuilds += 1;
                return payload::apply_delta(&base, &delta, expected, stats);
            }
        }
        let raw = self.read_raw(sha1)?;
        payload::decompress(&raw, expected_size, stats)
    }

    /// Path of a superbundle .toc in Data or Patch (`name` like "win32/xpack1/levels/mp_verdun").
    pub fn toc_path(&self, name: &str, patch: bool) -> PathBuf {
        let base = if patch { &self.patch_dir } else { &self.data_dir };
        let mut p = base.clone();
        for part in name.split('/') {
            p.push(part);
        }
        p.set_extension("toc");
        p
    }
}

/// Read a .toc (or layout.toc) and parse its DbObject.
pub fn read_toc(path: &Path) -> Result<(signed::HeaderKind, DbValue)> {
    let (kind, data) = signed::read_signed_file(path)?;
    let (value, _) = dbobject::parse(&data).with_context(|| format!("parse {}", path.display()))?;
    Ok((kind, value))
}

/// Parse the DbObject that starts at `offset` in a .sb file.
pub fn read_sb_object(sb: &[u8], offset: usize) -> Result<DbValue> {
    let slice = sb.get(offset..).context("sb offset past end of file")?;
    Ok(dbobject::parse(slice)?.0)
}

fn find_files(dir: &Path, name: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            find_files(&path, name, out)?;
        } else if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            out.push(path);
        }
    }
    Ok(())
}

fn find_files_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            find_files_ext(&path, ext, out)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(path);
        }
    }
    Ok(())
}
