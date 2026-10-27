pub fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Smooth start and stop. Input and output are in 0..=1.
pub fn ease_in_out_cubic(t: f64) -> f64 {
    let t = clamp01(t);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_monotonic() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert!((ease_in_out_cubic(1.0) - 1.0).abs() < 1e-12);
        let mut prev = 0.0;
        for i in 0..=100 {
            let v = ease_in_out_cubic(i as f64 / 100.0);
            assert!(v >= prev);
            prev = v;
        }
    }
}
