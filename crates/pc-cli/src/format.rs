use anyhow::{bail, Result};
use pc_core::{fmt_bytes, DerivedKind};
use pc_db::{Bundle, BundleState};
use std::collections::BTreeMap;

pub fn parse_size(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 1024i64),
        Some('M' | 'm') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G' | 'g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('T' | 't') => (&s[..s.len() - 1], 1024i64.pow(4)),
        _ => (s, 1),
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot parse the size “{s}” (examples: 100M, 1.5G, 4096)"))?;
    if v < 0.0 {
        bail!("a size cannot be negative");
    }
    Ok((v * mult as f64) as i64)
}

pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1i64),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        _ => (s, 86_400),
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot parse the period “{s}” (examples: 7d, 24h, 30m)"))?;
    Ok((v * mult as f64) as i64)
}

/// Order the kinds so the biggest, safest win is printed first and the
/// protected kind is printed last, where it reads as a note rather than an
/// option.
fn kind_order(k: DerivedKind) -> u8 {
    match k {
        DerivedKind::LrPreviews => 0,
        DerivedKind::LrSmartPreviews => 1,
        DerivedKind::LrDataOther => 2,
        DerivedKind::LrHelper => 3,
        DerivedKind::SystemJunk => 4,
        DerivedKind::LrCatalogData => 5,
    }
}

pub fn print_grouped(bundles: &[Bundle]) {
    print_grouped_opts(bundles, true)
}

pub fn print_grouped_opts(bundles: &[Bundle], footer: bool) {
    let mut groups: BTreeMap<(u8, &str), Vec<&Bundle>> = BTreeMap::new();
    for b in bundles {
        groups
            .entry((kind_order(b.kind), b.kind.as_str()))
            .or_default()
            .push(b);
    }

    let mut grand_removable = 0u64;

    for ((_, _), items) in &groups {
        let kind = items[0].kind;
        let removable: u64 = items
            .iter()
            .filter(|b| b.removable())
            .map(|b| b.size as u64)
            .sum();
        grand_removable += removable;

        let header = if kind.regenerable() {
            format!("{}  —  returns {}", kind.label(), fmt_bytes(removable))
        } else {
            format!("{}  —  NEVER REMOVED", kind.label())
        };
        println!(
            "\n{}\n{}",
            header.to_uppercase(),
            "─".repeat(header.chars().count())
        );

        // System junk is thousands of tiny files: summarise instead of listing.
        if kind == DerivedKind::SystemJunk {
            let files: i64 = items.iter().map(|b| b.file_count).sum();
            let size: i64 = items.iter().map(|b| b.size).sum();
            println!(
                "  {:>16}   {:>10}   .DS_Store, ._*, Thumbs.db, @eaDir, .thumbnails",
                pc_core::count(files, ["файл", "файла", "файлов"], ["file", "files"]),
                fmt_bytes(size as u64)
            );
            continue;
        }

        for b in items {
            let mark = match () {
                _ if b.state == BundleState::Quarantined => "[~]",
                _ if b.state == BundleState::Purged => "[.]",
                _ if !b.regenerable => "[—]",
                _ if b.blocked_code.is_some() => "[ ]",
                _ => "[x]",
            };
            let name = short_path(&b.path);
            let line = format!(
                "  {mark} {:<58} {:>13} {:>10}  {}",
                truncate(&name, 58),
                pc_core::count(b.file_count, ["файл", "файла", "файлов"], ["file", "files"]),
                fmt_bytes(b.size as u64),
                b.rebuild_cost_hint.as_deref().unwrap_or("")
            );
            println!("{}", line.trim_end());
            if let Some(detail) = &b.blocked_detail {
                println!("      └─ {detail}");
            }
            if b.state == BundleState::Quarantined {
                println!("      └─ in quarantine");
            }
        }
    }

    if footer {
        println!(
            "\nИтого к переносу: {}\n\
             (место освободится только после `derived purge`)",
            fmt_bytes(grand_removable)
        );
    }
}

/// Keep the last three components: enough to identify the catalog.
fn short_path(p: &str) -> String {
    let parts = pc_core::path_parts(p);
    if parts.len() <= 3 {
        return p.to_string();
    }
    format!("…/{}", parts[parts.len() - 3..].join("/"))
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let skip = n - keep;
    format!("…{}", s.chars().skip(skip).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("100M").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("1.5G").unwrap(), (1.5 * 1073741824.0) as i64);
        assert!(parse_size("one hundred").is_err());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("7d").unwrap(), 7 * 86400);
        assert_eq!(parse_duration("24h").unwrap(), 86400);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        // A bare number means days.
        assert_eq!(parse_duration("3").unwrap(), 3 * 86400);
    }

    #[test]
    fn shortens_long_paths() {
        assert_eq!(
            short_path("/mnt/disk3/data/media/foto/Lightroom_lib/X/X Previews.lrdata"),
            "…/Lightroom_lib/X/X Previews.lrdata"
        );
        assert_eq!(short_path("/a/b"), "/a/b");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        let s = "Тэфи_13.10.2025 Previews.lrdata";
        assert!(truncate(s, 10).chars().count() <= 10);
    }
}
