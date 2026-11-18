//! The few pieces of Python's string and number behaviour the rules depend
//! on, reproduced so the port gives the same answers on the same text.
//!
//! `emco_rules.py` leans on `str.splitlines`, `str.strip`, `str.split` and
//! the `{:g}` / `{:.Nf}` formats. Rust's `lines`, `trim` and `{}` differ at
//! the edges (a lone `\r` line break, `\x1c`..`\x1f`, `1e+06`), and a
//! finding's line number or message text is part of what the screen shows.

/// Python's `str.isspace` for one character: Rust's `White_Space` plus the
/// four ASCII information separators Python also treats as whitespace.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` with no argument: runs of whitespace separate, no empties.
pub fn split_whitespace(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|w| !w.is_empty())
}

/// `str.splitlines()`: breaks on `\n`, `\r\n`, `\r`, `\v`, `\f`, `\x1c`,
/// `\x1d`, `\x1e`, `\x85`, ` ` and ` `; no trailing empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn non_finite(x: f64) -> Option<String> {
    if x.is_nan() {
        Some("nan".into())
    } else if x.is_infinite() {
        Some(if x > 0.0 { "inf" } else { "-inf" }.into())
    } else {
        None
    }
}

/// Python's `"{:.Nf}".format(x)`.
pub fn fixed(x: f64, precision: usize) -> String {
    non_finite(x).unwrap_or_else(|| format!("{x:.precision$}"))
}

/// Python's `"{:g}".format(x)`: six significant digits, insignificant zeros
/// dropped, scientific notation (`1e+06`) below 1e-4 and from 1e6 up.
pub fn g(x: f64) -> String {
    if let Some(s) = non_finite(x) {
        return s;
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.into();
    }
    const P: i32 = 6;
    let sci = format!("{:.*e}", (P - 1) as usize, x);
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    if (-4..P).contains(&exp) {
        let digits = (P - 1 - exp) as usize;
        trim_zeros(&format!("{x:.digits$}")).to_string()
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", trim_zeros(mantissa), sign, exp.abs())
    }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_matches_python() {
        // python3 -c "print(['{:g}'.format(x) for x in (...)])"
        let cases = [
            (1400.0, "1400"),
            (96.0, "96"),
            (48.0, "48"),
            (0.5, "0.5"),
            (1e6, "1e+06"),
            (123456789.0, "1.23457e+08"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-0.0, "-0"),
            (5.99, "5.99"),
            (999999.5, "1e+06"),
            (-12.5, "-12.5"),
            (f64::NAN, "nan"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (x, want) in cases {
            assert_eq!(g(x), want, "{x}");
        }
    }

    #[test]
    fn fixed_matches_python() {
        assert_eq!(fixed(5.99, 4), "5.9900");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(1500.4, 0), "1500");
        assert_eq!(fixed(f64::NAN, 4), "nan");
    }

    #[test]
    fn splitlines_matches_python() {
        // "a\nb\r\nc\rd\x0be\x1cf g\n".splitlines()
        assert_eq!(
            splitlines("a\nb\r\nc\rd\u{0b}e\u{1c}f\u{2028}g\n"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\n"), ["", ""]);
        assert_eq!(splitlines("x"), ["x"]);
    }

    #[test]
    fn strip_and_split_match_python() {
        assert_eq!(strip("\u{1f} (a) \t"), "(a)");
        assert_eq!(
            split_whitespace(" O<R>  SUB\u{1d}X ").collect::<Vec<_>>(),
            ["O<R>", "SUB", "X"]
        );
    }
}
