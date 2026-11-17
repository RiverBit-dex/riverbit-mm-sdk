//! Exact decimal strings.
//!
//! Prices and sizes travel as decimal strings end to end. This small type
//! parses such a string without floating point so that book levels can be
//! compared and matched exactly, while the original text stays available.

use std::cmp::Ordering;
use std::fmt;

/// A non-negative decimal number: `mantissa / 10^scale`, with trailing zeros
/// stripped so that `"64530.0"` and `"64530"` compare equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    mantissa: u128,
    scale: u32,
}

impl Decimal {
    /// Parses `digits` or `digits.digits`. Signs, exponents, spaces, a bare
    /// leading or trailing dot and an empty string are rejected.
    pub fn parse(text: &str) -> Option<Decimal> {
        if !is_decimal_string(text) {
            return None;
        }
        let (int_part, frac_part) = match text.split_once('.') {
            Some((i, f)) => (i, f),
            None => (text, ""),
        };
        let mut mantissa: u128 = 0;
        for byte in int_part.bytes().chain(frac_part.bytes()) {
            mantissa = mantissa
                .checked_mul(10)?
                .checked_add(u128::from(byte - b'0'))?;
        }
        let mut scale = u32::try_from(frac_part.len()).ok()?;
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        if mantissa == 0 {
            scale = 0;
        }
        Some(Decimal { mantissa, scale })
    }

    /// Builds a decimal from a fixed-point integer: `value / 10^decimals`.
    pub fn from_fixed(value: u64, decimals: u32) -> Option<Decimal> {
        let mut mantissa = u128::from(value);
        let mut scale = decimals;
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        if mantissa == 0 {
            scale = 0;
        }
        Some(Decimal { mantissa, scale })
    }

    /// True for zero.
    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        // Align scales. If the aligned value does not fit in u128 the number
        // with the larger scaled value is the larger one.
        let (a, b) = match self.scale.cmp(&other.scale) {
            Ordering::Equal => (Some(self.mantissa), Some(other.mantissa)),
            Ordering::Less => (
                pow10(other.scale - self.scale).and_then(|p| self.mantissa.checked_mul(p)),
                Some(other.mantissa),
            ),
            Ordering::Greater => (
                Some(self.mantissa),
                pow10(self.scale - other.scale).and_then(|p| other.mantissa.checked_mul(p)),
            ),
        };
        match (a, b) {
            (Some(a), Some(b)) => a.cmp(&b),
            // The side that overflowed is far larger than the other one.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.mantissa.to_string();
        let scale = self.scale as usize;
        if scale == 0 {
            return f.write_str(&digits);
        }
        if digits.len() <= scale {
            write!(f, "0.{}{}", "0".repeat(scale - digits.len()), digits)
        } else {
            let (int_part, frac) = digits.split_at(digits.len() - scale);
            write!(f, "{int_part}.{frac}")
        }
    }
}

fn pow10(exp: u32) -> Option<u128> {
    10u128.checked_pow(exp)
}

/// True if `text` is `digits` or `digits.digits` (ASCII digits only).
pub fn is_decimal_string(text: &str) -> bool {
    let (int_part, frac_part) = match text.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (text, None),
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    all_digits(int_part) && frac_part.is_none_or(all_digits)
}

/// Renders a fixed-point integer exactly: `value / 10^decimals`, keeping all
/// `decimals` fractional digits (for example `format_fixed(1010, 1) == "101.0"`).
pub fn format_fixed(value: u64, decimals: u32) -> String {
    let digits = value.to_string();
    let d = decimals as usize;
    if d == 0 {
        return digits;
    }
    let padded = if digits.len() <= d {
        format!("{}{}", "0".repeat(d - digits.len() + 1), digits)
    } else {
        digits
    };
    let (int_part, frac) = padded.split_at(padded.len() - d);
    format!("{int_part}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_exactly() {
        let a = Decimal::parse("64530.0").unwrap();
        let b = Decimal::parse("64530").unwrap();
        assert_eq!(a, b);
        assert!(Decimal::parse("0.1").unwrap() < Decimal::parse("0.10001").unwrap());
        assert!(Decimal::parse("100.5").unwrap() > Decimal::parse("99.99").unwrap());
        assert_eq!(
            Decimal::parse("0.00").unwrap(),
            Decimal::parse("0").unwrap()
        );
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "", ".5", "5.", "-1", "+1", "1e3", " 1", "1 ", "1.2.3", "0x10",
        ] {
            assert!(Decimal::parse(bad).is_none(), "{bad}");
            assert!(!is_decimal_string(bad), "{bad}");
        }
    }

    #[test]
    fn formats_fixed_point() {
        assert_eq!(format_fixed(1010, 1), "101.0");
        assert_eq!(format_fixed(5, 3), "0.005");
        assert_eq!(format_fixed(42, 0), "42");
        assert_eq!(format_fixed(0, 2), "0.00");
        assert_eq!(Decimal::from_fixed(1010, 1).unwrap().to_string(), "101");
    }
}
