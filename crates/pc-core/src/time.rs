use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn mtime_unix(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Civil date and clock from a Unix timestamp, UTC.
///
/// Howard Hinnant's algorithm. A date library would be one dependency for
/// what fits in a dozen lines, and the timestamps in the archive are naive
/// local time written by cameras anyway — no zone arithmetic is wanted here.
pub fn civil_from_unix(ts: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = era * 400 + yoe + i64::from(m <= 2);
    (y, m, d, secs / 3600, (secs / 60) % 60, secs % 60)
}

/// Days since the epoch for a civil date, UTC. Inverse of `civil_from_unix`.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2019-07-14` — the form the reorganised tree is named after.
pub fn fmt_iso_date(ts: i64) -> String {
    let (y, m, d, ..) = civil_from_unix(ts);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `14.07.2019 15:42:01` — the form shown to the user.
pub fn fmt_datetime_ru(ts: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil_from_unix(ts);
    format!("{d:02}.{m:02}.{y} {hh:02}:{mm:02}:{ss:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_and_back_agree() {
        for ts in [0i64, 1_000_000_000, 1_563_000_000, 2_000_000_000] {
            let (y, m, d, hh, mm, ss) = civil_from_unix(ts);
            assert_eq!(
                days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss,
                ts
            );
        }
    }

    #[test]
    fn dates_are_formatted_for_the_tree_and_for_the_user() {
        // 2019-07-14 15:42:01 UTC
        let ts = 1_563_118_921;
        assert_eq!(fmt_iso_date(ts), "2019-07-14");
        assert_eq!(fmt_datetime_ru(ts), "14.07.2019 15:42:01");
    }
}
