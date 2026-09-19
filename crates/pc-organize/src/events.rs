//! Splitting a stream of frames into events.
//!
//! An event is what a person would call "that day at the sea" — frames taken
//! close enough together in time to belong to one occasion. The only signal
//! used here is the gap between consecutive frames, because it is the only
//! one that is always present and never wrong in an interesting way: a long
//! silence between two frames really does separate two occasions.

use pc_core::time::{civil_from_unix, fmt_iso_date};

/// Six hours: long enough that a day of shooting stays one event, short
/// enough that a morning and an evening shoot separate.
pub const DEFAULT_GAP_SECS: i64 = 6 * 3600;

/// Contiguous runs of `sorted` that belong to one occasion.
pub fn split(sorted: &[i64], gap_secs: i64) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    if sorted.is_empty() {
        return out;
    }
    let mut start = 0usize;
    for i in 1..sorted.len() {
        if sorted[i] - sorted[i - 1] > gap_secs {
            out.push(start..i);
            start = i;
        }
    }
    out.push(start..sorted.len());
    out
}

/// Directory name for an event: the date, plus the starting time when the
/// day holds more than one.
///
/// The first event of a day keeps the plain date, so an archive where most
/// days hold a single occasion reads as plain dates — and the days that need
/// distinguishing say why they are distinguished.
pub fn dir_name(start: i64, nth_in_day: usize) -> String {
    let date = fmt_iso_date(start);
    if nth_in_day == 0 {
        return date;
    }
    let (_, _, _, hh, mm, _) = civil_from_unix(start);
    format!("{date}_{hh:02}{mm:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;

    #[test]
    fn a_long_silence_separates_two_occasions() {
        let base = 1_563_118_921;
        let ts = vec![base, base + 60, base + 120, base + 9 * H, base + 9 * H + 30];
        let parts = split(&ts, DEFAULT_GAP_SECS);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], 0..3);
        assert_eq!(parts[1], 3..5);
    }

    #[test]
    fn a_day_of_shooting_stays_one_event() {
        let base = 1_563_118_921;
        let ts: Vec<i64> = (0..12).map(|i| base + i * H).collect();
        assert_eq!(split(&ts, DEFAULT_GAP_SECS).len(), 1);
    }

    #[test]
    fn the_second_event_of_a_day_is_named_by_its_start() {
        // 2019-07-14 15:42 UTC
        let ts = 1_563_118_921;
        assert_eq!(dir_name(ts, 0), "2019-07-14");
        assert_eq!(dir_name(ts, 1), "2019-07-14_1542");
    }

    #[test]
    fn nothing_in_gives_nothing_out() {
        assert!(split(&[], DEFAULT_GAP_SECS).is_empty());
    }
}
