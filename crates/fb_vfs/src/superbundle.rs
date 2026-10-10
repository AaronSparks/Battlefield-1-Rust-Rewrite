//! Superbundles: a .toc (list of bundles with offsets) plus a .sb (the bundles).
//!
//! When a superbundle exists in both Data and Patch, a bundle listed in the
//! Patch .toc replaces the bundle with the same id in the Data .toc.

use crate::dbobject::DbValue;
use crate::payload::{self, BlockStats};
use crate::{read_sb_object, read_toc, GameFs};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct BundleRef {
    pub id: String,
    pub from_patch: bool,
    pub offset: usize,
    pub size: usize,
    /// Flags other than id/offset/size found in the .toc entry (for example "delta" or "base").
    pub extra_flags: Vec<String>,
}

pub struct Superbundle {
    pub name: String,
    pub bundles: Vec<BundleRef>,
    data_sb: Option<Vec<u8>>,
    patch_sb: Option<Vec<u8>>,
}

impl Superbundle {
    pub fn open(fs: &GameFs, name: &str) -> Result<Superbundle> {
        let mut by_id: BTreeMap<String, BundleRef> = BTreeMap::new();
        let mut data_sb = None;
        let mut patch_sb = None;
        for from_patch in [false, true] {
            let toc_path = fs.toc_path(name, from_patch);
            if !toc_path.is_file() {
                continue;
            }
            let (_, toc) = read_toc(&toc_path)?;
            if toc.get("cas").and_then(DbValue::as_bool) != Some(true) {
                bail!("{}: not a cas superbundle (not supported)", toc_path.display());
            }
            let sb = std::fs::read(toc_path.with_extension("sb")).context("read .sb")?;
            for b in toc.get("bundles").and_then(DbValue::as_list).unwrap_or(&[]) {
                let id = b.get("id").and_then(DbValue::as_str).context("bundle without id")?.to_string();
                let offset = b.get("offset").and_then(DbValue::as_i64).context("bundle without offset")? as usize;
                let size = b.get("size").and_then(DbValue::as_i64).context("bundle without size")? as usize;
                let extra_flags = match b {
                    DbValue::Object(fields) => fields
                        .iter()
                        .map(|(n, _)| n.clone())
                        .filter(|n| !matches!(n.as_str(), "id" | "offset" | "size"))
                        .collect(),
                    _ => vec![],
                };
                by_id.insert(id.to_lowercase(), BundleRef { id, from_patch, offset, size, extra_flags });
            }
            if from_patch {
                patch_sb = Some(sb);
            } else {
                data_sb = Some(sb);
            }
        }
        if by_id.is_empty() {
            bail!("superbundle {name} not found in Data or Patch");
        }
        Ok(Superbundle { name: name.to_string(), bundles: by_id.into_values().collect(), data_sb, patch_sb })
    }

    pub fn read_bundle(&self, b: &BundleRef) -> Result<DbValue> {
        let sb = if b.from_patch { &self.patch_sb } else { &self.data_sb };
        let sb = sb.as_ref().context("sb missing")?;
        read_sb_object(sb, b.offset).with_context(|| format!("bundle {}", b.id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Ebx,
    Dbx,
    Res,
    Chunk,
}

/// The size the decompressed data must have, if the entry says so.
pub fn expected_size(kind: EntryKind, entry: &DbValue) -> Option<usize> {
    match kind {
        EntryKind::Chunk => {
            // Partial chunks (rangeStart/rangeEnd) hold only part of the data; we do not know its size yet.
            if entry.get("rangeStart").is_some() {
                return None;
            }
            let lo = entry.get("logicalOffset").and_then(DbValue::as_i64)?;
            let ls = entry.get("logicalSize").and_then(DbValue::as_i64)?;
            Some((lo + ls) as usize)
        }
        _ => entry.get("originalSize").and_then(DbValue::as_i64).map(|v| v as usize),
    }
}

/// Read one bundle entry (ebx, dbx, res or chunk) and decompress it.
pub fn read_entry(fs: &GameFs, kind: EntryKind, entry: &DbValue, stats: &mut BlockStats) -> Result<Vec<u8>> {
    let expected = expected_size(kind, entry);
    if entry.get("casPatchType").and_then(DbValue::as_i64) == Some(2) {
        let base = entry.get("baseSha1").and_then(DbValue::as_sha1).context("delta entry without baseSha1")?;
        let delta = entry.get("deltaSha1").and_then(DbValue::as_sha1).context("delta entry without deltaSha1")?;
        let expected = expected.context("delta entry without a known size")?;
        let base = fs.read_raw(base)?;
        let delta = fs.read_raw(delta)?;
        return payload::apply_delta(&base, &delta, expected, stats);
    }
    let sha1 = entry.get("sha1").and_then(DbValue::as_sha1).context("entry without sha1")?;
    fs.read(sha1, expected, stats)
}
