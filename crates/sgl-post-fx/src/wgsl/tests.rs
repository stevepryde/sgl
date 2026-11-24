//! Stripping cases whose expected text follows from WGSL's lexical grammar
//! (comments are blankspace, block comments nest, a line comment ends at a
//! line break) as naga 30 lexes it. Plausible defects: a comment glued
//! between two tokens joins them, a nested or `/*/` comment closes early or
//! late, a line comment swallows the code after a carriage return, a
//! line is dropped or merged so errors move, whitespace inside a line is
//! rewritten.
use super::{as_str, prefix, strip_padded, stripped};

#[test]
fn strips_comments_indentation_and_trailing_whitespace_keeping_each_line() {
    let cases = [
        // Line comment, indentation, trailing whitespace.
        (
            "  let a = 1;  // one\n\tfoo();\t \n",
            "let a = 1;\nfoo();\n",
        ),
        // A comment-only or blank line stays, empty.
        ("a\n// note\n   \n/* x */\nb", "a\n\n\n\nb"),
        // A comment between two tokens separates them.
        ("a/*x*/b", "a b"),
        ("x </**/< y", "x < < y"),
        // Nested block comments close at their match.
        ("a /* x /* y */ still */ b", "a b"),
        // `/*/` opens without closing; `**/` closes.
        ("a /*/ b */ c", "a c"),
        ("a /** x **/ b", "a b"),
        // A block comment over lines keeps them.
        ("a /* x\ny\n*/ b\n", "a\n\nb\n"),
        // Neither comment opens inside the other.
        ("a /* // */ b\nc // /* \nd", "a b\nc\nd"),
        // A line comment ends at a carriage return or U+2028: code follows.
        ("a // c\rb\n", "a b\n"),
        ("a // c\u{2028}b", "a \u{2028}b"),
        // Whitespace between tokens is kept as written.
        ("a  =\tb;", "a  =\tb;"),
        // Division and dereference are not comments.
        ("x = y / *p;", "x = y / *p;"),
        // Directives and attributes are code.
        (
            "    enable f16; // half\n@group(0) @binding(1) var t: texture_2d<f32>;",
            "enable f16;\n@group(0) @binding(1) var t: texture_2d<f32>;",
        ),
        ("", ""),
        ("a", "a"),
    ];
    for (source, expected) in cases {
        assert_eq!(stripped(source), expected, "{source:?}");
        assert_eq!(
            stripped(source).lines().count(),
            source.lines().count(),
            "{source:?}"
        );
    }
}

// The release path is the compile-time one, as `embed_wgsl!` runs it.
#[test]
fn compile_time_stripping_matches() {
    const SOURCE: &str = "  fn f() -> f32 { // one\n    return /* two */ 1.0;\n}\n";
    const PADDED: ([u8; SOURCE.len()], usize) = strip_padded(SOURCE);
    const STRIPPED: [u8; PADDED.1] = prefix(&PADDED.0);
    const TEXT: &str = as_str(&STRIPPED);
    assert_eq!(TEXT, "fn f() -> f32 {\nreturn 1.0;\n}\n");
}

// An open block comment would swallow the next composed module whole but
// end with its file stripped: refused.
#[test]
#[should_panic(expected = "not closed")]
fn refuses_an_open_block_comment() {
    let _ = stripped("fn f() {}\n/* open");
}
