//! DbObject: Frostbite's binary JSON-like format, used by .toc files and bundles.
//!
//! Each element is: one header byte (low 5 bits = type, bit 7 = no name),
//! a null-terminated name (unless the no-name bit is set), then the value.
//! Lists and objects store their byte length as a 7-bit varint and end with a 0 byte.

use anyhow::{bail, ensure, Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum DbValue {
    Null,
    Bool(bool),
    String(String),
    Int(u32),
    Long(u64),
    VarInt(i64),
    Float(f32),
    Double(f64),
    Guid([u8; 16]),
    Sha1([u8; 20]),
    Blob(Vec<u8>),
    List(Vec<DbValue>),
    Object(Vec<(String, DbValue)>),
    /// Types we keep as raw bytes because we have no use for them yet.
    Raw { type_id: u8, bytes: Vec<u8> },
}

impl DbValue {
    pub fn get(&self, name: &str) -> Option<&DbValue> {
        match self {
            DbValue::Object(fields) => fields.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[DbValue]> {
        match self {
            DbValue::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            DbValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            DbValue::Int(v) => Some(v as i64),
            DbValue::Long(v) => Some(v as i64),
            DbValue::VarInt(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            DbValue::Bool(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_sha1(&self) -> Option<&[u8; 20]> {
        match self {
            DbValue::Sha1(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_guid(&self) -> Option<&[u8; 16]> {
        match self {
            DbValue::Guid(v) => Some(v),
            _ => None,
        }
    }

    /// Short type name, for logs.
    pub fn type_name(&self) -> &'static str {
        match self {
            DbValue::Null => "null",
            DbValue::Bool(_) => "bool",
            DbValue::String(_) => "string",
            DbValue::Int(_) => "int",
            DbValue::Long(_) => "long",
            DbValue::VarInt(_) => "varint",
            DbValue::Float(_) => "float",
            DbValue::Double(_) => "double",
            DbValue::Guid(_) => "guid",
            DbValue::Sha1(_) => "sha1",
            DbValue::Blob(_) => "blob",
            DbValue::List(_) => "list",
            DbValue::Object(_) => "object",
            DbValue::Raw { .. } => "raw",
        }
    }
}

/// Parse one DbObject element that starts at `data[0]`. Returns the value and
/// the number of bytes it used.
pub fn parse(data: &[u8]) -> Result<(DbValue, usize)> {
    let mut r = Reader { data, pos: 0 };
    let (_, value) = r.element()?;
    Ok((value, r.pos))
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("size overflow")?;
        ensure!(end <= self.data.len(), "DbObject: read past end at 0x{:x} (+{n})", self.pos);
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<u64> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            ensure!(shift < 64, "DbObject: varint too long at 0x{:x}", self.pos);
        }
    }

    fn cstring(&mut self) -> Result<String> {
        let rest = &self.data[self.pos..];
        let len = rest.iter().position(|&b| b == 0).context("DbObject: unterminated name")?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    fn element(&mut self) -> Result<(String, DbValue)> {
        let start = self.pos;
        let header = self.u8()?;
        let type_id = header & 0x1f;
        let name = if header & 0x80 != 0 { String::new() } else { self.cstring()? };

        let value = match type_id {
            0x01 | 0x02 => {
                let size = self.varint()? as usize;
                let end = self.pos + size;
                ensure!(end <= self.data.len() && size >= 1, "DbObject: bad container size at 0x{start:x}");
                let mut fields = Vec::new();
                while self.pos < end - 1 {
                    fields.push(self.element()?);
                }
                ensure!(self.pos == end - 1 && self.u8()? == 0, "DbObject: container at 0x{start:x} does not end with 0");
                if type_id == 0x01 {
                    DbValue::List(fields.into_iter().map(|(_, v)| v).collect())
                } else {
                    DbValue::Object(fields)
                }
            }
            0x04 => DbValue::Null,
            0x05 => DbValue::Raw { type_id, bytes: self.take(12)?.to_vec() },
            0x06 => DbValue::Bool(self.u8()? != 0),
            0x07 => {
                let len = self.varint()? as usize;
                ensure!(len >= 1, "DbObject: empty string record at 0x{start:x}");
                let bytes = self.take(len)?;
                DbValue::String(String::from_utf8_lossy(&bytes[..len - 1]).into_owned())
            }
            0x08 => DbValue::Int(u32::from_le_bytes(self.array()?)),
            0x09 => DbValue::Long(u64::from_le_bytes(self.array()?)),
            0x0a => {
                let v = self.varint()?;
                // Zigzag encoding: the low bit is the sign.
                DbValue::VarInt(((v >> 1) as i64) ^ -((v & 1) as i64))
            }
            0x0b => DbValue::Float(f32::from_le_bytes(self.array()?)),
            0x0c => DbValue::Double(f64::from_le_bytes(self.array()?)),
            0x0d => DbValue::Raw { type_id, bytes: self.take(8)?.to_vec() },
            0x0e => DbValue::Raw { type_id, bytes: self.take(6)?.to_vec() },
            0x0f => DbValue::Guid(self.array()?),
            0x10 => DbValue::Sha1(self.array()?),
            0x11 => DbValue::Raw { type_id, bytes: self.take(64)?.to_vec() },
            0x12 => DbValue::Raw { type_id, bytes: self.take(16)?.to_vec() },
            0x13 => {
                let len = self.varint()? as usize;
                DbValue::Blob(self.take(len)?.to_vec())
            }
            0x14 => DbValue::Raw { type_id, bytes: self.take(20)?.to_vec() },
            0x15 => DbValue::Long(self.varint()?),
            other => bail!("DbObject: unknown type 0x{other:02x} at 0x{start:x}"),
        };
        Ok((name, value))
    }
}

/// Print the shape of a value: field names, types, list lengths and a few
/// sample values. Lists show only their first `max_items` entries.
pub fn describe(value: &DbValue, max_items: usize) -> String {
    let mut out = String::new();
    describe_into(&mut out, "", value, 0, max_items);
    out
}

fn describe_into(out: &mut String, name: &str, value: &DbValue, depth: usize, max_items: usize) {
    use std::fmt::Write;
    let indent = "  ".repeat(depth);
    let label = if name.is_empty() { String::from("-") } else { name.to_string() };
    match value {
        DbValue::Object(fields) => {
            let _ = writeln!(out, "{indent}{label}: object ({} fields)", fields.len());
            for (n, v) in fields {
                describe_into(out, n, v, depth + 1, max_items);
            }
        }
        DbValue::List(items) => {
            let _ = writeln!(out, "{indent}{label}: list ({} items)", items.len());
            for v in items.iter().take(max_items) {
                describe_into(out, "", v, depth + 1, max_items);
            }
            if items.len() > max_items {
                let _ = writeln!(out, "{indent}  ... {} more", items.len() - max_items);
            }
        }
        DbValue::String(s) => {
            let _ = writeln!(out, "{indent}{label}: string {s:?}");
        }
        DbValue::Sha1(b) => {
            let _ = writeln!(out, "{indent}{label}: sha1 {}", hex(b));
        }
        DbValue::Guid(b) => {
            let _ = writeln!(out, "{indent}{label}: guid {}", hex(b));
        }
        DbValue::Blob(b) => {
            let _ = writeln!(out, "{indent}{label}: blob ({} bytes) {}", b.len(), hex(&b[..b.len().min(16)]));
        }
        DbValue::Raw { type_id, bytes } => {
            let _ = writeln!(out, "{indent}{label}: raw type 0x{type_id:02x} {}", hex(bytes));
        }
        other => {
            let _ = writeln!(out, "{indent}{label}: {} {:?}", other.type_name(), other);
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_object_with_fields() {
        // Anonymous object { "a": int 5, "s": "hi", "l": [true] }
        let mut body = vec![];
        body.extend_from_slice(&[0x08, b'a', 0, 5, 0, 0, 0]);
        body.extend_from_slice(&[0x07, b's', 0, 3, b'h', b'i', 0]);
        body.extend_from_slice(&[0x01, b'l', 0, 3, 0x86, 1, 0]);
        body.push(0);
        let mut data = vec![0x82, body.len() as u8];
        data.extend_from_slice(&body);

        let (v, used) = parse(&data).unwrap();
        assert_eq!(used, data.len());
        assert_eq!(v.get("a").and_then(DbValue::as_i64), Some(5));
        assert_eq!(v.get("s").and_then(DbValue::as_str), Some("hi"));
        assert_eq!(v.get("l").and_then(DbValue::as_list).map(|l| l.len()), Some(1));
    }

    #[test]
    fn varint_is_zigzag() {
        let (v, _) = parse(&[0x8a, 0x03]).unwrap();
        assert_eq!(v, DbValue::VarInt(-2));
        let (v, _) = parse(&[0x8a, 0x04]).unwrap();
        assert_eq!(v, DbValue::VarInt(2));
    }
}
