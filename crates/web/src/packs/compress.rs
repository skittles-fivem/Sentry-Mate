//! Brotli and gzip copies of pack files, made once: the built-in packs' at build time (`build.rs` includes this
//! file), an uploaded pack's when it is loaded, and an `index.html` the server rewrites when its pack is loaded. A
//! request that accepts one gets it with `Content-Encoding` ([`super::serve_file`]).

/// Files smaller than this are sent as they are (the headers would eat what is saved).
pub const MIN_BYTES: usize = 512;

/// Text the browser reads (fonts and pictures are compressed already).
pub fn compressible(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "html" | "css" | "js" | "mjs" | "json" | "webmanifest" | "svg" | "txt" | "md" | "wasm" | "ico"
    )
}

/// `data` in brotli at `quality` (0 to 11; 11 is the smallest and slowest).
pub fn brotli(data: &[u8], quality: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 3 + 64);
    let params = brotli::enc::BrotliEncoderParams { quality: quality as i32, lgwin: 22, ..Default::default() };
    let mut input = data;
    // writing into a Vec can't fail
    let _ = brotli::BrotliCompress(&mut input, &mut out, &params);
    out
}

/// `data` in gzip at `level` (0 to 9).
pub fn gzip(data: &[u8], level: u32) -> Vec<u8> {
    use std::io::Write as _;
    let mut e = flate2::write::GzEncoder::new(Vec::with_capacity(data.len() / 3 + 64), flate2::Compression::new(level));
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// Whether a compressed copy is worth keeping: an eighth smaller at least.
pub fn worth(raw: usize, packed: usize) -> bool {
    packed > 0 && packed < raw - raw / 8
}
