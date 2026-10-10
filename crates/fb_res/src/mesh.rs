//! MeshSet resources (res type 0x49b156d4) and their vertex/index data.
//!
//! Layout for Battlefield 1, checked on the user's install (2026-10-03).
//! All "offset" fields are byte offsets from the start of the res data.
//! The layout was learned by reading FrostyToolsuite v1 (CC BY-NC-ND, read
//! only, no code copied) and confirmed against the data.
//!
//! MeshSet header:
//!   0x00 bounding box min (float3 + pad), 0x10 max (float3 + pad)
//!   0x20 6 x u64 LOD offsets, u64 unknown, u64 full name offset, u64 name offset
//!   0x68 u32 name hash, u32 mesh type (0 rigid, 1 skinned, 2 composite)
//!   0x70 12 x u16 LOD fade factors, u32 flags, u16 LOD count, u16 section count
//! LOD (192 bytes, fields packed without alignment):
//!   u32 mesh type, u32 max instances, u32 section count, u64 section offset,
//!   5 x (u32 count, u64 offset) subset categories (opaque, transparent,
//!   transparent decal, z only, shadow), u32 flags, u32 index format,
//!   u32 index buffer size, u32 vertex buffer size, u32 adjacency size,
//!   16-byte chunk id, u32 inline data offset, u64 adjacency offset,
//!   3 x u64 name offsets, u32 name hash, u64, ...
//! Section (304 bytes):
//!   3 x u64 (two runtime pointers, material name offset), i32 material id,
//!   u32 lightmap UV index, u32 primitive count, u32 start index,
//!   u32 vertex offset (bytes), u32 vertex count, u8 stride, u8 primitive type,
//!   ... bone data ..., 2 x geometry declaration (84 bytes each), ...
//! Geometry declaration: 16 elements (usage, format, offset, stream; offset
//!   0xff = unused), 8 streams (stride, classification), u8 element count,
//!   u8 stream count, 2 bytes padding.

use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeMap;

pub const RES_TYPE_MESHSET: u32 = 0x49b1_56d4;
const LOD_SIZE: usize = 192;
const SECTION_SIZE: usize = 304;
const DECL_SIZE: usize = 84;
const MAX_ELEMENTS: usize = 16;
const MAX_STREAMS: usize = 8;
/// Index format code seen with 16-bit index buffers.
pub const INDEX_FORMAT_U16: u32 = 0x21;

/// Vertex element usage codes (worked out from the data; see MODLOG.md).
pub mod usage {
    pub const POSITION: u8 = 0x01;
    pub const BLEND_INDICES: u8 = 0x02;
    pub const NORMAL: u8 = 0x06;
    pub const TANGENT: u8 = 0x07;
    pub const BINORMAL_SIGN: u8 = 0x09;
    pub const TEXCOORD0: u8 = 0x21;
    /// Second texture coordinate set (repeating textures on building meshes
    /// whose first set is a detail layout; DESIGN.md "flat grey walls").
    pub const TEXCOORD1: u8 = 0x22;
}

/// Subset categories of a LOD.
pub mod category {
    pub const OPAQUE: usize = 0;
    pub const TRANSPARENT: usize = 1;
    pub const TRANSPARENT_DECAL: usize = 2;
    pub const Z_ONLY: usize = 3;
    pub const SHADOW: usize = 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element {
    pub usage: u8,
    pub format: u8,
    pub offset: u8,
    pub stream: u8,
}

#[derive(Debug, Clone, Default)]
pub struct Declaration {
    pub elements: Vec<Element>,
    pub stream_strides: Vec<u8>,
}

impl Declaration {
    pub fn find(&self, usage: u8) -> Option<&Element> {
        self.elements.iter().find(|e| e.usage == usage)
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub material_name: String,
    pub material_id: i32,
    pub primitive_count: u32,
    pub start_index: u32,
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub stride: u8,
    pub primitive_type: u8,
    pub declaration: Declaration,
}

#[derive(Debug, Clone)]
pub struct Lod {
    pub name: String,
    pub mesh_type: u32,
    pub sections: Vec<Section>,
    /// Section indices for each subset category (see `category`).
    pub categories: [Vec<u8>; 5],
    pub index_format: u32,
    pub index_buffer_size: u32,
    pub vertex_buffer_size: u32,
    /// Chunk with vertex buffer then index buffer. None if the data is inline.
    pub chunk_id: Option<[u8; 16]>,
    pub inline_offset: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct MeshSet {
    pub full_name: String,
    pub mesh_type: u32,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub lods: Vec<Lod>,
}

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn bytes(&self, at: usize, n: usize) -> Result<&[u8]> {
        self.0.get(at..at + n).with_context(|| format!("meshset: read past end at 0x{at:x}"))
    }
    fn u8(&self, at: usize) -> Result<u8> {
        Ok(self.bytes(at, 1)?[0])
    }
    fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(at, 2)?.try_into().unwrap()))
    }
    fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(at, 4)?.try_into().unwrap()))
    }
    fn u64(&self, at: usize) -> Result<usize> {
        Ok(u64::from_le_bytes(self.bytes(at, 8)?.try_into().unwrap()) as usize)
    }
    fn f32(&self, at: usize) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(at, 4)?.try_into().unwrap()))
    }
    fn cstr(&self, at: usize) -> String {
        let rest = self.0.get(at..).unwrap_or(&[]);
        let n = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..n]).into_owned()
    }
}

pub fn parse_meshset(data: &[u8]) -> Result<MeshSet> {
    let r = R(data);
    let bbox_min = [r.f32(0)?, r.f32(4)?, r.f32(8)?];
    let bbox_max = [r.f32(16)?, r.f32(20)?, r.f32(24)?];
    let lod_offsets: Vec<usize> = (0..6).map(|i| r.u64(0x20 + i * 8)).collect::<Result<_>>()?;
    let full_name = r.cstr(r.u64(0x58)?);
    let mesh_type = r.u32(0x6c)?;
    let lod_count = r.u16(0x8c)? as usize;
    ensure!(lod_count <= 6, "meshset: {lod_count} LODs");

    let mut lods = Vec::with_capacity(lod_count);
    for &lod_at in lod_offsets.iter().take(lod_count) {
        ensure!(lod_at + LOD_SIZE <= data.len(), "meshset: LOD past end");
        let section_count = r.u32(lod_at + 8)? as usize;
        let section_at = r.u64(lod_at + 12)?;
        let mut categories: [Vec<u8>; 5] = Default::default();
        for (c, cat) in categories.iter_mut().enumerate() {
            let at = lod_at + 20 + c * 12;
            let n = r.u32(at)? as usize;
            let off = r.u64(at + 4)?;
            *cat = r.bytes(off, n)?.to_vec();
        }
        let p = lod_at + 80;
        let chunk: [u8; 16] = r.bytes(p + 20, 16)?.try_into().unwrap();
        let inline = r.u32(p + 36)?;
        let mut sections = Vec::with_capacity(section_count);
        for s in 0..section_count {
            sections.push(parse_section(&r, section_at + s * SECTION_SIZE)?);
        }
        lods.push(Lod {
            name: r.cstr(r.u64(p + 56)?),
            mesh_type: r.u32(lod_at)?,
            sections,
            categories,
            index_format: r.u32(p + 4)?,
            index_buffer_size: r.u32(p + 8)?,
            vertex_buffer_size: r.u32(p + 12)?,
            chunk_id: if chunk == [0; 16] { None } else { Some(chunk) },
            inline_offset: if inline == u32::MAX { None } else { Some(inline) },
        });
    }
    Ok(MeshSet { full_name, mesh_type, bbox_min, bbox_max, lods })
}

fn parse_section(r: &R, at: usize) -> Result<Section> {
    let decl_at = at + 76;
    let mut elements = Vec::new();
    for i in 0..MAX_ELEMENTS {
        let e = r.bytes(decl_at + i * 4, 4)?;
        if e[2] != 0xff {
            elements.push(Element { usage: e[0], format: e[1], offset: e[2], stream: e[3] });
        }
    }
    let stream_count = r.u8(decl_at + MAX_ELEMENTS * 4 + MAX_STREAMS * 2 + 1)? as usize;
    let stream_strides =
        (0..stream_count.min(MAX_STREAMS)).map(|i| r.u8(decl_at + MAX_ELEMENTS * 4 + i * 2)).collect::<Result<_>>()?;
    let _ = DECL_SIZE;
    Ok(Section {
        material_name: r.cstr(r.u64(at + 16)?),
        material_id: r.u32(at + 24)? as i32,
        primitive_count: r.u32(at + 32)?,
        start_index: r.u32(at + 36)?,
        vertex_offset: r.u32(at + 40)?,
        vertex_count: r.u32(at + 44)?,
        stride: r.u8(at + 48)?,
        primitive_type: r.u8(at + 49)?,
        declaration: Declaration { elements, stream_strides },
    })
}

/// Convert an IEEE half-float to f32.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((f & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

/// Read one vertex element as up to four floats. None for formats we do not know.
pub fn read_element(format: u8, b: &[u8]) -> Option<[f32; 4]> {
    let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    let h = |i: usize| half_to_f32(u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]));
    Some(match format {
        0x01 => [f(0), 0.0, 0.0, 0.0],
        0x02 => [f(0), f(1), 0.0, 0.0],
        0x03 => [f(0), f(1), f(2), 0.0],
        0x04 => [f(0), f(1), f(2), f(3)],
        0x05 => [h(0), 0.0, 0.0, 0.0],
        0x06 => [h(0), h(1), 0.0, 0.0],
        0x07 => [h(0), h(1), h(2), 0.0],
        0x08 => [h(0), h(1), h(2), h(3)],
        0x0c => [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32],
        0x0d => [b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0, b[3] as f32 / 255.0],
        _ => return None,
    })
}

/// Byte size of a vertex element format, if known.
pub fn format_size(format: u8) -> Option<usize> {
    Some(match format {
        0x01 => 4,
        0x02 => 8,
        0x03 => 12,
        0x04 => 16,
        0x05 => 2,
        0x06 => 4,
        0x07 => 6,
        0x08 => 8,
        0x0c => 4,
        0x0d => 4,
        _ => return None,
    })
}

/// A part of the geometry drawn with one material.
#[derive(Debug, Clone)]
pub struct SubMesh {
    pub material_id: i32,
    pub transparent: bool,
    /// From the "transparent decal" category (drawn over other surfaces).
    pub decal: bool,
    /// Range in `Geometry::indices`.
    pub first_index: usize,
    pub index_count: usize,
}

/// Decoded geometry of one LOD, ready to draw.
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub positions: Vec<[f32; 3]>,
    /// Empty if the mesh has no normals we can read.
    pub normals: Vec<[f32; 3]>,
    /// Empty if the mesh has no texture coordinates we can read.
    pub uvs: Vec<[f32; 2]>,
    /// Second texture coordinate set (usage 0x22); empty if no section has one.
    pub uvs1: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SubMesh>,
}

#[derive(Debug, Default, Clone)]
pub struct DecodeStats {
    pub unknown_formats: BTreeMap<(u8, u8), usize>,
    pub sections_skipped: usize,
}

/// Decode the opaque, transparent and transparent decal sections of a LOD
/// from its buffer (vertex buffer followed by index buffer).
pub fn decode_lod(lod: &Lod, buffer: &[u8], stats: &mut DecodeStats) -> Result<Geometry> {
    let vb_size = lod.vertex_buffer_size as usize;
    let ib_size = lod.index_buffer_size as usize;
    ensure!(buffer.len() >= vb_size + ib_size, "buffer {} bytes, need {}", buffer.len(), vb_size + ib_size);
    let index_size = if lod.index_format == INDEX_FORMAT_U16 { 2 } else { 4 };
    let ib = &buffer[vb_size..vb_size + ib_size];

    let mut g = Geometry::default();
    let mut drawn: Vec<(usize, bool, bool)> = Vec::new();
    for (cat, transparent, decal) in [(category::OPAQUE, false, false), (category::TRANSPARENT, true, false), (category::TRANSPARENT_DECAL, true, true)] {
        for &s in &lod.categories[cat] {
            if !drawn.iter().any(|(d, _, _)| *d == s as usize) {
                drawn.push((s as usize, transparent, decal));
            }
        }
    }

    for (si, transparent, decal) in drawn {
        let s = lod.sections.get(si).context("category refers to a missing section")?;
        if s.primitive_type != 3 {
            stats.sections_skipped += 1;
            continue;
        }
        let d = &s.declaration;
        let Some(pos) = d.find(usage::POSITION) else {
            stats.sections_skipped += 1;
            continue;
        };
        let stream_base = |stream: u8| -> usize {
            let before: usize = d.stream_strides.iter().take(stream as usize).map(|&st| st as usize * s.vertex_count as usize).sum();
            s.vertex_offset as usize + before
        };
        let stride_of = |stream: u8| -> usize { d.stream_strides.get(stream as usize).copied().unwrap_or(s.stride) as usize };
        let read = |e: &Element, v: usize| -> Option<[f32; 4]> {
            let at = stream_base(e.stream) + v * stride_of(e.stream) + e.offset as usize;
            let n = format_size(e.format)?;
            read_element(e.format, buffer.get(at..at + n)?)
        };
        for e in &d.elements {
            if format_size(e.format).is_none() {
                *stats.unknown_formats.entry((e.usage, e.format)).or_default() += 1;
            }
        }
        let normal = d.find(usage::NORMAL).filter(|e| format_size(e.format).is_some());
        let uv = d.find(usage::TEXCOORD0).filter(|e| format_size(e.format).is_some());
        let uv1 = d.find(usage::TEXCOORD1).filter(|e| format_size(e.format).is_some());

        let base = g.positions.len() as u32;
        // A section with a second set after sections without one: pad first.
        if uv1.is_some() && g.uvs1.len() < base as usize {
            g.uvs1.resize(base as usize, [0.0, 0.0]);
        }
        for v in 0..s.vertex_count as usize {
            let p = read(pos, v).with_context(|| format!("position of vertex {v} (format 0x{:02x})", pos.format))?;
            g.positions.push([p[0], p[1], p[2]]);
            if let Some(n) = normal {
                let n = read(n, v).unwrap_or([0.0, 1.0, 0.0, 0.0]);
                g.normals.push([n[0], n[1], n[2]]);
            }
            if let Some(t) = uv {
                let t = read(t, v).unwrap_or([0.0; 4]);
                g.uvs.push([t[0], t[1]]);
            }
            if let Some(t) = uv1 {
                let t = read(t, v).unwrap_or([0.0; 4]);
                g.uvs1.push([t[0], t[1]]);
            }
        }
        // Keep the attribute arrays the same length when some sections lack them.
        if normal.is_none() && !g.normals.is_empty() {
            g.normals.resize(g.positions.len(), [0.0, 1.0, 0.0]);
        }
        if uv.is_none() && !g.uvs.is_empty() {
            g.uvs.resize(g.positions.len(), [0.0, 0.0]);
        }
        if !g.uvs1.is_empty() {
            g.uvs1.resize(g.positions.len(), [0.0, 0.0]);
        }

        let first = g.indices.len();
        let count = s.primitive_count as usize * 3;
        for i in 0..count {
            let at = (s.start_index as usize + i) * index_size;
            let idx = match index_size {
                2 => u16::from_le_bytes(ib.get(at..at + 2).context("index past end")?.try_into().unwrap()) as u32,
                _ => u32::from_le_bytes(ib.get(at..at + 4).context("index past end")?.try_into().unwrap()),
            };
            if idx >= s.vertex_count {
                bail!("index {idx} >= vertex count {} in section {si}", s.vertex_count);
            }
            g.indices.push(base + idx);
        }
        g.submeshes.push(SubMesh { material_id: s.material_id, transparent, decal, first_index: first, index_count: count });
    }
    // Attribute arrays must match positions, or be empty.
    if !g.normals.is_empty() && g.normals.len() != g.positions.len() {
        g.normals.clear();
    }
    if !g.uvs.is_empty() && g.uvs.len() != g.positions.len() {
        g.uvs.clear();
    }
    if !g.uvs1.is_empty() && g.uvs1.len() != g.positions.len() {
        g.uvs1.clear();
    }
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x3555), 0.333_251_95);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert!((half_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12);
    }
}
