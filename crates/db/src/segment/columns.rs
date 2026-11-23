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
//!     4 SCALED    kind | parameter | how the integers are written | varint count of exceptions; the payload is the
//!                 exceptions (each: varint gap to the one before, the value's 8 bytes) and then the integers
//!                   kinds: 1 DIV k, 2 MUL k, 3 FACTOR f as above; 4 MILES k: value = n / 10^k × 1.609344, what an
//!                   API's miles with k decimals become in km; 5 FAHRENHEIT k: value = (n / 10^k - 32) × 5 / 9
//!                   integers: 0 deltas (as 1 to 3), 1 deltas of deltas (as the times), 2 runs: varint count of
//!                   values that stay the same, then the next delta, and so on
//!                 A value the scale doesn't give back bit for bit (a NaN, a -0.0, one odd reading) is an
//!                 exception: its bits are kept as they are, and the column stays scaled.
//!     5 BITS      how | payload: the values' 64 bits as integers
//!                   0 deltas, 1 deltas of deltas (zigzag varints): floats that climb evenly (an odometer worked
//!                   out with all its digits) differ by nearly the same integer from one to the next
//!                   2 each value XOR the previous, 3 each value minus the previous: the 8 bytes of every value
//!                   split into 8 runs (all the first bytes, then all the second ...)
//!   byte encodings (gear, charging, AC/DC):
//!     0 RAW       one byte per present value
//!     1 RUNS      value | varint how many more times it repeats, and so on
//! ```
//!
//! Every non-XOR encoding is used only where it gives back every value of the column bit for bit (checked while
//! encoding), so the encoding is always lossless; anything else falls back to XOR.
//!
//! There are two encoders. [`encode`] is the quick one, for days still being written: encodings 0 to 3 and RAW,
//! the first that fits. [`encode_small`] is for finished days: per column it tries every encoding above, compresses
//! each lightly and keeps the smallest; then it decodes what it made and compares it with the points bit for bit,
//! and gives [`encode`]'s body instead should they ever differ. Both are read by the same [`decode`].
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
const F_SCALED: u8 = 4;
const F_BITS: u8 = 5;
const B_RAW: u8 = 0;
const B_RUNS: u8 = 1;

/// The kinds of scale of a SCALED column.
const S_DIV: u8 = 1;
const S_MUL: u8 = 2;
const S_FACTOR: u8 = 3;
const S_MILES: u8 = 4;
const S_FAHRENHEIT: u8 = 5;

/// How a scaled column's integers are written.
const I_DELTA: u8 = 0;
const I_DOD: u8 = 1;
const I_RUNS: u8 = 2;

/// How a BITS column's integers are written.
const X_DELTA: u8 = 0;
const X_DOD: u8 = 1;
const X_XOR_PLANES: u8 = 2;
const X_DELTA_PLANES: u8 = 3;

/// Kilometres in a mile: what the Tesla API's and TeslaFi's miles are multiplied by.
const KM_PER_MILE: f64 = 1.609_344;

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
    /// Miles with k decimals, in km: the number read from the API's text, times [`KM_PER_MILE`].
    Miles(usize),
    /// °F with k decimals, in °C: `(f - 32.0) * 5.0 / 9.0`, as TeslaFi's files in °F are read.
    Fahrenheit(usize),
}

impl Scale {
    fn int(self, v: f64) -> f64 {
        match self {
            Scale::Div(k) | Scale::Mul(k) => (v * POW10[k]).round(),
            Scale::Factor(f) => (v / f).round(),
            Scale::Miles(k) => (v / KM_PER_MILE * POW10[k]).round(),
            Scale::Fahrenheit(k) => ((v * 9.0 / 5.0 + 32.0) * POW10[k]).round(),
        }
    }

    fn value(self, n: f64) -> f64 {
        match self {
            Scale::Div(k) => n / POW10[k],
            Scale::Mul(k) => n * NEG_POW10[k],
            Scale::Factor(f) => n * f,
            Scale::Miles(k) => n / POW10[k] * KM_PER_MILE,
            Scale::Fahrenheit(k) => (n / POW10[k] - 32.0) * 5.0 / 9.0,
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

fn encode_times<P: Point>(points: &[P], out: &mut Vec<u8>) {
    let (mut prev, mut prev_delta) = (0u64, 0i64);
    for (i, p) in points.iter().enumerate() {
        let t = p.t_ms();
        let delta = t.wrapping_sub(prev) as i64;
        let v = if i < 2 { delta } else { delta.wrapping_sub(prev_delta) };
        put_varint(out, zigzag(v));
        prev = t;
        prev_delta = delta;
    }
}

/// Encodes points (sorted by time, which the caller has checked) into columns, quickly: for days still being
/// written (see the module docs).
pub fn encode<P: Point>(points: &[P]) -> Vec<u8> {
    let n = points.len();
    let mut out = Vec::with_capacity(n * 8 + 64);
    put_varint(&mut out, n as u64);
    encode_times(points, &mut out);
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
                            // (not looked for here)
                            Scale::Miles(k) => out.extend_from_slice(&[F_SCALED, S_MILES, k as u8, I_DELTA, 0]),
                            Scale::Fahrenheit(k) => {
                                out.extend_from_slice(&[F_SCALED, S_FAHRENHEIT, k as u8, I_DELTA, 0]);
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

/// What [`encode_small`] may use (for measurements: each part can be left out).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    /// The scales for values converted from miles and from °F.
    pub units: bool,
    /// A scaled column may keep the few values that don't fit as they are.
    pub exceptions: bool,
    /// A scaled column's integers as deltas of deltas, and as runs of unchanged values.
    pub dod: bool,
    pub runs: bool,
    /// The 64 bits of unscaled floats as integer deltas, and split into byte planes.
    pub bit_deltas: bool,
    pub planes: bool,
    /// Byte columns as runs.
    pub byte_runs: bool,
}

impl Tuning {
    /// Everything: what finished days are packed with.
    pub const ALL: Tuning = Tuning {
        units: true,
        exceptions: true,
        dod: true,
        runs: true,
        bit_deltas: true,
        planes: true,
        byte_runs: true,
    };
    /// Nothing: the columns [`encode`] makes.
    pub const NONE: Tuning = Tuning {
        units: false,
        exceptions: false,
        dod: false,
        runs: false,
        bit_deltas: false,
        planes: false,
        byte_runs: false,
    };
}

/// What a column would take once compressed, about: its bytes compressed lightly. Candidates are compared by this.
fn trial(head: &[u8], payload: &[u8]) -> usize {
    if payload.len() < 48 {
        return head.len() + payload.len();
    }
    // (quality 5: a higher one picks the same candidates but for a few, measured: under 0.5% either way)
    let params =
        brotli::enc::BrotliEncoderParams { quality: 5, lgwin: 22, size_hint: payload.len(), ..Default::default() };
    let mut out = Vec::with_capacity(payload.len() / 4 + 64);
    match brotli::BrotliCompress(&mut &payload[..], &mut out, &params) {
        Ok(_) => head.len() + out.len(),
        Err(_) => usize::MAX,
    }
}

/// A column written one way: what goes before the payload's length, and the payload.
struct Candidate {
    head: Vec<u8>,
    payload: Vec<u8>,
}

/// The candidate that compresses smallest (the first of equals).
fn smallest(candidates: Vec<Candidate>) -> Option<Candidate> {
    if candidates.len() < 2 {
        return candidates.into_iter().next();
    }
    let sizes: Vec<usize> = candidates.iter().map(|c| trial(&c.head, &c.payload)).collect();
    let best = sizes.iter().enumerate().min_by_key(|(_, s)| **s).map(|(i, _)| i)?;
    candidates.into_iter().nth(best)
}

/// Scales to try on a column: per family (DIV, MUL, MILES, FAHRENHEIT) the one that leaves the fewest values out,
/// and the one of the fewest digits that leaves few out; each with how many it leaves out.
fn find_scales(values: &[f64], t: &Tuning) -> Vec<(Scale, usize)> {
    let families: &[fn(usize) -> Scale] =
        if t.units { &[Scale::Div, Scale::Mul, Scale::Miles, Scale::Fahrenheit] } else { &[Scale::Div, Scale::Mul] };
    // more left out than this and a scale is given up
    let limit = if t.exceptions { values.len() / 8 } else { 0 };
    let few = values.len() / 64;
    let mut fails = vec![[0usize; 10]; families.len()];
    let mut alive = families.len() * 10;
    for &v in values {
        for (family, fails) in families.iter().zip(fails.iter_mut()) {
            for (k, failed) in fails.iter_mut().enumerate() {
                if *failed <= limit && !family(k).exact(v) {
                    *failed += 1;
                    if *failed > limit {
                        alive -= 1;
                    }
                }
            }
        }
        if alive == 0 {
            return Vec::new();
        }
    }
    let mut out: Vec<(Scale, usize)> = Vec::new();
    for (f, family) in families.iter().enumerate() {
        let ok = |k: &usize| fails[f][*k] <= limit;
        let Some(best) = (0..10).filter(ok).min_by_key(|k| (fails[f][*k], *k)) else { continue };
        out.push((family(best), fails[f][best]));
        if let Some(short) = (0..best).find(|k| fails[f][*k] <= few.min(limit)) {
            out.push((family(short), fails[f][short]));
        }
    }
    out
}

/// A scaled column's integers (an exception repeats the integer before it), written one way.
fn encode_ints_as(ints: &[i64], how: u8, out: &mut Vec<u8>) {
    let (mut prev, mut prev_delta, mut zeros) = (0i64, 0i64, 0u64);
    for (i, &n) in ints.iter().enumerate() {
        let delta = n.wrapping_sub(prev);
        match how {
            I_DOD => put_varint(out, zigzag(if i < 2 { delta } else { delta.wrapping_sub(prev_delta) })),
            I_RUNS if delta == 0 => zeros += 1,
            I_RUNS => {
                put_varint(out, zeros);
                put_varint(out, zigzag(delta));
                zeros = 0;
            }
            _ => put_varint(out, zigzag(delta)),
        }
        prev = n;
        prev_delta = delta;
    }
    if how == I_RUNS && zeros > 0 {
        put_varint(out, zeros);
    }
}

/// A column under a scale, written every way allowed.
fn scaled_candidates(values: &[f64], s: Scale, t: &Tuning, out: &mut Vec<Candidate>) {
    let mut exceptions = Vec::new();
    let mut count = 0u64;
    let mut ints = Vec::with_capacity(values.len());
    let (mut prev, mut last_exception) = (0i64, None::<usize>);
    for (i, &v) in values.iter().enumerate() {
        if s.exact(v) {
            prev = s.int(v) as i64;
        } else {
            put_varint(&mut exceptions, last_exception.map_or(i, |l| i - l - 1) as u64);
            exceptions.extend_from_slice(&v.to_bits().to_be_bytes());
            last_exception = Some(i);
            count += 1;
        }
        ints.push(prev);
    }
    let hows: &[u8] = match (t.dod, t.runs) {
        (true, true) => &[I_DELTA, I_DOD, I_RUNS],
        (true, false) => &[I_DELTA, I_DOD],
        (false, true) => &[I_DELTA, I_RUNS],
        (false, false) => &[I_DELTA],
    };
    for &how in hows {
        let mut head = Vec::with_capacity(16);
        match (s, how, count) {
            // as the quick encoder writes it
            (Scale::Div(k), I_DELTA, 0) => head.extend_from_slice(&[F_DIV, k as u8]),
            (Scale::Mul(k), I_DELTA, 0) => head.extend_from_slice(&[F_MUL, k as u8]),
            (Scale::Factor(f), I_DELTA, 0) => {
                head.push(F_FACTOR);
                head.extend_from_slice(&f.to_be_bytes());
            }
            _ => {
                head.push(F_SCALED);
                match s {
                    Scale::Div(k) => head.extend_from_slice(&[S_DIV, k as u8]),
                    Scale::Mul(k) => head.extend_from_slice(&[S_MUL, k as u8]),
                    Scale::Miles(k) => head.extend_from_slice(&[S_MILES, k as u8]),
                    Scale::Fahrenheit(k) => head.extend_from_slice(&[S_FAHRENHEIT, k as u8]),
                    Scale::Factor(f) => {
                        head.push(S_FACTOR);
                        head.extend_from_slice(&f.to_be_bytes());
                    }
                }
                head.push(how);
                put_varint(&mut head, count);
            }
        }
        let mut payload = exceptions.clone();
        encode_ints_as(&ints, how, &mut payload);
        out.push(Candidate { head, payload });
    }
}

/// A column's bits as integers, written one way.
fn encode_bits_as(values: &[f64], how: u8) -> Vec<u8> {
    let n = values.len();
    let planes = matches!(how, X_XOR_PLANES | X_DELTA_PLANES);
    let mut out = if planes { vec![0u8; n * 8] } else { Vec::with_capacity(n * 4) };
    let (mut prev, mut prev_delta) = (0u64, 0i64);
    for (i, v) in values.iter().enumerate() {
        let bits = v.to_bits();
        let delta = bits.wrapping_sub(prev) as i64;
        match how {
            X_DELTA => put_varint(&mut out, zigzag(delta)),
            X_DOD => put_varint(&mut out, zigzag(if i < 2 { delta } else { delta.wrapping_sub(prev_delta) })),
            _ => {
                let x = if how == X_XOR_PLANES { bits ^ prev } else { zigzag(delta) };
                for (plane, b) in x.to_be_bytes().iter().enumerate() {
                    out[plane * n + i] = *b;
                }
            }
        }
        prev = bits;
        prev_delta = delta;
    }
    out
}

/// A float column of a finished day: every way allowed, the smallest kept.
fn small_float_column(values: &[f64], t: &Tuning) -> Option<Candidate> {
    let mut candidates = Vec::new();
    let scales = find_scales(values, t);
    for (s, _) in &scales {
        scaled_candidates(values, *s, t, &mut candidates);
    }
    if !scales.iter().any(|(_, left_out)| *left_out == 0) {
        if let Some(s) = find_factor(values) {
            scaled_candidates(values, s, t, &mut candidates);
        }
        let mut payload = Vec::new();
        encode_xor(values, &mut payload);
        candidates.push(Candidate { head: vec![F_XOR], payload });
        let hows: &[u8] = match (t.bit_deltas, t.planes) {
            (true, true) => &[X_DELTA, X_DOD, X_XOR_PLANES, X_DELTA_PLANES],
            (true, false) => &[X_DELTA, X_DOD],
            (false, true) => &[X_XOR_PLANES, X_DELTA_PLANES],
            (false, false) => &[],
        };
        for &how in hows {
            candidates.push(Candidate { head: vec![F_BITS, how], payload: encode_bits_as(values, how) });
        }
    }
    smallest(candidates)
}

/// A byte column of a finished day: as it is, or as runs.
fn small_byte_column(values: &[u8], t: &Tuning) -> Option<Candidate> {
    let mut candidates = vec![Candidate { head: vec![B_RAW], payload: values.to_vec() }];
    if t.byte_runs {
        let mut payload = Vec::new();
        let mut i = 0;
        while i < values.len() {
            let run = values[i..].iter().take_while(|v| **v == values[i]).count();
            payload.push(values[i]);
            put_varint(&mut payload, run as u64 - 1);
            i += run;
        }
        candidates.push(Candidate { head: vec![B_RUNS], payload });
    }
    smallest(candidates)
}

/// Whether two runs of points are the same bit for bit: the times, which fields are there, and every float's bits
/// (so a NaN equals itself and -0.0 is not 0.0).
#[doc(hidden)]
pub fn same_points<P: Point>(a: &[P], b: &[P]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.t_ms() == y.t_ms()
                && P::columns().iter().all(|c| match c {
                    Col::F64 { get, .. } => get(x).map(f64::to_bits) == get(y).map(f64::to_bits),
                    Col::U8 { get, .. } => get(x) == get(y),
                })
        })
}

/// Encodes a finished day's points (sorted by time) as small as it goes: see the module docs. Slower than
/// [`encode`] (every column is written several ways); what it gives always decodes to the same points, bit for bit.
pub fn encode_small<P: Point>(points: &[P]) -> Vec<u8> {
    let body = encode_tuned(points, &Tuning::ALL);
    match decode::<P>(&body) {
        Ok(back) if same_points(&back, points) => body,
        Ok(_) | Err(_) => encode(points),
    }
}

/// [`encode_small`] with only some of its parts, not checked (for measurements).
#[doc(hidden)]
pub fn encode_tuned<P: Point>(points: &[P], t: &Tuning) -> Vec<u8> {
    let n = points.len();
    let mut out = Vec::with_capacity(n * 4 + 64);
    put_varint(&mut out, n as u64);
    encode_times(points, &mut out);
    let cols = P::columns();
    put_varint(&mut out, cols.len() as u64);
    for col in cols {
        out.push(col.tag());
        let column = match col {
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
                small_float_column(&vals.into_iter().flatten().collect::<Vec<_>>(), t)
            }
            Col::U8 { get, .. } => {
                let vals: Vec<Option<u8>> = points.iter().map(get).collect();
                let (pres, map) = presence(&vals.iter().map(Option::is_some).collect::<Vec<_>>());
                out.push(pres);
                if pres == P_NONE {
                    continue;
                }
                out.extend_from_slice(&map);
                small_byte_column(&vals.into_iter().flatten().collect::<Vec<_>>(), t)
            }
        };
        // (there is always a candidate: XOR or RAW)
        let column = column.unwrap_or(Candidate { head: vec![0], payload: Vec::new() });
        out.extend_from_slice(&column.head);
        put_varint(&mut out, column.payload.len() as u64);
        out.extend_from_slice(&column.payload);
    }
    out
}

/// What one column of an encoded body takes (for measurements).
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSize {
    pub name: &'static str,
    /// `none`, `derived`, `xor`, `div`, `mul`, `factor`, `raw`, or what [`encode_small`] picked (`miles runs`,
    /// `bits dod`, ...).
    pub encoding: &'static str,
    pub values: usize,
    /// The presence bitmap and the payload, as they are in the body.
    pub bitmap: Vec<u8>,
    pub payload: Vec<u8>,
}

/// The bytes of the time column and each column of an encoded body (for measurements).
#[doc(hidden)]
pub fn describe<P: Point>(body: &[u8]) -> Result<(usize, Vec<ColumnSize>), Error> {
    let mut r = Reader::new(body);
    let n = r.count(1)?;
    let start = body.len() - r.rest().len();
    for _ in 0..n {
        r.varint()?;
    }
    let times = body.len() - r.rest().len() - start;
    let (_, readers) = parse::<P>(body)?;
    let mut out = Vec::new();
    for c in readers {
        let bitmap = match c.present {
            Present::Map(m) => m.to_vec(),
            Present::None | Present::All | Present::Derived => Vec::new(),
        };
        let values = match c.present {
            Present::All => n,
            Present::Map(m) => m.iter().map(|b| b.count_ones() as usize).sum(),
            Present::None | Present::Derived => 0,
        };
        let (encoding, payload) = match (&c.present, &c.vals) {
            (Present::Derived, _) => ("derived", Vec::new()),
            (_, Vals::None) => ("none", Vec::new()),
            (_, Vals::Xor { control, data, .. }) => ("xor", [control, data.rest()].concat()),
            (_, Vals::Ints { ints, scale, exceptions }) => (
                match (scale, ints.how, exceptions.left) {
                    (Scale::Div(_), I_DELTA, 0) => "div",
                    (Scale::Mul(_), I_DELTA, 0) => "mul",
                    (Scale::Factor(_), I_DELTA, 0) => "factor",
                    (Scale::Miles(_), I_DELTA, 0) => "miles",
                    (Scale::Fahrenheit(_), I_DELTA, 0) => "fahrenheit",
                    (Scale::Div(_), I_DOD, 0) => "div dod",
                    (Scale::Mul(_), I_DOD, 0) => "mul dod",
                    (Scale::Factor(_), I_DOD, 0) => "factor dod",
                    (Scale::Miles(_), I_DOD, 0) => "miles dod",
                    (Scale::Fahrenheit(_), I_DOD, 0) => "fahrenheit dod",
                    (Scale::Div(_), _, 0) => "div runs",
                    (Scale::Mul(_), _, 0) => "mul runs",
                    (Scale::Factor(_), _, 0) => "factor runs",
                    (Scale::Miles(_), _, 0) => "miles runs",
                    (Scale::Fahrenheit(_), _, 0) => "fahrenheit runs",
                    (Scale::Miles(_), _, _) => "miles, exceptions",
                    (Scale::Fahrenheit(_), _, _) => "fahrenheit, exceptions",
                    (_, _, _) => "scaled, exceptions",
                },
                [exceptions.r.rest(), ints.r.rest()].concat(),
            ),
            (_, Vals::Bits { r, dod: false, .. }) => ("bits delta", r.rest().to_vec()),
            (_, Vals::Bits { r, dod: true, .. }) => ("bits dod", r.rest().to_vec()),
            (_, Vals::Planes { b, xor: true, .. }) => ("xor planes", b.to_vec()),
            (_, Vals::Planes { b, xor: false, .. }) => ("delta planes", b.to_vec()),
            (_, Vals::Bytes { b, .. }) => ("raw", b.to_vec()),
            (_, Vals::ByteRuns { r, .. }) => ("byte runs", r.rest().to_vec()),
        };
        out.push(ColumnSize { name: c.col.name(), encoding, values, bitmap, payload });
    }
    Ok((times, out))
}

/// Where a column says its values are.
enum Present<'a> {
    None,
    All,
    Map(&'a [u8]),
    Derived,
}

/// A scaled column's integers being read.
#[derive(Clone)]
struct Ints<'a> {
    r: Reader<'a>,
    how: u8,
    prev: i64,
    prev_delta: i64,
    /// How many were read (deltas of deltas start at the third).
    read: usize,
    /// Runs: values still to come that repeat the last, and whether a count of them comes next.
    zeros: u64,
    count_next: bool,
}

impl<'a> Ints<'a> {
    fn new(payload: &'a [u8], how: u8) -> Ints<'a> {
        Ints { r: Reader::new(payload), how, prev: 0, prev_delta: 0, read: 0, zeros: 0, count_next: true }
    }

    fn next(&mut self) -> Result<i64, Error> {
        match self.how {
            I_DOD => {
                let v = unzigzag(self.r.varint()?);
                let delta = if self.read < 2 { v } else { self.prev_delta.wrapping_add(v) };
                self.prev = self.prev.wrapping_add(delta);
                self.prev_delta = delta;
                self.read += 1;
            }
            I_RUNS => {
                if self.count_next {
                    self.zeros = self.r.varint()?;
                    self.count_next = false;
                }
                if self.zeros > 0 {
                    self.zeros -= 1;
                } else {
                    self.prev = self.prev.wrapping_add(unzigzag(self.r.varint()?));
                    self.count_next = true;
                }
            }
            _ => self.prev = self.prev.wrapping_add(unzigzag(self.r.varint()?)),
        }
        Ok(self.prev)
    }

    fn finished(&self) -> bool {
        self.r.done() && self.zeros == 0
    }
}

/// A scaled column's exceptions being read: the values kept as their bits.
#[derive(Clone)]
struct Exceptions<'a> {
    r: Reader<'a>,
    left: u64,
    /// The place (among the column's values) of the next one, and of the value being read.
    next: u64,
    at: u64,
}

impl<'a> Exceptions<'a> {
    fn new(b: &'a [u8], count: u64) -> Result<Exceptions<'a>, Error> {
        let mut e = Exceptions { r: Reader::new(b), left: count, next: 0, at: 0 };
        if count > 0 {
            e.next = e.r.varint()?;
        }
        Ok(e)
    }

    /// The bits of the value being read, if it is an exception; then on to the next value.
    fn take(&mut self) -> Result<Option<u64>, Error> {
        let at = self.at;
        self.at += 1;
        if self.left == 0 || self.next != at {
            return Ok(None);
        }
        let mut bits = [0u8; 8];
        bits.copy_from_slice(self.r.bytes(8)?);
        self.left -= 1;
        if self.left > 0 {
            let gap = self.r.varint()?;
            self.next = at.checked_add(1).and_then(|n| n.checked_add(gap)).ok_or_else(|| corrupt("bad exception"))?;
        }
        Ok(Some(u64::from_be_bytes(bits)))
    }
}

/// A column being read, one value at a time.
enum Vals<'a> {
    None,
    Xor {
        control: &'a [u8],
        at: usize,
        data: Reader<'a>,
        prev: u64,
    },
    Ints {
        ints: Ints<'a>,
        scale: Scale,
        exceptions: Exceptions<'a>,
    },
    /// The bits as integer deltas (zigzag varints), or deltas of deltas.
    Bits {
        r: Reader<'a>,
        dod: bool,
        prev: u64,
        prev_delta: i64,
        read: usize,
    },
    /// The bits' 8 byte planes: each value XOR the previous, or (zigzag) minus the previous.
    Planes {
        b: &'a [u8],
        count: usize,
        at: usize,
        prev: u64,
        xor: bool,
    },
    Bytes {
        b: &'a [u8],
        at: usize,
    },
    ByteRuns {
        r: Reader<'a>,
        value: u8,
        left: u64,
    },
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
            Vals::Ints { ints, scale, exceptions } => {
                let n = ints.next()?;
                Ok(match exceptions.take()? {
                    Some(bits) => f64::from_bits(bits),
                    None => scale.value(n as f64),
                })
            }
            Vals::Bits { r, dod, prev, prev_delta, read } => {
                let v = unzigzag(r.varint()?);
                let delta = if *dod && *read >= 2 { prev_delta.wrapping_add(v) } else { v };
                *prev = prev.wrapping_add(delta as u64);
                *prev_delta = delta;
                *read += 1;
                Ok(f64::from_bits(*prev))
            }
            Vals::Planes { b, count, at, prev, xor } => {
                if *at >= *count {
                    return Err(corrupt("BITS column too short"));
                }
                let mut be = [0u8; 8];
                for (plane, x) in be.iter_mut().enumerate() {
                    *x = b[plane * *count + *at];
                }
                *at += 1;
                let x = u64::from_be_bytes(be);
                *prev = if *xor { *prev ^ x } else { prev.wrapping_add(unzigzag(x) as u64) };
                Ok(f64::from_bits(*prev))
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
            Vals::ByteRuns { r, value, left } => {
                if *left == 0 {
                    *value = r.u8()?;
                    *left = r.varint()?.checked_add(1).ok_or_else(|| corrupt("bad run"))?;
                }
                *left -= 1;
                Ok(*value)
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
            Vals::Ints { ints, exceptions, .. } => ints.finished() && exceptions.left == 0 && exceptions.r.done(),
            Vals::Bits { r, .. } => r.done(),
            Vals::Planes { count, at, .. } => at == count,
            Vals::Bytes { b, at } => *at == b.len(),
            Vals::ByteRuns { r, left, .. } => r.done() && *left == 0,
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
        let count = match present {
            Present::All => n,
            Present::Map(m) => m.iter().map(|b| b.count_ones() as usize).sum(),
            Present::None | Present::Derived => 0,
        };
        let how = r.u8()?;
        let decimals = |r: &mut Reader<'_>| -> Result<usize, Error> {
            let k = usize::from(r.u8()?);
            if k >= 10 {
                return Err(corrupt("bad scale"));
            }
            Ok(k)
        };
        let factor = |r: &mut Reader<'_>| -> Result<f64, Error> {
            let mut f = [0u8; 8];
            f.copy_from_slice(r.bytes(8)?);
            Ok(f64::from_be_bytes(f))
        };
        // what is written before the payload's length
        enum How {
            Xor,
            Scaled(Scale, u8, u64),
            Bits(u8),
            Raw,
            Runs,
        }
        let how = match (col, how) {
            (Col::F64 { .. }, F_XOR) => How::Xor,
            (Col::U8 { .. }, B_RAW) => How::Raw,
            (Col::U8 { .. }, B_RUNS) => How::Runs,
            (Col::F64 { .. }, F_DIV) => How::Scaled(Scale::Div(decimals(&mut r)?), I_DELTA, 0),
            (Col::F64 { .. }, F_MUL) => How::Scaled(Scale::Mul(decimals(&mut r)?), I_DELTA, 0),
            (Col::F64 { .. }, F_FACTOR) => How::Scaled(Scale::Factor(factor(&mut r)?), I_DELTA, 0),
            (Col::F64 { .. }, F_SCALED) => {
                let scale = match r.u8()? {
                    S_DIV => Scale::Div(decimals(&mut r)?),
                    S_MUL => Scale::Mul(decimals(&mut r)?),
                    S_FACTOR => Scale::Factor(factor(&mut r)?),
                    S_MILES => Scale::Miles(decimals(&mut r)?),
                    S_FAHRENHEIT => Scale::Fahrenheit(decimals(&mut r)?),
                    _ => return Err(corrupt("unknown scale")),
                };
                let ints = r.u8()?;
                if ints > I_RUNS {
                    return Err(corrupt("unknown encoding"));
                }
                How::Scaled(scale, ints, r.varint()?)
            }
            (Col::F64 { .. }, F_BITS) => {
                let bits = r.u8()?;
                if bits > X_DELTA_PLANES {
                    return Err(corrupt("unknown encoding"));
                }
                How::Bits(bits)
            }
            _ => return Err(corrupt("unknown encoding")),
        };
        let len = r.count(1)?;
        let payload = r.bytes(len)?;
        let vals = match how {
            How::Raw => Vals::Bytes { b: payload, at: 0 },
            How::Runs => Vals::ByteRuns { r: Reader::new(payload), value: 0, left: 0 },
            How::Scaled(scale, ints, exceptions) => {
                // the exceptions come first: each a varint and 8 bytes
                if exceptions > (payload.len() / 9) as u64 {
                    return Err(corrupt("count larger than the data"));
                }
                let mut skip = Reader::new(payload);
                for _ in 0..exceptions {
                    skip.varint()?;
                    skip.bytes(8)?;
                }
                let (head, rest) = payload.split_at(payload.len() - skip.rest().len());
                Vals::Ints { ints: Ints::new(rest, ints), scale, exceptions: Exceptions::new(head, exceptions)? }
            }
            How::Bits(X_XOR_PLANES | X_DELTA_PLANES) if payload.len() != count.saturating_mul(8) => {
                return Err(corrupt("BITS column of the wrong length"));
            }
            How::Bits(bits @ (X_XOR_PLANES | X_DELTA_PLANES)) => {
                Vals::Planes { b: payload, count, at: 0, prev: 0, xor: bits == X_XOR_PLANES }
            }
            How::Bits(bits) => {
                Vals::Bits { r: Reader::new(payload), dod: bits == X_DOD, prev: 0, prev_delta: 0, read: 0 }
            }
            How::Xor => {
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
