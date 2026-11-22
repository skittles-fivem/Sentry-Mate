//! Offline map tiles: reading one tile at a time out of a PMTiles (version 3) file, the single-file tile archive
//! Protomaps publishes (https://github.com/protomaps/PMTiles, spec v3).
//!
//! A PMTiles file is a 127-byte header, a root directory, optional leaf directories, the metadata and the tiles.
//! Directories map tile ids (z/x/y along a Hilbert curve per zoom) to byte ranges; they may be gzip compressed.
//! [`Archive::open`] reads the header and the root directory once; [`Archive::tile`] reads at most two leaf
//! directories (kept in a small cache) and the tile's bytes. Nothing is ever written to the file, and nothing here
//! reaches the network: the web layer serves the tiles to the server's own pages.
//!
//! [`maps_in`] lists the `.pmtiles` files in the data folder's `maps/` folder and [`pick`] chooses the one to use
//! (the one an admin chose, else the largest).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The data folder's map folder.
pub const MAPS_DIR: &str = "maps";
/// Directories bigger than this are not read (a damaged or hostile file).
const MAX_DIR_BYTES: u64 = 32 << 20;
/// Tiles bigger than this are not served.
const MAX_TILE_BYTES: u64 = 8 << 20;
/// Leaf directories kept in memory per archive.
const LEAF_CACHE: usize = 64;

/// What the tiles are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileType {
    Unknown,
    /// Mapbox Vector Tiles (a Protomaps basemap build).
    Mvt,
    Png,
    Jpeg,
    Webp,
    Avif,
}

impl TileType {
    fn from_byte(b: u8) -> TileType {
        match b {
            1 => TileType::Mvt,
            2 => TileType::Png,
            3 => TileType::Jpeg,
            4 => TileType::Webp,
            5 => TileType::Avif,
            _ => TileType::Unknown,
        }
    }

    /// `vector`, `raster` or `unknown`, as `/maps/config` says it.
    pub fn kind(self) -> &'static str {
        match self {
            TileType::Mvt => "vector",
            TileType::Png | TileType::Jpeg | TileType::Webp | TileType::Avif => "raster",
            TileType::Unknown => "unknown",
        }
    }

    /// The tile's Content-Type.
    pub fn content_type(self) -> &'static str {
        match self {
            TileType::Mvt => "application/vnd.mapbox-vector-tile",
            TileType::Png => "image/png",
            TileType::Jpeg => "image/jpeg",
            TileType::Webp => "image/webp",
            TileType::Avif => "image/avif",
            TileType::Unknown => "application/octet-stream",
        }
    }
}

/// How directories or tiles are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Unknown,
    None,
    Gzip,
    Brotli,
    Zstd,
}

impl Compression {
    fn from_byte(b: u8) -> Compression {
        match b {
            1 => Compression::None,
            2 => Compression::Gzip,
            3 => Compression::Brotli,
            4 => Compression::Zstd,
            _ => Compression::Unknown,
        }
    }

    /// The tile's Content-Encoding (the browser unpacks gzip tiles itself), `None` for plain tiles.
    pub fn content_encoding(self) -> Option<&'static str> {
        match self {
            Compression::Gzip => Some("gzip"),
            Compression::Brotli => Some("br"),
            Compression::Zstd => Some("zstd"),
            Compression::None | Compression::Unknown => None,
        }
    }
}

/// The parts of the header the server uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    root_offset: u64,
    root_length: u64,
    leaf_offset: u64,
    leaf_length: u64,
    data_offset: u64,
    data_length: u64,
    pub internal_compression: Compression,
    pub tile_compression: Compression,
    pub tile_type: TileType,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// [west, south, east, north] in degrees.
    pub bounds: [f64; 4],
    /// [lon, lat, zoom].
    pub center: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    tile_id: u64,
    offset: u64,
    length: u64,
    /// 0: a leaf directory.
    run_length: u64,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("not a usable PMTiles file: {msg}"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    i32::from_le_bytes(a)
}

impl Header {
    pub const LEN: usize = 127;

    pub fn parse(b: &[u8]) -> io::Result<Header> {
        if b.len() < Header::LEN || &b[0..7] != b"PMTiles" {
            return Err(bad("no PMTiles header"));
        }
        if b[7] != 3 {
            return Err(bad("only version 3 files can be read"));
        }
        let e7 = |at| f64::from(i32_at(b, at)) / 10_000_000.0;
        Ok(Header {
            root_offset: u64_at(b, 8),
            root_length: u64_at(b, 16),
            leaf_offset: u64_at(b, 40),
            leaf_length: u64_at(b, 48),
            data_offset: u64_at(b, 56),
            data_length: u64_at(b, 64),
            internal_compression: Compression::from_byte(b[97]),
            tile_compression: Compression::from_byte(b[98]),
            tile_type: TileType::from_byte(b[99]),
            min_zoom: b[100],
            max_zoom: b[101],
            bounds: [e7(102), e7(106), e7(110), e7(114)],
            center: [e7(119), e7(123), f64::from(b[118])],
        })
    }
}

/// The tile id of z/x/y: the tiles of every lower zoom, then the position along zoom z's Hilbert curve.
pub fn tile_id(z: u8, x: u32, y: u32) -> Option<u64> {
    if z > 31 || u64::from(x) >= 1u64 << z || u64::from(y) >= 1u64 << z {
        return None;
    }
    let before: u64 = ((1u128 << (2 * u32::from(z))) - 1) as u64 / 3;
    let n = 1u64 << z;
    let (mut x, mut y) = (u64::from(x), u64::from(y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = n - 1 - x;
                y = n - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    Some(before + d)
}

fn varint(b: &[u8], pos: &mut usize) -> io::Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos).ok_or_else(|| bad("a directory ends early"))?;
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(bad("a number in a directory is too long"))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Unpacks gzip (the deflate stream after a gzip header).
fn gunzip(b: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    if b.len() < 18 || b[0] != 0x1f || b[1] != 0x8b || b[2] != 8 {
        return Err(bad("a directory is not gzip"));
    }
    let flags = b[3];
    let mut pos = 10usize;
    if flags & 4 != 0 {
        let n = usize::from(*b.get(pos).ok_or_else(|| bad("gzip"))?)
            | usize::from(*b.get(pos + 1).ok_or_else(|| bad("gzip"))?) << 8;
        pos += 2 + n;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            while *b.get(pos).ok_or_else(|| bad("gzip"))? != 0 {
                pos += 1;
            }
            pos += 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    let body = b.get(pos..b.len().saturating_sub(8)).ok_or_else(|| bad("gzip"))?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, limit).map_err(|_| bad("a directory does not unpack"))
}

fn decode_dir(raw: &[u8]) -> io::Result<Vec<Entry>> {
    let mut pos = 0;
    let n = usize::try_from(varint(raw, &mut pos)?).map_err(|_| bad("directory size"))?;
    // every entry takes at least four bytes
    if n > raw.len() {
        return Err(bad("a directory is damaged"));
    }
    let mut e = vec![Entry { tile_id: 0, offset: 0, length: 0, run_length: 0 }; n];
    let mut last = 0u64;
    for x in e.iter_mut() {
        last = last.checked_add(varint(raw, &mut pos)?).ok_or_else(|| bad("tile ids"))?;
        x.tile_id = last;
    }
    for x in e.iter_mut() {
        x.run_length = varint(raw, &mut pos)?;
    }
    for x in e.iter_mut() {
        x.length = varint(raw, &mut pos)?;
    }
    for i in 0..n {
        let v = varint(raw, &mut pos)?;
        e[i].offset = if v == 0 && i > 0 {
            e[i - 1].offset.checked_add(e[i - 1].length).ok_or_else(|| bad("offsets"))?
        } else {
            v.checked_sub(1).ok_or_else(|| bad("the first offset"))?
        };
    }
    Ok(e)
}

fn find(dir: &[Entry], id: u64) -> Option<Entry> {
    let i = dir.partition_point(|e| e.tile_id <= id);
    let e = *dir.get(i.checked_sub(1)?)?;
    (e.run_length == 0 || id < e.tile_id + e.run_length).then_some(e)
}

/// Leaf directories read so far, by their (offset, length).
type LeafCache = HashMap<(u64, u64), std::sync::Arc<Vec<Entry>>>;

/// One open PMTiles file (its header and root directory in memory).
#[derive(Debug)]
pub struct Archive {
    path: PathBuf,
    pub header: Header,
    root: Vec<Entry>,
    leaves: Mutex<LeafCache>,
}

impl Archive {
    pub fn open(path: &Path) -> io::Result<Archive> {
        let mut f = File::open(path)?;
        let mut head = [0u8; Header::LEN];
        f.read_exact(&mut head)?;
        let header = Header::parse(&head)?;
        if header.internal_compression != Compression::None && header.internal_compression != Compression::Gzip {
            return Err(bad("only plain or gzip directories can be read"));
        }
        let a = Archive { path: path.to_path_buf(), header, root: Vec::new(), leaves: Mutex::new(HashMap::new()) };
        let root = a.read_dir(&mut f, a.header.root_offset, a.header.root_length)?;
        Ok(Archive { root, ..a })
    }

    fn read_at(f: &mut File, offset: u64, length: u64, most: u64) -> io::Result<Vec<u8>> {
        if length > most {
            return Err(bad("a part is too big"));
        }
        f.seek(SeekFrom::Start(offset))?;
        let mut b = vec![0u8; usize::try_from(length).map_err(|_| bad("length"))?];
        f.read_exact(&mut b)?;
        Ok(b)
    }

    fn read_dir(&self, f: &mut File, offset: u64, length: u64) -> io::Result<Vec<Entry>> {
        let raw = Archive::read_at(f, offset, length, MAX_DIR_BYTES)?;
        let raw = match self.header.internal_compression {
            Compression::Gzip => gunzip(&raw, MAX_DIR_BYTES as usize)?,
            _ => raw,
        };
        decode_dir(&raw)
    }

    /// The bytes of tile z/x/y as stored (compressed as [`Header::tile_compression`] says), `None` when the file
    /// has no such tile.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> io::Result<Option<Vec<u8>>> {
        let Some(id) = tile_id(z, x, y) else { return Ok(None) };
        if z < self.header.min_zoom || z > self.header.max_zoom {
            return Ok(None);
        }
        let mut f = File::open(&self.path)?;
        let mut dir = None::<std::sync::Arc<Vec<Entry>>>;
        for _depth in 0..4 {
            let e = match &dir {
                None => find(&self.root, id),
                Some(d) => find(d, id),
            };
            let Some(e) = e else { return Ok(None) };
            if e.run_length > 0 {
                let off = self.header.data_offset.checked_add(e.offset).ok_or_else(|| bad("offset"))?;
                if e.offset.saturating_add(e.length) > self.header.data_length {
                    return Err(bad("a tile lies outside the file"));
                }
                return Archive::read_at(&mut f, off, e.length, MAX_TILE_BYTES).map(Some);
            }
            if e.offset.saturating_add(e.length) > self.header.leaf_length {
                return Err(bad("a leaf directory lies outside its part"));
            }
            let key = (e.offset, e.length);
            let cached = self.leaves.lock().unwrap_or_else(|p| p.into_inner()).get(&key).cloned();
            let leaf = match cached {
                Some(l) => l,
                None => {
                    let l = std::sync::Arc::new(self.read_dir(&mut f, self.header.leaf_offset + e.offset, e.length)?);
                    let mut m = self.leaves.lock().unwrap_or_else(|p| p.into_inner());
                    if m.len() >= LEAF_CACHE {
                        m.clear();
                    }
                    m.insert(key, l.clone());
                    l
                }
            };
            dir = Some(leaf);
        }
        Err(bad("directories nest too deep"))
    }
}

/// A `.pmtiles` file in the map folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapFile {
    /// The file name (no folder).
    pub name: String,
    pub bytes: u64,
}

/// Whether a name is one this server will open: a plain file name ending in `.pmtiles` (letters, digits, `.`, `_`
/// and `-`, at most 120 characters, not starting with a dot).
pub fn name_ok(name: &str) -> bool {
    name.len() <= 120
        && name.len() > ".pmtiles".len()
        && name.ends_with(".pmtiles")
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The `.pmtiles` files in `data_dir/maps`, largest first (nothing when the folder is missing).
pub fn maps_in(data_dir: &Path) -> Vec<MapFile> {
    let Ok(rd) = std::fs::read_dir(data_dir.join(MAPS_DIR)) else { return Vec::new() };
    let mut v: Vec<MapFile> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            (name_ok(&name) && meta.is_file() && meta.len() >= Header::LEN as u64)
                .then_some(MapFile { name, bytes: meta.len() })
        })
        .collect();
    v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    v
}

/// The file to use: the one chosen when it is there, else the largest.
pub fn pick<'a>(files: &'a [MapFile], chosen: Option<&str>) -> Option<&'a MapFile> {
    chosen.and_then(|c| files.iter().find(|f| f.name == c)).or_else(|| files.first())
}

/// A PMTiles v3 file with these tiles (one root directory, no leaves): for tests and tools.
pub fn build(tiles: &[(u8, u32, u32, Vec<u8>)], tile_type: u8, gzip_dir: bool) -> Vec<u8> {
    let mut t: Vec<(u64, &Vec<u8>)> =
        tiles.iter().filter_map(|(z, x, y, b)| tile_id(*z, *x, *y).map(|id| (id, b))).collect();
    t.sort_by_key(|(id, _)| *id);
    let mut data = Vec::new();
    let mut entries = Vec::new();
    for (id, b) in &t {
        entries.push(Entry { tile_id: *id, offset: data.len() as u64, length: b.len() as u64, run_length: 1 });
        data.extend_from_slice(b);
    }
    let mut dir = Vec::new();
    put_varint(&mut dir, entries.len() as u64);
    let mut last = 0;
    for e in &entries {
        put_varint(&mut dir, e.tile_id - last);
        last = e.tile_id;
    }
    for e in &entries {
        put_varint(&mut dir, e.run_length);
    }
    for e in &entries {
        put_varint(&mut dir, e.length);
    }
    for (i, e) in entries.iter().enumerate() {
        let contiguous = i > 0 && e.offset == entries[i - 1].offset + entries[i - 1].length;
        put_varint(&mut dir, if contiguous { 0 } else { e.offset + 1 });
    }
    if gzip_dir {
        let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        g.extend(miniz_oxide::deflate::compress_to_vec(&dir, 6));
        g.extend(crc32fast::hash(&dir).to_le_bytes());
        g.extend((dir.len() as u32).to_le_bytes());
        dir = g;
    }
    let (zmin, zmax) = tiles.iter().fold((u8::MAX, 0), |(a, b), t| (a.min(t.0), b.max(t.0)));
    let mut h = vec![0u8; Header::LEN];
    h[0..7].copy_from_slice(b"PMTiles");
    h[7] = 3;
    let root_off = Header::LEN as u64;
    let data_off = root_off + dir.len() as u64;
    for (at, v) in [
        (8, root_off),
        (16, dir.len() as u64),
        (24, data_off),
        (32, 0),
        (40, data_off),
        (48, 0),
        (56, data_off),
        (64, data.len() as u64),
        (72, t.len() as u64),
        (80, t.len() as u64),
        (88, t.len() as u64),
    ] {
        h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    h[96] = 1;
    h[97] = if gzip_dir { 2 } else { 1 };
    h[98] = 1;
    h[99] = tile_type;
    h[100] = if zmin == u8::MAX { 0 } else { zmin };
    h[101] = zmax;
    for (at, v) in
        [(102, -1_800_000_000i32), (106, -850_000_000), (110, 1_800_000_000), (114, 850_000_000), (119, 0), (123, 0)]
    {
        h[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    h.extend(dir);
    h.extend(data);
    h
}
