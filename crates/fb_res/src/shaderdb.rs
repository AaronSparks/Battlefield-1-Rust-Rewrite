//! Level shader database (res type `IShaderDatabase`): just enough to find the
//! textures of the terrain shaders (worked out from the Verdun data; see
//! STATUS.md, update 8).
//!
//! - Description blocks start near the file start and follow each other: each
//!   starts with its u64 size, then u64 offsets of its parts. The texture
//!   list is from the first offset to the third: records of 152 bytes
//!   (u32 slot, u32 0, NUL-terminated asset name).
//! - At the end: a table of N permutation records (136 bytes each), a 28-byte
//!   header that ends with u32 N, then N key records (32 bytes each). Key k
//!   belongs to permutation k. A key starts with the Frostbite hash of the
//!   lower-case shader name. Permutation k holds at +88 the description block
//!   number, counting from a small first block at byte 0x1c (the terrain
//!   blocks follow it). Record 0 of the permutation table also holds N at +20.

use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

const PERM_SIZE: usize = 136;
const KEY_SIZE: usize = 32;
const TEXTURE_RECORD: usize = 152;

/// Frostbite name hash: h = h * 33 ^ byte, starting at 5381.
pub fn fb_hash(s: &str) -> u32 {
    s.bytes().fold(5381u32, |h, c| h.wrapping_mul(33) ^ c as u32)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

pub struct ShaderDb<'a> {
    data: &'a [u8],
    /// Start of each description block.
    blocks: Vec<usize>,
    perm0: usize,
    key0: usize,
    count: usize,
    /// Key indices by name hash.
    keys: HashMap<u32, Vec<usize>>,
}

impl<'a> ShaderDb<'a> {
    /// Locate the tables. `probe_hash` is the hash of a shader name known to be
    /// in the database (it anchors the key table).
    pub fn parse(data: &'a [u8], probe_hash: u32) -> Result<ShaderDb<'a>> {
        // Description blocks: the start that gives the longest chain of sizes.
        let walk = |start: usize| -> Vec<usize> {
            let mut out = Vec::new();
            let mut a = start;
            while let Some(size) = u64_at(data, a) {
                if !(0x50..=1_000_000).contains(&size) || size % 4 != 0 {
                    break;
                }
                out.push(a);
                a += size as usize;
            }
            out
        };
        let blocks = (0x10..0x1000).step_by(4).map(walk).max_by_key(|b| b.len()).unwrap_or_default();
        ensure!(blocks.len() >= 100, "shader database: no chain of description blocks");

        // Key table: from an occurrence of the probe hash, step back in 32-byte
        // steps until both copies of the count agree.
        let needle = probe_hash.to_le_bytes();
        let mut found = None;
        'search: for p in (0..data.len().saturating_sub(4)).filter(|&p| data[p..p + 4] == needle) {
            let mut k0 = p;
            while k0 >= 32 {
                if let Some(n) = u32_at(data, k0 - 4) {
                    let n = n as usize;
                    let span = 28 + PERM_SIZE * n;
                    if n > 0 && k0 >= span && (p - k0) / KEY_SIZE < n && k0 + n * KEY_SIZE <= data.len() && u32_at(data, k0 - span + 20) == Some(n as u32) {
                        found = Some((k0, k0 - span, n));
                        break 'search;
                    }
                }
                k0 -= KEY_SIZE;
            }
        }
        let (key0, perm0, count) = found.context("shader database: key table not found")?;
        let mut keys: HashMap<u32, Vec<usize>> = HashMap::new();
        for k in 0..count {
            if let Some(h) = u32_at(data, key0 + k * KEY_SIZE) {
                keys.entry(h).or_default().push(k);
            }
        }
        Ok(ShaderDb { data, blocks, perm0, key0, count, keys })
    }

    pub fn summary(&self) -> String {
        format!("{} description blocks, {} permutations, key table at {}, permutation table at {}", self.blocks.len(), self.count, self.key0, self.perm0)
    }

    /// Texture asset names of a block.
    pub fn block_textures(&self, b: usize) -> Vec<String> {
        self.block_texture_slots(b).into_iter().map(|(_, n)| n).collect()
    }

    /// Texture slots and asset names of a block.
    pub fn block_texture_slots(&self, b: usize) -> Vec<(u32, String)> {
        let Some(&at) = self.blocks.get(b) else { return vec![] };
        let (Some(first), Some(third)) = (u64_at(self.data, at + 8), u64_at(self.data, at + 24)) else { return vec![] };
        let (mut p, end) = (at + first as usize, at + third as usize);
        let mut out = Vec::new();
        while p + TEXTURE_RECORD <= end && p + TEXTURE_RECORD <= self.data.len() {
            let raw = &self.data[p + 8..p + TEXTURE_RECORD];
            let name = &raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())];
            if name.is_empty() {
                break;
            }
            let slot = u32_at(self.data, p).unwrap_or(u32::MAX);
            out.push((slot, String::from_utf8_lossy(name).into_owned()));
            p += TEXTURE_RECORD;
        }
        out
    }

    /// Description blocks of all permutations of a shader name.
    pub fn blocks_of(&self, name: &str) -> Vec<usize> {
        let Some(ks) = self.keys.get(&fb_hash(&name.to_lowercase())) else { return vec![] };
        ks.iter().filter_map(|&k| u64_at(self.data, self.perm0 + k * PERM_SIZE + 88).map(|b| b as usize)).collect()
    }

    /// Textures of all permutations of a shader name (union).
    pub fn textures_of(&self, name: &str) -> Option<HashSet<String>> {
        self.keys.get(&fb_hash(&name.to_lowercase()))?;
        let mut out = HashSet::new();
        for b in self.blocks_of(name) {
            out.extend(self.block_textures(b));
        }
        Some(out)
    }
}

/// Colour terrain shader names ("..._2d__d__N") in a layer combinations file,
/// with their layer numbers.
pub fn terrain_colour_shaders(layer_combinations: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut seen = HashSet::new();
    for run in layer_combinations.split(|&b| !(0x20..0x7f).contains(&b)) {
        let Ok(s) = std::str::from_utf8(run) else { continue };
        let Some(leaf) = s.rsplit('/').next() else { continue };
        let parts: Vec<&str> = leaf.split("__").collect();
        // e.g. Terrain_4km __ 0MV_2MV_11MV __ 2d __ d __ 0
        if parts.len() != 5 || parts[2] != "2d" || !seen.insert(s.to_string()) {
            continue;
        }
        let layers: Option<Vec<u8>> = parts[1].split('_').map(|l| l.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()).collect();
        if let Some(layers) = layers {
            out.push((s.to_string(), layers));
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct LayerTextureStats {
    pub names: usize,
    pub names_found: usize,
    /// Layers with no colour texture used by all their shaders.
    pub layers_without: Vec<u8>,
}

/// Main colour texture of each terrain layer: the most specific colour texture
/// (`..._CS`) that every colour shader with that layer uses.
pub fn terrain_layer_textures(db: &ShaderDb, shaders: &[(String, Vec<u8>)]) -> (BTreeMap<u8, String>, LayerTextureStats) {
    let mut stats = LayerTextureStats { names: shaders.len(), ..Default::default() };
    let mut uses: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut with_layer: BTreeMap<u8, HashSet<usize>> = BTreeMap::new();
    for (i, (name, layers)) in shaders.iter().enumerate() {
        let Some(tex) = db.textures_of(name) else { continue };
        stats.names_found += 1;
        for t in tex.into_iter().filter(|t| t.to_ascii_lowercase().ends_with("_cs")) {
            uses.entry(t).or_default().insert(i);
        }
        for &l in layers {
            with_layer.entry(l).or_default().insert(i);
        }
    }
    let mut out = BTreeMap::new();
    for (&l, names) in &with_layer {
        let best = uses.iter().filter(|(_, u)| names.is_subset(u)).min_by_key(|(t, u)| (u.len(), (*t).clone()));
        match best {
            Some((t, _)) => {
                out.insert(l, t.clone());
            }
            None => stats.layers_without.push(l),
        }
    }
    (out, stats)
}

/// Normal/roughness/height texture (`..._NRH`) of each colour texture
/// (`..._CS`): the texture in the next slot of the same shader block, counted
/// over all blocks; the most common pair wins. Keys are lower-case colour
/// texture names. Also returns the number of pairs that disagree with the
/// winner (0 on Verdun, STATUS/DESIGN "ground detail").
pub fn pair_next_slot_nrh(blocks: &[Vec<(u32, String)>]) -> (BTreeMap<String, String>, usize) {
    let mut votes: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for list in blocks {
        for (slot, name) in list {
            if !name.to_ascii_lowercase().ends_with("_cs") {
                continue;
            }
            let Some((_, next)) = list.iter().find(|(s, _)| *s == slot + 1) else { continue };
            if next.to_ascii_lowercase().ends_with("_nrh") {
                *votes.entry(name.to_ascii_lowercase()).or_default().entry(next.clone()).or_default() += 1;
            }
        }
    }
    let mut out = BTreeMap::new();
    let mut disagree = 0;
    for (cs, v) in votes {
        let total: usize = v.values().sum();
        if let Some((best, n)) = v.into_iter().max_by_key(|(_, n)| *n) {
            disagree += total - n;
            out.insert(cs, best);
        }
    }
    (out, disagree)
}

/// `pair_next_slot_nrh` over the blocks of the given terrain colour shaders.
pub fn terrain_layer_normals(db: &ShaderDb, shaders: &[(String, Vec<u8>)]) -> (BTreeMap<String, String>, usize) {
    let mut lists = Vec::new();
    let mut seen = HashSet::new();
    for (name, _) in shaders {
        for b in db.blocks_of(name) {
            if seen.insert(b) {
                lists.push(db.block_texture_slots(b));
            }
        }
    }
    pair_next_slot_nrh(&lists)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pairs_colour_with_next_slot_nrh() {
        use super::pair_next_slot_nrh;
        let block = |v: &[(u32, &str)]| v.iter().map(|(s, n)| (*s, n.to_string())).collect::<Vec<_>>();
        let blocks = vec![
            block(&[(6, "T/Mud_CS"), (7, "T/Mud_NRH"), (8, "T/Variation_RGB"), (10, "T/Grass_CS"), (11, "T/Grass_NRH")]),
            block(&[(6, "T/Mud_CS"), (7, "T/Mud_NRH")]),
            // One odd pair, and a colour texture without a normal texture next to it.
            block(&[(3, "T/Mud_CS"), (4, "T/Other_NRH"), (9, "T/Rock_CS"), (10, "T/Rock_NAH")]),
        ];
        let (map, disagree) = pair_next_slot_nrh(&blocks);
        assert_eq!(map.get("t/mud_cs").map(String::as_str), Some("T/Mud_NRH"));
        assert_eq!(map.get("t/grass_cs").map(String::as_str), Some("T/Grass_NRH"));
        assert!(!map.contains_key("t/rock_cs"));
        assert_eq!(disagree, 1);
    }

    use super::*;

    #[test]
    fn hash_matches_the_ebx_keyword_hash() {
        // h = 5381; h = h * 33 ^ c
        assert_eq!(fb_hash(""), 5381);
        assert_eq!(fb_hash("a"), (5381u32 * 33) ^ b'a' as u32);
    }

    #[test]
    fn parses_colour_shader_names() {
        let lc = b"\x00\x01Xpack1/Levels/X/Terrain_4km/Terrain_4km__0MV_2MV_11MV__2d__d__0\x00junk\x00Xpack1/Levels/X/Terrain_4km/Terrain_4km__0MF__Displacement2d__d__0\x00";
        let s = terrain_colour_shaders(lc);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].1, vec![0, 2, 11]);
    }
}
