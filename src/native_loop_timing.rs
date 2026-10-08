//! Opt-in Android native-loop diagnostic. Configure/snapshot only on the
//! rendering thread. This does not measure GPU time or alter frame operations.
use std::convert::TryFrom;
use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const CAPACITY: usize = 2048;
static ACTIVE: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub wall_ns: u64,
    pub thread_cpu_ns: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub epoch: u64,
    pub id: u64,
    pub relative_wall_ns: u64,
    pub offset_from_native_begin_ns: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u64,
    pub epoch: u64,
    pub begin_ns: u64,
    pub end_ns: u64,
    pub next_begin_ns: u64,
    pub begin_to_begin_ns: u64,
    /// Native begin through the finish hook after yield, including yield.
    pub frame_body_ns: u64,
    pub unsegmented_gap_ns: u64,
    /// Finish-hook end to next begin; excludes the small yield-end/finish gap.
    pub gap_after_yield_ns: u64,
    pub event_messages: u64,
    pub events: Option<Sample>,
    pub update: Option<Sample>,
    pub draw: Option<Sample>,
    pub swap: Option<Sample>,
    pub yield_phase: Option<Sample>,
    pub thread_cpu_total_ns: Option<u64>,
    pub unsegmented_thread_cpu_ns: Option<u64>,
    pub gap_after_yield_thread_cpu_ns: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Calibration {
    pub pairs: usize,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
}
#[derive(Clone, Debug)]
pub struct Report {
    pub enabled: bool,
    pub epoch: u64,
    pub cpu_mode: u8,
    pub cpu_requested: bool,
    pub cpu_enabled: bool,
    pub resolution_ns: Option<u64>,
    pub cpu_error: Option<String>,
    pub calibration: Option<Calibration>,
    pub cpu_reads: u64,
    pub cpu_failures: u64,
    pub wall_reads: u64,
    pub origin_set: bool,
    pub overflowed: bool,
    pub capacity: usize,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub report: Report,
    pub frames: Vec<Frame>,
    pub dropped: u64,
    pub discarded: u64,
    pub incomplete: u64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Phase {
    Events = 0,
    Update = 1,
    Draw = 2,
    Swap = 3,
    Yield = 4,
}
#[derive(Clone, Copy, Debug)]
struct Point {
    wall: u64,
    cpu: Option<u64>,
}
#[derive(Clone, Copy)]
pub(crate) struct Token {
    epoch: u64,
    id: u64,
    phase: Option<Phase>,
    start: Point,
}
struct Pending {
    id: u64,
    epoch: u64,
    begin: Point,
    end: Option<Point>,
    samples: [Option<Sample>; 5],
    next_phase: usize,
    last_wall: u64,
    messages: u64,
    cpu_valid: bool,
    valid: bool,
}
#[derive(Default)]
struct Journal {
    epoch: u64,
    sequence: u64,
    pending: Option<Pending>,
    frames: VecDeque<Frame>,
    dropped: u64,
    discarded: u64,
    overflowed: bool,
}
impl Journal {
    fn reset(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.discarded = u64::from(self.pending.take().is_some());
        self.frames.clear();
        self.dropped = 0;
    }
    fn discard(&mut self) {
        self.discarded = self.discarded.saturating_add(1);
    }
    fn invalidate_cpu(&mut self) {
        if let Some(p) = &mut self.pending {
            p.cpu_valid = false;
        }
    }
    fn begin(&mut self, now: Point) -> Option<Token> {
        if let Some(previous) = self.pending.take() {
            if let Some(frame) = Self::finalize(previous, now) {
                if self.frames.len() == CAPACITY {
                    self.frames.pop_front();
                    self.dropped = self.dropped.saturating_add(1);
                }
                self.frames.push_back(frame);
            } else {
                self.discard();
            }
        }
        let id = match self.sequence.checked_add(1) {
            Some(id) => id,
            None => {
                self.overflowed = true;
                return None;
            }
        };
        self.sequence = id;
        self.pending = Some(Pending {
            id,
            epoch: self.epoch,
            begin: now,
            end: None,
            samples: [None; 5],
            next_phase: 0,
            last_wall: now.wall,
            messages: 0,
            cpu_valid: now.cpu.is_some(),
            valid: true,
        });
        Some(Token {
            epoch: self.epoch,
            id,
            phase: None,
            start: now,
        })
    }
    fn accepts(&self, t: Token) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.epoch == t.epoch && p.id == t.id && p.end.is_none())
    }
    fn record(&mut self, t: Token, end: Point) {
        if !self.accepts(t) {
            return;
        }
        let p = self.pending.as_mut().unwrap();
        let phase = match t.phase {
            Some(phase) => phase as usize,
            None => {
                p.valid = false;
                return;
            }
        };
        if phase != p.next_phase || t.start.wall < p.last_wall || end.wall < t.start.wall {
            p.valid = false;
            return;
        }
        let cpu = t.start.cpu.zip(end.cpu).and_then(|(a, b)| b.checked_sub(a));
        p.cpu_valid &= cpu.is_some();
        p.samples[phase] = Some(Sample {
            wall_ns: end.wall - t.start.wall,
            thread_cpu_ns: cpu,
        });
        p.next_phase += 1;
        p.last_wall = end.wall;
    }
    fn skip(&mut self, phase: Phase) {
        if let Some(p) = &mut self.pending {
            if p.end.is_some() {
                return;
            }
            if phase as usize != p.next_phase {
                p.valid = false;
                return;
            }
            p.next_phase += 1;
        }
    }
    fn finish(&mut self, t: Token, end: Point) {
        if !self.accepts(t) {
            return;
        }
        let p = self.pending.as_mut().unwrap();
        p.cpu_valid &= end.cpu.is_some();
        if p.next_phase != 5 || end.wall < p.last_wall {
            p.valid = false;
        }
        p.end = Some(end);
    }
    fn message(&mut self) {
        if let Some(p) = &mut self.pending {
            if p.end.is_none() {
                match p.messages.checked_add(1) {
                    Some(count) => p.messages = count,
                    None => p.valid = false,
                }
            }
        }
    }
    fn finalize(mut p: Pending, next: Point) -> Option<Frame> {
        let end = p.end?;
        if !p.valid || p.next_phase != 5 || end.wall < p.begin.wall || next.wall < end.wall {
            return None;
        }
        let period = next.wall - p.begin.wall;
        let covered = p
            .samples
            .iter()
            .flatten()
            .try_fold(0u64, |sum, s| sum.checked_add(s.wall_ns))?;
        let gap = period.checked_sub(covered)?;
        p.cpu_valid &= next.cpu.is_some();
        let cpu_total = p
            .begin
            .cpu
            .zip(next.cpu)
            .and_then(|(a, b)| b.checked_sub(a));
        let cpu_after = end.cpu.zip(next.cpu).and_then(|(a, b)| b.checked_sub(a));
        let cpu_covered = p
            .samples
            .iter()
            .flatten()
            .try_fold(0u64, |sum, s| sum.checked_add(s.thread_cpu_ns?));
        let cpu_gap = cpu_total
            .zip(cpu_covered)
            .and_then(|(total, covered)| total.checked_sub(covered));
        p.cpu_valid &= cpu_total.is_some() && cpu_after.is_some() && cpu_gap.is_some();
        if !p.cpu_valid {
            for sample in p.samples.iter_mut().flatten() {
                sample.thread_cpu_ns = None;
            }
        }
        Some(Frame {
            id: p.id,
            epoch: p.epoch,
            begin_ns: p.begin.wall,
            end_ns: end.wall,
            next_begin_ns: next.wall,
            begin_to_begin_ns: period,
            frame_body_ns: end.wall - p.begin.wall,
            unsegmented_gap_ns: gap,
            gap_after_yield_ns: next.wall - end.wall,
            event_messages: p.messages,
            events: p.samples[0],
            update: p.samples[1],
            draw: p.samples[2],
            swap: p.samples[3],
            yield_phase: p.samples[4],
            thread_cpu_total_ns: p.cpu_valid.then_some(cpu_total).flatten(),
            unsegmented_thread_cpu_ns: p.cpu_valid.then_some(cpu_gap).flatten(),
            gap_after_yield_thread_cpu_ns: p.cpu_valid.then_some(cpu_after).flatten(),
        })
    }
}
struct State {
    enabled: bool,
    cpu_mode: u8,
    cpu_requested: bool,
    cpu_enabled: bool,
    resolution: Option<u64>,
    cpu_error: Option<String>,
    calibration: Option<Calibration>,
    cpu_reads: u64,
    cpu_failures: u64,
    wall_reads: u64,
    last_cpu: Option<u64>,
    origin: Option<Instant>,
    journal: Journal,
}
impl Default for State {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_mode: 0,
            cpu_requested: false,
            cpu_enabled: false,
            resolution: None,
            cpu_error: None,
            calibration: None,
            cpu_reads: 0,
            cpu_failures: 0,
            wall_reads: 0,
            last_cpu: None,
            origin: None,
            journal: Journal::default(),
        }
    }
}
impl State {
    fn report(&self) -> Report {
        Report {
            enabled: self.enabled,
            epoch: self.journal.epoch,
            cpu_mode: self.cpu_mode,
            cpu_requested: self.cpu_requested,
            cpu_enabled: self.cpu_enabled,
            resolution_ns: self.resolution,
            cpu_error: self.cpu_error.clone(),
            calibration: self.calibration,
            cpu_reads: self.cpu_reads,
            cpu_failures: self.cpu_failures,
            wall_reads: self.wall_reads,
            origin_set: self.origin.is_some(),
            overflowed: self.journal.overflowed,
            capacity: CAPACITY,
        }
    }
    fn accept_cpu(&mut self, value: Result<u64, String>) -> Option<u64> {
        self.cpu_reads += 1;
        match value {
            Ok(ns) if self.last_cpu.is_none_or(|last| ns >= last) => {
                self.last_cpu = Some(ns);
                Some(ns)
            }
            value => {
                self.cpu_failures += 1;
                self.cpu_enabled = false;
                self.cpu_error = Some(
                    value
                        .err()
                        .unwrap_or_else(|| "Thread CPU clock moved backwards".into()),
                );
                self.journal.invalidate_cpu();
                None
            }
        }
    }
    fn cpu(&mut self) -> Option<u64> {
        if self.cpu_enabled {
            self.accept_cpu(native_cpu(false))
        } else {
            None
        }
    }
    fn relative(&mut self, wall: Instant) -> Option<u64> {
        let origin = *self.origin.get_or_insert(wall);
        let ns = wall.checked_duration_since(origin)?.as_nanos();
        match u64::try_from(ns) {
            Ok(ns) => Some(ns),
            Err(_) => {
                self.journal.overflowed = true;
                self.enabled = false;
                ACTIVE.store(false, Ordering::Relaxed);
                None
            }
        }
    }
    fn start(&mut self) -> Option<Point> {
        let wall = Instant::now();
        self.wall_reads += 1;
        let cpu = self.cpu();
        Some(Point {
            wall: self.relative(wall)?,
            cpu,
        })
    }
    fn end(&mut self) -> Option<Point> {
        let cpu = self.cpu();
        let wall = Instant::now();
        self.wall_reads += 1;
        Some(Point {
            wall: self.relative(wall)?,
            cpu,
        })
    }
}
thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}

fn timespec_ns(sec: i64, nsec: i64) -> Result<u64, String> {
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err("Invalid thread CPU timespec".into());
    }
    (sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(nsec as u64))
        .ok_or_else(|| "Thread CPU timespec overflow".into())
}
#[cfg(target_os = "android")]
fn native_cpu(resolution: bool) -> Result<u64, String> {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    let status = unsafe {
        if resolution {
            libc::clock_getres(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts)
        } else {
            libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts)
        }
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    timespec_ns(ts.tv_sec as i64, ts.tv_nsec as i64)
}
#[cfg(not(target_os = "android"))]
fn native_cpu(_: bool) -> Result<u64, String> {
    Err("Android thread CPU clock unavailable on this target".into())
}

/// Same renderer-thread API. Epoch/ring/origin reset immediately; if called
/// inside draw, that native frame is discarded and only the next begin starts
/// measuring. Calibration cost occurs in the discarded configuration frame;
/// it is reported separately and is not included in later native samples or
/// subtracted from any measured interval.
pub fn configure(enabled: bool, cpu_mode: u8) -> Report {
    ACTIVE.store(false, Ordering::Relaxed);
    STATE.with(|slot| {
        let mut s = slot.borrow_mut();
        let epoch = match s.journal.epoch.checked_add(1) {
            Some(epoch) => epoch,
            None => {
                s.journal.overflowed = true;
                s.enabled = false;
                return s.report();
            }
        };
        s.journal.reset(epoch);
        s.enabled = enabled && !s.journal.overflowed;
        s.cpu_mode = cpu_mode;
        s.cpu_requested = enabled && cpu_mode != 0;
        s.cpu_enabled = false;
        s.resolution = None;
        s.cpu_error = None;
        s.calibration = None;
        s.cpu_reads = 0;
        s.cpu_failures = 0;
        s.wall_reads = 0;
        s.last_cpu = None;
        s.origin = None;
        if s.enabled {
            if s.journal.frames.capacity() < CAPACITY {
                s.journal.frames.reserve(CAPACITY);
            }
            match cpu_mode {
                0 => {}
                2 => s.cpu_error = Some("Forced missing thread CPU capability".into()),
                1 => {
                    match native_cpu(true) {
                        Ok(ns) if ns > 0 => {
                            s.resolution = Some(ns);
                            s.cpu_enabled = true;
                        }
                        value => {
                            s.cpu_error = Some(
                                value
                                    .err()
                                    .unwrap_or_else(|| "Zero thread CPU clock resolution".into()),
                            )
                        }
                    }
                    if s.cpu_enabled {
                        let mut pairs = [0u64; 256];
                        let mut complete = true;
                        for value in &mut pairs {
                            let wall = Instant::now();
                            s.wall_reads += 1;
                            let a = s.cpu();
                            let b = s.cpu();
                            *value = u64::try_from(wall.elapsed().as_nanos()).unwrap_or(u64::MAX);
                            s.wall_reads += 1;
                            if a.zip(b).and_then(|(a, b)| b.checked_sub(a)).is_none() {
                                complete = false;
                                break;
                            }
                        }
                        if complete {
                            pairs.sort_unstable();
                            s.calibration = Some(Calibration {
                                pairs: 256,
                                p50_ns: pairs[128],
                                p95_ns: pairs[243],
                                max_ns: pairs[255],
                                mean_ns: pairs.iter().map(|&v| v as f64).sum::<f64>() / 256.,
                            });
                        }
                    }
                }
                _ => s.cpu_error = Some("Invalid thread CPU mode".into()),
            }
        }
        ACTIVE.store(s.enabled, Ordering::Relaxed);
        s.report()
    })
}
pub fn disable() {
    configure(false, 0);
}
pub fn current_frame_id() -> Option<u64> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    STATE.with(|slot| {
        slot.borrow()
            .journal
            .pending
            .as_ref()
            .filter(|p| p.end.is_none())
            .map(|p| p.id)
    })
}
/// Uses the caller's existing Instant: no extra clock read or mutation.
pub fn stamp_at(wall: Instant) -> Option<Stamp> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    STATE.with(|slot| {
        let s = slot.borrow();
        let p = s.journal.pending.as_ref().filter(|p| p.end.is_none())?;
        let relative = u64::try_from(wall.checked_duration_since(s.origin?)?.as_nanos()).ok()?;
        Some(Stamp {
            epoch: p.epoch,
            id: p.id,
            relative_wall_ns: relative,
            offset_from_native_begin_ns: relative.checked_sub(p.begin.wall)?,
        })
    })
}
/// Returns completed records only. The last body-complete record is awaiting
/// its NEXT begin and remains excluded, together with a current in-flight frame.
pub fn snapshot() -> Snapshot {
    STATE.with(|slot| {
        let s = slot.borrow();
        Snapshot {
            report: s.report(),
            frames: s.journal.frames.iter().copied().collect(),
            dropped: s.journal.dropped,
            discarded: s.journal.discarded,
            incomplete: u64::from(s.journal.pending.is_some()),
        }
    })
}

pub(crate) fn begin_frame() -> Option<Token> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    STATE.with(|slot| {
        let mut s = slot.borrow_mut();
        let now = s.start()?;
        let token = s.journal.begin(now);
        if token.is_none() {
            s.enabled = false;
            s.cpu_enabled = false;
            ACTIVE.store(false, Ordering::Relaxed);
        }
        token
    })
}
pub(crate) fn begin_phase(phase: Phase) -> Option<Token> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    STATE.with(|slot| {
        let mut s = slot.borrow_mut();
        let p = s.journal.pending.as_ref().filter(|p| p.end.is_none())?;
        let (epoch, id) = (p.epoch, p.id);
        let start = s.start()?;
        Some(Token {
            epoch,
            id,
            phase: Some(phase),
            start,
        })
    })
}
pub(crate) fn end_phase(token: Option<Token>) {
    let Some(token) = token else { return };
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|slot| {
        let mut s = slot.borrow_mut();
        if !s.journal.accepts(token) {
            return;
        }
        if let Some(end) = s.end() {
            s.journal.record(token, end);
        }
    });
}
pub(crate) fn skip_phase(phase: Phase) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|slot| slot.borrow_mut().journal.skip(phase));
}
pub(crate) fn event_message() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|slot| slot.borrow_mut().journal.message());
}
pub(crate) fn finish_frame(token: Option<Token>) {
    let Some(token) = token else { return };
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|slot| {
        let mut s = slot.borrow_mut();
        if !s.journal.accepts(token) {
            return;
        }
        if let Some(end) = s.end() {
            s.journal.finish(token, end);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    static API_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn point(wall: u64, cpu: Option<u64>) -> Point {
        Point { wall, cpu }
    }
    fn complete(j: &mut Journal, t: Token, skip_draw: bool, cpu: bool) {
        for (index, phase) in [
            Phase::Events,
            Phase::Update,
            Phase::Draw,
            Phase::Swap,
            Phase::Yield,
        ]
        .iter()
        .copied()
        .enumerate()
        {
            if skip_draw && (index == 2 || index == 3) {
                j.skip(phase);
                continue;
            }
            let start = t.start.wall + 10 + index as u64 * 20;
            j.record(
                Token {
                    phase: Some(phase),
                    start: point(start, cpu.then_some(start / 2)),
                    ..t
                },
                point(start + 7, cpu.then_some((start + 7) / 2)),
            );
        }
        j.finish(
            t,
            point(t.start.wall + 120, cpu.then_some((t.start.wall + 120) / 2)),
        );
    }
    #[test]
    fn journal_forward_period_own_stages_gap_and_inflight_exclusion() {
        let mut j = Journal::default();
        j.epoch = 1;
        let a = j.begin(point(1000, Some(500))).unwrap();
        complete(&mut j, a, false, true);
        assert!(j.frames.is_empty());
        let b = j.begin(point(1200, Some(600))).unwrap();
        let frame = j.frames[0];
        assert_eq!(
            (frame.id, frame.begin_ns, frame.end_ns, frame.next_begin_ns),
            (a.id, 1000, 1120, 1200)
        );
        assert_eq!(frame.begin_to_begin_ns, 200);
        assert_eq!(frame.frame_body_ns, 120);
        assert_eq!(frame.unsegmented_gap_ns, 165);
        assert_eq!(frame.gap_after_yield_ns, 80);
        assert_eq!(frame.thread_cpu_total_ns, Some(100));
        assert_eq!(frame.unsegmented_thread_cpu_ns, Some(85));
        assert_eq!(frame.gap_after_yield_thread_cpu_ns, Some(40));
        complete(&mut j, b, true, true);
        j.begin(point(1400, Some(700))).unwrap();
        assert!(j.frames[1].draw.is_none() && j.frames[1].swap.is_none());
    }
    #[test]
    fn any_cpu_failure_clears_every_phase_total_and_gap_not_zero() {
        for missing in [false, true] {
            let mut j = Journal::default();
            let t = j.begin(point(0, Some(0))).unwrap();
            complete(&mut j, t, false, true);
            if missing {
                j.invalidate_cpu();
            }
            j.begin(point(200, None)).unwrap();
            let f = j.frames[0];
            for phase in [f.events, f.update, f.draw, f.swap, f.yield_phase]
                .into_iter()
                .flatten()
            {
                assert_eq!(phase.thread_cpu_ns, None);
            }
            assert!(
                f.thread_cpu_total_ns.is_none()
                    && f.unsegmented_thread_cpu_ns.is_none()
                    && f.gap_after_yield_thread_cpu_ns.is_none()
            );
        }
        let mut s = State::default();
        s.cpu_enabled = true;
        s.last_cpu = Some(9);
        assert_eq!(s.accept_cpu(Ok(8)), None);
        assert!(!s.cpu_enabled);
        assert_eq!(s.cpu_failures, 1);
        assert!(timespec_ns(-1, 0).is_err() && timespec_ns(0, 1_000_000_000).is_err());
    }
    #[test]
    fn ring_is_fixed_drop_counted_and_sequence_cannot_wrap() {
        let mut j = Journal::default();
        for i in 0..CAPACITY + 8 {
            let t = j.begin(point(i as u64 * 200, None)).unwrap();
            complete(&mut j, t, false, false);
        }
        j.begin(point((CAPACITY + 8) as u64 * 200, None)).unwrap();
        assert_eq!(j.frames.len(), CAPACITY);
        assert_eq!(j.dropped, 8);
        assert_eq!(j.frames.front().unwrap().id, 9);
        j.sequence = u64::MAX;
        j.pending = None;
        assert!(j.begin(point(999999, None)).is_none());
        assert!(j.overflowed);
    }
    #[test]
    fn reset_mid_stage_old_epoch_cannot_commit_half_frame() {
        let mut j = Journal::default();
        j.epoch = 1;
        let old = j.begin(point(0, None)).unwrap();
        j.reset(2);
        j.record(
            Token {
                phase: Some(Phase::Draw),
                ..old
            },
            point(50, None),
        );
        j.finish(old, point(80, None));
        assert!(j.pending.is_none() && j.frames.is_empty());
        let new = j.begin(point(100, None)).unwrap();
        assert!(new.id > old.id);
        j.finish(new, point(130, None));
        j.begin(point(300, None)).unwrap();
        assert!(j.frames.is_empty());
        assert_eq!(j.discarded, 2);
    }
    #[test]
    fn disabled_hooks_and_forced_missing_cpu_do_not_read_clocks() {
        let _lock = API_TEST_LOCK.lock().unwrap();
        configure(false, 0);
        let before = snapshot().report;
        let frame = begin_frame();
        let phase = begin_phase(Phase::Draw);
        event_message();
        end_phase(phase);
        finish_frame(frame);
        let after = snapshot().report;
        assert_eq!(before.wall_reads, 0);
        assert_eq!(after.wall_reads, 0);
        assert_eq!(after.cpu_reads, 0);
        configure(true, 2);
        assert!(!snapshot().report.cpu_enabled);
        assert_eq!(snapshot().report.cpu_reads, 0);
        assert_eq!(current_frame_id(), None);
        let t = begin_frame();
        assert!(current_frame_id().is_some());
        for phase in [
            Phase::Events,
            Phase::Update,
            Phase::Draw,
            Phase::Swap,
            Phase::Yield,
        ] {
            let token = begin_phase(phase);
            end_phase(token);
        }
        finish_frame(t);
        assert!(snapshot().frames.is_empty());
        begin_frame();
        let s = snapshot();
        assert_eq!(s.frames.len(), 1);
        assert!(s.frames[0].thread_cpu_total_ns.is_none());
        assert_eq!(s.report.cpu_reads, 0);
        disable();
    }
    #[test]
    fn stamp_at_uses_existing_instant_and_own_native_begin_offset() {
        let _lock = API_TEST_LOCK.lock().unwrap();
        configure(true, 0);
        assert!(stamp_at(Instant::now()).is_none());
        let t = begin_frame().unwrap();
        let wall = Instant::now();
        let before = snapshot().report.wall_reads;
        let stamp = stamp_at(wall).unwrap();
        let after = snapshot().report.wall_reads;
        assert_eq!(stamp.id, t.id);
        assert_eq!(stamp.epoch, t.epoch);
        assert_eq!(before, after);
        assert_eq!(stamp.relative_wall_ns, stamp.offset_from_native_begin_ns);
        disable();
        assert!(stamp_at(wall).is_none());
    }
}
