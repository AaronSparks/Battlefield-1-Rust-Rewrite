//! Write a partition as indented text, one field per line.

use crate::{format_guid, Complex, Guid, Partition, Pointer, Value};
use std::fmt::Write;

/// Looks up names for things the text refers to.
pub trait Names {
    /// Asset name of a partition, by its file GUID.
    fn partition_name(&self, _file_guid: &Guid) -> Option<String> {
        None
    }
    /// Res name, by its resource id.
    fn res_name(&self, _rid: u64) -> Option<String> {
        None
    }
}

pub struct NoNames;
impl Names for NoNames {}

pub fn to_text(p: &Partition, names: &dyn Names) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}  file {}", p.name().unwrap_or("?"), format_guid(&p.file_guid));
    for (i, inst) in p.instances.iter().enumerate() {
        let id = match &inst.guid {
            Some(g) => format_guid(g),
            None => format!("#{i}"),
        };
        let _ = writeln!(out, "{} {id}", inst.value.type_name);
        write_fields(&mut out, p, &inst.value, 1, names);
    }
    out
}

fn write_fields(out: &mut String, p: &Partition, c: &Complex, depth: usize, names: &dyn Names) {
    for (name, v) in &c.fields {
        write_value(out, p, name, v, depth, names);
    }
}

fn write_value(out: &mut String, p: &Partition, label: &str, v: &Value, depth: usize, names: &dyn Names) {
    let ind = "\t".repeat(depth);
    match v {
        Value::Struct(c) => {
            let _ = writeln!(out, "{ind}{label} ::{}", c.type_name);
            write_fields(out, p, c, depth + 1, names);
        }
        Value::Array { element_type, items } => {
            if items.is_empty() {
                let _ = writeln!(out, "{ind}{label} ::{element_type} []");
            } else {
                let _ = writeln!(out, "{ind}{label} ::{element_type} [{}]", items.len());
                for (i, item) in items.iter().enumerate() {
                    write_value(out, p, &format!("[{i}]"), item, depth + 1, names);
                }
            }
        }
        other => {
            let _ = writeln!(out, "{ind}{label} {}", scalar(p, other, names));
        }
    }
}

fn scalar(p: &Partition, v: &Value, names: &dyn Names) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::String(s) => format!("{s:?}"),
        Value::FileRef(s) => format!("file:{s:?}"),
        Value::Enum { value, name } => match name {
            Some(n) => n.clone(),
            None => format!("enum({value})"),
        },
        Value::Guid(g) => format_guid(g),
        Value::Sha1(h) => h.iter().map(|b| format!("{b:02x}")).collect(),
        Value::ResourceRef(rid) => match names.res_name(*rid) {
            Some(n) => format!("res {rid:016x} {n}"),
            None => format!("res {rid:016x}"),
        },
        Value::Pointer(Pointer::Null) => "null".into(),
        Value::Pointer(Pointer::Internal(i)) => {
            let target = p.instances.get(*i);
            let ty = target.map(|t| t.value.type_name.as_str()).unwrap_or("?");
            match target.and_then(|t| t.guid.as_ref()) {
                Some(g) => format!("-> {ty} {}", format_guid(g)),
                None => format!("-> {ty} #{i}"),
            }
        }
        Value::Pointer(Pointer::External(f, i)) => {
            let file = names.partition_name(f).unwrap_or_else(|| format_guid(f));
            format!("-> {file} / {}", format_guid(i))
        }
        Value::Unknown(t) => format!("<unknown type 0x{t:02x}>"),
        Value::Struct(_) | Value::Array { .. } => unreachable!(),
    }
}
