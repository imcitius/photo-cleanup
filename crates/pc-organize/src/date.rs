//! Which day a photograph belongs to, and how much that answer is worth.
//!
//! The ladder is ordered by how much the source knows: the camera that took
//! the frame, then whoever named the file, then whoever named the folder,
//! then the filesystem — which usually records when the archive was last
//! copied, not when the shutter fired. The answer travels with its source so
//! the user can see which files were placed on a guess.

use pc_core::time::days_from_civil;
use pc_db::OrganizeRow;

/// Where the date came from, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// `DateTimeOriginal` — when the shutter fired.
    Manual,
    Exif,
    /// `CreateDate` / `DateTimeDigitized`.
    Digitized,
    /// `DateTime` — last written, often the edit rather than the capture.
    FileDateTime,
    Filename,
    Path,
    /// `mtime`: when the bytes were last written, which for a copied archive
    /// is the day of the copy.
    Mtime,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Exif => "exif",
            Self::Digitized => "digitized",
            Self::FileDateTime => "file-datetime",
            Self::Filename => "filename",
            Self::Path => "path",
            Self::Mtime => "mtime",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "вручную",
            Self::Exif => "съёмка (EXIF)",
            Self::Digitized => "оцифровка (EXIF)",
            Self::FileDateTime => "правка (EXIF)",
            Self::Filename => "имя файла",
            Self::Path => "путь",
            Self::Mtime => "mtime файла",
        }
    }

    /// Whether the source itself knows about the photograph, as opposed to
    /// about the file that happens to hold it.
    pub fn confident(self) -> bool {
        matches!(
            self,
            Self::Manual | Self::Exif | Self::Digitized | Self::FileDateTime
        )
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => Self::Manual,
            "exif" => Self::Exif,
            "digitized" => Self::Digitized,
            "file-datetime" => Self::FileDateTime,
            "filename" => Self::Filename,
            "path" => Self::Path,
            "mtime" => Self::Mtime,
            _ => return None,
        })
    }
}

/// How specific the answer is. A folder named `2019` dates a file to a year
/// and no further, and pretending otherwise would invent a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy)]
pub struct Dated {
    pub ts: i64,
    pub source: Source,
    pub precision: Precision,
}

impl Dated {
    /// A date the user should look at before trusting the placement.
    pub fn uncertain(&self) -> bool {
        !self.source.confident() || self.precision != Precision::Day
    }
}

/// Timestamps outside this range are a dead camera battery, not a date.
/// A Sony with a flat clock writes 1980-01-01; a wrong year in the future is
/// just as much a mistake, and both would build a whole folder around a lie.
const EARLIEST: i64 = 631_152_000; // 1990-01-01

fn plausible(ts: i64) -> bool {
    ts >= EARLIEST && ts <= pc_core::time::now_unix() + 86_400
}

fn year_at(comp: &str) -> Option<i64> {
    let digits: String = comp.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 4 {
        return None;
    }
    let y: i64 = digits.parse().ok()?;
    (1990..=2100).contains(&y).then_some(y)
}

/// `2019-07`, `2019_07`, `2019.07`, and `2019-07 Отпуск`.
fn year_month(comp: &str) -> Option<(i64, i64)> {
    let y = year_at(comp)?;
    let rest = &comp[4..];
    let mut it = rest.chars();
    if !matches!(it.next(), Some('-' | '_' | '.' | ' ')) {
        return None;
    }
    let digits: String = it.take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 2 {
        return None;
    }
    let m: i64 = digits.parse().ok()?;
    (1..=12).contains(&m).then_some((y, m))
}

fn bare_month(comp: &str) -> Option<i64> {
    if comp.len() != 2 || !comp.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let m: i64 = comp.parse().ok()?;
    (1..=12).contains(&m).then_some(m)
}

/// The `_HHMM` an event directory carries when its day holds more than one.
///
/// Reading our own naming back matters more than it looks: on a second pass
/// the reorganised tree *is* the input, and a file whose only date was its
/// mtime would otherwise come back dated to midnight and be shuffled into a
/// different event than the one it was just placed in.
fn event_time(comp: &str) -> Option<i64> {
    let (date, rest) = comp.split_at_checked(10)?;
    let mut d = date.chars();
    let shaped = d.by_ref().take(4).all(|c| c.is_ascii_digit())
        && d.next() == Some('-')
        && d.by_ref().take(2).all(|c| c.is_ascii_digit())
        && d.next() == Some('-')
        && d.take(2).all(|c| c.is_ascii_digit());
    if !shaped {
        return None;
    }
    let rest = rest.strip_prefix('_')?;
    if rest.len() != 4 || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hh: i64 = rest[0..2].parse().ok()?;
    let mm: i64 = rest[2..4].parse().ok()?;
    ((0..24).contains(&hh) && (0..60).contains(&mm)).then_some(hh * 3600 + mm * 60)
}

/// A date read out of the directories a file sits in.
///
/// Deepest first: `.../2019/2019-07-14 Крым/` should answer with the day, not
/// with the year two levels up.
pub fn date_from_path(dir: &str) -> Option<(i64, Precision)> {
    let comps: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for (i, comp) in comps.iter().enumerate().rev() {
        if let Some(ts) = pc_image::meta::date_from_name(comp) {
            return Some((ts + event_time(comp).unwrap_or(0), Precision::Day));
        }
        if let Some((y, m)) = year_month(comp) {
            return Some((days_from_civil(y, m, 1) * 86_400, Precision::Month));
        }
        // `2019/07/` — the month means nothing without the year above it.
        if let Some(m) = bare_month(comp) {
            if let Some(y) = i.checked_sub(1).and_then(|p| year_at(comps[p])) {
                return Some((days_from_civil(y, m, 1) * 86_400, Precision::Month));
            }
        }
        if let Some(y) = year_at(comp) {
            if comp.len() == 4 || !comp[4..].starts_with(|c: char| c.is_ascii_digit()) {
                return Some((days_from_civil(y, 1, 1) * 86_400, Precision::Year));
            }
        }
    }
    None
}

/// Walk the ladder until something answers.
pub fn resolve(row: &OrganizeRow) -> Dated {
    if let Some(ts) = row
        .taken_at
        .filter(|ts| row.date_source.as_deref() == Some("manual") || plausible(*ts))
    {
        let source = row
            .date_source
            .as_deref()
            .and_then(Source::parse)
            // A date with no recorded source came from the container itself.
            .unwrap_or(Source::FileDateTime);
        return Dated {
            ts,
            source,
            precision: Precision::Day,
        };
    }

    let dir = row.path.rsplit_once('/').map_or("", |(a, _)| a);
    if let Some((ts, precision)) = date_from_path(dir).filter(|(ts, _)| plausible(*ts)) {
        return Dated {
            ts,
            source: Source::Path,
            precision,
        };
    }

    Dated {
        ts: row.mtime,
        source: Source::Mtime,
        precision: Precision::Day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, taken: Option<i64>, source: Option<&str>, mtime: i64) -> OrganizeRow {
        OrganizeRow {
            id: 1,
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            mtime,
            taken_at: taken,
            date_source: source.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn exif_wins_over_everything_else() {
        let d = resolve(&row(
            "/mnt/disk3/2005/DSC01234.ARW",
            Some(1_563_118_921),
            Some("exif"),
            1_700_000_000,
        ));
        assert_eq!(d.source, Source::Exif);
        assert_eq!(d.ts, 1_563_118_921);
        assert!(!d.uncertain());
    }

    #[test]
    fn a_dead_camera_clock_is_not_a_date() {
        // 1980-01-01, which is what a Sony writes with a flat battery.
        let d = resolve(&row(
            "/mnt/disk3/2019/2019-07-14/DSC01234.ARW",
            Some(315_532_800),
            Some("exif"),
            1_700_000_000,
        ));
        assert_eq!(
            d.source,
            Source::Path,
            "неправдоподобный EXIF должен уступить"
        );
        assert_eq!(pc_core::time::fmt_iso_date(d.ts), "2019-07-14");
    }

    #[test]
    fn the_path_answers_at_the_precision_it_knows() {
        assert_eq!(
            date_from_path("/mnt/disk3/foto/2019/2019-07-14 Крым")
                .unwrap()
                .1,
            Precision::Day
        );
        assert_eq!(
            date_from_path("/mnt/disk3/foto/2019-07 Отпуск").unwrap().1,
            Precision::Month
        );
        assert_eq!(
            date_from_path("/mnt/disk3/foto/2019/07").unwrap().1,
            Precision::Month
        );
        assert_eq!(
            date_from_path("/mnt/disk3/foto/2019").unwrap().1,
            Precision::Year
        );
        assert!(date_from_path("/mnt/disk3/foto/отпуск").is_none());
    }

    #[test]
    fn an_event_directory_reads_back_at_the_time_it_was_named_with() {
        let (ts, precision) = date_from_path("/dst/2019/2019-07-14_2000").unwrap();
        assert_eq!(precision, Precision::Day);
        assert_eq!(pc_core::time::fmt_datetime_ru(ts), "14.07.2019 20:00:00");

        // A plain event directory still means the day and nothing finer.
        let (ts, _) = date_from_path("/dst/2019/2019-07-14").unwrap();
        assert_eq!(pc_core::time::fmt_datetime_ru(ts), "14.07.2019 00:00:00");
        // Not our scheme: the trailing digits are part of somebody's name.
        let (ts, _) = date_from_path("/dst/2019/2019-07-14_крым").unwrap();
        assert_eq!(pc_core::time::fmt_datetime_ru(ts), "14.07.2019 00:00:00");
    }

    #[test]
    fn a_bare_month_without_a_year_above_it_means_nothing() {
        assert!(date_from_path("/mnt/disk3/foto/07").is_none());
    }

    #[test]
    fn mtime_is_the_last_resort_and_is_marked_as_a_guess() {
        let d = resolve(&row(
            "/mnt/disk3/foto/отпуск/img.jpg",
            None,
            None,
            1_563_118_921,
        ));
        assert_eq!(d.source, Source::Mtime);
        assert!(d.uncertain());
    }
}
