// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Exact integer amounts (`INV-11`).

use core::fmt;
use core::str::FromStr;

/// An exact, non-negative integer count of an asset's minor units (`INV-11`).
///
/// Arithmetic is checked: an overflow or an underflow is an [`AmountError`], never a wrap or a
/// saturation. There is no conversion from or to a floating-point number, and no arithmetic
/// operator is implemented, so every step that could fail says so in its type.
///
/// The textual form is the decimal integer, digits only: it is what the store's `amount` domain
/// (`NUMERIC`, integer-valued, at most `u128::MAX`) accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(u128);

/// Why an amount operation or parse failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountError {
    /// The result would exceed `u128::MAX` minor units.
    Overflow,
    /// The result would be below zero.
    Underflow,
    /// The text is not a decimal integer made of digits only (no sign, point, exponent or space).
    NotAnInteger,
}

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AmountError::Overflow => "amount overflow",
            AmountError::Underflow => "amount underflow",
            AmountError::NotAnInteger => "amount is not a decimal integer of minor units",
        })
    }
}

impl std::error::Error for AmountError {}

impl Amount {
    /// Zero minor units.
    pub const ZERO: Amount = Amount(0);
    /// The largest amount representable, `u128::MAX` minor units.
    pub const MAX: Amount = Amount(u128::MAX);

    /// An amount of `minor_units`.
    pub const fn from_minor_units(minor_units: u128) -> Self {
        Amount(minor_units)
    }

    /// The count of minor units.
    pub const fn minor_units(self) -> u128 {
        self.0
    }

    /// `self + rhs`, or [`AmountError::Overflow`].
    pub const fn checked_add(self, rhs: Amount) -> Result<Amount, AmountError> {
        match self.0.checked_add(rhs.0) {
            Some(v) => Ok(Amount(v)),
            None => Err(AmountError::Overflow),
        }
    }

    /// `self - rhs`, or [`AmountError::Underflow`].
    pub const fn checked_sub(self, rhs: Amount) -> Result<Amount, AmountError> {
        match self.0.checked_sub(rhs.0) {
            Some(v) => Ok(Amount(v)),
            None => Err(AmountError::Underflow),
        }
    }

    /// Whether this is zero minor units.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl From<u128> for Amount {
    fn from(minor_units: u128) -> Self {
        Amount(minor_units)
    }
}

impl From<u64> for Amount {
    fn from(minor_units: u64) -> Self {
        Amount(u128::from(minor_units))
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Amount {
    type Err = AmountError;

    /// Parses a decimal integer of minor units. Only ASCII digits are accepted: `"1.0"`, `"1e3"`,
    /// `"+1"`, `"-1"` and `" 1"` are all [`AmountError::NotAnInteger`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AmountError::NotAnInteger);
        }
        s.parse::<u128>()
            .map(Amount)
            .map_err(|_| AmountError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inv_11_overflow_is_an_error_not_a_wrap() {
        assert_eq!(
            Amount::MAX.checked_add(Amount::from(1u64)),
            Err(AmountError::Overflow)
        );
        assert_eq!(
            Amount::MAX.checked_add(Amount::MAX),
            Err(AmountError::Overflow)
        );
        assert_eq!(Amount::MAX.checked_add(Amount::ZERO), Ok(Amount::MAX));
    }

    #[test]
    fn inv_11_underflow_is_an_error_not_a_wrap() {
        assert_eq!(
            Amount::ZERO.checked_sub(Amount::from(1u64)),
            Err(AmountError::Underflow)
        );
        assert_eq!(
            Amount::from(5u64).checked_sub(Amount::from(5u64)),
            Ok(Amount::ZERO)
        );
    }

    #[test]
    fn inv_11_checked_arithmetic_is_exact() {
        let a = Amount::from_minor_units(u128::MAX - 1);
        assert_eq!(a.checked_add(Amount::from(1u64)), Ok(Amount::MAX));
        assert_eq!(Amount::MAX.checked_sub(a), Ok(Amount::from_minor_units(1)));
    }

    #[test]
    fn inv_11_text_form_is_a_decimal_integer_only() {
        assert_eq!("0".parse::<Amount>(), Ok(Amount::ZERO));
        assert_eq!(u128::MAX.to_string().parse::<Amount>(), Ok(Amount::MAX));
        assert_eq!(Amount::MAX.to_string(), u128::MAX.to_string());
        for bad in [
            "", "1.5", "1.0", "1e3", "-1", "+1", " 1", "1 ", "0x10", "NaN", "inf",
        ] {
            assert_eq!(
                bad.parse::<Amount>(),
                Err(AmountError::NotAnInteger),
                "{bad:?}"
            );
        }
        // One past u128::MAX.
        assert_eq!(
            "340282366920938463463374607431768211456".parse::<Amount>(),
            Err(AmountError::Overflow)
        );
    }
}
