//! Addresses with no internet: the nearest town from GeoNames' list of towns (`cities1000`: every place of 1,000
//! people or more, about 150,000; `cities500`: 500 or more, about 200,000), with its region from
//! `admin1CodesASCII.txt`, as "Town, Region, CC".
//!
//! The files are public data (CC BY 4.0, credit [`GEONAMES_CREDIT`] wherever addresses show), downloaded once into
//! the data folder by [`super::download`]. They hold nothing about the owner, so they are kept as GeoNames ships
//! them. Loaded once for the whole server (about 15 MB in memory for cities500) and shared by every car; a lookup
//! is a KD-tree search, a few microseconds.
//!
//! The files come from the network, so they are read with fixed limits: lines over 64 KiB, rows that don't parse,
//! positions that aren't real and country codes that aren't two letters are skipped and counted, never trusted.

use super::kdtree::KdTree;
use super::{Address, AddressSource, clean_text};
use crate::geo::{self, m_for_chord2, unit};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

/// Credit line for addresses found offline (GeoNames' licence asks for it).
pub const GEONAMES_CREDIT: &str = "Town names from GeoNames (geonames.org), CC BY 4.0";
/// The folder in the data folder that holds the GeoNames files.
pub const GEONAMES_DIR: &str = "geonames";
/// Region (first-level division) names.
pub const ADMIN1_FILE: &str = "admin1CodesASCII.txt";
/// Longest town list read (cities500 is about 50 MB).
pub const MAX_CITIES_BYTES: u64 = 512 * 1024 * 1024;
/// Longest region list read (about 150 KB).
pub const MAX_ADMIN1_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LINE: usize = 64 * 1024;
const NO_REGION: u32 = u32::MAX;

/// Which GeoNames town list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Dataset {
    /// Towns of 1,000 people or more.
    #[default]
    Cities1000,
    /// Towns of 500 people or more (more villages, a little larger).
    Cities500,
}

impl Dataset {
    pub fn name(self) -> &'static str {
        match self {
            Dataset::Cities1000 => "cities1000",
            Dataset::Cities500 => "cities500",
        }
    }

    /// The text file inside the zip, and its name in the data folder.
    pub fn txt(self) -> String {
        format!("{}.txt", self.name())
    }

    /// The file GeoNames serves.
    pub fn zip(self) -> String {
        format!("{}.zip", self.name())
    }
}

/// The GeoNames folder in a data folder.
pub fn geonames_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(GEONAMES_DIR)
}

#[derive(Debug, thiserror::Error)]
pub enum OfflineError {
    #[error("no offline address data at {0} (download it in Settings → Maps and addresses)")]
    Missing(PathBuf),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is larger than {limit} bytes")]
    TooBig { path: PathBuf, limit: u64 },
}

/// What a load read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadReport {
    pub towns: usize,
    pub regions: usize,
    /// Rows left out (too long, not parseable, no real position or country code).
    pub skipped: usize,
}

struct Town {
    lat: f64,
    lon: f64,
    name: (u32, u32),
    region: u32,
    cc: [u8; 2],
}

/// A town found near a position.
#[derive(Debug, Clone, PartialEq)]
pub struct NearestTown<'a> {
    pub name: &'a str,
    pub region: Option<&'a str>,
    /// ISO 3166-1 alpha-2, upper case.
    pub country_code: &'a str,
    pub lat: f64,
    pub lon: f64,
    /// From the position to the town's centre, m.
    pub distance_m: f64,
}

impl NearestTown<'_> {
    /// "Town, Region, CC" (the region left out when not known).
    pub fn short(&self) -> String {
        match self.region {
            Some(r) if r != self.name => format!("{}, {r}, {}", self.name, self.country_code),
            _ => format!("{}, {}", self.name, self.country_code),
        }
    }
}

/// GeoNames towns, ready for nearest lookups. Immutable once loaded; share it (`Arc`) between all cars.
pub struct OfflineGeocoder {
    towns: Vec<Town>,
    /// Every name, one after another; towns and regions point into it.
    text: String,
    regions: Vec<(u32, u32)>,
    tree: KdTree,
}

/// Reads one line of at most `max` bytes into `buf`. `None` at the end; `Some(false)` for a longer line (skipped).
fn next_line<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<Option<bool>> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(any.then_some(!too_long));
        }
        any = true;
        let (take, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (avail.len(), false),
        };
        let chunk = &avail[..if done { take - 1 } else { take }];
        if !too_long {
            if buf.len() + chunk.len() > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        r.consume(take);
        if done {
            return Ok(Some(!too_long));
        }
    }
}

fn push_text(text: &mut String, s: &str) -> (u32, u32) {
    let at = text.len() as u32;
    text.push_str(s);
    (at, s.len() as u32)
}

impl OfflineGeocoder {
    /// Loads `<data>/geonames/<dataset>.txt` and, when there, `admin1CodesASCII.txt` for region names.
    pub fn open(data_dir: &Path, dataset: Dataset) -> Result<(OfflineGeocoder, LoadReport), OfflineError> {
        let dir = geonames_dir(data_dir);
        let cities_path = dir.join(dataset.txt());
        let admin_path = dir.join(ADMIN1_FILE);
        let open = |path: &Path, limit: u64| -> Result<Option<io::BufReader<std::fs::File>>, OfflineError> {
            let io_err = |source| OfflineError::Io { path: path.to_path_buf(), source };
            let f = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(io_err(e)),
            };
            if f.metadata().map_err(io_err)?.len() > limit {
                return Err(OfflineError::TooBig { path: path.to_path_buf(), limit });
            }
            Ok(Some(io::BufReader::with_capacity(256 * 1024, f)))
        };
        let cities = open(&cities_path, MAX_CITIES_BYTES)?.ok_or_else(|| OfflineError::Missing(cities_path.clone()))?;
        let admin = open(&admin_path, MAX_ADMIN1_BYTES)?;
        OfflineGeocoder::from_readers(cities, admin).map_err(|source| OfflineError::Io { path: cities_path, source })
    }

    /// Reads a GeoNames town list (tab-separated, the `geoname` table's 19 columns) and optionally its region list.
    pub fn from_readers(
        mut cities: impl BufRead,
        admin1: Option<impl BufRead>,
    ) -> io::Result<(OfflineGeocoder, LoadReport)> {
        let mut report = LoadReport::default();
        let mut text = String::new();
        let mut regions = Vec::new();
        let mut region_ix: HashMap<String, u32> = HashMap::new();
        let mut buf = Vec::new();
        if let Some(mut admin) = admin1 {
            while let Some(fits) = next_line(&mut admin, &mut buf, MAX_LINE)? {
                let Some(line) = fits.then(|| std::str::from_utf8(&buf).ok()).flatten() else { continue };
                let mut cols = line.trim_end_matches('\r').split('\t');
                let (Some(code), Some(name)) = (cols.next(), cols.next()) else { continue };
                let (Some(code), Some(name)) = (clean_text(code, 40), clean_text(name, 100)) else { continue };
                if region_ix.contains_key(&code) || text.len() + name.len() >= u32::MAX as usize {
                    continue;
                }
                region_ix.insert(code, regions.len() as u32);
                regions.push(push_text(&mut text, &name));
            }
        }
        report.regions = regions.len();

        let mut towns = Vec::new();
        while let Some(fits) = next_line(&mut cities, &mut buf, MAX_LINE)? {
            let Some(line) = fits.then(|| std::str::from_utf8(&buf).ok()).flatten() else {
                report.skipped += 1;
                continue;
            };
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let parsed = (|| {
                if cols.len() < 11 {
                    return None;
                }
                let lat: f64 = cols[4].trim().parse().ok()?;
                let lon: f64 = cols[5].trim().parse().ok()?;
                let cc = cols[8].trim().as_bytes();
                if !geo::is_valid(lat, lon) || cc.len() != 2 || !cc.iter().all(u8::is_ascii_uppercase) {
                    return None;
                }
                let name = clean_text(cols[1], 100)?;
                let region = region_ix.get(&format!("{}.{}", cols[8].trim(), cols[10].trim())).copied();
                Some((lat, lon, [cc[0], cc[1]], name, region.unwrap_or(NO_REGION)))
            })();
            let Some((lat, lon, cc, name, region)) = parsed else {
                report.skipped += 1;
                continue;
            };
            // Offsets into the name text are 32-bit; a list that big is not GeoNames' anyway.
            if towns.len() >= u32::MAX as usize - 1 || text.len() + name.len() >= u32::MAX as usize {
                report.skipped += 1;
                continue;
            }
            let name = push_text(&mut text, &name);
            towns.push(Town { lat, lon, name, region, cc });
        }
        report.towns = towns.len();
        let tree = KdTree::build(towns.iter().enumerate().map(|(i, t)| (unit(t.lat, t.lon), i as u32)).collect());
        text.shrink_to_fit();
        towns.shrink_to_fit();
        Ok((OfflineGeocoder { towns, text, regions, tree }, report))
    }

    pub fn len(&self) -> usize {
        self.tree.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.len() == 0
    }

    fn span(&self, (at, len): (u32, u32)) -> &str {
        self.text.get(at as usize..(at + len) as usize).unwrap_or("")
    }

    /// The town nearest the position, however far.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<NearestTown<'_>> {
        if !geo::is_valid(lat, lon) {
            return None;
        }
        let (i, c2) = self.tree.nearest(&unit(lat, lon))?;
        let t = self.towns.get(i as usize)?;
        let region = (t.region != NO_REGION).then(|| self.regions.get(t.region as usize).map(|&r| self.span(r)));
        Some(NearestTown {
            name: self.span(t.name),
            region: region.flatten().filter(|r| !r.is_empty()),
            country_code: std::str::from_utf8(&t.cc).unwrap_or(""),
            lat: t.lat,
            lon: t.lon,
            distance_m: m_for_chord2(c2),
        })
    }

    /// The address of a position: the nearest town, if it is within `max_m` (a town 300 km away over the sea is no
    /// address).
    pub fn address(&self, lat: f64, lon: f64, max_m: f64) -> Option<Address> {
        let t = self.nearest(lat, lon)?;
        if t.distance_m > max_m {
            return None;
        }
        Some(Address {
            short: t.short(),
            name: None,
            street: None,
            locality: Some(t.name.to_string()),
            region: t.region.map(str::to_string),
            postcode: None,
            country_code: Some(t.country_code.to_string()),
            full: None,
            source: AddressSource::Offline,
            distance_m: Some((t.distance_m * 10.0).round() / 10.0),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn line_reader_limits_and_ends() {
        let data = b"short\n0123456789abcdef\nlast";
        let mut r = io::BufReader::with_capacity(4, &data[..]);
        let mut buf = vec![];
        assert_eq!(next_line(&mut r, &mut buf, 8).unwrap(), Some(true));
        assert_eq!(buf, b"short");
        assert_eq!(next_line(&mut r, &mut buf, 8).unwrap(), Some(false));
        assert_eq!(next_line(&mut r, &mut buf, 8).unwrap(), Some(true));
        assert_eq!(buf, b"last");
        assert_eq!(next_line(&mut r, &mut buf, 8).unwrap(), None);
    }
}
