//! Separable DCT-II, used by the perceptual hash.

/// Cosine table for one dimension, computed once per side length.
fn table(n: usize) -> Vec<f32> {
    let mut t = vec![0.0f32; n * n];
    for (u, row) in t.chunks_mut(n).enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            *cell =
                (((2 * x + 1) as f32) * u as f32 * std::f32::consts::PI / (2.0 * n as f32)).cos();
        }
    }
    t
}

/// Two-dimensional DCT-II of an `n`x`n` block, applied as rows then columns.
pub fn dct_2d(input: &[f32], n: usize) -> Vec<f32> {
    assert_eq!(input.len(), n * n, "блок должен быть квадратным");
    let t = table(n);
    let mut rows = vec![0.0f32; n * n];
    for y in 0..n {
        for u in 0..n {
            let mut sum = 0.0;
            for x in 0..n {
                sum += input[y * n + x] * t[u * n + x];
            }
            rows[y * n + u] = sum;
        }
    }
    let mut out = vec![0.0f32; n * n];
    for x in 0..n {
        for v in 0..n {
            let mut sum = 0.0;
            for y in 0..n {
                sum += rows[y * n + x] * t[v * n + y];
            }
            out[v * n + x] = sum;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_block_has_all_its_energy_in_the_dc_term() {
        let n = 8;
        let flat = vec![100.0f32; n * n];
        let c = dct_2d(&flat, n);
        assert!(c[0].abs() > 1000.0);
        for v in &c[1..] {
            assert!(v.abs() < 0.01, "ненулевой коэффициент {v}");
        }
    }

    #[test]
    fn a_gradient_puts_energy_in_the_low_frequencies() {
        let n = 8;
        let ramp: Vec<f32> = (0..n * n).map(|i| (i % n) as f32 * 30.0).collect();
        let c = dct_2d(&ramp, n);
        assert!(
            c[1].abs() > c[n - 1].abs(),
            "низкие частоты должны доминировать"
        );
    }
}
