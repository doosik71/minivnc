//! Congestion control for clients that support fences, ported from TurboVNC
//! `flowcontrol.c` (TCP Vegas on top of RTT pings), numbers included.
//!
//! Positions are byte offsets in the stream to the client and wrap like the
//! original's `unsigned`; times are kept in milliseconds as there.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Gets going fairly quickly on a network with decent bandwidth.
const INITIAL_WINDOW: u32 = 16384;
/// TCP's minimum is 3 * MSS; the MSS is unknown, so 4 KB is a guess.
const MINIMUM_WINDOW: u32 = 4096;
/// Linux's default maximum window (4 MB).
const MAXIMUM_WINDOW: u32 = 4_194_304;
/// `(unsigned)-1`: no measurement yet.
const UNKNOWN: u32 = u32::MAX;

fn ms_between(first: Instant, second: Instant) -> u32 {
    second.saturating_duration_since(first).as_millis() as u32
}

/// `a` comes after `b`, even across wraparound.
fn is_after(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) <= u32::MAX / 2
}

/// Bytes delayed by buffering, as milliseconds at the current window.
fn buffer_delay(extra: u32, base_rtt: u32, window: u32) -> u32 {
    extra.wrapping_mul(base_rtt) / window
}

#[derive(Clone, Copy, Debug)]
struct Ping {
    time: Instant,
    pos: u32,
    extra: u32,
    congested: bool,
}

#[derive(Debug)]
pub(crate) struct Congestion {
    window: u32,
    in_slow_start: bool,
    last_position: u32,
    extra_buffer: u32,
    last_update: Instant,
    last_sent: Instant,
    last_adjustment: Instant,
    last_pong: Ping,
    last_pong_arrival: Instant,
    base_rtt: u32,
    min_rtt: u32,
    min_congested_rtt: u32,
    measurements: u32,
    pings: VecDeque<Ping>,
}

impl Congestion {
    pub fn new(now: Instant) -> Self {
        Self {
            window: INITIAL_WINDOW,
            in_slow_start: true,
            last_position: 0,
            extra_buffer: 0,
            last_update: now,
            last_sent: now,
            last_adjustment: now,
            last_pong: Ping {
                time: now,
                pos: 0,
                extra: 0,
                congested: false,
            },
            last_pong_arrival: now,
            base_rtt: UNKNOWN,
            min_rtt: UNKNOWN,
            min_congested_rtt: UNKNOWN,
            measurements: 0,
            pings: VecDeque::new(),
        }
    }

    /// `rfbUpdatePosition`: registers the current stream position.
    pub fn update_position(&mut self, now: Instant, pos: u32) {
        let delta = pos.wrapping_sub(self.last_position);
        if delta > 0 || self.extra_buffer > 0 {
            self.last_sent = now;
        }

        // Idle for too long (a crude RTO)? Close the window and measure
        // the wire latency again.
        if ms_between(self.last_sent, now) > self.base_rtt.wrapping_mul(2).max(100) {
            self.window = self.window.min(INITIAL_WINDOW);
            self.base_rtt = UNKNOWN;
            self.measurements = 0;
            self.last_adjustment = now;
            self.min_rtt = UNKNOWN;
            self.min_congested_rtt = UNKNOWN;
            self.in_slow_start = true;
        }

        // Estimate the extra delay of overbuffering, to separate it from the
        // delay caused by a wrong congestion window.
        if self.base_rtt != UNKNOWN {
            self.extra_buffer = self.extra_buffer.wrapping_add(delta);
            let consumed = (u64::from(ms_between(self.last_update, now)) * u64::from(self.window)
                / u64::from(self.base_rtt)) as u32;
            self.extra_buffer = self.extra_buffer.saturating_sub(consumed);
        }

        self.last_position = pos;
        self.last_update = now;
    }

    /// `rfbSendRTTPing`, minus the fence itself: call just before sending a
    /// `Request | BlockBefore` fence with payload `[1]`.
    pub fn ping(&mut self, now: Instant, pos: u32) {
        self.update_position(now, pos);
        let ping = Ping {
            time: now,
            pos: self.last_position,
            extra: self.extra_buffer_at(now),
            congested: self.congested_at(now),
        };
        self.pings.push_back(ping);
    }

    /// `HandleRTTPong`: the client answered the oldest ping.
    pub fn pong(&mut self, now: Instant) {
        let Some(ping) = self.pings.pop_front() else {
            return;
        };
        self.last_pong = ping;
        self.last_pong_arrival = now;

        let mut rtt = ms_between(ping.time, now).max(1);
        // The lowest observed latency estimates the wire latency.
        if rtt < self.base_rtt {
            self.base_rtt = rtt;
        }
        // Pings from before the last adjustment do not measure the current
        // window.
        if ping.time < self.last_adjustment {
            return;
        }
        // Remove the delay added by overtaxed buffers.
        let delay = buffer_delay(ping.extra, self.base_rtt, self.window);
        rtt = if delay < rtt { rtt - delay } else { 1 };
        // Less than the wire latency means the window was underestimated;
        // pretend there was no buffer latency.
        rtt = rtt.max(self.base_rtt);

        self.min_rtt = self.min_rtt.min(rtt);
        if ping.congested {
            self.min_congested_rtt = self.min_congested_rtt.min(rtt);
        }
        self.measurements += 1;
        self.update_congestion(now);
    }

    fn congested_at(&self, now: Instant) -> bool {
        self.in_flight(now) >= self.window
    }

    /// `rfbIsCongested`: `None` if more data can be sent now, otherwise how
    /// long to wait before trying again.
    pub fn congested(&mut self, now: Instant, pos: u32) -> Option<Duration> {
        self.update_position(now, pos);
        if !self.congested_at(now) {
            return None;
        }
        let eta = self.uncongested_eta(now);
        Some(Duration::from_millis(if eta <= 0 { 1 } else { eta as u64 }))
    }

    /// `GetUncongestedETA`: milliseconds until the transport is no longer
    /// congested, 0 if it is not, -1 if unknown.
    fn uncongested_eta(&self, now: Instant) -> i64 {
        let target_acked = self.last_position.wrapping_sub(self.window);
        if is_after(self.last_pong.pos, target_acked) {
            return 0;
        }
        if self.base_rtt == UNKNOWN {
            return -1;
        }
        let mut prev = self.last_pong;
        let mut eta: u32 = 0;
        let elapsed = ms_between(self.last_pong_arrival, now);
        // Walk the pings to find the one whose pong ends the congestion. If
        // there is none, pretend a ping followed the last position update.
        let last = Ping {
            time: self.last_update,
            pos: self.last_position,
            extra: self.extra_buffer,
            congested: false,
        };
        for cur in self.pings.iter().copied().chain(std::iter::once(last)) {
            let mut eta_next = ms_between(prev.time, cur.time).wrapping_add(buffer_delay(
                cur.extra,
                self.base_rtt,
                self.window,
            ));
            let delay = buffer_delay(prev.extra, self.base_rtt, self.window);
            eta_next = eta_next.saturating_sub(delay);
            if is_after(cur.pos, target_acked) {
                eta = eta.wrapping_add(
                    eta_next.wrapping_mul(cur.pos.wrapping_sub(target_acked))
                        / cur.pos.wrapping_sub(prev.pos).max(1),
                );
                return i64::from(eta.saturating_sub(elapsed));
            }
            eta = eta.wrapping_add(eta_next);
            prev = cur;
        }
        -1
    }

    /// `GetExtraBuffer`
    fn extra_buffer_at(&self, now: Instant) -> u32 {
        if self.base_rtt == UNKNOWN {
            return 0;
        }
        let consumed = (u64::from(ms_between(self.last_update, now)) * u64::from(self.window)
            / u64::from(self.base_rtt)) as u32;
        self.extra_buffer.saturating_sub(consumed)
    }

    /// `GetInFlight`: bytes sent but not yet through to the client.
    fn in_flight(&self, now: Instant) -> u32 {
        if self.last_position == self.last_pong.pos {
            return 0;
        }
        if self.base_rtt == UNKNOWN {
            return self
                .pings
                .front()
                .map_or(0, |p| self.last_position.wrapping_sub(p.pos));
        }
        // Without a ping in flight, pretend one followed the last update.
        let next = self.pings.front().copied().unwrap_or(Ping {
            time: self.last_update,
            pos: self.last_position,
            extra: self.extra_buffer,
            congested: false,
        });
        // Interpolate how far the next pong should have got by now.
        let mut eta_next = ms_between(self.last_pong.time, next.time).wrapping_add(buffer_delay(
            next.extra,
            self.base_rtt,
            self.window,
        ));
        let delay = buffer_delay(self.last_pong.extra, self.base_rtt, self.window);
        eta_next = eta_next.saturating_sub(delay);
        let elapsed = ms_between(self.last_pong_arrival, now);
        // The pong should be here very soon: be optimistic.
        let acked = if eta_next <= elapsed {
            next.pos
        } else {
            self.last_pong.pos.wrapping_add(
                (u64::from(next.pos.wrapping_sub(self.last_pong.pos)) * u64::from(elapsed)
                    / u64::from(eta_next)) as u32,
            )
        };
        self.last_position.wrapping_sub(acked)
    }

    /// `UpdateCongestion`: adjusts the window from the latest measurements.
    fn update_congestion(&mut self, now: Instant) {
        // At least three measurements, to avoid noise.
        if self.measurements < 3 {
            return;
        }
        // Aim for a window slightly too large: a few extra ms of delay.
        let mut diff = self.min_rtt.wrapping_sub(self.base_rtt);

        if diff > 100u32.max(self.base_rtt / 2) {
            // A massive latency spike probably means packet loss.
            self.window = (u64::from(self.window) * u64::from(self.base_rtt)
                / u64::from(self.min_rtt)) as u32;
            self.in_slow_start = false;
        }

        if self.in_slow_start {
            if diff > 25 {
                // Increased latency: the limit is reached.
                self.window = (u64::from(self.window) * u64::from(self.base_rtt)
                    / u64::from(self.min_rtt)) as u32;
                self.in_slow_start = false;
            } else {
                // Grow only if the whole window was used.
                diff = self.min_congested_rtt.wrapping_sub(self.base_rtt);
                if diff < 25 {
                    self.window = self.window.wrapping_mul(2);
                }
            }
        } else if diff > 50 {
            // Congestion avoidance (Vegas): slightly too fast. After a
            // spike the window can already be below 4096; the original's
            // unsigned subtraction would wrap to the maximum window.
            self.window = self.window.saturating_sub(4096);
        } else {
            diff = self.min_congested_rtt.wrapping_sub(self.base_rtt);
            if diff < 5 {
                self.window = self.window.wrapping_add(8192);
            } else if diff < 25 {
                self.window = self.window.wrapping_add(4096);
            }
        }

        self.window = self.window.clamp(MINIMUM_WINDOW, MAXIMUM_WINDOW);
        self.measurements = 0;
        self.last_adjustment = now;
        self.min_rtt = UNKNOWN;
        self.min_congested_rtt = UNKNOWN;
    }

    #[cfg(test)]
    fn window(&self) -> u32 {
        self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn idle_connection_is_not_congested() {
        let t0 = Instant::now();
        let mut c = Congestion::new(t0);
        assert_eq!(c.congested(t0, 0), None);
        // Before any pong, everything since the first ping is in flight.
        c.ping(t0, 0);
        assert_eq!(c.congested(t0, 10_000), None);
        assert!(c.congested(t0, INITIAL_WINDOW).is_some());
    }

    #[test]
    fn fast_link_grows_window_in_slow_start() {
        let mut t = Instant::now();
        let mut c = Congestion::new(t);
        let mut pos = 0u32;
        for _ in 0..3 {
            // A full window per round trip, answered after 10 ms.
            c.ping(t, pos);
            pos = pos.wrapping_add(c.window());
            c.ping(t, pos);
            t += 10 * MS;
            c.pong(t);
            c.pong(t);
        }
        assert_eq!(c.window(), INITIAL_WINDOW * 2);
    }

    #[test]
    fn latency_spike_shrinks_window() {
        let mut t = Instant::now();
        let mut c = Congestion::new(t);
        let mut pos = 0u32;
        // Three quick round trips set a 10 ms base RTT.
        for _ in 0..3 {
            c.ping(t, pos);
            t += 10 * MS;
            pos += 100;
            c.update_position(t, pos);
            c.pong(t);
        }
        assert_eq!(c.window(), INITIAL_WINDOW);
        // Then pongs take 300 ms, while data keeps flowing (not idle).
        for _ in 0..3 {
            c.ping(t, pos);
            for _ in 0..6 {
                t += 50 * MS;
                pos += 100;
                c.update_position(t, pos);
            }
            c.pong(t);
        }
        assert_eq!(c.window(), MINIMUM_WINDOW);
    }
}
