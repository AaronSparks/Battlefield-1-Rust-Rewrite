//! The signed-file header used by .toc, .cat, layout.toc and initfs.
//!
//! Magic `00 D1 CE 00`: a signature, then a 257-byte XOR key, then XOR-ed data.
//! Magic `00 D1 CE 01` or `00 D1 CE 03`: a signature and an empty key; the data
//! after byte 556 is plain. Battlefield 1 uses `01`.
//! Any other start: the file has no header.

use anyhow::{Context, Result};
use std::path::Path;

pub const SIGNED_HEADER_SIZE: usize = 556;
const XOR_KEY_OFFSET: usize = 296;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    None,
    Xor,
    Signed,
}

/// Read a signed file and return only its payload.
pub fn read_signed_file(path: &Path) -> Result<(HeaderKind, Vec<u8>)> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    strip_header(data).with_context(|| format!("header of {}", path.display()))
}

pub fn strip_header(mut data: Vec<u8>) -> Result<(HeaderKind, Vec<u8>)> {
    match data.get(..4) {
        Some([0x00, 0xd1, 0xce, 0x00]) => {
            anyhow::ensure!(data.len() >= XOR_KEY_OFFSET + 260, "file too short for XOR header");
            let key: Vec<u8> = data[XOR_KEY_OFFSET..XOR_KEY_OFFSET + 257].iter().map(|b| b ^ 0x7b).collect();
            let payload = data.split_off(XOR_KEY_OFFSET + 260);
            let plain = payload.iter().enumerate().map(|(i, b)| b ^ key[i % 257]).collect();
            Ok((HeaderKind::Xor, plain))
        }
        Some([0x00, 0xd1, 0xce, 0x01]) | Some([0x00, 0xd1, 0xce, 0x03]) => {
            anyhow::ensure!(data.len() >= SIGNED_HEADER_SIZE, "file too short for signed header");
            Ok((HeaderKind::Signed, data.split_off(SIGNED_HEADER_SIZE)))
        }
        _ => Ok((HeaderKind::None, data)),
    }
}
