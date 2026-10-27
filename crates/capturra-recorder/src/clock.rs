use std::time::Instant;

/// One monotonic clock shared by every recorded stream. Never use wall-clock time.
///
/// It also remembers where it started on the macOS host clock (`mach_absolute_time`, the clock
/// ScreenCaptureKit stamps frames with), so a frame's presentation timestamp can be placed on it.
#[derive(Clone, Copy)]
pub struct Clock {
    start: Instant,
    host_origin_ns: u64,
}

impl Clock {
    pub fn start() -> Self {
        Clock { start: Instant::now(), host_origin_ns: host_now_ns() }
    }

    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Nanoseconds since the clock started.
    pub fn now_ns(&self) -> i64 {
        self.start.elapsed().as_nanos() as i64
    }

    /// Host-clock time (ns) at which this clock read zero; 0 where there is no host clock.
    pub fn host_origin_ns(&self) -> u64 {
        self.host_origin_ns
    }
}

/// Nanoseconds on the host clock that ScreenCaptureKit presentation timestamps use
/// (`CLOCK_UPTIME_RAW`, i.e. `mach_absolute_time`).
#[cfg(target_os = "macos")]
pub fn host_now_ns() -> u64 {
    extern "C" {
        fn clock_gettime_nsec_np(clock_id: u32) -> u64;
    }
    const CLOCK_UPTIME_RAW: u32 = 8;
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}

#[cfg(not(target_os = "macos"))]
pub fn host_now_ns() -> u64 {
    0
}

/// A frame stamped later than it arrived is impossible; allow this much for the two clock reads.
const FUTURE_SLACK_MS: u64 = 20;
/// A frame that arrives more than this long after it was captured means the timestamp is not on the
/// host clock (or the stream stalled); trust the arrival time instead.
pub const MAX_PTS_LAG_MS: u64 = 2000;

/// Where a frame captured at presentation time `pts_ns` (host clock) lands on the recording clock,
/// and whether that came from the timestamp. A frame captured before the clock started lands on 0.
/// If the timestamp is in the future or absurdly old, `arrival_ms` is used and the flag is false.
pub fn pts_to_clock_ms(pts_ns: u64, host_origin_ns: u64, arrival_ms: u64) -> (u64, bool) {
    if host_origin_ns == 0 || pts_ns == 0 {
        return (arrival_ms, false);
    }
    if host_origin_ns.saturating_sub(pts_ns) / 1_000_000 > MAX_PTS_LAG_MS {
        return (arrival_ms, false); // captured long before the clock started: not the host clock
    }
    let ms = pts_ns.saturating_sub(host_origin_ns) / 1_000_000;
    if ms > arrival_ms + FUTURE_SLACK_MS || arrival_ms.saturating_sub(ms) > MAX_PTS_LAG_MS {
        return (arrival_ms, false);
    }
    (ms.min(arrival_ms), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: u64 = 5_000_000_000_000;

    #[test]
    fn a_timestamp_lands_on_the_clock_relative_to_its_start() {
        // captured 1200 ms after the clock started, delivered 25 ms later
        assert_eq!(pts_to_clock_ms(ORIGIN + 1_200_000_000, ORIGIN, 1225), (1200, true));
    }

    #[test]
    fn a_frame_captured_before_the_clock_started_lands_on_zero() {
        assert_eq!(pts_to_clock_ms(ORIGIN - 30_000_000, ORIGIN, 10), (0, true));
    }

    #[test]
    fn a_timestamp_in_the_future_or_far_in_the_past_falls_back_to_arrival() {
        assert_eq!(pts_to_clock_ms(ORIGIN + 5_000_000_000, ORIGIN, 1000), (1000, false));
        assert_eq!(pts_to_clock_ms(ORIGIN - 3_000_000_000, ORIGIN, 100), (100, false), "3 s before the clock started");
        assert_eq!(pts_to_clock_ms(ORIGIN + 1_000_000_000, ORIGIN, 9000), (9000, false), "delivered 8 s late");
    }

    #[test]
    fn no_host_clock_means_arrival_time() {
        assert_eq!(pts_to_clock_ms(123, 0, 500), (500, false));
        assert_eq!(pts_to_clock_ms(0, ORIGIN, 500), (500, false));
    }

    #[test]
    fn two_clock_reads_a_few_ms_apart_do_not_push_a_frame_past_its_arrival() {
        assert_eq!(pts_to_clock_ms(ORIGIN + 1_010_000_000, ORIGIN, 1000), (1000, true));
    }

    #[test]
    fn the_host_clock_matches_the_instant_clock() {
        let c = Clock::start();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let host_ms = host_now_ns().saturating_sub(c.host_origin_ns()) / 1_000_000;
        let now = c.now_ms();
        if cfg!(target_os = "macos") {
            assert!(host_ms.abs_diff(now) <= 10, "host {host_ms} ms vs instant {now} ms");
        }
    }
}
