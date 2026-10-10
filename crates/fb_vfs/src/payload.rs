//! Compressed payloads inside .cas files, and delta patches.
//!
//! A payload is a run of blocks. Each block has an 8-byte big-endian header:
//!   u32: bits 24..32 = custom-dictionary flag, bits 0..24 = uncompressed size
//!   u32: bits 24..32 = compression type, bits 20..24 = type flag, bits 0..20 = compressed size
//! Compression types: 0x00 none, 0x02 zlib, 0x09 LZ4, 0x0f zstd, 0x15 Oodle.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;
use std::io::Read;

#[derive(Debug, Default, Clone)]
pub struct BlockStats {
    /// Number of blocks seen per compression type.
    pub by_type: BTreeMap<u8, u64>,
    /// Blocks that set the custom-dictionary flag.
    pub dictionary_blocks: u64,
    /// Files rebuilt from a base + delta.
    pub delta_rebuilds: u64,
}

impl BlockStats {
    pub fn merge(&mut self, other: &BlockStats) {
        for (k, v) in &other.by_type {
            *self.by_type.entry(*k).or_default() += v;
        }
        self.dictionary_blocks += other.dictionary_blocks;
        self.delta_rebuilds += other.delta_rebuilds;
    }
}

struct BlockHeader {
    dictionary: bool,
    uncompressed: usize,
    kind: u8,
    type_flag: u8,
    compressed: usize,
}

fn read_header(src: &[u8], pos: usize) -> Result<BlockHeader> {
    ensure!(pos + 8 <= src.len(), "block header past end of payload at 0x{pos:x}");
    let a = u32::from_be_bytes(src[pos..pos + 4].try_into().unwrap());
    let b = u32::from_be_bytes(src[pos + 4..pos + 8].try_into().unwrap());
    Ok(BlockHeader {
        dictionary: a & 0xff00_0000 != 0,
        uncompressed: (a & 0x00ff_ffff) as usize,
        kind: (b >> 24) as u8,
        type_flag: ((b >> 20) & 0xf) as u8,
        compressed: (b & 0x000f_ffff) as usize,
    })
}

/// Decompress one block at `src[*pos..]`, append it to `out`, and move `pos` past it.
fn decompress_block(src: &[u8], pos: &mut usize, out: &mut Vec<u8>, stats: &mut BlockStats) -> Result<usize> {
    let h = read_header(src, *pos)?;
    let mut kind = h.kind;
    // Legacy layout: no type flag means "zlib if sizes differ, else stored".
    if h.type_flag == 0 {
        kind = if h.uncompressed != h.compressed { 0x02 } else { 0x00 };
    }
    let start = *pos + 8;
    let end = start + h.compressed;
    ensure!(end <= src.len(), "block at 0x{:x} runs past end of payload", *pos);
    let input = &src[start..end];
    *stats.by_type.entry(kind).or_default() += 1;
    if h.dictionary {
        stats.dictionary_blocks += 1;
    }

    let before = out.len();
    match kind {
        0x00 => out.extend_from_slice(input),
        0x02 => {
            flate2::read::ZlibDecoder::new(input).read_to_end(out).context("zlib block")?;
        }
        0x09 => {
            out.resize(before + h.uncompressed, 0);
            let n = lz4_flex::block::decompress_into(input, &mut out[before..]).context("LZ4 block")?;
            out.truncate(before + n);
        }
        0x0f => {
            ensure!(!h.dictionary, "zstd block with a custom dictionary (not supported yet)");
            let mut dec = ruzstd::decoding::StreamingDecoder::new(input).map_err(|e| anyhow::anyhow!("zstd: {e}"))?;
            dec.read_to_end(out).context("zstd block")?;
        }
        0x15 => bail!("Oodle block (not expected in Battlefield 1)"),
        other => bail!("unknown compression type 0x{other:02x} at 0x{:x}", *pos),
    }
    let produced = out.len() - before;
    ensure!(produced == h.uncompressed, "block at 0x{:x}: got {produced} bytes, header says {}", *pos, h.uncompressed);
    *pos = end;
    Ok(produced)
}

/// Decompress a whole payload. Stops early when `expected_size` bytes are produced.
pub fn decompress(src: &[u8], expected_size: Option<usize>, stats: &mut BlockStats) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_size.unwrap_or(src.len() * 2));
    let mut pos = 0;
    while pos < src.len() {
        decompress_block(src, &mut pos, &mut out, stats)?;
        if expected_size == Some(out.len()) {
            break;
        }
    }
    if let Some(n) = expected_size {
        ensure!(out.len() == n, "payload gave {} bytes, expected {n}", out.len());
    }
    Ok(out)
}

/// Rebuild a patched payload from a base payload and a delta payload
/// (`casPatchType` 2). The delta is a list of instructions; the top 4 bits of
/// each u32 (big-endian) are the instruction type, the low 28 bits its size.
pub fn apply_delta(base: &[u8], delta: &[u8], expected_size: usize, stats: &mut BlockStats) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_size);
    let mut bpos = 0usize;
    let mut dpos = 0usize;
    let be16 = |d: &[u8], at: usize| -> Result<usize> {
        ensure!(at + 2 <= d.len(), "delta truncated");
        Ok(u16::from_be_bytes([d[at], d[at + 1]]) as usize)
    };

    while dpos < delta.len() && out.len() < expected_size {
        ensure!(dpos + 4 <= delta.len(), "delta instruction truncated");
        let word = u32::from_be_bytes(delta[dpos..dpos + 4].try_into().unwrap());
        dpos += 4;
        let (kind, size) = (word >> 28, (word & 0x0fff_ffff) as usize);
        match kind {
            // Copy `size` base blocks unchanged.
            0 => {
                for _ in 0..size {
                    decompress_block(base, &mut bpos, &mut out, stats)?;
                    if out.len() >= expected_size {
                        break;
                    }
                }
            }
            // Large edits to one base block: `size` pairs of (keep up to, skip), each followed by a delta block.
            1 => {
                let mut block = Vec::new();
                decompress_block(base, &mut bpos, &mut block, stats)?;
                let mut cur = 0usize;
                for _ in 0..size {
                    let keep_to = be16(delta, dpos)?;
                    let skip = be16(delta, dpos + 2)?;
                    dpos += 4;
                    ensure!(keep_to >= cur && keep_to <= block.len(), "delta type 1: bad offset");
                    out.extend_from_slice(&block[cur..keep_to]);
                    cur = keep_to + skip;
                    decompress_block(delta, &mut dpos, &mut out, stats)?;
                }
                if cur < block.len() {
                    out.extend_from_slice(&block[cur..]);
                }
            }
            // Small edits to one base block: `size` bytes of (keep up to u16, skip u8, add count u8, added bytes).
            2 => {
                let _block_size = be16(delta, dpos)? + 1;
                dpos += 2;
                let section_end = dpos + size;
                let mut block = Vec::new();
                decompress_block(base, &mut bpos, &mut block, stats)?;
                let mut cur = 0usize;
                while dpos < section_end {
                    ensure!(dpos + 4 <= delta.len(), "delta type 2 truncated");
                    let keep_to = be16(delta, dpos)?;
                    let skip = delta[dpos + 2] as usize;
                    let add = delta[dpos + 3] as usize;
                    dpos += 4;
                    ensure!(keep_to >= cur && keep_to <= block.len(), "delta type 2: bad offset");
                    out.extend_from_slice(&block[cur..keep_to]);
                    cur = keep_to + skip;
                    ensure!(dpos + add <= delta.len(), "delta type 2: added bytes truncated");
                    out.extend_from_slice(&delta[dpos..dpos + add]);
                    dpos += add;
                }
                if cur < block.len() {
                    out.extend_from_slice(&block[cur..]);
                }
            }
            // Add `size` delta blocks.
            3 => {
                for _ in 0..size {
                    decompress_block(delta, &mut dpos, &mut out, stats)?;
                    if out.len() >= expected_size {
                        break;
                    }
                }
            }
            // Skip `size` base blocks.
            4 => {
                for _ in 0..size {
                    let h = read_header(base, bpos)?;
                    bpos += 8 + h.compressed;
                }
            }
            other => bail!("unknown delta instruction {other} at 0x{:x}", dpos - 4),
        }
    }
    // The rest comes from the base, unchanged.
    while out.len() < expected_size {
        ensure!(bpos < base.len(), "delta: base ended at {} of {expected_size} bytes", out.len());
        decompress_block(base, &mut bpos, &mut out, stats)?;
    }
    ensure!(out.len() == expected_size, "delta result {} bytes, expected {expected_size}", out.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_block(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(&((0x70u32 << 16) | data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn stored_and_lz4_blocks() {
        let text = b"hello hello hello hello frostbite";
        let lz = lz4_flex::block::compress(text);
        let mut src = stored_block(b"abc");
        src.extend_from_slice(&(text.len() as u32).to_be_bytes());
        src.extend_from_slice(&((0x09u32 << 24) | (0x7 << 20) | lz.len() as u32).to_be_bytes());
        src.extend_from_slice(&lz);

        let mut stats = BlockStats::default();
        let out = decompress(&src, None, &mut stats).unwrap();
        assert_eq!(&out[..3], b"abc");
        assert_eq!(&out[3..], text);
        assert_eq!(stats.by_type.get(&0x09), Some(&1));
    }

    #[test]
    fn delta_small_edit() {
        let base = stored_block(b"0123456789");
        // Type 2: keep "012", skip 2 ("34"), add "XY"; rest from base block.
        let mut delta = Vec::new();
        let body = [0u8, 3, 2, 2, b'X', b'Y'];
        delta.extend_from_slice(&((2u32 << 28) | body.len() as u32).to_be_bytes());
        delta.extend_from_slice(&9u16.to_be_bytes());
        delta.extend_from_slice(&body);
        let mut stats = BlockStats::default();
        let out = apply_delta(&base, &delta, 10, &mut stats).unwrap();
        assert_eq!(&out, b"012XY56789");
    }
}
