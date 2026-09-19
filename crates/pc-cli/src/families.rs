//! Printing families as the derivation tree the design calls for.
//!
//! A flat list of "duplicates" would be a lie about what is in the archive.
//! Indentation shows what came from what, so a raw frame, the JPEG beside it
//! and an export read as three renditions of one photograph rather than
//! three copies of one file.

use pc_core::{count_ru, fmt_bytes};
use pc_db::{Db, FamilyRow};
use pc_family::Role;

fn role_rank(role: &str) -> u8 {
    match Role::parse(role) {
        Some(Role::Original) => 0,
        Some(Role::CameraJpeg) => 1,
        Some(Role::Converted) => 2,
        Some(Role::Export) => 3,
        Some(Role::Resize) => 4,
        Some(Role::Copy) => 5,
        _ => 6,
    }
}

fn short_date(ts: Option<i64>) -> String {
    let Some(ts) = ts else {
        return "дата неизвестна".into();
    };
    // Civil date from a Unix timestamp, without pulling in a date library
    // for one line of output.
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
    format!(
        "{:02}.{:02}.{} {:02}:{:02}:{:02}",
        d,
        m,
        y,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

pub fn print_family(f: &FamilyRow, verbose: bool) {
    let mut members = f.members.clone();
    members.sort_by_key(|m| (role_rank(&m.role), std::cmp::Reverse(m.size)));

    let title = members.first().map(|m| m.name.clone()).unwrap_or_default();
    println!(
        "\n📷  {}  ·  {}  ·  {}  —  {}, {}",
        title,
        short_date(f.taken_at),
        f.camera.as_deref().unwrap_or("камера неизвестна"),
        count_ru(members.len() as i64, "файл", "файла", "файлов"),
        fmt_bytes(f.total_size() as u64)
    );

    for m in &members {
        let role = Role::parse(&m.role).unwrap_or(Role::Unknown);
        let mark = if role.removable_by_default() {
            "[x]"
        } else if m.is_keeper {
            " ★ "
        } else {
            "   "
        };
        println!(
            "  {mark} {:<11} {:<40} {:>10}  {:>9}",
            role.label(),
            truncate(&m.name, 40),
            format!("{}×{}", m.width, m.height),
            fmt_bytes(m.size as u64)
        );
        if verbose {
            let dir = m.path.rsplit_once('/').map_or("", |(a, _)| a);
            println!("       {}", truncate_start(dir, 72));
            if let Some(ev) = &m.evidence {
                if let Some(detail) = short_evidence(ev) {
                    println!("       связь: {detail}");
                }
            }
            println!("       оценка {:.0}: {}", m.quality, m.breakdown);
        }
    }
}

fn short_evidence(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let kind = v.get("kind")?.as_str()?;
    let detail = v.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    Some(format!("{kind} — {detail}"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max - 1).collect::<String>())
}

fn truncate_start(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    format!("…{}", s.chars().skip(n - max + 1).collect::<String>())
}

pub fn print_summary(db: &Db) -> anyhow::Result<()> {
    let rows = db.role_counts()?;
    if rows.is_empty() {
        println!("Семейства не построены. Выполните `photo-cleanup families build`.");
        return Ok(());
    }
    println!("\nПо ролям:");
    for (role, count, bytes) in rows {
        let r = Role::parse(&role).unwrap_or(Role::Unknown);
        let note = if r.removable_by_default() {
            "  ← удаляется по умолчанию"
        } else {
            ""
        };
        println!(
            "  {:<12} {:>8}  {:>10}{}",
            r.label(),
            count_ru(count, "файл", "файла", "файлов"),
            fmt_bytes(bytes as u64),
            note
        );
    }
    println!(
        "\nСемейств: {} (из них с несколькими файлами: {})",
        db.family_count(false)?,
        db.family_count(true)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_date_from_a_timestamp() {
        assert_eq!(short_date(Some(1_563_129_125)), "14.07.2019 18:32:05");
        assert_eq!(short_date(Some(951_868_800)), "01.03.2000 00:00:00");
        assert_eq!(short_date(None), "дата неизвестна");
    }

    #[test]
    fn roles_print_originals_before_copies() {
        assert!(role_rank("original") < role_rank("export"));
        assert!(role_rank("export") < role_rank("copy"));
    }

    #[test]
    fn truncation_counts_characters() {
        let s = "Тэфи_13.10.2025_очень_длинное_имя_файла.ARW";
        assert!(truncate(s, 10).chars().count() <= 10);
        assert!(truncate_start(s, 10).chars().count() <= 10);
    }
}
