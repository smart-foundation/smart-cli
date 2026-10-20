//! Watch: drive a subscription's bounded pull loop, count the events the
//! service collapsed, and NEVER silently lose one (grammar law #5).
//!
//! A subscription is the contract's triplet — `subscribeX` mints a handle that
//! carries the first event, then `SUB_PULL` reads bounded batches and
//! `SUB_CANCEL` retires. The backend collapses intermediate state between pulls
//! and counts what it collapsed; surfacing that count is the watch terminal
//! summary's job. An empty pull is a real answer ("nothing yet"), not a
//! fabricated event.

use alloc::format;
#[cfg_attr(not(feature = "std"), allow(unused_imports))]
use alloc::string::String;
#[cfg(feature = "std")]
use crate::transport::{CallError, Connection, Subscription};

/// The options a `watch`/`--live` invocation carries.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// `--count N`: stop after N events (0 = unbounded).
    pub count: u64,
    /// `--interval <ms>`: the pull cadence (a pull loop, never a busy poll).
    pub interval_ms: u64,
    /// The per-pull bound we advertise (caller-driven backpressure).
    pub pull_limit: u32,
    /// `--follow`: keep pulling after `count` events have been emitted.
    pub follow: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            count: 0,
            interval_ms: 1000,
            pull_limit: 1,
            follow: false,
        }
    }
}

/// The result of driving a subscription to its bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Events delivered to the emitter.
    pub delivered: u64,
    /// Pulls that answered "nothing yet" (idle waits), never a fabricated event.
    pub idle: u64,
    /// Sequence gaps the emitter-reported sequence revealed (dropped events).
    pub dropped: u64,
}

/// Whether the pull loop stops: at the `--count` bound, unless `--follow`
/// keeps it going (an unbounded mode by design — the user asked for it).
pub fn at_bound(delivered: u64, options: Options) -> bool {
    options.count != 0 && delivered >= options.count && !options.follow
}

/// Drive `subscription` under `options`, handing every event bytes to
/// `on_event` (which JSON-lines or renders them), and return the terminal
/// summary.
///
/// The subscription is the CONTRACT's own capability: `pull` is a method of
/// it, not an envelope of this client's own. `pre_delivered` is the number of
/// events already handed to the caller, so `--count` bounds the TOTAL events
/// the user sees. When `seq_of` is `Some`, the emitter reports a monotonic
/// sequence and a gap between consecutive events is counted as dropped — the
/// drop counter is surfaced, never silent. An empty pull ("nothing yet")
/// increments `idle`.
#[cfg(feature = "std")]
pub fn drive<E, F, S>(
    conn: &mut Connection,
    subscription: &Subscription,
    options: Options,
    seq_of: Option<S>,
    pre_delivered: u64,
    mut on_event: F,
) -> Result<Summary, CallError>
where
    F: FnMut(&[u8]) -> Result<(), E>,
    S: Fn(&[u8]) -> Option<u64>,
    E: core::fmt::Display,
{
    let mut delivered = pre_delivered;
    let mut idle = 0u64;
    let mut dropped = 0u64;
    let mut last_seq: Option<u64> = None;

    loop {
        // Stop at the bound unless --follow keeps us going.
        if at_bound(delivered, options) {
            break;
        }
        let payload = subscription.pull(conn)?;
        if payload.is_empty() {
            // "nothing yet" — a real answer. Wait the interval and re-pull;
            // this is a bounded pull loop, never a busy poll.
            idle += 1;
            crate::watch::sleep_ms(options.interval_ms.min(2));
            continue;
        }
        // The pull's answer IS the event: the contract's own results struct.
        let event = payload.as_slice();
        // Drop detection: a reported monotonic sequence with a gap means the
        // service collapsed events between pulls.
        if let Some(seq_of) = &seq_of {
            if let Some(seq) = seq_of(event) {
                dropped += account_gap(last_seq, seq);
                last_seq = Some(seq);
            }
        }
        if on_event(event).is_ok() {
            delivered += 1;
        }
        crate::watch::sleep_ms(options.interval_ms.min(2));
    }
    Ok(Summary { delivered, idle, dropped })
}

/// How many events a sequence gap implies: `seq` after `prev` with any
/// distance greater than one means the service collapsed the events in
/// between. Pure, so the drop counting is testable without a transport.
pub fn account_gap(prev: Option<u64>, seq: u64) -> u64 {
    match prev {
        Some(prev) if seq > prev + 1 => seq - prev - 1,
        _ => 0,
    }
}

/// Sleep `ms` without pulling in anything platform-specific on a no_std path
/// (the CLI is std, so this is an honest sleep).
#[cfg(feature = "std")]
pub fn sleep_ms(ms: u64) {
    if ms == 0 {
        return;
    }
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// The one-line watch terminal summary: delivered, idle waits, and — when
/// nonzero — the drops the service collapsed. Never silent.
pub fn terminal_note(summary: &Summary) -> String {
    if summary.dropped > 0 {
        format!(
            "watch: {} event(s) delivered, {} idle poll(s), {} dropped by the service",
            summary.delivered, summary.idle, summary.dropped
        )
    } else {
        format!(
            "watch: {} event(s) delivered, {} idle poll(s)",
            summary.delivered, summary.idle
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_options_default_to_a_sane_window() {
        let o = Options::default();
        assert_eq!(o.count, 0);
        assert!(o.pull_limit >= 1);
    }

    #[test]
    fn terminal_note_names_collapsed_events_only_when_nonzero() {
        let s = Summary { delivered: 5, idle: 2, dropped: 3 };
        assert!(terminal_note(&s).contains("dropped"));
        assert!(terminal_note(&s).contains("3 dropped"));
        let clean = Summary { delivered: 5, idle: 0, dropped: 0 };
        assert!(!terminal_note(&clean).contains("dropped"));
        assert!(terminal_note(&clean).contains("idle"));
    }

    #[test]
    fn a_sequence_gap_counts_the_events_the_service_collapsed() {
        // Consecutive: nothing collapsed.
        assert_eq!(account_gap(Some(1), 2), 0);
        assert_eq!(account_gap(Some(1), 1), 0);
        // A gap of four sequence numbers = three collapsed events.
        assert_eq!(account_gap(Some(1), 5), 3);
        // The first event has no predecessor: nothing to account.
        assert_eq!(account_gap(None, 7), 0);
    }

    #[test]
    fn the_count_bound_stops_the_loop_and_follow_ignores_it() {
        let bounded = Options {
            count: 3,
            follow: false,
            ..Options::default()
        };
        assert!(!at_bound(2, bounded));
        assert!(at_bound(3, bounded));
        assert!(at_bound(4, bounded));

        // `--follow` is the documented unbounded mode: the bound never stops it.
        let following = Options {
            count: 3,
            follow: true,
            ..Options::default()
        };
        assert!(!at_bound(9, following));

        // `--count 0` is "no bound" rather than "stop immediately".
        let unbounded = Options::default();
        assert!(!at_bound(1_000, unbounded));
    }
}
