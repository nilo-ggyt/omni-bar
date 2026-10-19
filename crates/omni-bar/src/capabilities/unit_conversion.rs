//! Finite conversions for the supported length, mass and temperature dimensions.

#![forbid(unsafe_code)]

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A dimension constrains which scale/offset conversions can be combined.
enum MeasurementDimension {
    Length,
    Mass,
    Temperature,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Static presentation metadata for a supported measurement unit.
pub(crate) struct UnitDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub symbol: &'static str,
    pub dimension: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
/// One finite conversion and the canonical copyable display result.
pub(crate) struct UnitConversionResult {
    pub value: f64,
    pub display_value: String,
    pub from_unit: &'static str,
    pub to_unit: &'static str,
}

impl UnitConversionResult {
    /// Present the target symbol while leaving the canonical numeric clipboard value unchanged.
    pub(crate) fn display_with_unit(&self) -> Result<String, UnitConversionError> {
        // Resolve the stable target identity through the same catalog that produced the result.
        let target = find_unit(self.to_unit)?;
        Ok(format!(
            "{} {}",
            self.display_value, target.definition.symbol
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Expected validation failures independent of any native control.
pub(crate) enum UnitConversionError {
    #[error("不支持的单位：{unit}")]
    UnknownUnit { unit: String },
    #[error("不能在不同类别的单位之间换算")]
    DimensionMismatch,
    #[error("换算值必须是有效数字")]
    NonFiniteValue,
    #[error("温度不能低于绝对零度")]
    BelowAbsoluteZero,
}

#[derive(Clone, Copy)]
/// A unit maps to its dimension's base using one affine transformation.
struct Unit {
    definition: UnitDefinition,
    dimension: MeasurementDimension,
    scale_to_base: f64,
    offset_to_base: f64,
}

const UNITS: &[Unit] = &[
    length("millimeter", "毫米", "mm", 0.001),
    length("centimeter", "厘米", "cm", 0.01),
    length("meter", "米", "m", 1.0),
    length("kilometer", "千米", "km", 1_000.0),
    length("inch", "英寸", "in", 0.0254),
    length("foot", "英尺", "ft", 0.3048),
    length("mile", "英里", "mi", 1_609.344),
    mass("gram", "克", "g", 0.001),
    mass("kilogram", "千克", "kg", 1.0),
    mass("tonne", "吨", "t", 1_000.0),
    mass("ounce", "盎司", "oz", 0.028_349_523_125),
    mass("pound", "磅", "lb", 0.453_592_37),
    temperature("celsius", "摄氏度", "°C", 1.0, 273.15),
    temperature(
        "fahrenheit",
        "华氏度",
        "°F",
        5.0 / 9.0,
        255.372_222_222_222_2,
    ),
    temperature("kelvin", "开尔文", "K", 1.0, 0.0),
];

/// Create a static length unit normalized to meters.
const fn length(id: &'static str, label: &'static str, symbol: &'static str, scale: f64) -> Unit {
    // Lengths are proportional to meters, so the static scale needs no additive offset.
    Unit {
        definition: UnitDefinition {
            id,
            label,
            symbol,
            dimension: "length",
        },
        dimension: MeasurementDimension::Length,
        scale_to_base: scale,
        offset_to_base: 0.0,
    }
}

/// Create a static mass unit normalized to kilograms.
const fn mass(id: &'static str, label: &'static str, symbol: &'static str, scale: f64) -> Unit {
    // Masses are proportional to kilograms; their labels borrow static storage and require no registry.
    Unit {
        definition: UnitDefinition {
            id,
            label,
            symbol,
            dimension: "mass",
        },
        dimension: MeasurementDimension::Mass,
        scale_to_base: scale,
        offset_to_base: 0.0,
    }
}

/// Create a static temperature unit normalized to kelvin.
const fn temperature(
    id: &'static str,
    label: &'static str,
    symbol: &'static str,
    scale: f64,
    offset: f64,
) -> Unit {
    // Temperature needs both scale and offset to reach kelvin; convert enforces the absolute-zero bound.
    Unit {
        definition: UnitDefinition {
            id,
            label,
            symbol,
            dimension: "temperature",
        },
        dimension: MeasurementDimension::Temperature,
        scale_to_base: scale,
        offset_to_base: offset,
    }
}

/// Return the fixed finite unit catalog for native picker construction.
pub(crate) fn definitions() -> Vec<UnitDefinition> {
    // Copy the finite static catalog into a short-lived UI construction snapshot.
    UNITS
        .iter()
        .map(|unit| {
            // Presentation receives no mutable access to the canonical scale/offset table.
            unit.definition
        })
        .collect()
}

/// Validate a dimension-compatible affine conversion and normalize its displayed value.
pub(crate) fn convert(
    value: f64,
    from_unit: &str,
    to_unit: &str,
) -> Result<UnitConversionResult, UnitConversionError> {
    // Reject infinities and NaN before catalog lookup or arithmetic.
    if !value.is_finite() {
        // Invalid user numbers never become a successful conversion.
        return Err(UnitConversionError::NonFiniteValue);
    }

    // Resolve both units once, then enforce dimension compatibility before using their scales.
    let from = find_unit(from_unit)?;
    let to = find_unit(to_unit)?;
    if from.dimension != to.dimension {
        // A shared numeric representation does not make unrelated dimensions convertible.
        return Err(UnitConversionError::DimensionMismatch);
    }

    // Perform the affine transform through the dimension's common base.
    let base_value = value.mul_add(from.scale_to_base, from.offset_to_base);
    if from.dimension == MeasurementDimension::Temperature && base_value < 0.0 {
        // Negative kelvin would violate the supported temperature model.
        return Err(UnitConversionError::BelowAbsoluteZero);
    }

    let converted = (base_value - to.offset_to_base) / to.scale_to_base;
    if !converted.is_finite() {
        // Extreme finite inputs may still overflow an output unit's scale.
        return Err(UnitConversionError::NonFiniteValue);
    }

    // Avoid displaying a negative sign for either zero encoding.
    let converted = if converted == 0.0 {
        // All exact zero results share the ordinary product representation.
        0.0
    } else {
        // Preserve the unrounded finite result for downstream operations.
        converted
    };
    Ok(UnitConversionResult {
        value: converted,
        display_value: format_value(converted),
        from_unit: from.definition.id,
        to_unit: to.definition.id,
    })
}

/// Resolve a supported stable identifier and bound unknown user input in errors.
fn find_unit(id: &str) -> Result<&'static Unit, UnitConversionError> {
    UNITS
        .iter()
        .find(|unit| {
            // Unit identity is an exact stable catalog key, independent of localized labels.
            unit.definition.id == id
        })
        .ok_or_else(|| {
            // Keep the rejected identifier bounded while preserving a useful input error.
            UnitConversionError::UnknownUnit {
                unit: id.chars().take(32).collect(),
            }
        })
}

/// Use compact decimal output and scientific notation outside the useful decimal range.
fn format_value(value: f64) -> String {
    if value.abs() >= 1_000_000_000_000.0 || (value != 0.0 && value.abs() < 0.000_000_001) {
        // Scientific output avoids very long values or an incorrectly rounded visible zero.
        format!("{value:.10e}")
    } else {
        // Ordinary magnitudes fit a compact decimal with insignificant trailing zeros removed.
        let value = format!("{value:.10}");
        value.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{UnitConversionError, convert, definitions};

    #[test]
    /// Preserve exact scale factors for the common length and mass paths.
    fn converts_length_and_mass_units() {
        // Independently known decimal scales expose either an incorrect base or rounding drift.
        assert_eq!(convert(1.0, "kilometer", "meter").unwrap().value, 1_000.0);
        assert!((convert(1.0, "pound", "gram").unwrap().value - 453.592_37).abs() < 1e-9);
    }

    #[test]
    /// Keep visible target-unit context separate from the numeric value copied by the native action.
    fn presents_target_symbol_without_changing_numeric_copy_value() {
        // Length and temperature symbols must come from the actual selected target catalog entry.
        let length = convert(1.0, "meter", "kilometer").unwrap();
        assert_eq!(length.display_with_unit().unwrap(), "0.001 km");
        assert_eq!(length.display_value, "0.001");
        let temperature = convert(0.0, "celsius", "fahrenheit").unwrap();
        assert_eq!(temperature.display_with_unit().unwrap(), "32 °F");
        assert_eq!(temperature.display_value, "32");
    }

    #[test]
    /// Preserve offset conversion while rejecting physically invalid temperatures.
    fn converts_temperature_with_absolute_zero_validation() {
        // Exercise both an affine offset and the physical lower boundary.
        assert!((convert(32.0, "fahrenheit", "celsius").unwrap().value).abs() < 1e-10);
        assert_eq!(
            convert(-274.0, "celsius", "kelvin").unwrap_err(),
            UnitConversionError::BelowAbsoluteZero
        );
    }

    #[test]
    /// Keep incompatible dimensions out of the numeric conversion path.
    fn rejects_cross_dimension_conversion() {
        // A valid number and two known units still require a matching dimension.
        assert_eq!(
            convert(1.0, "meter", "kilogram").unwrap_err(),
            UnitConversionError::DimensionMismatch
        );
    }

    #[test]
    /// Non-finite inputs and overflow cannot reach native output or clipboard copy.
    fn rejects_non_finite_inputs_and_scale_overflow() {
        // Both IEEE non-finite classes must fail before any scale/offset lookup or arithmetic.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                convert(value, "meter", "meter"),
                Err(UnitConversionError::NonFiniteValue)
            );
        }
        // A finite value can overflow only after conversion to the smaller target scale.
        assert_eq!(
            convert(f64::MAX, "kilometer", "millimeter"),
            Err(UnitConversionError::NonFiniteValue)
        );
        assert_eq!(convert(-0.0, "meter", "meter").unwrap().display_value, "0");
    }

    #[test]
    /// Ensure the UI catalog cannot introduce an unimplemented measurement dimension.
    fn exposes_only_the_three_mvp_dimensions() {
        // Normalize catalog dimensions to verify the product exposes only supported families.
        let mut dimensions = definitions()
            .into_iter()
            .map(|definition| {
                // Only the categorical identity matters to this catalog contract assertion.
                definition.dimension
            })
            .collect::<Vec<_>>();
        dimensions.sort_unstable();
        dimensions.dedup();
        assert_eq!(dimensions, ["length", "mass", "temperature"]);
    }
}
