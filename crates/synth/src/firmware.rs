//! Made-up firmware fingerprints ([`dbc::firmware`]): the info frames of the four modules on the vehicle bus, with
//! values chosen by the test, a change of some of them at the car's software update, gateway copies on the chassis
//! bus, and corrupt frames when asked.

use crate::signals::RawFrame;
use crate::sim::{CAN0, CAN1};
use dbc::firmware::MODULES;

/// What the made-up modules broadcast. Off unless a scenario is given one ([`crate::Scenario::firmware`]): a
/// scenario without it makes exactly the logs it always made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    /// Each field's value (by the DBC's name) until the software update; fields not listed are not sent.
    pub before: Vec<(&'static str, u64)>,
    /// The fields the software update changes, and their new values.
    pub after: Vec<(&'static str, u64)>,
    /// Gateway copies of the frames on the chassis bus (`can1`): `None` sends none; `Some` sends them, with these
    /// values in place of the vehicle bus's (empty: the same values), so a test sees they are not read.
    pub can1: Option<Vec<(&'static str, u64)>>,
    /// At each of these times (ms), the next info frame on the vehicle bus comes corrupt (its bytes after the
    /// multiplexer flipped): a value seen once.
    pub corrupt_at_ms: Vec<u64>,
}

/// How often each module sends each of its multiplexer values (ms).
pub const PERIOD_MS: u64 = 2_000;

impl Firmware {
    /// A made-up set: CRCs and build ids filled in, the app git hashes zero (as on a 2026 Model Y: not sent),
    /// the boot git hashes filled in; the update changes the battery's and the rear drive unit's app CRCs and the
    /// body controller's app CRC.
    pub fn made_up() -> Firmware {
        Firmware {
            before: vec![
                ("VCFRONT_infoAppCrc", 0x5eed_0001),
                ("VCFRONT_infoBootCrc", 0x5eed_0002),
                ("VCFRONT_infoBootGitHash", 0x00c0_ffee_0000_0001),
                ("VCFRONT_infoBuildType", 2),
                ("VCFRONT_infoBuildConfigId", 0x0101),
                ("VCFRONT_hcmlAppCRC", 0x5eed_0003),
                ("VCFRONT_hcmrAppCRC", 0x5eed_0004),
                ("BMS_appCrc", 0x5eed_0101),
                ("BMS_bootCrc", 0x5eed_0102),
                ("BMS_buildType", 2),
                ("BMS_buildConfigId", 0x0202),
                ("DIR_applicationCrc", 0x5eed_0201),
                ("DIR_bootloaderCrc", 0x5eed_0202),
                ("DIR_subcomponentGitHash", 0x00c0_ffee_0002_0001),
                ("DIR_buildType", 2),
                ("DIR_buildConfigurationId", 0x0303),
                ("DIR_oilPumpAppCrc", 0x5eed_0203),
                ("DIR_oilPumpBuildType", 1),
                ("DIF_applicationCrc", 0x5eed_0301),
                ("DIF_bootloaderCrc", 0x5eed_0302),
                ("DIF_buildType", 2),
                ("DIF_buildConfigurationId", 0x0404),
                ("DIF_oilPumpAppCrc", 0x5eed_0303),
                ("DIF_oilPumpBuildType", 1),
            ],
            after: vec![
                ("VCFRONT_infoAppCrc", 0x5eed_1001),
                ("BMS_appCrc", 0x5eed_1101),
                ("DIR_applicationCrc", 0x5eed_1201),
            ],
            can1: Some(Vec::new()),
            corrupt_at_ms: Vec::new(),
        }
    }

    /// The values sent now (`updated`: after the software update).
    pub fn values(&self, updated: bool) -> Vec<(&'static str, u64)> {
        let mut v = self.before.clone();
        if updated {
            for (name, x) in &self.after {
                match v.iter_mut().find(|(n, _)| n == name) {
                    Some(e) => e.1 = *x,
                    None => v.push((name, *x)),
                }
            }
        }
        v
    }
}

/// Sends a [`Firmware`]'s frames.
#[derive(Debug, Clone)]
pub(crate) struct Sender {
    fw: Firmware,
    /// The next corrupt time not yet used.
    corrupt: usize,
}

impl Sender {
    pub(crate) fn new(mut fw: Firmware) -> Sender {
        fw.corrupt_at_ms.sort_unstable();
        Sender { fw, corrupt: 0 }
    }

    /// The info frames of `[from_us, to_us)` on these buses: every module's multiplexer values each [`PERIOD_MS`],
    /// one module after another, at fixed offsets (no randomness: the scenario's other frames stay as they were).
    pub(crate) fn frames(&mut self, from_us: u64, to_us: u64, buses: u8, updated: bool, out: &mut Vec<RawFrame>) {
        if buses & (CAN0 | CAN1) == 0 || to_us <= from_us {
            return;
        }
        let values = self.fw.values(updated);
        let period_us = PERIOD_MS * 1000;
        let mut k = from_us / period_us;
        while k * period_us < to_us {
            for (mi, m) in MODULES.iter().enumerate() {
                let mut muxes: Vec<u8> = m.fields.iter().map(|f| f.mux).collect();
                muxes.dedup();
                for (j, mux) in muxes.into_iter().enumerate() {
                    let t = k * period_us + (mi as u64) * 50_000 + (j as u64) * 200_000 + 7_000;
                    if t < from_us || t >= to_us {
                        continue;
                    }
                    let mine: Vec<(&str, u64)> = m
                        .fields
                        .iter()
                        .filter(|f| f.mux == mux)
                        .filter_map(|f| values.iter().find(|(n, _)| *n == f.name).copied())
                        .collect();
                    if mine.is_empty() {
                        continue;
                    }
                    if buses & CAN0 != 0 {
                        let mut data = dbc::firmware::frame(m, mux, &mine);
                        if self.fw.corrupt_at_ms.get(self.corrupt).is_some_and(|&c| c * 1000 <= t) {
                            for b in &mut data[1..] {
                                *b ^= 0x5A;
                            }
                            self.corrupt += 1;
                        }
                        out.push(RawFrame { t_us: t, bus: 0, id: m.can_id, data: data.to_vec() });
                    }
                    if buses & CAN1 != 0
                        && let Some(over) = &self.fw.can1
                    {
                        let theirs: Vec<(&str, u64)> = mine
                            .iter()
                            .map(|&(n, v)| (n, over.iter().find(|(o, _)| *o == n).map_or(v, |o| o.1)))
                            .collect();
                        let data = dbc::firmware::frame(m, mux, &theirs);
                        out.push(RawFrame { t_us: t + 300, bus: 1, id: m.can_id, data: data.to_vec() });
                    }
                }
            }
            k += 1;
        }
    }
}
