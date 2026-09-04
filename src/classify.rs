//! Lightweight format classification shared by scanning and analyzer dispatch.

use anyhow::Result;
use std::io::{Read, Seek, SeekFrom};

pub(crate) fn is_native_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"\x7fELF")
        || magic.starts_with(b"MZ")
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
}

/// Returns whether a PE file has a non-empty CLR runtime header directory.
pub(crate) fn is_dotnet_pe(path: &std::path::Path) -> Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length < 0x40 {
        return Ok(false);
    }
    let mut dos = [0_u8; 0x40];
    file.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        return Ok(false);
    }
    let pe_offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().unwrap()) as u64;
    if pe_offset.checked_add(24).is_none_or(|end| end > length) {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(pe_offset))?;
    let mut header = [0_u8; 24];
    file.read_exact(&mut header)?;
    if &header[..4] != b"PE\0\0" {
        return Ok(false);
    }
    let optional_size = u16::from_le_bytes(header[20..22].try_into().unwrap()) as u64;
    let optional_offset = pe_offset + 24;
    if optional_size < 2
        || optional_offset
            .checked_add(optional_size)
            .is_none_or(|end| end > length)
    {
        return Ok(false);
    }
    let mut magic = [0_u8; 2];
    file.read_exact(&mut magic)?;
    let directory_offset = match u16::from_le_bytes(magic) {
        0x10b => 96_u64,
        0x20b => 112_u64,
        _ => return Ok(false),
    };
    const CLR_DIRECTORY: u64 = 14;
    let clr_offset = directory_offset + CLR_DIRECTORY * 8;
    if optional_size < clr_offset + 8
        || optional_offset
            .checked_add(clr_offset + 8)
            .is_none_or(|end| end > length)
    {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(optional_offset + clr_offset))?;
    let mut directory = [0_u8; 8];
    file.read_exact(&mut directory)?;
    let rva = u32::from_le_bytes(directory[..4].try_into().unwrap());
    let size = u32::from_le_bytes(directory[4..].try_into().unwrap());
    Ok(rva != 0 && size != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    fn distinguishes_managed_and_native_pe_files() {
        for managed in [false, true] {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            let mut dos = [0_u8; 0x40];
            dos[..2].copy_from_slice(b"MZ");
            dos[0x3c..].copy_from_slice(&0x80_u32.to_le_bytes());
            file.write_all(&dos).unwrap();
            file.as_file_mut().seek(SeekFrom::Start(0x80)).unwrap();
            let mut pe = [0_u8; 24];
            pe[..4].copy_from_slice(b"PE\0\0");
            pe[20..22].copy_from_slice(&224_u16.to_le_bytes());
            file.write_all(&pe).unwrap();
            let mut optional = [0_u8; 224];
            optional[..2].copy_from_slice(&0x10b_u16.to_le_bytes());
            if managed {
                optional[208..212].copy_from_slice(&0x2000_u32.to_le_bytes());
                optional[212..216].copy_from_slice(&72_u32.to_le_bytes());
            }
            file.write_all(&optional).unwrap();
            assert_eq!(is_dotnet_pe(file.path()).unwrap(), managed);
        }
    }
}
