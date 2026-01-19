//! Precision-safe decimal types for trading.
//!
//! Uses `rust_decimal` for exact decimal arithmetic, avoiding
//! floating-point rounding errors critical in financial calculations.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, Div, Mul, Sub};
use std::str::FromStr;

/// Price with exact decimal precision.
///
/// Wraps `Decimal` to provide type safety and prevent mixing
/// prices with sizes in calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Price(pub Decimal);

impl Price {
    pub const ZERO: Self = Self(Decimal::ZERO);
    pub const ONE: Self = Self(Decimal::ONE);

    #[inline]
    pub fn new(value: Decimal) -> Self {
        Self(value)
    }

    #[inline]
    pub fn inner(&self) -> Decimal {
        self.0
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    #[inline]
    pub fn is_positive(&self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }

    /// Round to tick size (floor for buys, ceil for sells typically).
    #[inline]
    pub fn round_to_tick(&self, tick_size: Price) -> Self {
        if tick_size.is_zero() {
            return *self;
        }
        Self((self.0 / tick_size.0).floor() * tick_size.0)
    }

    /// Calculate basis points difference from another price.
    #[inline]
    pub fn bps_from(&self, other: Price) -> Option<Decimal> {
        if other.is_zero() {
            return None;
        }
        Some((self.0 - other.0) / other.0 * Decimal::from(10000))
    }

    /// Calculate percentage difference from another price.
    #[inline]
    pub fn pct_from(&self, other: Price) -> Option<Decimal> {
        if other.is_zero() {
            return None;
        }
        Some((self.0 - other.0) / other.0 * Decimal::from(100))
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Price {
    type Err = rust_decimal::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl From<Decimal> for Price {
    fn from(d: Decimal) -> Self {
        Self(d)
    }
}

impl Add for Price {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Price {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Mul<Decimal> for Price {
    type Output = Self;

    fn mul(self, rhs: Decimal) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl Div<Decimal> for Price {
    type Output = Self;

    fn div(self, rhs: Decimal) -> Self::Output {
        Self(self.0 / rhs)
    }
}

/// Size/quantity with exact decimal precision.
///
/// Wraps `Decimal` to provide type safety and prevent mixing
/// sizes with prices in calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Size(pub Decimal);

impl Size {
    pub const ZERO: Self = Self(Decimal::ZERO);
    pub const ONE: Self = Self(Decimal::ONE);

    #[inline]
    pub fn new(value: Decimal) -> Self {
        Self(value)
    }

    #[inline]
    pub fn inner(&self) -> Decimal {
        self.0
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    #[inline]
    pub fn is_positive(&self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }

    /// Round down to lot size.
    #[inline]
    pub fn round_to_lot(&self, lot_size: Size) -> Self {
        if lot_size.is_zero() {
            return *self;
        }
        Self((self.0 / lot_size.0).floor() * lot_size.0)
    }

    /// Calculate notional value: size * price.
    #[inline]
    pub fn notional(&self, price: Price) -> Decimal {
        self.0 * price.0
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Size {
    type Err = rust_decimal::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl From<Decimal> for Size {
    fn from(d: Decimal) -> Self {
        Self(d)
    }
}

impl Add for Size {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Size {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Mul<Decimal> for Size {
    type Output = Self;

    fn mul(self, rhs: Decimal) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl Div<Decimal> for Size {
    type Output = Self;

    fn div(self, rhs: Decimal) -> Self::Output {
        Self(self.0 / rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_price_bps() {
        let p1 = Price::new(dec!(100));
        let p2 = Price::new(dec!(101));

        let bps = p2.bps_from(p1).unwrap();
        assert_eq!(bps, dec!(100)); // 1% = 100 bps
    }

    #[test]
    fn test_price_round_to_tick() {
        let price = Price::new(dec!(12345.6789));
        let tick = Price::new(dec!(0.01));

        let rounded = price.round_to_tick(tick);
        assert_eq!(rounded.0, dec!(12345.67));
    }

    #[test]
    fn test_size_round_to_lot() {
        let size = Size::new(dec!(1.2345));
        let lot = Size::new(dec!(0.001));

        let rounded = size.round_to_lot(lot);
        assert_eq!(rounded.0, dec!(1.234));
    }

    #[test]
    fn test_notional_calculation() {
        let size = Size::new(dec!(0.5));
        let price = Price::new(dec!(50000));

        let notional = size.notional(price);
        assert_eq!(notional, dec!(25000));
    }
}
