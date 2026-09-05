//! Primitive conversions for the six NumericTag variants in Vanilla 26.3-pre-2.
//!
//! Integer narrowing retains low bits. Floating byte/short/int conversions floor
//! before a saturating i32 cast; Float-to-long truncates, Double-to-long floors.
//! The pinned Mth.floor uses Math.floor followed by a cast, not older wrapping
//! decrement implementations. These functions inspect existing tag values and
//! do not perform valueOf's separate signed-zero canonicalization.

use super::Tag;

impl Tag {
    /// NumericTag.byteValue; nonnumeric tags return None.
    pub fn as_byte(&self) -> Option<i8> {
        self.as_int().map(|value| value as i8)
    }

    /// NumericTag.shortValue; nonnumeric tags return None.
    pub fn as_short(&self) -> Option<i16> {
        self.as_int().map(|value| value as i16)
    }

    /// NumericTag.intValue, including floor and Java saturating casts for floats.
    pub fn as_int(&self) -> Option<i32> {
        Some(match self {
            Self::Byte(value) => i32::from(*value),
            Self::Short(value) => i32::from(*value),
            Self::Int(value) => *value,
            Self::Long(value) => *value as i32,
            Self::Float(value) => value.floor() as i32,
            Self::Double(value) => value.floor() as i32,
            _ => return None,
        })
    }

    /// NumericTag.longValue: Float truncates; Double floors before saturation.
    pub fn as_long(&self) -> Option<i64> {
        Some(match self {
            Self::Byte(value) => i64::from(*value),
            Self::Short(value) => i64::from(*value),
            Self::Int(value) => i64::from(*value),
            Self::Long(value) => *value,
            Self::Float(value) => *value as i64,
            Self::Double(value) => value.floor() as i64,
            _ => return None,
        })
    }

    /// NumericTag.floatValue; conversion does not promise a particular NaN payload.
    pub fn as_float(&self) -> Option<f32> {
        Some(match self {
            Self::Byte(value) => f32::from(*value),
            Self::Short(value) => f32::from(*value),
            Self::Int(value) => *value as f32,
            Self::Long(value) => *value as f32,
            Self::Float(value) => *value,
            Self::Double(value) => *value as f32,
            _ => return None,
        })
    }

    /// NumericTag.doubleValue; conversion does not promise a particular NaN payload.
    pub fn as_double(&self) -> Option<f64> {
        Some(match self {
            Self::Byte(value) => f64::from(*value),
            Self::Short(value) => f64::from(*value),
            Self::Int(value) => f64::from(*value),
            Self::Long(value) => *value as f64,
            Self::Float(value) => f64::from(*value),
            Self::Double(value) => *value,
            _ => return None,
        })
    }
}
