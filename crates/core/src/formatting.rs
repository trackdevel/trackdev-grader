pub trait FloatFormatInput {
    fn as_optional_float(&self) -> Option<f64>;
}

impl FloatFormatInput for f64 {
    fn as_optional_float(&self) -> Option<f64> {
        Some(*self)
    }
}

impl FloatFormatInput for Option<f64> {
    fn as_optional_float(&self) -> Option<f64> {
        *self
    }
}

impl FloatFormatInput for &Option<f64> {
    fn as_optional_float(&self) -> Option<f64> {
        **self
    }
}

fn fmt_present_float(v: f64, decimals: usize) -> String {
    let factor = 10_f64.powi(decimals as i32);
    let rounded = (v * factor).round() / factor;
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    if rounded.fract().abs() < f64::EPSILON {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.decimals$}")
    }
}

/// Format a float-like value with up to `decimals` decimal places, omitting
/// the decimal part when the rounded value is an integer.
///
/// Accepts `f64`, `Option<f64>`, and `&Option<f64>`. Absent optional values
/// are formatted as `None`.
pub fn fmt_float<T: FloatFormatInput>(v: T, decimals: usize) -> String {
    match v.as_optional_float() {
        Some(x) => fmt_present_float(x, decimals),
        None => "None".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_float;

    #[test]
    fn float_format_omits_zero_decimal_for_integers() {
        assert_eq!(fmt_float(12.0, 1), "12");
        assert_eq!(fmt_float(12.04, 1), "12");
        assert_eq!(fmt_float(12.05, 1), "12.1");
        assert_eq!(fmt_float(12.34, 1), "12.3");
        assert_eq!(fmt_float(12.96, 1), "13");
        assert_eq!(fmt_float(-0.04, 1), "0");
    }

    #[test]
    fn float_format_uses_requested_decimal_places() {
        assert_eq!(fmt_float(12.345, 0), "12");
        assert_eq!(fmt_float(12.345, 1), "12.3");
        assert_eq!(fmt_float(12.345, 2), "12.35");
        assert_eq!(fmt_float(12.3, 2), "12.30");
        assert_eq!(fmt_float(12.0, 2), "12");
    }

    #[test]
    fn float_format_accepts_optional_values() {
        let some = Some(4.567);
        let none = None;
        assert_eq!(fmt_float(some, 2), "4.57");
        assert_eq!(fmt_float(&Some(4.0), 2), "4");
        assert_eq!(fmt_float(&none, 2), "None");
    }
}
