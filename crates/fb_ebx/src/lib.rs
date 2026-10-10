//! EBX: Frostbite asset files ("partitions"). Battlefield 1 uses the legacy
//! little-endian layout with magic `CE D1 B2 0F`. Each file carries its own
//! type descriptors (names, fields, offsets), so no external type list is needed.
//!
//! Layout notes were learned from Frostbite-Scripts (MIT, NicknineTheEagle,
//! Frankelstner, NoFaTe). See CREDITS.md. No code was copied.
//!
//! File layout:
//!   magic u32
//!   header: string offset u32, length string section to EOF u32, external GUID count u32,
//!           instance repeater count u16, GUID repeater count u16, unknown u16,
//!           complex count u16, field count u16, keyword section length u16,
//!           string section length u32, array repeater count u32, payload length u32
//!   file GUID (16), pad to 16
//!   external references: (file GUID, instance GUID) pairs
//!   keywords: null-separated names (fields refer to them by hash)
//!   field descriptors (16 bytes), complex descriptors (16 bytes),
//!   instance repeaters (4 bytes), pad to 16, array repeaters (12 bytes)
//!   strings at `string offset`, payload after the strings, arrays after the payload

pub mod text;

use anyhow::{bail, ensure, Context, Result};
use std::collections::HashMap;

pub const MAGIC_LE: [u8; 4] = [0xce, 0xd1, 0xb2, 0x0f];

pub type Guid = [u8; 16];

/// Field types (from the type bits of a field descriptor).
pub mod field_type {
    pub const VOID: u8 = 0x00; // base class
    pub const DB_OBJECT: u8 = 0x01;
    pub const VALUE_TYPE: u8 = 0x02; // embedded struct
    pub const CLASS: u8 = 0x03; // pointer to an instance
    pub const ARRAY: u8 = 0x04;
    pub const FIXED_ARRAY: u8 = 0x05;
    pub const STRING: u8 = 0x06;
    pub const CSTRING: u8 = 0x07;
    pub const ENUM: u8 = 0x08;
    pub const FILE_REF: u8 = 0x09;
    pub const BOOLEAN: u8 = 0x0a;
    pub const INT8: u8 = 0x0b;
    pub const UINT8: u8 = 0x0c;
    pub const INT16: u8 = 0x0d;
    pub const UINT16: u8 = 0x0e;
    pub const INT32: u8 = 0x0f;
    pub const UINT32: u8 = 0x10;
    pub const INT64: u8 = 0x11;
    pub const UINT64: u8 = 0x12;
    pub const FLOAT32: u8 = 0x13;
    pub const FLOAT64: u8 = 0x14;
    pub const GUID: u8 = 0x15;
    pub const SHA1: u8 = 0x16;
    pub const RESOURCE_REF: u8 = 0x17;
}

#[derive(Debug, Clone)]
pub struct FieldDesc {
    pub name: String,
    pub type_bits: u16,
    pub reference: u16,
    pub offset: i32,
    pub secondary_offset: i32,
}

impl FieldDesc {
    pub fn kind(&self) -> u8 {
        ((self.type_bits >> 4) & 0x1f) as u8
    }
}

#[derive(Debug, Clone)]
pub struct ComplexDesc {
    pub name: String,
    pub field_start: u32,
    pub field_count: usize,
    pub alignment: usize,
    pub type_bits: u16,
    pub size: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pointer {
    Null,
    /// Index into `Partition::instances`.
    Internal(usize),
    /// (file GUID, instance GUID) of another partition.
    External(Guid, Guid),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    FileRef(String),
    Enum { value: i32, name: Option<String> },
    Guid(Guid),
    Sha1([u8; 20]),
    ResourceRef(u64),
    Pointer(Pointer),
    Array { element_type: String, items: Vec<Value> },
    Struct(Complex),
    Unknown(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Complex {
    pub type_name: String,
    pub fields: Vec<(String, Value)>,
}

impl Complex {
    /// Find a field by name, looking through base classes too.
    pub fn get(&self, name: &str) -> Option<&Value> {
        for (n, v) in &self.fields {
            if n == name {
                return Some(v);
            }
        }
        for (n, v) in &self.fields {
            if n == "$" {
                if let Value::Struct(base) = v {
                    return base.get(name);
                }
            }
        }
        None
    }

    /// True if this type or one of its base classes has this name.
    pub fn is_a(&self, type_name: &str) -> bool {
        self.type_name == type_name
            || self.fields.iter().any(|(n, v)| n == "$" && matches!(v, Value::Struct(b) if b.is_a(type_name)))
    }
}

#[derive(Debug, Clone)]
pub struct Instance {
    /// None for instances stored without a GUID.
    pub guid: Option<Guid>,
    pub value: Complex,
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub file_guid: Guid,
    pub external: Vec<(Guid, Guid)>,
    pub instances: Vec<Instance>,
}

impl Partition {
    /// The first instance. Its type is the asset type (for example "LevelData").
    pub fn primary(&self) -> Option<&Instance> {
        self.instances.first()
    }

    /// The `Name` field of the primary instance (the asset path).
    pub fn name(&self) -> Option<&str> {
        match self.primary()?.value.get("Name")? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn find_instance(&self, guid: &Guid) -> Option<usize> {
        self.instances.iter().position(|i| i.guid.as_ref() == Some(guid))
    }
}

/// Format a GUID as Frostbite prints it: the first three parts are
/// little-endian, the last 8 bytes are big-endian.
pub fn format_guid(g: &Guid) -> String {
    let a = u32::from_le_bytes(g[0..4].try_into().unwrap());
    let b = u16::from_le_bytes(g[4..6].try_into().unwrap());
    let c = u16::from_le_bytes(g[6..8].try_into().unwrap());
    let d = u64::from_be_bytes(g[8..16].try_into().unwrap());
    format!("{a:08x}-{b:04x}-{c:04x}-{:04x}-{:012x}", d >> 48, d & 0x0000_ffff_ffff_ffff)
}

/// 32-bit hash of a keyword (start 5381, multiply by 33, XOR each byte).
pub fn hash_keyword(s: &str) -> u32 {
    s.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32)
}

struct ArrayRepeater {
    offset: u32,
    count: u32,
}

struct Reader<'a> {
    data: &'a [u8],
    string_start: usize,
    array_start: usize,
    fields: Vec<FieldDesc>,
    complexes: Vec<ComplexDesc>,
    arrays: Vec<ArrayRepeater>,
    instance_guid_count: usize,
    external: Vec<(Guid, Guid)>,
}

fn u16_at(d: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(d.get(at..at + 2).context("read past end")?.try_into().unwrap()))
}
fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(at..at + 4).context("read past end")?.try_into().unwrap()))
}
fn guid_at(d: &[u8], at: usize) -> Result<Guid> {
    Ok(d.get(at..at + 16).context("read past end")?.try_into().unwrap())
}
fn align(pos: usize, to: usize) -> usize {
    if to <= 1 {
        pos
    } else {
        pos.div_ceil(to) * to
    }
}

/// Parse a whole EBX file.
pub fn parse(data: &[u8]) -> Result<Partition> {
    ensure!(data.len() >= 56, "too short for EBX");
    ensure!(data[0..4] == MAGIC_LE, "not a little-endian EBX (magic {:02x?})", &data[0..4]);
    let string_offset = u32_at(data, 4)? as usize;
    let external_count = u32_at(data, 12)? as usize;
    let instance_repeater_count = u16_at(data, 16)? as usize;
    let guid_repeater_count = u16_at(data, 18)? as usize;
    let complex_count = u16_at(data, 22)? as usize;
    let field_count = u16_at(data, 24)? as usize;
    let keyword_len = u16_at(data, 26)? as usize;
    let string_len = u32_at(data, 28)? as usize;
    let array_repeater_count = u32_at(data, 32)? as usize;
    let payload_len = u32_at(data, 36)? as usize;
    let file_guid = guid_at(data, 40)?;

    let mut pos = align(56, 16);
    let mut external = Vec::with_capacity(external_count);
    for _ in 0..external_count {
        external.push((guid_at(data, pos)?, guid_at(data, pos + 16)?));
        pos += 32;
    }

    let keywords_bytes = data.get(pos..pos + keyword_len).context("keyword section past end")?;
    let mut keywords: HashMap<u32, String> = HashMap::new();
    for k in keywords_bytes.split(|&b| b == 0) {
        let k = String::from_utf8_lossy(k).into_owned();
        keywords.insert(hash_keyword(&k), k);
    }
    pos += keyword_len;
    let name_of = |h: u32| keywords.get(&h).cloned().unwrap_or_else(|| format!("#{h:08x}"));

    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let name = name_of(u32_at(data, pos)?);
        let offset = u32_at(data, pos + 8)? as i32;
        fields.push(FieldDesc {
            offset: if name == "$" { offset - 8 } else { offset },
            name,
            type_bits: u16_at(data, pos + 4)?,
            reference: u16_at(data, pos + 6)?,
            secondary_offset: u32_at(data, pos + 12)? as i32,
        });
        pos += 16;
    }

    let mut complexes = Vec::with_capacity(complex_count);
    for _ in 0..complex_count {
        let field_count_lo = *data.get(pos + 8).context("read past end")? as usize;
        let alignment_raw = *data.get(pos + 9).context("read past end")? as usize;
        complexes.push(ComplexDesc {
            name: name_of(u32_at(data, pos)?),
            field_start: u32_at(data, pos + 4)?,
            // The 9th bit of the field count is kept in the top bit of the alignment byte.
            field_count: field_count_lo | ((alignment_raw << 1) & 0x100),
            alignment: alignment_raw & 0x7f,
            type_bits: u16_at(data, pos + 10)?,
            size: u16_at(data, pos + 12)?,
        });
        pos += 16;
    }

    let mut instance_repeaters = Vec::with_capacity(instance_repeater_count);
    for _ in 0..instance_repeater_count {
        instance_repeaters.push((u16_at(data, pos)? as usize, u16_at(data, pos + 2)? as usize));
        pos += 4;
    }
    pos = align(pos, 16);

    let mut arrays = Vec::with_capacity(array_repeater_count);
    for _ in 0..array_repeater_count {
        arrays.push(ArrayRepeater { offset: u32_at(data, pos)?, count: u32_at(data, pos + 4)? });
        pos += 12;
    }

    let payload_start = string_offset + string_len;
    let reader = Reader {
        data,
        string_start: string_offset,
        array_start: payload_start + payload_len,
        fields,
        complexes,
        arrays,
        instance_guid_count: guid_repeater_count,
        external: external.clone(),
    };

    let mut instances = Vec::new();
    let mut pos = payload_start;
    for (rep_index, &(complex_index, count)) in instance_repeaters.iter().enumerate() {
        let desc = reader.complexes.get(complex_index).context("instance repeater: bad complex index")?;
        for _ in 0..count {
            pos = align(pos, desc.alignment);
            let guid = if rep_index < reader.instance_guid_count {
                let g = guid_at(data, pos)?;
                pos += 16;
                Some(g)
            } else {
                None
            };
            let (value, end) = reader.read_complex(complex_index, pos, true)?;
            pos = end;
            instances.push(Instance { guid, value });
        }
    }

    Ok(Partition { file_guid, external, instances })
}

impl Reader<'_> {
    fn cstring(&self, at: usize) -> Result<String> {
        let rest = self.data.get(at..).context("string past end")?;
        let len = rest.iter().position(|&b| b == 0).context("unterminated string")?;
        Ok(String::from_utf8_lossy(&rest[..len]).into_owned())
    }

    /// Read a complex that starts at `start`. Returns the value and the position after it.
    fn read_complex(&self, index: usize, start: usize, is_instance: bool) -> Result<(Complex, usize)> {
        let desc = self.complexes.get(index).context("bad complex index")?;
        // Instances with alignment 4 store their fields 8 bytes lower than the descriptors say.
        let shift: i64 = if is_instance && desc.alignment == 4 { 8 } else { 0 };
        let mut fields = Vec::with_capacity(desc.field_count);
        for fi in desc.field_start as usize..desc.field_start as usize + desc.field_count {
            let fd = self.fields.get(fi).context("bad field index")?;
            let at = start as i64 + fd.offset as i64 - shift;
            ensure!(at >= 0, "negative field offset in {}", desc.name);
            let (value, _) = self.read_field(fi, at as usize).with_context(|| format!("{}.{}", desc.name, fd.name))?;
            fields.push((fd.name.clone(), value));
        }
        let end = (start as i64 + desc.size as i64 - shift) as usize;
        Ok((Complex { type_name: desc.name.clone(), fields }, end))
    }

    /// Read one field at `at`. Returns the value and the position after it.
    fn read_field(&self, field_index: usize, at: usize) -> Result<(Value, usize)> {
        use field_type::*;
        let fd = &self.fields[field_index];
        let d = self.data;
        let get = |n: usize| -> Result<&[u8]> { d.get(at..at + n).context("field past end of file") };
        Ok(match fd.kind() {
            VOID | VALUE_TYPE => {
                let (c, end) = self.read_complex(fd.reference as usize, at, false)?;
                (Value::Struct(c), end)
            }
            CLASS => {
                let raw = u32_at(d, at)?;
                let p = if raw & 0x8000_0000 != 0 {
                    let i = (raw & 0x7fff_ffff) as usize;
                    let (file, inst) = *self.external.get(i).context("bad external reference index")?;
                    Pointer::External(file, inst)
                } else if raw == 0 {
                    Pointer::Null
                } else {
                    Pointer::Internal(raw as usize - 1)
                };
                (Value::Pointer(p), at + 4)
            }
            ARRAY => {
                let rep = self.arrays.get(u32_at(d, at)? as usize).context("bad array repeater index")?;
                let elem_desc = self.complexes.get(fd.reference as usize).context("bad array complex")?;
                let elem_field = elem_desc.field_start as usize;
                ensure!(elem_field < self.fields.len(), "bad array element field");
                let mut pos = self.array_start + rep.offset as usize;
                let mut items = Vec::with_capacity(rep.count.min(1 << 20) as usize);
                for _ in 0..rep.count {
                    let (v, end) = self.read_field(elem_field, pos)?;
                    items.push(v);
                    pos = end;
                }
                (Value::Array { element_type: elem_desc.name.clone(), items }, at + 4)
            }
            CSTRING | FILE_REF => {
                let off = u32_at(d, at)? as i32;
                let s = if off == -1 { String::new() } else { self.cstring(self.string_start + off as usize)? };
                let v = if fd.kind() == CSTRING { Value::String(s) } else { Value::FileRef(s) };
                (v, at + 4)
            }
            ENUM => {
                let value = u32_at(d, at)? as i32;
                let name = self.complexes.get(fd.reference as usize).and_then(|e| {
                    (e.field_start as usize..e.field_start as usize + e.field_count)
                        .filter_map(|i| self.fields.get(i))
                        .find(|f| f.offset == value)
                        .map(|f| f.name.clone())
                });
                (Value::Enum { value, name }, at + 4)
            }
            BOOLEAN => (Value::Bool(get(1)?[0] != 0), at + 1),
            INT8 => (Value::Int(get(1)?[0] as i8 as i64), at + 1),
            UINT8 => (Value::UInt(get(1)?[0] as u64), at + 1),
            INT16 => (Value::Int(u16_at(d, at)? as i16 as i64), at + 2),
            UINT16 => (Value::UInt(u16_at(d, at)? as u64), at + 2),
            INT32 => (Value::Int(u32_at(d, at)? as i32 as i64), at + 4),
            UINT32 => (Value::UInt(u32_at(d, at)? as u64), at + 4),
            INT64 => (Value::Int(i64::from_le_bytes(get(8)?.try_into().unwrap())), at + 8),
            UINT64 => (Value::UInt(u64::from_le_bytes(get(8)?.try_into().unwrap())), at + 8),
            FLOAT32 => (Value::Float(f32::from_le_bytes(get(4)?.try_into().unwrap()) as f64), at + 4),
            FLOAT64 => (Value::Float(f64::from_le_bytes(get(8)?.try_into().unwrap())), at + 8),
            GUID => (Value::Guid(guid_at(d, at)?), at + 16),
            SHA1 => (Value::Sha1(get(20)?.try_into().unwrap()), at + 20),
            RESOURCE_REF => (Value::ResourceRef(u64::from_le_bytes(get(8)?.try_into().unwrap())), at + 8),
            other => bail!("unknown field type 0x{other:02x}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_hash_matches_reference_algorithm() {
        // 5381 * 33 ^ 'a' (0x61), computed by hand.
        assert_eq!(hash_keyword(""), 5381);
        assert_eq!(hash_keyword("a"), (5381u32 * 33) ^ 0x61);
    }

    #[test]
    fn guid_format_mixes_endianness() {
        let g: Guid = [0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(format_guid(&g), "12345678-9abc-def0-0102-030405060708");
    }

    #[test]
    fn rejects_non_ebx() {
        assert!(parse(&[0u8; 64]).is_err());
    }
}
