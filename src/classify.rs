//! Lightweight format classification shared by scanning and analyzer dispatch.

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
