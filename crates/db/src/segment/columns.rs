//! Columnar encoding of a run of points (a day of [`model::Sample`]s or [`model::ChargePoint`]s), before compression.
//!
//! Each field is its own column, so a compressor sees runs of alike bytes (a temperature that barely moves, a gear
//! that stays in P) instead of whole points interleaved:
//!
//! ```text
//! body   = varint n | time column | varint column count | column ...
//! times  = t0 | t1 - t0 | then delta-of-delta: (t[i] - t[i-1]) - (t[i-1] - t[i-2]), all zigzag varints
//!          (1 Hz points cost one zero byte each)
//! column = tag | presence | [bitmap] | encoding | [parameter] | varint payload length | payload
//!   presence: 0 never present (nothing follows), 1 always present, 2 a bitmap of n bits (1 = present),
//!             3 worked out from other columns of the same point (nothing follows; e.g. power = V × I / 1000)
//!   float encodings:
//!     0 XOR       Gorilla style: each value XOR the previous; a control byte per value (0: the same value, else
//!                 1 + 8 × leading zero bytes + trailing zero bytes), then all the middle bytes, byte aligned so
//!                 the compressor still finds patterns
//!     1 DIV k     value = n / 10^k
//!     2 MUL k     value = n × 10^-k, the way a DBC factor of 0.1, 0.01, ... makes values such as 401.40000000000003
//!     3 FACTOR f  value = n × f for a factor f found in the data (f64, big endian): the 0.000544 V of the 12 V
//!                 battery, the 0.0333 V of the charge line
//!                 (1 to 3: n as zigzag varint deltas)
//!   byte encodings (gear, charging, AC/DC):
//!     0 RAW       one byte per present value
//! ```
//!
//! Every non-XOR encoding is used only where it gives back every value of the column bit for bit (checked while
//! encoding), so the encoding is always lossless; anything else falls back to XOR.
//!
//! Decoding reads all columns side by side and builds each point once, so a day of 86,400 points is one pass over
//! memory, not one per field; [`decode_fields`] gives only the fields asked for, as columns, for charts.

use crate::error::Error;

pub(crate) fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub(crate) fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub(crate) fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn corrupt(why: &str) -> Error {
    Error::Corrupt(why.to_string())
}

/// Reads what [`put_varint`] and friends wrote, refusing anything that runs past the end.
#[derive(Clone)]
pub(crate) struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, pos: 0 }
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        let v = *self.b.get(self.pos).ok_or_else(|| corrupt("cut short"))?;
        self.pos += 1;
        Ok(v)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(corrupt("varint too long"))
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| corrupt("cut short"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// A count that must be possible given the bytes left (each counted thing takes at least `min_each` bytes), so a
    /// damaged count can't ask for a huge allocation.
    pub(crate) fn count(&mut self, min_each: usize) -> Result<usize, Error> {
        let n = self.varint()?;
        let left = (self.b.len() - self.pos) as u64;
        if n.saturating_mul(min_each as u64) > left {
            return Err(corrupt("count larger than the data"));
        }
        Ok(n as usize)
    }

    pub(crate) fn done(&self) -> bool {
        self.pos == self.b.len()
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.b[self.pos..]
    }
}

/// A field worked out from other fields of the same point.
pub struct Derived<P> {
    pub derive: fn(&P) -> Option<f64>,
    /// The tags of the fields it is worked out from.
    pub from: &'static [u8],
}

/// One field of a point type.
pub enum Col<P> {
    F64 {
        tag: u8,
        name: &'static str,
        get: fn(&P) -> Option<f64>,
        set: fn(&mut P, f64),
        /// Stored as nothing when every value of a day equals this (bit for bit).
        derived: Option<Derived<P>>,
    },
    /// A small enum or flag; `set` returns false for a code it doesn't know.
    U8 { tag: u8, name: &'static str, get: fn(&P) -> Option<u8>, set: fn(&mut P, u8) -> bool },
}

impl<P> Col<P> {
    pub fn tag(&self) -> u8 {
        match self {
            Col::F64 { tag, .. } | Col::U8 { tag, .. } => *tag,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Col::F64 { name, .. } | Col::U8 { name, .. } => name,
        }
    }

    /// The field's value as a number (codes for enums and flags).
    pub fn value(&self, p: &P) -> Option<f64> {
        match self {
            Col::F64 { get, .. } => get(p),
            Col::U8 { get, .. } => get(p).map(f64::from),
        }
    }
}

/// A kind of point kept in segments.
pub trait Point:
    crate::rows::sealed::Sealed + Clone + Default + PartialEq + std::fmt::Debug + Send + Sync + 'static
{
    /// Names the series on disk (`samples`, `charge`).
    const SERIES: &'static str;
    fn t_ms(&self) -> u64;
    fn set_t_ms(&mut self, t: u64);
    /// Every field but the time, each with a tag that never changes meaning.
    fn columns() -> &'static [Col<Self>];
}

const P_NONE: u8 = 0;
const P_ALL: u8 = 1;
const P_MAP: u8 = 2;
const P_DERIVED: u8 = 3;

const F_XOR: u8 = 0;
const F_DIV: u8 = 1;
const F_MUL: u8 = 2;
const F_FACTOR: u8 = 3;
const B_RAW: u8 = 0;

/// 10^k and 10^-k for k = 0..=9, as the f64 constants a DBC factor would be.
const POW10: [f64; 10] = [1.0, 10.0, 100.0, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9];
const NEG_POW10: [f64; 10] = [1.0, 0.1, 0.01, 0.001, 1e-4, 1e-5, 1e-6, 1e-7, 1e-8, 1e-9];
/// Integers below this are exact in an f64.
const EXACT: f64 = 9_007_199_254_740_992.0;

/// How a float column's values are stored.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scale {
    Div(usize),
    Mul(usize),
    Factor(f64),
}

impl Scale {
    fn int(self, v: f64) -> f64 {
        match self {
            Scale::Div(k) | Scale::Mul(k) => (v * POW10[k]).round(),
            Scale::Factor(f) => (v / f).round(),
        }
    }

    fn value(self, n: f64) -> f64 {
        match self {
            Scale::Div(k) => n / POW10[k],
            Scale::Mul(k) => n * NEG_POW10[k],
            Scale::Factor(f) => n * f,
        }
    }

    /// Whether `v` comes back bit for bit through the integer that is stored (-0.0 doesn't: it is stored as 0).
    fn exact(self, v: f64) -> bool {
        let n = self.int(v);
        n.abs() < EXACT && self.value(n as i64 as f64).to_bits() == v.to_bits()
    }
}

/// The smallest decimal k (DIV or MUL) that gives back every value exactly, if any.
fn find_decimal(values: &[f64]) -> Option<Scale> {
    let mut div_ok = [true; 10];
    let mut mul_ok = [true; 10];
    for &v in values {
        if !v.is_finite() {
            return None;
        }
        for k in 0..10 {
            if div_ok[k] && !Scale::Div(k).exact(v) {
                div_ok[k] = false;
            }
            if mul_ok[k] && !Scale::Mul(k).exact(v) {
                mul_ok[k] = false;
            }
        }
        if !div_ok.iter().chain(mul_ok.iter()).any(|&ok| ok) {
            return None;
        }
    }
    (0..10).find_map(|k| {
        if div_ok[k] {
            Some(Scale::Div(k))
        } else if mul_ok[k] {
            Some(Scale::Mul(k))
        } else {
            None
        }
    })
}

/// `x` rounded to `digits` significant decimal digits (how a factor is written in a DBC file).
fn snap(x: f64, digits: usize) -> Option<f64> {
    format!("{:.*e}", digits.saturating_sub(1), x).parse().ok()
}

/// A factor `f` with every value = n × f exactly: found from the smallest step between values, rounded to a short
/// decimal the way DBC files write factors, then checked against every value.
fn find_factor(values: &[f64]) -> Option<Scale> {
    let mut u: Vec<f64> = values.iter().copied().take(1 << 16).collect();
    if u.iter().any(|v| !v.is_finite()) {
        return None;
    }
    u.sort_by(f64::total_cmp);
    u.dedup_by(|a, b| a.to_bits() == b.to_bits());
    let step = u.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0.0).fold(f64::INFINITY, f64::min);
    if !step.is_finite() {
        return None;
    }
    for m in 1..=4 {
        for digits in 1..=6 {
            let Some(f) = snap(step / m as f64, digits).filter(|f| *f > 0.0) else { continue };
            let s = Scale::Factor(f);
            if values.iter().all(|&v| s.exact(v)) {
                return Some(s);
            }
        }
    }
    None
}

fn encode_xor(values: &[f64], out: &mut Vec<u8>) {
    let mut control = Vec::with_capacity(values.len());
    let mut data = Vec::new();
    let mut prev = 0u64;
    for v in values {
        let bits = v.to_bits();
        let x = bits ^ prev;
        prev = bits;
        if x == 0 {
            control.push(0);
            continue;
        }
        let lead = (x.leading_zeros() / 8) as usize;
        let trail = (x.trailing_zeros() / 8) as usize;
        control.push(1 + (lead * 8 + trail) as u8);
        data.extend_from_slice(&x.to_be_bytes()[lead..8 - trail]);
    }
    out.extend_from_slice(&control);
    out.extend_from_slice(&data);
}

fn encode_ints(values: &[f64], s: Scale, out: &mut Vec<u8>) {
    let mut prev = 0i64;
    for &v in values {
        let n = s.int(v) as i64;
        put_varint(out, zigzag(n.wrapping_sub(prev)));
        prev = n;
    }
}

fn presence(present: &[bool]) -> (u8, Vec<u8>) {
    let count = present.iter().filter(|&&p| p).count();
    if count == 0 {
        return (P_NONE, Vec::new());
    }
    if count == present.len() {
        return (P_ALL, Vec::new());
    }
    let mut map = vec![0u8; present.len().div_ceil(8)];
    for (i, _) in present.iter().enumerate().filter(|(_, p)| **p) {
        map[i / 8] |= 1 << (i % 8);
    }
    (P_MAP, map)
}

/// Encodes points (sorted by time, which the caller has checked) into columns.
pub fn encode<P: Point>(points: &[P]) -> Vec<u8> {
    let n = points.len();
    let mut out = Vec::with_capacity(n * 8 + 64);
    put_varint(&mut out, n as u64);
    let (mut prev, mut prev_delta) = (0u64, 0i64);
    for (i, p) in points.iter().enumerate() {
        let t = p.t_ms();
        let delta = t.wrapping_sub(prev) as i64;
        let v = if i < 2 { delta } else { delta.wrapping_sub(prev_delta) };
        put_varint(&mut out, zigzag(v));
        prev = t;
        prev_delta = delta;
    }
    let cols = P::columns();
    put_varint(&mut out, cols.len() as u64);
    let mut payload = Vec::new();
    for col in cols {
        out.push(col.tag());
        payload.clear();
        match col {
            Col::F64 { get, derived, .. } => {
                let vals: Vec<Option<f64>> = points.iter().map(get).collect();
                if let Some(d) = derived {
                    let same =
                        points.iter().zip(&vals).all(|(p, v)| v.map(f64::to_bits) == (d.derive)(p).map(f64::to_bits));
                    if same && vals.iter().any(Option::is_some) {
                        out.push(P_DERIVED);
                        continue;
                    }
                }
                let (pres, map) = presence(&vals.iter().map(Option::is_some).collect::<Vec<_>>());
                out.push(pres);
                if pres == P_NONE {
                    continue;
                }
                out.extend_from_slice(&map);
                let present: Vec<f64> = vals.into_iter().flatten().collect();
                match find_decimal(&present).or_else(|| find_factor(&present)) {
                    Some(s) => {
                        match s {
                            Scale::Div(k) => out.extend_from_slice(&[F_DIV, k as u8]),
                            Scale::Mul(k) => out.extend_from_slice(&[F_MUL, k as u8]),
                            Scale::Factor(f) => {
                                out.push(F_FACTOR);
                                out.extend_from_slice(&f.to_be_bytes());
                            }
                        }
                        encode_ints(&present, s, &mut payload);
                    }
                    None => {
                        out.push(F_XOR);
                        encode_xor(&present, &mut payload);
                    }
                }
            }
            Col::U8 { get, .. } => {
                let vals: Vec<Option<u8>> = points.iter().map(get).collect();
                let (pres, map) = presence(&vals.iter().map(Option::is_some).collect::<Vec<_>>());
                out.push(pres);
                if pres == P_NONE {
                    continue;
                }
                out.extend_from_slice(&map);
                out.push(B_RAW);
                payload.extend(vals.into_iter().flatten());
            }
        }
        put_varint(&mut out, payload.len() as u64);
        out.extend_from_slice(&payload);
    }
    out
}

/// Where a column says its values are.
enum Present<'a> {
    None,
    All,
    Map(&'a [u8]),
    Derived,
}

/// A column being read, one value at a time.
enum Vals<'a> {
    None,
    Xor { control: &'a [u8], at: usize, data: Reader<'a>, prev: u64 },
    Ints { r: Reader<'a>, prev: i64, scale: Scale },
    Bytes { b: &'a [u8], at: usize },
}

struct ColReader<'a, P: 'static> {
    col: &'static Col<P>,
    present: Present<'a>,
    vals: Vals<'a>,
}

impl<P> ColReader<'_, P> {
    fn is_present(&self, row: usize) -> bool {
        match self.present {
            Present::All => true,
            Present::Map(m) => m[row / 8] & (1 << (row % 8)) != 0,
            Present::None | Present::Derived => false,
        }
    }

    fn next_f64(&mut self) -> Result<f64, Error> {
        match &mut self.vals {
            Vals::Xor { control, at, data, prev } => {
                let c = *control.get(*at).ok_or_else(|| corrupt("XOR column too short"))?;
                *at += 1;
                if c != 0 {
                    let c = usize::from(c - 1);
                    let (lead, trail) = (c / 8, c % 8);
                    if c >= 64 || lead + trail > 7 {
                        return Err(corrupt("bad XOR control byte"));
                    }
                    let mid = data.bytes(8 - lead - trail)?;
                    let mut be = [0u8; 8];
                    be[lead..8 - trail].copy_from_slice(mid);
                    *prev ^= u64::from_be_bytes(be);
                }
                Ok(f64::from_bits(*prev))
            }
            Vals::Ints { r, prev, scale } => {
                *prev = prev.wrapping_add(unzigzag(r.varint()?));
                Ok(scale.value(*prev as f64))
            }
            _ => Err(corrupt("not a float column")),
        }
    }

    fn next_u8(&mut self) -> Result<u8, Error> {
        match &mut self.vals {
            Vals::Bytes { b, at } => {
                let v = *b.get(*at).ok_or_else(|| corrupt("byte column too short"))?;
                *at += 1;
                Ok(v)
            }
            _ => Err(corrupt("not a byte column")),
        }
    }

    /// Reads this row's value (if any) into the point.
    fn fill(&mut self, row: usize, p: &mut P) -> Result<(), Error> {
        if !self.is_present(row) {
            return Ok(());
        }
        match self.col {
            Col::F64 { set, .. } => set(p, self.next_f64()?),
            Col::U8 { set, .. } => {
                if !set(p, self.next_u8()?) {
                    return Err(corrupt("unknown code"));
                }
            }
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        match &self.vals {
            Vals::None => true,
            Vals::Xor { control, at, data, .. } => *at == control.len() && data.done(),
            Vals::Ints { r, .. } => r.done(),
            Vals::Bytes { b, at } => *at == b.len(),
        }
    }
}

/// Reads the times and the columns' headers; values are read later, row by row.
fn parse<P: Point>(body: &[u8]) -> Result<(Vec<u64>, Vec<ColReader<'_, P>>), Error> {
    let mut r = Reader::new(body);
    let n = r.count(1)?;
    let mut times = Vec::with_capacity(n);
    let (mut prev, mut prev_delta) = (0u64, 0i64);
    for i in 0..n {
        let v = unzigzag(r.varint()?);
        let delta = if i < 2 { v } else { prev_delta.wrapping_add(v) };
        let t = prev.wrapping_add(delta as u64);
        times.push(t);
        prev = t;
        prev_delta = delta;
    }
    let ncols = r.count(2)?;
    let cols = P::columns();
    let mut readers: Vec<ColReader<'_, P>> = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let tag = r.u8()?;
        let col = cols.iter().find(|c| c.tag() == tag).ok_or_else(|| corrupt("unknown column"))?;
        if readers.iter().any(|c| c.col.tag() == tag) {
            return Err(corrupt("a column twice"));
        }
        let present = match r.u8()? {
            P_NONE => Present::None,
            P_ALL => Present::All,
            P_MAP => Present::Map(r.bytes(n.div_ceil(8))?),
            P_DERIVED if matches!(col, Col::F64 { derived: Some(_), .. }) => Present::Derived,
            _ => return Err(corrupt("bad presence")),
        };
        if matches!(present, Present::None | Present::Derived) {
            readers.push(ColReader { col, present, vals: Vals::None });
            continue;
        }
        let how = r.u8()?;
        let scale = match (col, how) {
            (Col::F64 { .. }, F_XOR) | (Col::U8 { .. }, B_RAW) => None,
            (Col::F64 { .. }, F_DIV | F_MUL) => {
                let k = usize::from(r.u8()?);
                if k >= 10 {
                    return Err(corrupt("bad scale"));
                }
                Some(if how == F_DIV { Scale::Div(k) } else { Scale::Mul(k) })
            }
            (Col::F64 { .. }, F_FACTOR) => {
                let mut f = [0u8; 8];
                f.copy_from_slice(r.bytes(8)?);
                Some(Scale::Factor(f64::from_be_bytes(f)))
            }
            _ => return Err(corrupt("unknown encoding")),
        };
        let len = r.count(1)?;
        let payload = r.bytes(len)?;
        let vals = match (col, scale) {
            (Col::U8 { .. }, _) => Vals::Bytes { b: payload, at: 0 },
            (_, Some(scale)) => Vals::Ints { r: Reader::new(payload), prev: 0, scale },
            (_, None) => {
                let count = match present {
                    Present::All => n,
                    Present::Map(m) => m.iter().map(|b| b.count_ones() as usize).sum(),
                    _ => 0,
                };
                let control = payload.get(..count).ok_or_else(|| corrupt("XOR column too short"))?;
                Vals::Xor { control, at: 0, data: Reader::new(&payload[count..]), prev: 0 }
            }
        };
        readers.push(ColReader { col, present, vals });
    }
    if !r.done() {
        return Err(corrupt("trailing bytes after the columns"));
    }
    Ok((times, readers))
}

fn fill_derived<P>(readers: &[ColReader<'_, P>], p: &mut P) {
    for c in readers.iter().filter(|c| matches!(c.present, Present::Derived)) {
        if let Col::F64 { set, derived: Some(d), .. } = c.col
            && let Some(v) = (d.derive)(p)
        {
            set(p, v);
        }
    }
}

fn check_finished<P>(readers: &[ColReader<'_, P>]) -> Result<(), Error> {
    match readers.iter().all(ColReader::finished) {
        true => Ok(()),
        false => Err(corrupt("a column has values left over")),
    }
}

/// Decodes what [`encode`] wrote. Every count and code is checked: a damaged body is an error, never a panic or a
/// huge allocation.
pub fn decode<P: Point>(body: &[u8]) -> Result<Vec<P>, Error> {
    let (times, mut readers) = parse::<P>(body)?;
    let mut points = Vec::with_capacity(times.len());
    for (row, &t) in times.iter().enumerate() {
        let mut p = P::default();
        p.set_t_ms(t);
        for c in readers.iter_mut() {
            c.fill(row, &mut p)?;
        }
        fill_derived(&readers, &mut p);
        points.push(p);
    }
    check_finished(&readers)?;
    Ok(points)
}

/// Some fields of a run of points, as columns (what a chart wants): `t_ms[i]` goes with `values[f][i]`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct Fields {
    pub t_ms: Vec<u64>,
    /// In the order asked for; enums and flags as their codes (gear P 1, R 2, N 3, D 4; charging 0 or 1; AC 1,
    /// DC 2).
    pub values: Vec<(String, Vec<Option<f64>>)>,
}

impl Fields {
    pub(crate) fn new(names: &[&str]) -> Fields {
        Fields { t_ms: Vec::new(), values: names.iter().map(|n| (n.to_string(), Vec::new())).collect() }
    }

    pub fn len(&self) -> usize {
        self.t_ms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t_ms.is_empty()
    }

    /// Adds the rows of `more` whose time is in `[from, to)` (both are in time order).
    pub(crate) fn extend_range(&mut self, more: Fields, from: u64, to: u64) {
        let a = more.t_ms.partition_point(|&t| t < from);
        let b = more.t_ms.partition_point(|&t| t < to);
        self.t_ms.extend_from_slice(&more.t_ms[a..b]);
        for ((_, mine), (_, theirs)) in self.values.iter_mut().zip(more.values) {
            mine.extend_from_slice(&theirs[a..b]);
        }
    }
}

/// Checks field names against a point type (unknown names are refused).
pub(crate) fn field_columns<P: Point>(names: &[&str]) -> Result<Vec<&'static Col<P>>, Error> {
    names
        .iter()
        .map(|n| {
            P::columns()
                .iter()
                .find(|c| c.name() == *n)
                .ok_or_else(|| Error::invalid(format!("no field {n} in {}", P::SERIES)))
        })
        .collect()
}

/// Decodes only some fields (and what they are worked out from), as columns.
pub fn decode_fields<P: Point>(body: &[u8], names: &[&str]) -> Result<Fields, Error> {
    let wanted = field_columns::<P>(names)?;
    let (times, readers) = parse::<P>(body)?;
    // the columns to read: those asked for and those a derived one is worked out from
    let mut need: Vec<u8> = wanted.iter().map(|c| c.tag()).collect();
    for c in &wanted {
        if let Col::F64 { derived: Some(d), .. } = c {
            need.extend_from_slice(d.from);
        }
    }
    let mut readers: Vec<ColReader<'_, P>> = readers.into_iter().filter(|c| need.contains(&c.col.tag())).collect();
    let mut out = Fields::new(names);
    out.t_ms = times;
    for (_, v) in out.values.iter_mut() {
        v.reserve(out.t_ms.len());
    }
    for row in 0..out.t_ms.len() {
        let mut p = P::default();
        for c in readers.iter_mut() {
            c.fill(row, &mut p)?;
        }
        fill_derived(&readers, &mut p);
        for ((_, v), c) in out.values.iter_mut().zip(&wanted) {
            v.push(c.value(&p));
        }
    }
    check_finished(&readers)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn varints_and_zigzag_round_trip() {
        for v in [0i64, 1, -1, 63, -64, 64, i64::MAX, i64::MIN, 1_794_801_056_391] {
            let mut b = Vec::new();
            put_varint(&mut b, zigzag(v));
            assert_eq!(unzigzag(Reader::new(&b).varint().unwrap()), v);
        }
    }

    #[test]
    fn scales_are_found_only_when_exact() {
        // what a DBC factor makes: raw × 0.01
        let dbc: Vec<f64> = (40_100..40_200).map(|r| r as f64 * 0.01).collect();
        assert_eq!(find_decimal(&dbc), Some(Scale::Mul(2)));
        assert_eq!(find_decimal(&[71.0, 70.0, 69.0]), Some(Scale::Div(0)));
        assert_eq!(find_decimal(&[0.5, 1.5, -40.0]), Some(Scale::Div(1)));
        // a factor that isn't a power of ten (the charge line's 0.0333 V), found from the data's smallest step
        let line: Vec<f64> = (7400..7500).map(|r| r as f64 * 0.0333).collect();
        assert_eq!(find_decimal(&line), None);
        assert_eq!(find_factor(&line), Some(Scale::Factor(0.0333)));
        let lv: Vec<f64> = [28_859.0, 28_861.0, 28_870.0, 28_850.0].iter().map(|r| r * 0.000_544).collect();
        let s = find_decimal(&lv).or_else(|| find_factor(&lv)).unwrap();
        assert!(lv.iter().all(|&v| s.exact(v)));
        // a unit conversion has no short exact form
        let mixed = [256.0 * 1.609_344, 255.0 * 1.609_344, 1.0 / 3.0];
        assert_eq!(find_decimal(&mixed).or_else(|| find_factor(&mixed)), None);
        assert_eq!(find_decimal(&[f64::NAN]), None);
        assert_eq!(find_factor(&[f64::INFINITY, 1.0]), None);
    }
}
