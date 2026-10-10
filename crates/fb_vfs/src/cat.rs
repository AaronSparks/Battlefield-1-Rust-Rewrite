//! cas.cat: the index from SHA1 to (cas file, offset, size).
//!
//! Battlefield 1 layout, after the 556-byte signed header (checked on the
//! user's install, 2026-10-03):
//!   16 bytes  "NyanNyanNyanNyan"
//!   u32       entry count
//!   u32       patch entry count (0 in Data, non-zero in some Patch cats)
//!   entries   36 bytes each: sha1 (20), offset u32, size u32, unknown u32, cas number u32
//!   patches   60 bytes each: sha1 (20), base sha1 (20), delta sha1 (20)

use crate::signed::read_signed_file;
use anyhow::{ensure, Result};
use std::path::{Path, PathBuf};

pub const NYAN: &[u8; 16] = b"NyanNyanNyanNyan";
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 36;
const PATCH_ENTRY_SIZE: usize = 60;

#[derive(Debug, Clone)]
pub struct CatEntry {
    pub sha1: [u8; 20],
    pub offset: u32,
    pub size: u32,
    pub cas_number: u32,
}

#[derive(Debug, Clone)]
pub struct CatPatchEntry {
    pub sha1: [u8; 20],
    pub base_sha1: [u8; 20],
    pub delta_sha1: [u8; 20],
}

#[derive(Debug)]
pub struct CatFile {
    pub path: PathBuf,
    pub entries: Vec<CatEntry>,
    pub patches: Vec<CatPatchEntry>,
}

impl CatFile {
    pub fn cas_path(&self, cas_number: u32) -> PathBuf {
        self.path.with_file_name(format!("cas_{cas_number:02}.cas"))
    }
}

pub fn read_cat(path: &Path) -> Result<CatFile> {
    let (_, data) = read_signed_file(path)?;
    ensure!(data.get(..16) == Some(&NYAN[..]), "{}: no Nyan magic", path.display());
    ensure!(data.len() >= HEADER_SIZE, "{}: too short", path.display());
    let u32_at = |at: usize| -> u32 { u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) };
    let sha1_at = |at: usize| -> [u8; 20] { data[at..at + 20].try_into().unwrap() };
    let count = u32_at(16) as usize;
    let patch_count = u32_at(20) as usize;
    let expected = HEADER_SIZE + count * ENTRY_SIZE + patch_count * PATCH_ENTRY_SIZE;
    ensure!(
        expected == data.len(),
        "{}: unknown cat layout ({count} entries + {patch_count} patches = {expected} bytes, file has {})",
        path.display(),
        data.len()
    );

    let entries = (0..count)
        .map(|i| {
            let at = HEADER_SIZE + i * ENTRY_SIZE;
            CatEntry { sha1: sha1_at(at), offset: u32_at(at + 20), size: u32_at(at + 24), cas_number: u32_at(at + 32) }
        })
        .collect();
    let patch_start = HEADER_SIZE + count * ENTRY_SIZE;
    let patches = (0..patch_count)
        .map(|i| {
            let at = patch_start + i * PATCH_ENTRY_SIZE;
            CatPatchEntry { sha1: sha1_at(at), base_sha1: sha1_at(at + 20), delta_sha1: sha1_at(at + 40) }
        })
        .collect();
    Ok(CatFile { path: path.to_path_buf(), entries, patches })
}
