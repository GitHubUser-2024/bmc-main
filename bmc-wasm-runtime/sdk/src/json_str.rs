// Copyright (C) 2026  Braiins Forge s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. and Braiins Forge s.r.o. each reserve the right
// to grant any party a license to this program, or any part thereof,
// under any terms, and such a grant shall be considered distinct from
// the grant above.

//! RFC 8259 string-body escaping for `json!` macro `#s(...)` interpolations.
//!
//! `JsonStr` wraps any `&str`-convertible value so that `ufmt`/[`crate::fmt!`]
//! writes it with `"`, `\`, and ASCII control characters properly escaped per
//! the JSON string grammar. Multi-byte UTF-8 sequences pass through unchanged.

use ufmt::{Formatter, uDisplay, uWrite};

/// Wrapper that escapes its inner value as a JSON string body when written
/// via `ufmt`.
///
/// The macro [`crate::json!`] wraps `#s(expr)` interpolations in this type so
/// the surrounding `"..."` template literal remains valid even when `expr`
/// contains characters that would otherwise terminate the string.
#[derive(Debug)]
pub struct JsonStr<T>(pub T);

impl<T: AsRef<str>> uDisplay for JsonStr<T> {
    #[expect(
        clippy::string_slice,
        reason = "split only at ASCII bytes, never inside a UTF-8 sequence"
    )]
    fn fmt<W: uWrite + ?Sized>(&self, f: &mut Formatter<'_, W>) -> Result<(), W::Error> {
        // Runs that need no escaping go out in one write: a write per
        // character costs a widget its frame budget on long strings. Every
        // byte that needs escaping is ASCII, so splitting at one never cuts
        // a multi-byte UTF-8 sequence.
        let text = self.0.as_ref();
        let mut run_start = 0;
        let mut buf = [b'\\', b'u', b'0', b'0', 0, 0];
        for (i, byte) in text.bytes().enumerate() {
            let escaped = match byte {
                b'"' => "\\\"",
                b'\\' => "\\\\",
                b'\n' => "\\n",
                b'\r' => "\\r",
                b'\t' => "\\t",
                0x08 => "\\b",
                0x0C => "\\f",
                0x00..=0x1F => {
                    let hex = b"0123456789abcdef";
                    buf[4] = hex[usize::from(byte >> 4)];
                    buf[5] = hex[usize::from(byte & 0xf)];
                    // SAFETY: every byte was selected from the ASCII set above.
                    unsafe { core::str::from_utf8_unchecked(&buf) }
                }
                _ => continue,
            };
            f.write_str(&text[run_start..i])?;
            f.write_str(escaped)?;
            run_start = i + 1;
        }
        f.write_str(&text[run_start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render<T: uDisplay>(v: T) -> String {
        let mut s = String::new();
        ufmt::uwrite!(&mut s, "{}", v).expect("BUG: String uWrite cannot fail");
        s
    }

    #[test]
    fn plain_passes_through() {
        assert_eq!(render(JsonStr("hello world")), "hello world");
    }

    #[test]
    fn double_quote_escaped() {
        assert_eq!(render(JsonStr(r#"a"b"#)), r#"a\"b"#);
    }

    #[test]
    fn backslash_escaped() {
        assert_eq!(render(JsonStr(r"a\b")), r"a\\b");
    }

    #[test]
    fn whitespace_chars() {
        assert_eq!(render(JsonStr("\n\r\t")), r"\n\r\t");
    }

    #[test]
    fn backspace_and_form_feed() {
        assert_eq!(render(JsonStr("\u{0008}\u{000C}")), r"\b\f");
    }

    #[test]
    fn other_control_chars_unicode_escape() {
        assert_eq!(render(JsonStr("\u{0001}\u{001F}")), r"\u0001\u001f");
    }

    #[test]
    fn utf8_passes_through() {
        assert_eq!(render(JsonStr("Příliš")), "Příliš");
        assert_eq!(render(JsonStr("🦀")), "🦀");
    }

    #[test]
    fn del_and_above_pass_through() {
        // RFC 8259 only requires escaping U+0000..U+001F plus `"` and `\`.
        // U+007F (DEL) and above pass through.
        assert_eq!(render(JsonStr("\u{007F}")), "\u{007F}");
    }

    #[test]
    fn escapes_between_plain_runs() {
        assert_eq!(
            render(JsonStr("Příliš \"žluťoučký\"\n🦀")),
            r#"Příliš \"žluťoučký\"\n🦀"#
        );
        assert_eq!(render(JsonStr("")), "");
    }

    #[test]
    fn injection_attempt_neutralised() {
        // Reviewer's concern: a value tries to break out of the string and
        // inject a sibling field.
        assert_eq!(
            render(JsonStr(r#"foo","admin":true,"x":"#)),
            r#"foo\",\"admin\":true,\"x\":"#,
        );
    }
}
