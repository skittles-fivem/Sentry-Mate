//! A made-up Screenmate CAN Bridge: takes the car's frames as they come, writes one log a minute (and closes one
//! `quiet` after 60 s without frames) with the bridge's own comment lines, packs it in either format, signs it and
//! chains it, and writes its sidecar (SMO `docs/sentry-manager-format.md`).

use crate::plan::Trouble;
use crate::signals::RawFrame;
use crate::signer::Signer;
use std::collections::VecDeque;
use std::fmt::Write as _;

/// Which format the logs are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Formats {
    /// `canbridge-candump-xz/1` only (a server that doesn't list /2 in its ping).
    Candump,
    /// `canbridge-columns-xz/2` only.
    #[default]
    Columns,
    /// Candump text for the first `n` logs, then columns (the bridge saw the server's ping list /2).
    Switch(u32),
}

/// One finished log and its sidecar, as the bridge uploads them.
#[derive(Debug, Clone)]
pub struct LogFile {
    /// `<vehicleKey>_<UTC yyyyMMddTHHmmssZ>_<seq>.log.xz`
    pub name: String,
    pub bytes: Vec<u8>,
    pub sidecar: serde_json::Value,
    pub format: logfmt::Format,
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: u64,
    pub closed_because: &'static str,
}

impl LogFile {
    /// `<name>.json`
    pub fn sidecar_name(&self) -> String {
        format!("{}.json", self.name)
    }

    pub fn sidecar_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.sidecar).unwrap_or_default()
    }
}

/// Its settings.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub vehicle_key: String,
    pub formats: Formats,
    pub xz_preset: u32,
    pub app_build: u32,
    pub trouble: Trouble,
}

struct Open {
    start_us: u64,
    text: String,
    frames: u64,
    last_frame_us: Option<u64>,
    clock_pairs: Vec<(u64, i64)>,
    next_clock_us: u64,
    dropped_chunks: u32,
    lost: bool,
    drop_note: bool,
}

/// A wall time as candump writes it.
fn wall(us: u64) -> String {
    format!("{}.{:06}", us / 1_000_000, us % 1_000_000)
}

/// `yyyyMMddTHHmmssZ` of a UTC time (ms).
pub fn utc_stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil from days (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub struct Bridge {
    cfg: BridgeConfig,
    signer: Signer,
    open: Option<Open>,
    out: VecDeque<LogFile>,
    seq: u32,
    written: u32,
    /// The Screenmate's boot: (wall µs, monotonic ns) at one moment.
    boot: (u64, i64),
    /// The helper's anchor (wall µs, monotonic ns) and its next `# helper-clock` line.
    anchor: (u64, i64),
    next_helper_us: u64,
    /// Recording paused since (µs), and frames dropped meanwhile.
    paused: Option<(u64, u64)>,
    note: Option<String>,
    /// Restarting (an app update) until.
    restarting_until: Option<u64>,
    pub lost: Vec<String>,
    pub not_recorded: Vec<(u64, u64)>,
    pub dropped: Vec<String>,
    /// When the app really updated itself (ms).
    pub app_updates: Vec<u64>,
}

const QUIET_US: u64 = 60_000_000;
const ROTATE_US: u64 = 60_000_000;
const HELPER_EVERY_US: u64 = 20_000_000;

impl Bridge {
    pub fn new(cfg: BridgeConfig, signer: Signer, start_us: u64, uptime_s: u64) -> Bridge {
        let boot_mono = (uptime_s as i64) * 1_000_000_000 + 123_456_789;
        let boot = (start_us, boot_mono);
        Bridge {
            cfg,
            signer,
            open: None,
            out: VecDeque::new(),
            seq: 0,
            written: 0,
            boot,
            anchor: boot,
            next_helper_us: start_us,
            paused: None,
            note: None,
            restarting_until: None,
            lost: Vec::new(),
            not_recorded: Vec::new(),
            dropped: Vec::new(),
            app_updates: Vec::new(),
        }
    }

    pub fn signer(&self) -> &Signer {
        &self.signer
    }

    pub fn take(&mut self) -> Option<LogFile> {
        self.out.pop_front()
    }

    fn mono(&self, us: u64) -> i64 {
        self.boot.1 + (us as i64 - self.boot.0 as i64) * 1000
    }

    fn header(&self) -> String {
        let b = self.cfg.app_build;
        format!(
            "# canbridge v1 app=0.2.{b} build={b} device=Screenmate_One ts_source=helper-receive \
             jitter=median1.2ms,p99_6ms,max19ms\n"
        )
    }

    /// The timed comment lines due before `us` in the open log.
    fn timed_lines(&mut self, us: u64) {
        let (anchor, boot) = (self.anchor, self.boot);
        let Some(o) = self.open.as_mut() else { return };
        loop {
            let helper = self.next_helper_us;
            let clock = o.next_clock_us;
            if helper.min(clock) >= us {
                break;
            }
            let mono = |t: u64| boot.1 + (t as i64 - boot.0 as i64) * 1000;
            if helper <= clock {
                if helper >= o.start_us {
                    let _ = writeln!(
                        o.text,
                        "# helper-clock anchor_wall_us={} anchor_mono_ns={} now_wall_us={helper} now_mono_ns={} uid=0",
                        anchor.0,
                        anchor.1,
                        mono(helper)
                    );
                }
                self.next_helper_us = helper + HELPER_EVERY_US;
            } else {
                let _ = writeln!(o.text, "# clock wall={} mono_ns={}", wall(clock), mono(clock));
                o.clock_pairs.push((clock / 1000, mono(clock)));
                o.next_clock_us = clock + ROTATE_US;
            }
        }
    }

    fn open_at(&mut self, us: u64) {
        let lost = self.cfg.trouble.lose_log_at.iter().any(|&t| t * 1000 >= us && t * 1000 < us + ROTATE_US);
        let mut text = self.header();
        if let Some(n) = self.note.take() {
            text.push_str(&n);
        }
        let mono = self.mono(us);
        let _ = writeln!(text, "# clock wall={} mono_ns={mono}", wall(us));
        self.seq += 1;
        self.open = Some(Open {
            start_us: us,
            text,
            frames: 0,
            last_frame_us: None,
            clock_pairs: vec![(us / 1000, mono)],
            next_clock_us: us + ROTATE_US,
            dropped_chunks: 0,
            lost,
            drop_note: false,
        });
        // the helper's lines go on from where they are
        while self.next_helper_us < us {
            self.next_helper_us += HELPER_EVERY_US;
        }
    }

    fn close(&mut self, end_us: u64, why: &'static str) {
        self.timed_lines(end_us);
        let Some(mut o) = self.open.take() else { return };
        let mono_end = self.mono(end_us);
        let _ = writeln!(o.text, "# clock wall={} mono_ns={mono_end} end", wall(end_us));
        o.clock_pairs.push((end_us / 1000, mono_end));
        let columns = match self.cfg.formats {
            Formats::Candump => false,
            Formats::Columns => true,
            Formats::Switch(n) => self.written >= n,
        };
        self.written += 1;
        let body = if columns {
            crate::columns::encode(o.text.lines().map(str::as_bytes))
        } else {
            o.text.clone().into_bytes()
        };
        let Ok(bytes) = crate::xz::pack(&body, self.cfg.xz_preset) else { return };
        let start_ms = o.start_us / 1000;
        let name = format!("{}_{}_{}.log.xz", self.cfg.vehicle_key, utc_stamp(start_ms), self.seq);
        let integrity = self.signer.sign(&self.cfg.vehicle_key, &name, &bytes);
        let format = if columns { logfmt::Format::Columns } else { logfmt::Format::Candump };
        let pairs: Vec<serde_json::Value> =
            o.clock_pairs.iter().map(|&(w, m)| serde_json::json!({ "wall_ms": w, "mono_ns": m })).collect();
        let sidecar = serde_json::json!({
            "format": format.name(),
            "file": name,
            "start_wall_ms": start_ms,
            "end_wall_ms": end_us / 1000,
            "start_mono_ns": self.mono(o.start_us),
            "end_mono_ns": mono_end,
            "frames": o.frames,
            "dropped_chunks": o.dropped_chunks,
            "closed_because": why,
            "clock_pairs": pairs,
            "app_version": format!("0.2.{}", self.cfg.app_build),
            "app_build": self.cfg.app_build,
            "device": "Screenmate_One",
            "ts_source": "helper-receive",
            "jitter": { "median_ms": 1.2, "p99_ms": 6.0, "max_ms": 19.0, "measured": true },
            "bus": { "can0": "vehicle (inferred from IDs)", "can1": "chassis (inferred from IDs)", "can3": "screenmate-internal" },
            "dbc": [{ "name": "Model3CAN.dbc", "source": "bundled" }],
            "integrity": integrity,
        });
        let log = LogFile {
            name,
            bytes,
            sidecar,
            format,
            start_ms,
            end_ms: end_us / 1000,
            frames: o.frames,
            closed_because: why,
        };
        if o.lost {
            self.lost.push(log.name);
        } else {
            if o.drop_note {
                self.dropped.push(log.name.clone());
            }
            self.out.push_back(log);
        }
    }

    /// What happens at `us` whatever the car does: a pause in recording, an app update, the Screenmate off.
    fn events(&mut self, us: u64) -> bool {
        let ms = us / 1000;
        // the app updates itself: the log closes early, the helper starts again
        if let Some(until) = self.restarting_until {
            if us < until {
                return false;
            }
            self.restarting_until = None;
            self.seq = 0;
            self.cfg.app_build += 1;
            self.anchor = (us, self.mono(us));
            self.next_helper_us = us;
        }
        // (whatever the car does: the log open then, if any, closes `flush`)
        if let Some(&(at, len)) = self.cfg.trouble.app_update_at.iter().find(|&&(at, _)| ms >= at) {
            if self.open.is_some() {
                self.close(at * 1000, "flush");
            }
            self.cfg.trouble.app_update_at.retain(|&(a, _)| a != at);
            self.app_updates.push(at);
            self.restarting_until = Some((at + len) * 1000);
            return false;
        }
        // the Screenmate is off: nothing at all, and it boots again after
        if let Some(&(a, b)) = self.cfg.trouble.offline.iter().find(|&&(a, b)| ms >= a && ms < b) {
            if self.open.is_some() {
                self.close(a * 1000, "stop");
            }
            self.boot = (b * 1000, 9_000_000_000);
            self.anchor = self.boot;
            self.seq = 0;
            self.next_helper_us = b * 1000;
            return false;
        }
        // recording paused (the log folder is full)
        let pause = self.cfg.trouble.not_recorded.iter().find(|&&(a, b)| ms >= a && ms < b).copied();
        match (pause, self.paused) {
            (Some((a, _)), None) => {
                if self.open.is_some() {
                    self.close(a * 1000, "rotate");
                }
                self.paused = Some((a * 1000, 0));
                false
            }
            (Some(_), Some(_)) => false,
            (None, Some((since, frames))) => {
                self.paused = None;
                let s = (us.saturating_sub(since)).div_ceil(1_000_000);
                self.note = Some(format!(
                    "# not recorded: paused {s} s, about {} KB of frames (log folder full)\n",
                    (frames * 28).div_ceil(1024)
                ));
                self.not_recorded.push((since / 1000, ms));
                true
            }
            (None, None) => true,
        }
    }

    /// The first time at or after `from_us` that one of the bridge's own events begins (the Screenmate off, a pause
    /// in recording, an app update), µs.
    fn next_event_us(&self, from_us: u64) -> u64 {
        let t = &self.cfg.trouble;
        t.offline
            .iter()
            .map(|&(a, _)| a)
            .chain(t.not_recorded.iter().map(|&(a, _)| a))
            .chain(t.app_update_at.iter().map(|&(a, _)| a))
            .map(|a| a * 1000)
            .filter(|&a| a >= from_us)
            .min()
            .unwrap_or(u64::MAX)
    }

    /// Every time in `(from_us, to_us)` where the bridge's trouble begins or ends, in order (µs): a sleep is cut there,
    /// so each happens on time and each pause is measured from its real edges.
    pub fn boundaries(&self, from_us: u64, to_us: u64) -> Vec<u64> {
        let t = &self.cfg.trouble;
        let mut b: Vec<u64> = t
            .offline
            .iter()
            .chain(&t.not_recorded)
            .flat_map(|&(a, b)| [a, b])
            .chain(t.app_update_at.iter().flat_map(|&(a, l)| [a, a + l]))
            .chain(t.lose_log_at.iter().copied())
            .map(|ms| ms * 1000)
            .filter(|&us| us > from_us && us < to_us)
            .collect();
        b.sort_unstable();
        b.dedup();
        b
    }

    /// A log 60 s without frames closes `quiet` before anything the bridge does later (`us` at the latest).
    fn quiet_before_events(&mut self, us: u64) {
        let Some(o) = &self.open else { return };
        let last = o.last_frame_us.unwrap_or(o.start_us);
        let cut = us.min(self.next_event_us(last));
        self.quiet_check(cut);
    }

    /// Frames of one step, in time order.
    pub fn frames(&mut self, frames: &[RawFrame]) {
        for f in frames {
            self.quiet_before_events(f.t_us);
            if !self.events(f.t_us) {
                if let Some((since, n)) = self.paused {
                    self.paused = Some((since, n + 1));
                }
                continue;
            }
            self.quiet_check(f.t_us);
            match &self.open {
                None => self.open_at(f.t_us),
                Some(o) if f.t_us >= o.start_us + ROTATE_US => {
                    let end = o.start_us + ROTATE_US;
                    self.close(end, "rotate");
                    self.open_at(end);
                }
                Some(_) => {}
            }
            self.timed_lines(f.t_us);
            let drop =
                self.cfg.trouble.drop_at.iter().any(|&(at, len)| f.t_us >= at * 1000 && f.t_us < (at + len) * 1000);
            let Some(o) = self.open.as_mut() else { continue };
            if drop {
                if !o.drop_note {
                    o.drop_note = true;
                    o.dropped_chunks += 3;
                    o.text.push_str("# dropped 3 chunks of frames: logger fell behind\n");
                }
                continue;
            }
            let _ = write!(o.text, "({}) can{} ", wall(f.t_us), f.bus);
            if f.id > 0x7FF {
                let _ = write!(o.text, "{:08X}#", f.id);
            } else {
                let _ = write!(o.text, "{:03X}#", f.id);
            }
            for b in &f.data {
                let _ = write!(o.text, "{b:02X}");
            }
            o.text.push('\n');
            o.frames += 1;
            o.last_frame_us = Some(f.t_us);
        }
    }

    /// Time passes to `us` with no frames before it: a log 60 s without frames closes `quiet`.
    pub fn quiet_check(&mut self, us: u64) {
        let Some(o) = &self.open else { return };
        let last = o.last_frame_us.unwrap_or(o.start_us);
        if us >= last + QUIET_US {
            self.close(last + QUIET_US, "quiet");
        }
    }

    /// The time is now `us` (an asleep span, or a step with no frames).
    pub fn tick(&mut self, us: u64) {
        self.quiet_before_events(us);
        let _ = self.events(us);
        self.quiet_check(us);
    }

    /// The scenario's end at `us`: a log that would close `quiet` by then does; the one still being written is not
    /// finished, so it is not given (a longer run of the same scenario finishes it, under the same name), and the
    /// logs given are always the start of a longer run's, byte for byte.
    pub fn finish(&mut self, us: u64) {
        self.tick(us);
        // not signed: the chain goes on from the log before it
        self.open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamps() {
        assert_eq!(utc_stamp(1_772_409_600_000), "20260302T000000Z");
        assert_eq!(utc_stamp(1_767_225_600_000 + 13 * 3_600_000 + 61_000), "20260101T130101Z");
        assert_eq!(utc_stamp(951_782_400_000), "20000229T000000Z");
    }
}
