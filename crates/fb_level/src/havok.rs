//! Minimal reader for Havok 2013 binary packfiles (magic `57 E0 E0 57 10 C0 C0 10`).
//!
//! We only need enough to find objects by class name and follow their pointers:
//! - header: version, layout rules (pointer size), section count
//! - sections: `__classnames__`, `__types__`, `__data__`, each with fix-up tables
//! - local fix-ups: (pointer location, target) pairs inside one section
//! - virtual fix-ups: (object location, class name offset) - one per object
//!
//! Format knowledge comes from the public Havok packfile layout (as used by
//! many open tools). No Havok SDK code is used.

use anyhow::{bail, ensure, Context, Result};
use std::collections::HashMap;

pub const MAGIC: [u8; 8] = [0x57, 0xe0, 0xe0, 0x57, 0x10, 0xc0, 0xc0, 0x10];

#[derive(Debug, Clone)]
pub struct Section {
    pub tag: String,
    /// Absolute start of the section data in the packfile.
    pub start: usize,
    pub local_fixups: usize,
    pub global_fixups: usize,
    pub virtual_fixups: usize,
    pub exports: usize,
    pub imports: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct HkObject {
    /// Offset inside the data section.
    pub offset: usize,
    pub class: String,
}

pub struct Packfile<'a> {
    pub data: &'a [u8],
    pub version: u32,
    pub pointer_size: usize,
    pub contents_version: String,
    pub sections: Vec<Section>,
    /// Objects in the data section, in file order.
    pub objects: Vec<HkObject>,
    /// Local pointers in the data section: pointer location -> target (both section offsets).
    pub pointers: HashMap<usize, usize>,
    data_section: usize,
}

fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(at..at + 4).context("havok: read past end")?.try_into().unwrap()))
}

fn cstr(d: &[u8], at: usize) -> String {
    let rest = &d[at.min(d.len())..];
    let len = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..len]).into_owned()
}

/// Find the packfile inside a buffer (Frostbite puts its own header in front).
pub fn find_packfile(buf: &[u8]) -> Option<usize> {
    buf.windows(8).position(|w| w == MAGIC)
}

impl<'a> Packfile<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Packfile<'a>> {
        ensure!(data.len() >= 0x40 && data[..8] == MAGIC, "not a Havok packfile");
        let version = u32_at(data, 0x0c)?;
        ensure!(version == 11, "Havok packfile version {version} (only 11 is supported)");
        let pointer_size = data[0x10] as usize;
        ensure!(data[0x11] == 1, "big-endian Havok packfile");
        let num_sections = u32_at(data, 0x14)? as usize;
        let contents_version = cstr(data, 0x28);
        let predicate_padding = i16::from_le_bytes([data[0x3e], data[0x3f]]).max(0) as usize;

        let mut sections = Vec::with_capacity(num_sections);
        let mut at = 0x40 + predicate_padding;
        for _ in 0..num_sections {
            let tag = cstr(data, at);
            let start = u32_at(data, at + 20)? as usize;
            let rel = |o: usize| -> Result<usize> { Ok(start + u32_at(data, at + o)? as usize) };
            sections.push(Section {
                tag,
                start,
                local_fixups: rel(24)?,
                global_fixups: rel(28)?,
                virtual_fixups: rel(32)?,
                exports: rel(36)?,
                imports: rel(40)?,
                end: rel(44)?,
            });
            at += 0x40;
        }

        let class_section = sections.iter().position(|s| s.tag == "__classnames__").context("no __classnames__ section")?;
        let data_section = sections.iter().position(|s| s.tag == "__data__").context("no __data__ section")?;
        let ds = sections[data_section].clone();
        let cs = sections[class_section].clone();
        ensure!(ds.end <= data.len(), "data section past end of file");

        // Local fix-ups: pairs of (src, dst), terminated or padded with 0xFFFFFFFF.
        let mut pointers = HashMap::new();
        let mut p = ds.local_fixups;
        while p + 8 <= ds.global_fixups {
            let src = u32_at(data, p)?;
            if src != 0xffff_ffff {
                pointers.insert(src as usize, u32_at(data, p + 4)? as usize);
            }
            p += 8;
        }

        // Virtual fix-ups: triples of (object offset, class section index, class name offset).
        let mut objects = Vec::new();
        let mut p = ds.virtual_fixups;
        while p + 12 <= ds.exports {
            let src = u32_at(data, p)?;
            if src != 0xffff_ffff {
                let name_off = u32_at(data, p + 8)? as usize;
                objects.push(HkObject { offset: src as usize, class: cstr(data, cs.start + name_off) });
            }
            p += 12;
        }
        objects.sort_by_key(|o| o.offset);

        Ok(Packfile { data, version, pointer_size, contents_version, sections, objects, pointers, data_section })
    }

    /// Bytes of the data section.
    pub fn data_bytes(&self) -> &'a [u8] {
        let s = &self.sections[self.data_section];
        &self.data[s.start..s.end]
    }

    /// Byte length of an object: up to the next object (or end of data section).
    pub fn object_len(&self, index: usize) -> usize {
        let end = self.objects.get(index + 1).map(|o| o.offset).unwrap_or(self.data_bytes().len());
        end - self.objects[index].offset
    }

    pub fn find_class(&self, class: &str) -> Vec<usize> {
        (0..self.objects.len()).filter(|&i| self.objects[i].class == class).collect()
    }

    /// Arrays inside an object: (field offset in object, target offset, count).
    /// An hkArray is a pointer followed by a 32-bit size and a 32-bit capacity.
    pub fn arrays_in(&self, index: usize) -> Vec<(usize, usize, usize)> {
        let o = &self.objects[index];
        let d = self.data_bytes();
        let mut out = Vec::new();
        for field in 0..self.object_len(index) {
            let at = o.offset + field;
            if let Some(&target) = self.pointers.get(&at) {
                if let Ok(count) = u32_at(d, at + self.pointer_size) {
                    out.push((field, target, count as usize));
                }
            }
        }
        out.sort();
        out
    }
}

/// Read four floats.
pub fn vec4(d: &[u8], at: usize) -> Result<[f32; 4]> {
    let b = d.get(at..at + 16).context("havok: vector past end")?;
    Ok([0, 4, 8, 12].map(|i| f32::from_le_bytes(b[i..i + 4].try_into().unwrap())))
}

pub fn check_magic(buf: &[u8]) -> Result<usize> {
    match find_packfile(buf) {
        Some(at) => Ok(at),
        None => bail!("no Havok packfile in buffer"),
    }
}
