//! The lexing `Scene::add_shader` and `Scene::add_hdr_effect` do before
//! naga parses a game's module (`validate`): its tokens with comments
//! removed, as naga 30 lexes WGSL, the directive it starts with and the
//! names it declares at module scope.

/// A token of WGSL source with its comments removed: an identifier or
/// keyword, or one other character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Token {
    Word(String),
    Mark(char),
}

/// `source`'s tokens, line and nested block comments skipped, as naga 30
/// lexes WGSL (`front::wgsl::parse::lexer`, `consume_token`): a line
/// comment ends at any of its line breaks and its blankspace separates
/// tokens, so no declaration naga sees is hidden from `declared_names`.
pub(super) fn tokens(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if is_comment_end(next) {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut depth = 1;
                let mut previous = ' ';
                for next in chars.by_ref() {
                    match (previous, next) {
                        ('/', '*') => {
                            depth += 1;
                            previous = ' ';
                            continue;
                        }
                        ('*', '/') => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            previous = ' ';
                            continue;
                        }
                        _ => {}
                    }
                    previous = next;
                }
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if !(next.is_alphanumeric() || next == '_') {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
            c if is_blankspace(c) => {}
            c => tokens.push(Token::Mark(c)),
        }
    }
    tokens
}

/// Whether `c` ends a line comment: WGSL's line breaks, naga 30's
/// `is_comment_end` (`front::wgsl::parse::lexer`).
const fn is_comment_end(c: char) -> bool {
    matches!(
        c,
        '\u{000a}'..='\u{000d}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// Whether `c` is WGSL blankspace (Unicode Pattern_White_Space), naga 30's
/// `is_blankspace` (`front::wgsl::parse::lexer`).
const fn is_blankspace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r' | ' ' | '\u{0085}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}'
    )
}

/// The directive the module starts with, if any, up to its `;`.
pub(super) fn leading_directive(tokens: &[Token]) -> Option<String> {
    let Some(Token::Word(word)) = tokens.first() else {
        return None;
    };
    if !matches!(word.as_str(), "enable" | "requires" | "diagnostic") {
        return None;
    }
    let mut directive = String::new();
    for token in tokens
        .iter()
        .take_while(|token| **token != Token::Mark(';'))
    {
        if !directive.is_empty() {
            directive.push(' ');
        }
        match token {
            Token::Word(word) => directive.push_str(word),
            Token::Mark(mark) => directive.push(*mark),
        }
    }
    Some(directive)
}

/// The names the module declares at module scope: each `fn`, `struct`,
/// `const`, `override`, `alias` and `var` outside any brace.
pub(super) fn declared_names(tokens: &[Token]) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut at = 0;
    while at < tokens.len() {
        match &tokens[at] {
            Token::Mark('{') => depth += 1,
            Token::Mark('}') => depth = depth.saturating_sub(1),
            Token::Word(word)
                if depth == 0
                    && matches!(
                        word.as_str(),
                        "fn" | "struct" | "const" | "override" | "alias" | "var"
                    ) =>
            {
                let mut next = at + 1;
                // A variable's address space and access: `var<...>`.
                if word == "var" && tokens.get(next) == Some(&Token::Mark('<')) {
                    let mut angles = 0usize;
                    while let Some(token) = tokens.get(next) {
                        match token {
                            Token::Mark('<') => angles += 1,
                            Token::Mark('>') => {
                                angles -= 1;
                                if angles == 0 {
                                    next += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        next += 1;
                    }
                }
                if let Some(Token::Word(name)) = tokens.get(next) {
                    names.push(name.clone());
                }
                at = next;
            }
            _ => {}
        }
        at += 1;
    }
    names
}
