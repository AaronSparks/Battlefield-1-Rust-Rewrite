//! Texture resources (the header that a TextureAsset points to). The pixels
//! are in a chunk named by `chunk_id`, largest mip first.
//!
//! Layout for Battlefield 1 (learned by reading FrostyToolsuite v1, read
//! only, no code copied; checked against the data):
//!   u32 mip offset 0, u32 mip offset 1, u32 type, i32 pixel format,
//!   u16 flags, u16 width, u16 height, u16 depth, u16 slice count,
//!   u8 mip count, u8 first mip, 16-byte chunk id, 15 x u32 mip sizes,
//!   u32 chunk size, u32 asset name hash, 16-byte texture group name.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct TextureHeader {
    pub mip_offsets: [u32; 2],
    pub kind: u32,
    pub pixel_format: i32,
    pub flags: u16,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub slices: u16,
    pub mip_count: u8,
    pub first_mip: u8,
    pub chunk_id: [u8; 16],
    pub mip_sizes: Vec<u32>,
    pub chunk_size: u32,
    pub group: String,
}

impl TextureHeader {
    /// Bytes per pixel of the largest mip (0.5 for BC1/BC4, 1 for BC2/3/5/7, 4 for RGBA8).
    pub fn bytes_per_pixel(&self) -> f32 {
        let pixels = self.width as f32 * self.height as f32 * self.depth.max(1) as f32;
        if pixels == 0.0 {
            0.0
        } else {
            self.mip_sizes.first().copied().unwrap_or(0) as f32 / pixels
        }
    }
}

/// GPU block-compression format of a pixel format code. Worked out from the
/// data (bytes per pixel, texture names, normal-map check); see MODLOG.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockFormat {
    Bc1 { srgb: bool },
    Bc3 { srgb: bool },
    Bc5,
    Bc7 { srgb: bool },
}

pub fn block_format(pixel_format: i32) -> Option<BlockFormat> {
    Some(match pixel_format {
        54 => BlockFormat::Bc1 { srgb: false },
        55 => BlockFormat::Bc1 { srgb: true },
        60 => BlockFormat::Bc3 { srgb: false },
        61 => BlockFormat::Bc3 { srgb: true },
        63 => BlockFormat::Bc5,
        66 => BlockFormat::Bc7 { srgb: false },
        67 => BlockFormat::Bc7 { srgb: true },
        _ => return None,
    })
}

impl BlockFormat {
    pub fn bytes_per_block(self) -> usize {
        match self {
            BlockFormat::Bc1 { .. } => 8,
            _ => 16,
        }
    }
}

/// One mip level inside the texture chunk.
#[derive(Debug, Clone, Copy)]
pub struct Mip {
    pub offset: usize,
    pub size: usize,
    pub width: u32,
    pub height: u32,
}

impl TextureHeader {
    /// Mip levels in chunk order (largest first), using the stored mip sizes.
    pub fn mips(&self) -> Vec<Mip> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        for i in 0..self.mip_count as usize {
            let size = self.mip_sizes.get(i).copied().unwrap_or(0) as usize;
            out.push(Mip {
                offset,
                size,
                width: (self.width as u32 >> i).max(1),
                height: (self.height as u32 >> i).max(1),
            });
            offset += size;
        }
        out
    }
}

pub fn parse_texture(d: &[u8]) -> Result<TextureHeader> {
    let b = |at: usize, n: usize| d.get(at..at + n).context("texture header too short");
    let u16_at = |at: usize| -> Result<u16> { Ok(u16::from_le_bytes(b(at, 2)?.try_into().unwrap())) };
    let u32_at = |at: usize| -> Result<u32> { Ok(u32::from_le_bytes(b(at, 4)?.try_into().unwrap())) };
    let mip_sizes = (0..15).map(|i| u32_at(44 + i * 4)).collect::<Result<Vec<_>>>()?;
    let group = b(112, 16)?;
    let glen = group.iter().position(|&c| c == 0).unwrap_or(16);
    Ok(TextureHeader {
        mip_offsets: [u32_at(0)?, u32_at(4)?],
        kind: u32_at(8)?,
        pixel_format: u32_at(12)? as i32,
        flags: u16_at(16)?,
        width: u16_at(18)?,
        height: u16_at(20)?,
        depth: u16_at(22)?,
        slices: u16_at(24)?,
        mip_count: b(26, 1)?[0],
        first_mip: b(27, 1)?[0],
        chunk_id: b(28, 16)?.try_into().unwrap(),
        mip_sizes,
        chunk_size: u32_at(104)?,
        group: String::from_utf8_lossy(&group[..glen]).into_owned(),
    })
}
