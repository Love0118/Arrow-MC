use super::{Diagnostic, DiagnosticArgument, Error, ErrorKind};

impl Diagnostic {
    /// Appends this diagnostic's zero or one translation argument. Returns
    /// whether an argument exists, including a possible empty argument.
    /// Input spans are checked before use; failure restores original output.
    pub fn write_argument(
        &self,
        input: &[u16],
        output: &mut Vec<u16>,
        max_output_units: usize,
    ) -> Result<bool, Error> {
        let start = output.len();
        let mut writer = ArgumentWriter {
            output,
            start,
            limit: max_output_units,
        };
        let result = (|| {
            match self.argument {
                DiagnosticArgument::None => return Ok(false),
                DiagnosticArgument::Literal { first, second } => {
                    writer.units(&[first])?;
                    if let Some(second) = second {
                        writer.units(&[124, second])?;
                    }
                }
                DiagnosticArgument::HexWidth(width) => writer.unsigned(u64::from(width))?,
                DiagnosticArgument::CodePoint(code_point) => {
                    writer.text("U+")?;
                    let mut digits = [0u16; 8];
                    for (index, digit) in digits.iter_mut().enumerate() {
                        *digit = u16::from(
                            b"0123456789ABCDEF"[((code_point >> (28 - index * 4)) & 15) as usize],
                        );
                    }
                    writer.units(&digits)?;
                }
                DiagnosticArgument::Operation {
                    name_start,
                    name_end,
                    arity,
                } => {
                    let name = input
                        .get(name_start..name_end)
                        .ok_or_else(|| writer.error(ErrorKind::InvalidDiagnostic))?;
                    writer.units(name)?;
                    writer.text("/")?;
                    writer.unsigned(arity as u64)?;
                }
                DiagnosticArgument::Number {
                    digits_start,
                    digits_end,
                    radix,
                    width,
                    unsigned,
                    negative,
                } => {
                    let digits = input
                        .get(digits_start..digits_end)
                        .ok_or_else(|| writer.error(ErrorKind::InvalidDiagnostic))?;
                    if !matches!(radix, 2 | 10 | 16)
                        || !matches!(width, 8 | 16 | 32 | 64)
                        || digits.is_empty()
                        || digits.first() == Some(&95)
                        || digits.last() == Some(&95)
                        || digits.iter().any(|&unit| {
                            unit != 95 && digit(unit).is_none_or(|value| value >= radix)
                        })
                    {
                        return Err(writer.error(ErrorKind::InvalidDiagnostic));
                    }
                    writer.number(digits, radix, width, unsigned, negative)?;
                }
            }
            Ok(true)
        })();
        if result.is_err() {
            writer.output.truncate(start);
        }
        result
    }
}

struct ArgumentWriter<'a> {
    output: &'a mut Vec<u16>,
    start: usize,
    limit: usize,
}

impl ArgumentWriter<'_> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            offset_utf16: self.output.len() - self.start,
            kind,
            diagnostic: None,
        }
    }

    fn reserve(&mut self, count: usize) -> Result<(), Error> {
        let length = (self.output.len() - self.start)
            .checked_add(count)
            .ok_or_else(|| self.error(ErrorKind::OutputLimit))?;
        if length > self.limit {
            return Err(self.error(ErrorKind::OutputLimit));
        }
        self.output
            .try_reserve(count)
            .map_err(|_| self.error(ErrorKind::AllocationFailed))
    }

    fn units(&mut self, units: &[u16]) -> Result<(), Error> {
        self.reserve(units.len())?;
        self.output.extend_from_slice(units);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), Error> {
        self.reserve(text.len())?;
        self.output.extend(text.bytes().map(u16::from));
        Ok(())
    }

    fn unsigned(&mut self, mut value: u64) -> Result<(), Error> {
        let mut digits = [0u16; 20];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = 48 + (value % 10) as u16;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.units(&digits[start..])
    }

    fn digits(&mut self, digits: &[u16], negative: bool, count: usize) -> Result<(), Error> {
        if negative {
            self.text("-")?;
        }
        for &unit in digits.iter().filter(|&&unit| unit != 95).take(count) {
            self.units(&[unit])?;
        }
        Ok(())
    }

    fn input_error(
        &mut self,
        digits: &[u16],
        negative: bool,
        radix: u8,
        count: usize,
    ) -> Result<(), Error> {
        self.text("For input string: \"")?;
        self.digits(digits, negative, count)?;
        self.text("\"")?;
        if radix != 10 {
            self.text(" under radix ")?;
            self.unsigned(u64::from(radix))?;
        }
        Ok(())
    }

    fn number(
        &mut self,
        digits: &[u16],
        radix: u8,
        width: u8,
        unsigned: bool,
        negative: bool,
    ) -> Result<(), Error> {
        let count = digits.iter().filter(|&&unit| unit != 95).count();
        let value = magnitude(digits, radix, count);
        let parse_limit = if width <= 16 { 1u64 << 31 } else { 1u64 << 63 };
        let fits_signed_parser =
            value.is_some_and(|value| value <= parse_limit - u64::from(!negative));
        if width <= 16 && fits_signed_parser {
            if unsigned {
                self.text("out of range: ")?;
                self.unsigned(value.unwrap())
            } else {
                self.text("Value out of range. Value:\"")?;
                self.digits(digits, negative, count)?;
                self.text("\" Radix:")?;
                self.unsigned(u64::from(radix))
            }
        } else if !unsigned || width <= 16 {
            self.input_error(digits, negative, radix, count)
        } else if width == 32 {
            self.text("String value ")?;
            self.digits(digits, false, count)?;
            self.text(" exceeds range of unsigned int.")
        } else {
            self.text("String value ")?;
            self.digits(digits, false, count)?;
            self.text(" exceeds range of unsigned long.")
        }
    }
}

fn digit(unit: u16) -> Option<u8> {
    match unit {
        48..=57 => Some((unit - 48) as u8),
        65..=70 => Some((unit - 55) as u8),
        97..=102 => Some((unit - 87) as u8),
        _ => None,
    }
}

fn magnitude(digits: &[u16], radix: u8, count: usize) -> Option<u64> {
    digits
        .iter()
        .filter(|&&unit| unit != 95)
        .take(count)
        .try_fold(0u64, |value, &unit| {
            value
                .checked_mul(u64::from(radix))?
                .checked_add(u64::from(digit(unit)?))
        })
}
