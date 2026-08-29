//! Reading metadata from XTC / XTCH files — a custom binary book/comic format
//! (see the upstream `xtc-format-spec.md`). We parse the 56-byte header and, if
//! present, the 256-byte metadata block. Cover images are stored as page data
//! in a bespoke codec, so cover extraction is not supported; XTC-only books fall
//! back to a generated placeholder cover.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

use crate::epub::EpubMeta;

/// `mark` values identifying the format (little-endian uint32 at offset 0).
const MARK_XTC: u32 = 0x0043_5458; // "XTC\0"
const MARK_XTCH: u32 = 0x4843_5458; // "XTCH"

/// Whether `mark` is a recognized XTC/XTCH signature.
fn is_xtc_mark(mark: u32) -> bool {
    mark == MARK_XTC || mark == MARK_XTCH
}

/// Read the metadata from an XTC/XTCH file.
pub fn read_meta(path: &Path) -> io::Result<EpubMeta> {
    let mut file = File::open(path)?;

    let mut header = [0u8; 56];
    file.read_exact(&mut header)?;
    let mark = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if !is_xtc_mark(mark) {
        return Err(io::Error::new(ErrorKind::InvalidData, "not an XTC file"));
    }

    let mut meta = EpubMeta::default();

    let has_metadata = header[0x09] != 0;
    if has_metadata {
        let metadata_offset = u64::from_le_bytes(header[0x10..0x18].try_into().unwrap());
        file.seek(SeekFrom::Start(metadata_offset))?;
        let mut block = [0u8; 256];
        file.read_exact(&mut block)?;

        // Layout: title[128], author[64], publisher[32], language[16],
        // creation_time(u32), cover_page(u16), chapter_count(u16).
        meta.title = read_cstr(&block[0..128]);
        meta.author = read_cstr(&block[128..192]);
        meta.language = read_cstr(&block[224..240]);
        let created = u32::from_le_bytes(block[240..244].try_into().unwrap());
        if created != 0 {
            meta.modified = jiff::Timestamp::from_second(created as i64)
                .ok()
                .map(|t| t.to_string());
        }
    }

    Ok(meta)
}

/// Read a null-terminated, zero-padded UTF-8 string field.
fn read_cstr(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    (!text.is_empty()).then_some(text)
}
