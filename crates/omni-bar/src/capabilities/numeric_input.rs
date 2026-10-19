//! Bounded numeric-field grammar and whitespace shared by the migrated built-in surfaces.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Preserve the native control's existing bound before conversion or a second owned input copy.
pub(crate) const MAX_INPUT_UTF16: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
/// Input-bound failures stay distinct from the unit-conversion domain's finite-value errors.
pub(crate) enum NumericInputError {
    #[error("待换算数值过长")]
    TooLong,
    #[error("请输入有效数字")]
    InvalidNumber,
}

/// Recognize the baseline's ECMAScript whitespace and line terminators, including BOM but not NEL.
fn is_whitespace(character: char) -> bool {
    // This explicit set follows the baseline runtime instead of Rust's broader Unicode White_Space set.
    matches!(character,
        '\u{0009}'..='\u{000D}' | ' ' | '\u{00A0}' | '\u{1680}' |
        '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' |
        '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

/// Preserve the frozen unit-conversion baseline's neutral blank-input state.
pub(crate) fn is_blank(input: &str) -> bool {
    // Borrowed trimming creates no second string and never treats NEL as a valid ECMAScript blank.
    input.trim_matches(is_whitespace).is_empty()
}

/// Parse the nonempty finite subset of baseline Number(input), with fixed scratch and no evaluation.
pub(crate) fn parse_finite_number(input: &str) -> Result<f64, NumericInputError> {
    // Stop counting after the first over-budget UTF-16 unit, independently of the native preflight.
    if input.encode_utf16().take(MAX_INPUT_UTF16 + 1).count() > MAX_INPUT_UTF16 {
        // Bound violations are explicit rather than silently truncating a potentially different value.
        return Err(NumericInputError::TooLong);
    }
    let input = input.trim_matches(is_whitespace);
    if input.is_empty() {
        // The baseline's pre-Number blank guard rejects the empty-string-to-zero JavaScript conversion.
        return Err(NumericInputError::InvalidNumber);
    }

    // Only unsigned nondecimal prefixes are legal; a sign is valid exclusively in the decimal grammar.
    let value = match input.as_bytes() {
        // Base two contributes one exact binary digit per input character.
        [b'0', b'b' | b'B', digits @ ..] => parse_radix_integer(digits, 1)?,
        // Base eight contributes three exact binary digits per input character.
        [b'0', b'o' | b'O', digits @ ..] => parse_radix_integer(digits, 3)?,
        // Base sixteen contributes four exact binary digits per input character.
        [b'0', b'x' | b'X', digits @ ..] => parse_radix_integer(digits, 4)?,
        _ => {
            // Validate the whole decimal grammar before delegating correctly rounded conversion to Rust.
            if !is_decimal_literal(input.as_bytes()) {
                // Reject Rust-only spellings, separators, signed radix forms and unsupported trailing tokens.
                return Err(NumericInputError::InvalidNumber);
            }
            input
                .parse::<f64>()
                .map_err(|_| NumericInputError::InvalidNumber)?
        }
    };
    if !value.is_finite() {
        // Infinity/NaN are never accepted by the baseline's subsequent Number.isFinite boundary.
        return Err(NumericInputError::InvalidNumber);
    }
    Ok(value)
}

/// Validate signed decimal/fraction/exponent syntax without accepting a valid prefix alone.
fn is_decimal_literal(bytes: &[u8]) -> bool {
    let mut cursor = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        // A single leading sign belongs to decimal input; a second sign cannot become a digit.
        cursor += 1;
    }
    let mut digits = 0;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        // Each ASCII digit advances the bounded cursor without splitting a UTF-8 sequence.
        cursor += 1;
        digits += 1;
    }
    if bytes.get(cursor) == Some(&b'.') {
        // Either side of the decimal point may be empty, provided at least one digit exists overall.
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            // Fraction digits share the same input-bound progress guarantee as integer digits.
            cursor += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        // A bare sign/point or any unsupported initial character has no numeric value.
        return false;
    }
    if matches!(bytes.get(cursor), Some(b'e' | b'E')) {
        // Exponent signs are permitted only after e/E and require at least one following digit.
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            // Preserve negative and positive exponents without allowing whitespace within the literal.
            cursor += 1;
        }
        let exponent_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            // Each consumed exponent digit makes progress within the same fixed input budget.
            cursor += 1;
        }
        if cursor == exponent_start {
            // An incomplete exponent is invalid even though its mantissa alone was valid.
            return false;
        }
    }
    // Trailing whitespace was trimmed once; every remaining byte must belong to the exact grammar.
    cursor == bytes.len()
}

/// Convert a power-of-two radix integer with one nearest-even rounding and constant scalar scratch.
fn parse_radix_integer(digits: &[u8], bits_per_digit: u32) -> Result<f64, NumericInputError> {
    if digits.is_empty() {
        // A prefix without a digit cannot fall back to the decimal zero prefix.
        return Err(NumericInputError::InvalidNumber);
    }
    let mut significand = 0u64;
    let mut bit_count = 0usize;
    let mut guard = false;
    let mut sticky = false;
    for byte in digits {
        // Validate every digit, including low bits beyond binary64 precision; suffix garbage never disappears.
        let digit = match byte {
            // Decimal characters are shared by all supported bases; the radix limit is checked below.
            b'0'..=b'9' => u32::from(byte - b'0'),
            // Hexadecimal accepts both ASCII cases without accepting Unicode lookalikes.
            b'a'..=b'f' => u32::from(byte - b'a') + 10,
            // Uppercase hexadecimal contributes the same exact digit value.
            b'A'..=b'F' => u32::from(byte - b'A') + 10,
            _ => {
                // Separators, suffixes and internal whitespace are not numeric-string digits.
                return Err(NumericInputError::InvalidNumber);
            }
        };
        if digit >= 1 << bits_per_digit {
            // A valid hexadecimal character may still be invalid in binary or octal input.
            return Err(NumericInputError::InvalidNumber);
        }
        for bit in (0..bits_per_digit).rev() {
            // Visit at most four bits per bounded input digit, retaining only precision and rounding metadata.
            let set = digit & (1 << bit) != 0;
            if bit_count == 0 && !set {
                // Leading zero bits do not increase the integer's exponent or consume significand precision.
                continue;
            }
            bit_count += 1;
            if bit_count <= 53 {
                // Binary64 stores exactly its first 53 significant bits, including the implicit leading one.
                significand = (significand << 1) | u64::from(set);
            } else if bit_count == 54 {
                // The first discarded bit distinguishes below-half from half-or-above rounding.
                guard = set;
            } else {
                // Any later one distinguishes a strict-above-half value from an exact halfway tie.
                sticky |= set;
            }
        }
    }
    if bit_count <= 53 {
        // Zero and all exactly representable small integers need no rounding or exponent construction.
        return Ok(significand as f64);
    }

    let mut exponent = bit_count - 1;
    if guard && (sticky || significand & 1 != 0) {
        // Above-half rounds up; an exact halfway tie rounds toward the even retained significand.
        significand += 1;
        if significand == 1 << 53 {
            // Carrying past the leading precision bit raises the exponent while restoring normalized width.
            significand >>= 1;
            exponent += 1;
        }
    }
    if exponent > 1023 {
        // Keep overflow explicit if the configured input bound ever permits more than 1024 significant bits.
        return Err(NumericInputError::InvalidNumber);
    }
    // The integer is normal and positive; encode its exact exponent/fraction after the sole rounding step.
    let fraction = significand & ((1 << 52) - 1);
    Ok(f64::from_bits(((exponent as u64 + 1023) << 52) | fraction))
}

#[cfg(test)]
mod tests {
    use super::{MAX_INPUT_UTF16, NumericInputError, is_blank, parse_finite_number};

    #[test]
    /// Preserve actual Node Number finite-input outcomes without introducing a runtime/CI dependency.
    fn matches_frozen_baseline_number_reference() {
        // Generated from Node v24.21.0 using the retained React blank/finite guards; binary64 bits avoid display rounding.
        for line in include_str!("numeric_input/reference.tsv").lines() {
            // Comment lines retain provenance and do not masquerade as numeric test cases.
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let (input, expected) = line
                .split_once('\t')
                .expect("reference has input and outcome");
            let actual = parse_finite_number(input);
            if expected == "invalid" {
                // Invalid and non-finite baseline values remain typed validation failures.
                assert_eq!(actual, Err(NumericInputError::InvalidNumber), "{input:?}");
            } else {
                // Compare all floating-point bits, including negative zero and halfway radix rounding.
                let bits =
                    u64::from_str_radix(expected, 16).expect("reference contains binary64 bits");
                assert_eq!(actual.unwrap().to_bits(), bits, "{input:?}");
            }
        }
    }

    #[test]
    /// Match the full ECMAScript blank set while excluding broader Rust Unicode whitespace.
    fn preserves_whitespace_and_blank_boundaries() {
        // Every ECMAScript whitespace/line-terminator scalar wraps a valid finite value or forms a neutral blank.
        for character in [
            '\t', '\n', '\u{000B}', '\u{000C}', '\r', ' ', '\u{00A0}', '\u{1680}', '\u{2000}',
            '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
            '\u{2008}', '\u{2009}', '\u{200A}', '\u{2028}', '\u{2029}', '\u{202F}', '\u{205F}',
            '\u{3000}', '\u{FEFF}',
        ] {
            assert!(is_blank(&character.to_string()));
            assert_eq!(
                parse_finite_number(&format!("{character}1{character}")),
                Ok(1.0)
            );
        }
        // NEL, the historical Mongolian separator and zero-width space are not accepted blank characters.
        for character in ['\u{0085}', '\u{180E}', '\u{200B}'] {
            assert!(!is_blank(&character.to_string()));
            assert_eq!(
                parse_finite_number(&format!("{character}1")),
                Err(NumericInputError::InvalidNumber)
            );
        }
        assert!(is_blank(""));
        assert!(!is_blank(" 1 "));
    }

    #[test]
    /// Bound complete input work before trimming without truncating leading zeroes or Unicode into another value.
    fn enforces_utf16_budget_without_changing_valid_boundary_values() {
        // Exactly 256 ASCII units remain legal and use fixed precision/rounding scratch.
        assert_eq!(
            parse_finite_number(&format!("{}1", "0".repeat(MAX_INPUT_UTF16 - 1))),
            Ok(1.0)
        );
        assert!(
            parse_finite_number(&format!("0x{}", "f".repeat(MAX_INPUT_UTF16 - 2)))
                .unwrap()
                .is_finite()
        );
        // Leading whitespace and astral input still count toward the same native UTF-16 bound.
        assert_eq!(
            parse_finite_number(&format!("{}1", " ".repeat(MAX_INPUT_UTF16))),
            Err(NumericInputError::TooLong)
        );
        assert_eq!(
            parse_finite_number(&"😀".repeat(MAX_INPUT_UTF16 / 2 + 1)),
            Err(NumericInputError::TooLong)
        );
    }
}
