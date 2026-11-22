//! xz as the CAN Bridge writes it: one stream, LZMA2, CRC64, a dictionary no larger than needed.

use std::io::Write;

/// Packs `body` with the xz preset (0 to 9; the bridge uses 6).
pub fn pack(body: &[u8], preset: u32) -> std::io::Result<Vec<u8>> {
    let mut opts = lzma_rust2::XzOptions::with_preset(preset.min(9));
    let need = u32::try_from(body.len()).unwrap_or(u32::MAX).max(4096);
    opts.lzma_options.dict_size = opts.lzma_options.dict_size.min(need);
    let mut w = lzma_rust2::XzWriter::new(Vec::with_capacity(body.len() / 4 + 64), opts)?;
    w.write_all(body)?;
    w.finish()
}
