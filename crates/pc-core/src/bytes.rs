/// Human-readable binary sizes, matching the units used throughout the design doc.
pub fn fmt_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    let (val, unit) = if n >= K * K * K * K {
        (n / (K * K * K * K), "TiB")
    } else if n >= K * K * K {
        (n / (K * K * K), "GiB")
    } else if n >= K * K {
        (n / (K * K), "MiB")
    } else if n >= K {
        (n / K, "KiB")
    } else {
        (n, "B")
    };
    if unit == "B" {
        format!("{val:.0} B")
    } else {
        format!("{val:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_bytes;

    #[test]
    fn formats_each_unit() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
