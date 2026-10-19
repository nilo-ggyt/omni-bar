//! Bounded arithmetic parsing for the native calculator, with no platform dependency.

#![forbid(unsafe_code)]

use thiserror::Error;

const MAX_EXPRESSION_BYTES: usize = 256;
const MAX_TOKENS: usize = 128;
const MAX_NESTING_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
/// A finite numeric value and its canonical copyable representation.
pub(crate) struct CalculationResult {
    pub value: f64,
    pub display_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Deterministic input/complexity/numeric failures suitable for inline presentation.
pub(crate) enum CalculationError {
    #[error("表达式不能为空")]
    Empty,
    #[error("表达式过长")]
    TooLong,
    #[error("表达式结构过于复杂")]
    TooComplex,
    #[error("表达式在第 {position} 个字符附近无效")]
    InvalidSyntax { position: usize },
    #[error("不能除以零")]
    DivisionByZero,
    #[error("计算结果超出有效数值范围")]
    NonFiniteResult,
}

/// Validate one bounded expression and evaluate it without filesystem or application state.
pub(crate) fn evaluate(expression: &str) -> Result<CalculationResult, CalculationError> {
    // Whitespace-only input is an empty query; limits apply to the trimmed grammar.
    let expression = expression.trim();
    if expression.is_empty() {
        // An empty field has no arithmetic value and must not invoke the parser.
        return Err(CalculationError::Empty);
    }
    if expression.len() > MAX_EXPRESSION_BYTES {
        // Reject oversized user input before any recursive parsing work.
        return Err(CalculationError::TooLong);
    }

    // Evaluate exactly one expression and reject any unsupported trailing token.
    let mut parser = ExpressionParser::new(expression);
    let value = parser.parse_expression()?;
    parser.skip_ascii_whitespace();
    if !parser.is_at_end() {
        // A valid prefix cannot hide unparsed operators, identifiers or Unicode syntax.
        return Err(parser.invalid_syntax());
    }
    if !value.is_finite() {
        // The final boundary independently excludes non-finite clipboard output.
        return Err(CalculationError::NonFiniteResult);
    }

    // Normalize signed zero so display and copied values use the same representation.
    let value = if value == 0.0 {
        // Both floating-point zero encodings represent the same product result.
        0.0
    } else {
        // Preserve every other finite value without rounding the calculation.
        value
    };
    Ok(CalculationResult {
        value,
        display_value: value.to_string(),
    })
}

/// Cursor, token budget and recursion budget for one borrowed expression.
struct ExpressionParser<'a> {
    expression: &'a str,
    bytes: &'a [u8],
    position: usize,
    token_count: usize,
    nesting_depth: usize,
}

impl<'a> ExpressionParser<'a> {
    /// Borrow the input and initialize one independent parsing budget.
    fn new(expression: &'a str) -> Self {
        // The parser borrows one immutable input and starts with no consumed budget.
        Self {
            expression,
            bytes: expression.as_bytes(),
            position: 0,
            token_count: 0,
            nesting_depth: 0,
        }
    }

    /// Parse left-associative addition and subtraction after higher-precedence terms.
    fn parse_expression(&mut self) -> Result<f64, CalculationError> {
        let mut value = self.parse_term()?;

        // Each successful iteration consumes an operator; the first other token ends this level.
        loop {
            self.skip_ascii_whitespace();
            if self.consume_operator(b'+')? {
                // Addition applies after the complete higher-precedence right operand.
                value = finite(value + self.parse_term()?)?;
            } else if self.consume_operator(b'-')? {
                // Subtraction has the same precedence and left associativity as addition.
                value = finite(value - self.parse_term()?)?;
            } else {
                // The next token belongs to the caller or marks the end of this precedence level.
                return Ok(value);
            }
        }
    }

    /// Parse multiplication, division and remainder with explicit zero-divisor rejection.
    fn parse_term(&mut self) -> Result<f64, CalculationError> {
        let mut value = self.parse_unary()?;

        // Each successful iteration consumes an operator; the first other token ends this level.
        loop {
            self.skip_ascii_whitespace();
            if self.consume_operator(b'*')? {
                // Multiplication combines unary/power operands before additive operations.
                value = finite(value * self.parse_unary()?)?;
            } else if self.consume_operator(b'/')? {
                // Division validates the complete divisor before computing a finite quotient.
                let divisor = self.parse_unary()?;
                if divisor == 0.0 {
                    // Both positive and negative zero would make the operation undefined.
                    return Err(CalculationError::DivisionByZero);
                }
                value = finite(value / divisor)?;
            } else if self.consume_operator(b'%')? {
                // Remainder shares division precedence and rejects a zero divisor.
                let divisor = self.parse_unary()?;
                if divisor == 0.0 {
                    // Both positive and negative zero would make the operation undefined.
                    return Err(CalculationError::DivisionByZero);
                }
                value = finite(value % divisor)?;
            } else {
                // The next token belongs to the caller or marks the end of this precedence level.
                return Ok(value);
            }
        }
    }

    /// Parse unary signs while sharing the same recursion budget as nested powers.
    fn parse_unary(&mut self) -> Result<f64, CalculationError> {
        self.skip_ascii_whitespace();
        if self.consume_operator(b'+')? {
            // Unary plus consumes a bounded recursion step without changing the value.
            return self.with_nested_expression(Self::parse_unary);
        }
        if self.consume_operator(b'-')? {
            // Unary minus applies after power parsing, so -2^2 remains -(2^2).
            let value = self.with_nested_expression(Self::parse_unary)?;
            return finite(-value);
        }
        self.parse_power()
    }

    /// Parse right-associative exponentiation without changing unary-minus precedence.
    fn parse_power(&mut self) -> Result<f64, CalculationError> {
        let base = self.parse_primary()?;
        self.skip_ascii_whitespace();
        if !self.consume_operator(b'^')? {
            // A primary without an exponent already forms a complete power operand.
            return Ok(base);
        }

        // Parsing the exponent recursively preserves both negative and right-associated powers.
        let exponent = self.with_nested_expression(Self::parse_unary)?;
        finite(base.powf(exponent))
    }

    /// Parse a parenthesized expression or a finite numeric literal.
    fn parse_primary(&mut self) -> Result<f64, CalculationError> {
        self.skip_ascii_whitespace();
        if self.peek() == Some(b'(') {
            // Parentheses create one bounded nested expression and require a matching close.
            self.consume_token()?;
            self.position += 1;
            let value = self.with_nested_expression(Self::parse_expression)?;
            self.skip_ascii_whitespace();
            if self.peek() != Some(b')') {
                // Incomplete grouping is a syntax error at the actual missing-close position.
                return Err(self.invalid_syntax());
            }
            self.consume_token()?;
            self.position += 1;
            return Ok(value);
        }

        self.parse_number()
    }

    /// Parse bounded decimal/scientific syntax and reject partial exponents.
    fn parse_number(&mut self) -> Result<f64, CalculationError> {
        self.skip_ascii_whitespace();
        let start = self.position;
        let mut digit_count = 0;

        // Decimal digits are ASCII, so every cursor position remains a UTF-8 boundary.
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
            digit_count += 1;
        }

        if self.peek() == Some(b'.') {
            // A decimal point may have digits on either side, but not zero digits overall.
            self.position += 1;
            // Decimal digits are ASCII, so every cursor position remains a UTF-8 boundary.
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
                digit_count += 1;
            }
        }

        if digit_count == 0 {
            // A bare dot, unsupported token or absent operand is not a numeric literal.
            return Err(self.invalid_syntax());
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            // Scientific notation requires exponent digits after its optional sign.
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                // Only the exponent sign is consumed here; unary expression signs belong outside.
                self.position += 1;
            }

            let exponent_start = self.position;
            // Decimal digits are ASCII, so every cursor position remains a UTF-8 boundary.
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == exponent_start {
                // Do not accept a numeric prefix when its exponent is incomplete.
                return Err(self.invalid_syntax());
            }
        }

        // Charge one numeric token and parse only the already-validated ASCII slice.
        self.consume_token()?;
        let value = self.expression[start..self.position]
            .parse::<f64>()
            .map_err(|_| self.invalid_syntax())?;
        finite(value)
    }

    /// Apply one recursive parser step and restore the nesting counter on every outcome.
    fn with_nested_expression(
        &mut self,
        parse: fn(&mut Self) -> Result<f64, CalculationError>,
    ) -> Result<f64, CalculationError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            // Bound stack growth for parentheses, unary chains and power chains alike.
            return Err(CalculationError::TooComplex);
        }

        // Always restore depth even when the nested parse returns an expected input error.
        self.nesting_depth += 1;
        let result = parse(self);
        self.nesting_depth -= 1;
        result
    }

    /// Consume a matching operator only after charging the shared token budget.
    fn consume_operator(&mut self, operator: u8) -> Result<bool, CalculationError> {
        if self.peek() != Some(operator) {
            // A different token remains untouched for the caller's grammar level.
            return Ok(false);
        }

        // Only an actual grammar operator charges the budget and advances the input cursor.
        self.consume_token()?;
        self.position += 1;
        Ok(true)
    }

    /// Bound total expression complexity independently from nesting depth.
    fn consume_token(&mut self) -> Result<(), CalculationError> {
        // Count before accepting a token so the first over-budget item is never evaluated.
        self.token_count += 1;
        if self.token_count > MAX_TOKENS {
            // Excessive flat expressions are rejected even without deep nesting.
            return Err(CalculationError::TooComplex);
        }
        Ok(())
    }

    /// Advance only over grammar-supported whitespace without splitting UTF-8 sequences.
    fn skip_ascii_whitespace(&mut self) {
        // Whitespace advancement consumes bytes and therefore always makes bounded progress.
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }

    /// Read the next byte without advancing or indexing beyond the input.
    fn peek(&self) -> Option<u8> {
        // Missing bytes represent end-of-input without an unchecked indexing operation.
        self.bytes.get(self.position).copied()
    }

    /// Report whether all input was consumed, including trailing whitespace.
    fn is_at_end(&self) -> bool {
        // Grammar paths advance only through existing ASCII bytes, so equality is sufficient.
        self.position == self.bytes.len()
    }

    /// Convert the valid byte cursor to a user-visible one-based character position.
    fn invalid_syntax(&self) -> CalculationError {
        // Accepted cursor movement preserves a UTF-8 boundary for character-position reporting.
        CalculationError::InvalidSyntax {
            position: self.expression[..self.position].chars().count() + 1,
        }
    }
}

/// Reject overflow and invalid floating-point results at each arithmetic step.
fn finite(value: f64) -> Result<f64, CalculationError> {
    // Every arithmetic stage shares the same finite-result rejection policy.
    value
        .is_finite()
        .then_some(value)
        .ok_or(CalculationError::NonFiniteResult)
}

#[cfg(test)]
mod tests {
    use super::{CalculationError, evaluate};

    #[test]
    /// Keep operator precedence, grouping and numeric syntax observable in domain tests.
    fn evaluates_precedence_parentheses_and_scientific_notation() {
        // Known independent results distinguish grouping from ordinary operator precedence.
        assert_eq!(evaluate("2 + 3 * 4").unwrap().value, 14.0);
        assert_eq!(evaluate("(2 + 3) * 4").unwrap().value, 20.0);
        assert_eq!(evaluate("1.5e2 / 3").unwrap().value, 50.0);
    }

    #[test]
    /// Preserve the product grammar for powers and unary signs.
    fn evaluates_right_associative_powers_and_unary_signs() {
        // These expressions differentiate right association and the power/unary boundary.
        assert_eq!(evaluate("2 ^ 3 ^ 2").unwrap().value, 512.0);
        assert_eq!(evaluate("-2 ^ 2").unwrap().value, -4.0);
        assert_eq!(evaluate("2 ^ -2").unwrap().value, 0.25);
    }

    #[test]
    /// Ensure invalid arithmetic cannot become a copied infinite or NaN result.
    fn rejects_division_by_zero_and_non_finite_results() {
        // Neither undefined division nor an invalid real-valued power can become copyable output.
        assert_eq!(
            evaluate("3 / 0").unwrap_err(),
            CalculationError::DivisionByZero
        );
        assert_eq!(
            evaluate("(-1) ^ 0.5").unwrap_err(),
            CalculationError::NonFiniteResult
        );
    }

    #[test]
    /// Reject partial syntax, unsupported Unicode and flat complexity without panicking.
    fn rejects_incomplete_tokens_and_flat_token_exhaustion() {
        // A valid prefix must not hide a missing exponent operand or unsupported trailing syntax.
        for expression in ["1e", "1e+", ".", "1 +", "1中文", "（1）", "1 2"] {
            assert!(matches!(
                evaluate(expression),
                Err(CalculationError::InvalidSyntax { .. })
            ));
        }
        // Sixty-five terms fit the byte limit but exceed the independent 128-token budget.
        let flat = std::iter::repeat_n("1", 65).collect::<Vec<_>>().join("+");
        assert_eq!(evaluate(&flat), Err(CalculationError::TooComplex));
        assert_eq!(evaluate("-0").unwrap().display_value, "0");
    }

    #[test]
    /// Verify hostile long or deeply nested expressions stop at their configured budgets.
    fn enforces_expression_and_nesting_limits() {
        // Independently exceed byte and recursion budgets without relying on timing assumptions.
        assert_eq!(
            evaluate(&"1".repeat(257)).unwrap_err(),
            CalculationError::TooLong
        );

        let nested = format!("{}1{}", "(".repeat(33), ")".repeat(33));
        assert_eq!(evaluate(&nested).unwrap_err(), CalculationError::TooComplex);
    }
}
