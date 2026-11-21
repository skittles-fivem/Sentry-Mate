//! The firmware fingerprints the car's main modules broadcast on the vehicle bus (`can0`), about once a second while
//! awake: CRCs and git hashes of their application and boot code, build types and configurations. Tesla's version
//! number (2025.x) is not on CAN, but these change when an update reflashes a module, so comparing them between
//! drives shows that the car was updated and which modules changed.
//!
//! The layouts are model3dbc's `Model3CAN.dbc` (VehicleBus), read here as raw integers: the 56-bit git hashes do not
//! fit the `f64` the DBC decoder gives (they are read into a `u64`, exactly). All four messages are multiplexed with
//! the multiplexer in byte 0 (bits 0-7), every field Intel byte order (`@1+`), unsigned:
//!
//! | Message | Module | Fields (mux: start\|length) |
//! |---|---|---|
//! | 0x301 `VCFRONT_info` | front body controller | m13 infoAppCrc 8\|32, m20 infoBootCrc 24\|32, m17 infoAppGitHash 8\|56, m18 infoBootGitHash 8\|56, m10 infoBuildType 8\|3 + infoBuildConfigId 16\|16, m16 hcmlAppCRC 32\|32, m23 hcmrAppCRC 32\|32 |
//! | 0x300 `BMS_info` | battery management | m13 appCrc 32\|32, m20 bootCrc 32\|32, m17 appGitHash 8\|56, m18 bootGitHash 8\|56, m10 buildType 8\|3 + buildConfigId 16\|16 |
//! | 0x335 `DIR` info | rear drive unit | m13 applicationCrc 32\|32, m20 bootloaderCrc 32\|32, m17 appGitHash 8\|56, m18 bootGitHash 8\|56, m31 subcomponentGitHash 8\|56, m10 buildType 8\|3 + buildConfigurationId 16\|16, m16 oilPumpAppCrc 16\|32 + oilPumpBuildType 8\|3 |
//! | 0x656 `DIF` info | front drive unit | as the rear drive unit |
//!
//! A value of zero (a module that does not fill the field: on a 2026 Model Y the app git hashes read zero) and a value
//! with every bit of its field set (SNA, "signal not available": 255 for an 8-bit field) say nothing and are not
//! given.

/// One fingerprint field of a module's info message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The DBC's signal name (`BMS_appCrc`).
    pub name: &'static str,
    /// The multiplexer value (byte 0) of the frames that carry it.
    pub mux: u8,
    /// Intel (little-endian) start bit and length, as the DBC gives them.
    pub start: u32,
    pub len: u32,
}

impl Field {
    /// The largest value the field holds: every bit set (SNA).
    pub fn sna(&self) -> u64 {
        if self.len >= 64 { u64::MAX } else { (1u64 << self.len) - 1 }
    }

    /// How many hex digits the field is shown with: 14 for a 56-bit hash, 8 for a 32-bit CRC, 4 for a 16-bit id, 1
    /// for a 3-bit build type.
    pub fn hex_digits(&self) -> usize {
        self.len.div_ceil(4) as usize
    }

    /// The value as lowercase hex, zero-padded to the field's width.
    pub fn hex(&self, value: u64) -> String {
        format!("{value:0width$x}", width = self.hex_digits())
    }
}

/// A module that broadcasts fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    /// `vcfront`, `bms`, `dir`, `dif`.
    pub id: &'static str,
    /// What the pages call it.
    pub name: &'static str,
    pub can_id: u32,
    pub fields: &'static [Field],
}

const fn f(name: &'static str, mux: u8, start: u32, len: u32) -> Field {
    Field { name, mux, start, len }
}

const VCFRONT: &[Field] = &[
    f("VCFRONT_infoAppCrc", 13, 8, 32),
    f("VCFRONT_infoBootCrc", 20, 24, 32),
    f("VCFRONT_infoAppGitHash", 17, 8, 56),
    f("VCFRONT_infoBootGitHash", 18, 8, 56),
    f("VCFRONT_infoBuildType", 10, 8, 3),
    f("VCFRONT_infoBuildConfigId", 10, 16, 16),
    f("VCFRONT_hcmlAppCRC", 16, 32, 32),
    f("VCFRONT_hcmrAppCRC", 23, 32, 32),
];

const BMS: &[Field] = &[
    f("BMS_appCrc", 13, 32, 32),
    f("BMS_bootCrc", 20, 32, 32),
    f("BMS_appGitHash", 17, 8, 56),
    f("BMS_bootGitHash", 18, 8, 56),
    f("BMS_buildType", 10, 8, 3),
    f("BMS_buildConfigId", 10, 16, 16),
];

const DIR: &[Field] = &[
    f("DIR_applicationCrc", 13, 32, 32),
    f("DIR_bootloaderCrc", 20, 32, 32),
    f("DIR_appGitHash", 17, 8, 56),
    f("DIR_bootGitHash", 18, 8, 56),
    f("DIR_subcomponentGitHash", 31, 8, 56),
    f("DIR_buildType", 10, 8, 3),
    f("DIR_buildConfigurationId", 10, 16, 16),
    f("DIR_oilPumpAppCrc", 16, 16, 32),
    f("DIR_oilPumpBuildType", 16, 8, 3),
];

const DIF: &[Field] = &[
    f("DIF_applicationCrc", 13, 32, 32),
    f("DIF_bootloaderCrc", 20, 32, 32),
    f("DIF_appGitHash", 17, 8, 56),
    f("DIF_bootGitHash", 18, 8, 56),
    f("DIF_subcomponentGitHash", 31, 8, 56),
    f("DIF_buildType", 10, 8, 3),
    f("DIF_buildConfigurationId", 10, 16, 16),
    f("DIF_oilPumpAppCrc", 16, 16, 32),
    f("DIF_oilPumpBuildType", 16, 8, 3),
];

/// The modules, in the order the pages show them.
pub const MODULES: [Module; 4] = [
    Module { id: "vcfront", name: "Front body controller", can_id: 0x301, fields: VCFRONT },
    Module { id: "bms", name: "Battery management", can_id: 0x300, fields: BMS },
    Module { id: "dir", name: "Rear drive unit", can_id: 0x335, fields: DIR },
    Module { id: "dif", name: "Front drive unit", can_id: 0x656, fields: DIF },
];

/// The module that sends this id.
pub fn module_of(can_id: u32) -> Option<&'static Module> {
    MODULES.iter().find(|m| m.can_id == can_id)
}

/// A field by its name, with its module.
pub fn field(name: &str) -> Option<(&'static Module, &'static Field)> {
    MODULES.iter().find_map(|m| m.fields.iter().find(|f| f.name == name).map(|f| (m, f)))
}

/// The raw value of an Intel-order field: `len` bits from bit `start` (bit 0 is the low bit of byte 0). `None` when
/// the frame is too short to hold it.
pub fn raw(data: &[u8], start: u32, len: u32) -> Option<u64> {
    if len == 0 || len > 64 || start.checked_add(len)? as usize > data.len() * 8 {
        return None;
    }
    let mut v = 0u64;
    for i in 0..len {
        let bit = start + i;
        if (data[(bit >> 3) as usize] >> (bit & 7)) & 1 == 1 {
            v |= 1 << i;
        }
    }
    Some(v)
}

/// Writes an Intel-order field (the value cut to its width); the frame's other bits stay.
pub fn put(data: &mut [u8], start: u32, len: u32, value: u64) {
    for i in 0..len.min(64) {
        let bit = start + i;
        let Some(b) = data.get_mut((bit >> 3) as usize) else { return };
        if (value >> i) & 1 == 1 {
            *b |= 1 << (bit & 7);
        } else {
            *b &= !(1 << (bit & 7));
        }
    }
}

/// The fingerprints one frame carries: each field of its multiplexer value, with a value that says something (not
/// zero, not SNA). Empty for another id, a frame too short, or a multiplexer value without fingerprints. Which bus it
/// came on is the caller's to check (the vehicle bus only).
pub fn read(can_id: u32, data: &[u8]) -> Vec<(&'static Module, &'static Field, u64)> {
    let Some(m) = module_of(can_id) else { return Vec::new() };
    let Some(&mux) = data.first() else { return Vec::new() };
    m.fields
        .iter()
        .filter(|f| f.mux == mux)
        .filter_map(|f| {
            let v = raw(data, f.start, f.len)?;
            (v != 0 && v != f.sna()).then_some((m, f, v))
        })
        .collect()
}

/// An 8-byte info frame of `module` with multiplexer `mux` and these field values (by name; others of the frame are
/// zero), for tests and made-up logs.
pub fn frame(module: &Module, mux: u8, values: &[(&str, u64)]) -> [u8; 8] {
    let mut data = [0u8; 8];
    data[0] = mux;
    for (name, v) in values {
        if let Some(f) = module.fields.iter().find(|f| f.name == *name && f.mux == mux) {
            put(&mut data, f.start, f.len, *v);
        }
    }
    data
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn the_table_is_the_dbc_s() {
        // the bundled model3dbc file has every field with the same multiplexer, start, length and byte order
        let dbc = crate::Profile::Tesla3Y.dbc();
        for m in &MODULES {
            let msg = &dbc.messages[&m.can_id];
            assert_eq!(msg.bus, "VehicleBus", "{}", m.id);
            for f in m.fields {
                let s = msg.signals.iter().find(|s| s.name == f.name).unwrap_or_else(|| unreachable!("{}", f.name));
                assert_eq!((s.start, s.length, s.little, s.signed), (f.start, f.len, true, false), "{}", f.name);
                assert_eq!(s.mux, crate::Mux::Value(u64::from(f.mux)), "{}", f.name);
            }
            let mux = &msg.signals[msg.mux.unwrap()];
            assert_eq!((mux.start, mux.length, mux.little), (0, 8, true), "{}: the multiplexer is byte 0", m.id);
        }
    }

    #[test]
    fn fields_bit_exact_from_hand_built_frames() {
        // 0x300 m13: BMS_appCrc 32|32 is bytes 4-7, low byte first
        let d = bytes("0D000000484280F2");
        assert_eq!(read(0x300, &d), vec![(&MODULES[1], &BMS[0], 0xF280_4248)]);
        assert_eq!(BMS[0].hex(0xF280_4248), "f2804248");
        // 0x301 m13: VCFRONT_infoAppCrc 8|32 is bytes 1-4
        let d = bytes("0D78563412000000");
        assert_eq!(read(0x301, &d)[0].2, 0x1234_5678);
        // 0x301 m20: VCFRONT_infoBootCrc 24|32 is bytes 3-6
        let d = bytes("14000044332211FF");
        assert_eq!(read(0x301, &d)[0].2, 0x1122_3344);
        // a 56-bit git hash, bytes 1-7, exactly (more than a double holds)
        let d = bytes("11EFCDAB89674523");
        let got = read(0x335, &d);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].1.name, got[0].2), ("DIR_appGitHash", 0x23_4567_89AB_CDEF));
        assert_eq!(got[0].1.hex(got[0].2), "23456789abcdef");
        assert_eq!(got[0].1.hex(0x1F), "0000000000001f");
        // m10: build type 8|3 (the low 3 bits of byte 1) and build configuration 16|16 (bytes 2-3)
        let d = bytes("0AFA341200000000");
        let got = read(0x656, &d);
        let names: Vec<(&str, u64)> = got.iter().map(|(_, f, v)| (f.name, *v)).collect();
        assert_eq!(names, vec![("DIF_buildType", 2), ("DIF_buildConfigurationId", 0x1234)]);
        // m16 on a drive unit: oil pump CRC 16|32 (bytes 2-5) and its build type 8|3
        let d = bytes("1003DDCCBBAA0000");
        let names: Vec<(&str, u64)> = read(0x335, &d).iter().map(|(_, f, v)| (f.name, *v)).collect();
        assert_eq!(names, vec![("DIR_oilPumpAppCrc", 0xAABB_CCDD), ("DIR_oilPumpBuildType", 3)]);
        // m16 / m23 on the body controller: the headlights' CRCs, 32|32
        let d = bytes("1000000001020304");
        assert_eq!(read(0x301, &d)[0], (&MODULES[0], &VCFRONT[6], 0x0403_0201));
        let d = bytes("17000000A0B0C0D0");
        assert_eq!(read(0x301, &d)[0].1.name, "VCFRONT_hcmrAppCRC");
    }

    #[test]
    fn zero_sna_short_and_other_frames_say_nothing() {
        // zero
        assert!(read(0x300, &bytes("0D00000000000000")).is_empty());
        // SNA: every bit of the field set (a 3-bit build type of 7, a 32-bit CRC of ffffffff)
        assert!(read(0x300, &bytes("0D000000FFFFFFFF")).is_empty());
        let got = read(0x300, &bytes("0A07FFFF00000000"));
        assert!(got.is_empty(), "{got:?}");
        // a multiplexer value without fingerprints, another id, a frame too short
        assert!(read(0x300, &bytes("0E000000484280F2")).is_empty());
        assert!(read(0x302, &bytes("0D000000484280F2")).is_empty());
        assert!(read(0x300, &bytes("0D0000004842")).is_empty());
        assert!(read(0x300, &[]).is_empty());
        assert_eq!(raw(&[0xFF], 4, 8), None);
    }

    #[test]
    fn frames_written_read_back() {
        for m in &MODULES {
            for fl in m.fields {
                let mut v = 0x0123_4567_89AB_CDEF & fl.sna();
                if v == fl.sna() {
                    v -= 1;
                }
                let d = frame(m, fl.mux, &[(fl.name, v)]);
                assert_eq!(raw(&d, fl.start, fl.len), Some(v), "{}", fl.name);
                assert!(read(m.can_id, &d).iter().any(|(_, g, x)| g.name == fl.name && *x == v), "{}", fl.name);
            }
        }
        assert_eq!(field("DIF_oilPumpAppCrc").map(|(m, f)| (m.id, f.hex_digits())), Some(("dif", 8)));
        assert_eq!(field("nope"), None);
    }
}
