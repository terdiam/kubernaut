//! Kubernetes resource quantity parsing.
//!
//! Quantities are strings with two different suffix families that are easy to
//! confuse: binary (`Ki`, `Mi`, `Gi` — powers of 1024) and decimal (`k`, `M`,
//! `G` — powers of 1000), plus `m` for milli, which is the one people meet as
//! `100m` CPU. Getting `M` and `m` the wrong way round is a factor of a
//! billion, so this is parsed explicitly and tested rather than eyeballed.

/// Parse a quantity into its base unit: cores for CPU, bytes for memory.
/// Returns `None` for anything unparseable rather than guessing.
pub fn parse(value: &str) -> Option<f64> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }

    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let number: f64 = number.parse().ok()?;

    let multiplier = match suffix.trim() {
        "" => 1.0,
        // Milli — the only fractional suffix, and lower-case on purpose.
        "m" => 1e-3,
        "n" => 1e-9,
        "u" => 1e-6,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        // Scientific notation, e.g. `1e3`.
        other if other.starts_with('e') || other.starts_with('E') => {
            let exponent: i32 = other[1..].parse().ok()?;
            10f64.powi(exponent)
        }
        _ => return None,
    };

    Some(number * multiplier)
}

/// Parse from a `Quantity`, treating an absent value as zero.
pub fn parse_or_zero(
    value: Option<&k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
) -> f64 {
    value.and_then(|q| parse(&q.0)).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_numbers_are_cores() {
        assert_eq!(parse("2"), Some(2.0));
        assert_eq!(parse("0.5"), Some(0.5));
    }

    #[test]
    fn milli_is_a_thousandth() {
        assert_eq!(parse("100m"), Some(0.1));
        assert_eq!(parse("1500m"), Some(1.5));
    }

    /// The mistake this module exists to prevent.
    #[test]
    fn upper_m_and_lower_m_differ_by_a_billion() {
        assert_eq!(parse("1M"), Some(1e6));
        assert_eq!(parse("1m"), Some(1e-3));
    }

    #[test]
    fn binary_suffixes_use_1024() {
        assert_eq!(parse("1Ki"), Some(1024.0));
        assert_eq!(parse("1Mi"), Some(1_048_576.0));
        assert_eq!(parse("2Gi"), Some(2.0 * 1024.0 * 1024.0 * 1024.0));
    }

    #[test]
    fn decimal_suffixes_use_1000() {
        assert_eq!(parse("1k"), Some(1000.0));
        assert_eq!(parse("1G"), Some(1e9));
    }

    #[test]
    fn nano_cores_from_metrics_server() {
        // metrics.k8s.io reports CPU in nanocores. Compared with an epsilon
        // because 352e6 * 1e-9 is not exactly representable in binary floating
        // point.
        let cores = parse("352000000n").unwrap();
        assert!((cores - 0.352).abs() < 1e-12, "got {cores}");
    }

    #[test]
    fn scientific_notation_is_accepted() {
        assert_eq!(parse("1e3"), Some(1000.0));
    }

    #[test]
    fn junk_is_rejected_not_guessed() {
        assert_eq!(parse("banana"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("10Zi"), None);
    }
}
