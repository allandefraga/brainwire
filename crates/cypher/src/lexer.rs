//! Tokenizer for the Cypher subset (spec 3.1). `tokenize` makes a single,
//! non-recursive pass over the source and returns the full token stream up
//! front, so it cannot overflow the stack no matter how long or how
//! repetitive the input is (nesting limits are a parser concern; see
//! `parser::MAX_NESTING`). Every offset in a `Spanned` and in a `ParseError`
//! it produces lands on a `char` boundary of the original source, since it
//! only ever slices at positions handed back by `CharIndices` or at
//! `src.len()`.
//!
//! Keywords are not their own token kind: `MATCH`, `AND`, `count` and so on
//! all come back as `Tok::Ident`, and the parser compares their text
//! case-insensitively wherever a keyword is expected. That mirrors the
//! grammar, where the same word is a keyword in one position and a plain
//! identifier in another (`count` is a variable unless immediately followed
//! by `(`). A backtick-quoted identifier is never a keyword, whatever text it
//! holds, so it gets its own token kind (`QuotedIdent`).

use crate::parser::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Dollar,
    Semicolon,
    Star,
    Minus,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `->`
    Arrow,
    /// `<-`
    LArrow,
    Plus,
    /// `+=`
    PlusEq,
    Slash,
    Percent,
    Caret,
    Pipe,
    /// `..`
    DotDot,
    /// `=~`
    RegexMatch,
    /// A bare identifier: letters/digits/`_`, starting with a letter or `_`.
    Ident(String),
    /// A backtick-quoted identifier; its text is never treated as a keyword.
    QuotedIdent(String),
    Int(i64),
    /// Magnitude 2^63: valid only immediately after a unary minus.
    MinIntMagnitude,
    Float(f64),
    Str(String),
    /// Sentinel end-of-input token; `start == end == source.len()`.
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned {
    pub tok: Tok,
    pub start: usize,
    pub end: usize,
}

/// Scans `src` into a flat token stream, always ending with one `Tok::Eof`.
pub(crate) fn tokenize(src: &str) -> Result<Vec<Spanned>, ParseError> {
    let mut lx = Lexer {
        src,
        iter: src.char_indices().peekable(),
    };
    let mut out = Vec::new();
    loop {
        lx.skip_trivia();
        let start = lx.offset();
        let Some((_, c)) = lx.peek() else {
            out.push(Spanned {
                tok: Tok::Eof,
                start,
                end: start,
            });
            break;
        };
        out.push(lx.scan_token(c, start)?);
    }
    Ok(out)
}

struct Lexer<'a> {
    src: &'a str,
    iter: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Lexer<'a> {
    fn peek(&mut self) -> Option<(usize, char)> {
        self.iter.peek().copied()
    }

    fn peek_char(&mut self) -> Option<char> {
        self.iter.peek().map(|&(_, c)| c)
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        self.iter.next()
    }

    /// Byte offset of the next unconsumed character, or `src.len()` at the end.
    /// Always a char boundary: either a `CharIndices` offset or the source length.
    fn offset(&mut self) -> usize {
        match self.iter.peek() {
            Some(&(i, _)) => i,
            None => self.src.len(),
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek_char() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') => {
                    let mut lookahead = self.iter.clone();
                    lookahead.next();
                    if matches!(lookahead.peek(), Some((_, '/'))) {
                        self.bump(); // first '/'
                        self.bump(); // second '/'
                        loop {
                            match self.peek_char() {
                                None | Some('\n') => break,
                                Some(_) => {
                                    self.bump();
                                }
                            }
                        }
                    } else {
                        // A lone '/' is not trivia; leave it for scan_token,
                        // which reports it as an unexpected character.
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    fn scan_token(&mut self, c: char, start: usize) -> Result<Spanned, ParseError> {
        let one = |tok: Tok| Spanned {
            tok,
            start,
            end: start + 1,
        };
        match c {
            '(' => {
                self.bump();
                Ok(one(Tok::LParen))
            }
            ')' => {
                self.bump();
                Ok(one(Tok::RParen))
            }
            '[' => {
                self.bump();
                Ok(one(Tok::LBracket))
            }
            ']' => {
                self.bump();
                Ok(one(Tok::RBracket))
            }
            '{' => {
                self.bump();
                Ok(one(Tok::LBrace))
            }
            '}' => {
                self.bump();
                Ok(one(Tok::RBrace))
            }
            ':' => {
                self.bump();
                Ok(one(Tok::Colon))
            }
            ',' => {
                self.bump();
                Ok(one(Tok::Comma))
            }
            '.' => {
                self.bump();
                if let Some((i, '.')) = self.peek() {
                    self.bump();
                    return Ok(Spanned {
                        tok: Tok::DotDot,
                        start,
                        end: i + 1,
                    });
                }
                Ok(one(Tok::Dot))
            }
            '+' => {
                self.bump();
                if let Some((i, '=')) = self.peek() {
                    self.bump();
                    return Ok(Spanned {
                        tok: Tok::PlusEq,
                        start,
                        end: i + 1,
                    });
                }
                Ok(one(Tok::Plus))
            }
            '/' => {
                self.bump();
                Ok(one(Tok::Slash))
            }
            '%' => {
                self.bump();
                Ok(one(Tok::Percent))
            }
            '^' => {
                self.bump();
                Ok(one(Tok::Caret))
            }
            '|' => {
                self.bump();
                Ok(one(Tok::Pipe))
            }
            '!' => {
                self.bump();
                match self.peek() {
                    Some((i, '=')) => {
                        self.bump();
                        Ok(Spanned {
                            tok: Tok::Ne,
                            start,
                            end: i + 1,
                        })
                    }
                    _ => Err(ParseError {
                        message: "unexpected character '!'".into(),
                        offset: start,
                    }),
                }
            }
            '$' => {
                self.bump();
                Ok(one(Tok::Dollar))
            }
            ';' => {
                self.bump();
                Ok(one(Tok::Semicolon))
            }
            '*' => {
                self.bump();
                Ok(one(Tok::Star))
            }
            '=' => {
                self.bump();
                if let Some((i, '~')) = self.peek() {
                    self.bump();
                    return Ok(Spanned {
                        tok: Tok::RegexMatch,
                        start,
                        end: i + 1,
                    });
                }
                Ok(one(Tok::Eq))
            }
            '-' => {
                self.bump();
                if matches!(self.peek(), Some((_, '>'))) {
                    let (i, _) = self.bump().expect("peeked");
                    Ok(Spanned {
                        tok: Tok::Arrow,
                        start,
                        end: i + 1,
                    })
                } else {
                    Ok(one(Tok::Minus))
                }
            }
            '<' => {
                self.bump();
                match self.peek() {
                    Some((i, '=')) => {
                        self.bump();
                        Ok(Spanned {
                            tok: Tok::Le,
                            start,
                            end: i + 1,
                        })
                    }
                    Some((i, '-')) => {
                        self.bump();
                        Ok(Spanned {
                            tok: Tok::LArrow,
                            start,
                            end: i + 1,
                        })
                    }
                    Some((i, '>')) => {
                        self.bump();
                        Ok(Spanned {
                            tok: Tok::Ne,
                            start,
                            end: i + 1,
                        })
                    }
                    _ => Ok(one(Tok::Lt)),
                }
            }
            '>' => {
                self.bump();
                match self.peek() {
                    Some((i, '=')) => {
                        self.bump();
                        Ok(Spanned {
                            tok: Tok::Ge,
                            start,
                            end: i + 1,
                        })
                    }
                    _ => Ok(one(Tok::Gt)),
                }
            }
            '\'' | '"' => self.scan_string(start, c),
            '`' => self.scan_quoted_ident(start),
            d if d.is_ascii_digit() => self.scan_number(start),
            c if is_ident_start(c) => Ok(self.scan_ident(start)),
            other => Err(ParseError {
                message: format!("unexpected character '{other}'"),
                offset: start,
            }),
        }
    }

    fn scan_string(&mut self, start: usize, quote: char) -> Result<Spanned, ParseError> {
        self.bump(); // opening quote
        let mut value = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(ParseError {
                        message: "unterminated string".into(),
                        offset: start,
                    });
                }
                Some((i, c)) if c == quote => {
                    return Ok(Spanned {
                        tok: Tok::Str(value),
                        start,
                        end: i + 1,
                    });
                }
                Some((bs, '\\')) => match self.bump() {
                    None => {
                        return Err(ParseError {
                            message: "unterminated string".into(),
                            offset: start,
                        });
                    }
                    Some((_, '\\')) => value.push('\\'),
                    Some((_, '\'')) => value.push('\''),
                    Some((_, '"')) => value.push('"'),
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, 't')) => value.push('\t'),
                    Some((_, 'r')) => value.push('\r'),
                    Some((_, 'b')) => value.push('\u{8}'),
                    Some((_, 'f')) => value.push('\u{c}'),
                    Some((_, 'u')) => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let digit =
                                self.bump()
                                    .and_then(|(_, c)| c.to_digit(16))
                                    .ok_or_else(|| ParseError {
                                        message: "invalid \\u escape".into(),
                                        offset: bs,
                                    })?;
                            code = code * 16 + digit;
                        }
                        value.push(char::from_u32(code).ok_or_else(|| ParseError {
                            message: "invalid \\u escape".into(),
                            offset: bs,
                        })?);
                    }
                    Some((_, other)) => {
                        return Err(ParseError {
                            message: format!("invalid escape sequence \\{other}"),
                            offset: bs,
                        });
                    }
                },
                Some((_, c)) => value.push(c),
            }
        }
    }

    fn scan_quoted_ident(&mut self, start: usize) -> Result<Spanned, ParseError> {
        self.bump(); // opening backtick
        let mut value = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(ParseError {
                        message: "unterminated identifier".into(),
                        offset: start,
                    });
                }
                Some((i, '`')) => {
                    return Ok(Spanned {
                        tok: Tok::QuotedIdent(value),
                        start,
                        end: i + 1,
                    });
                }
                Some((_, c)) => value.push(c),
            }
        }
    }

    fn scan_ident(&mut self, start: usize) -> Spanned {
        let mut end = start;
        while let Some((i, c)) = self.peek() {
            if is_ident_continue(c) {
                end = i + c.len_utf8();
                self.bump();
            } else {
                break;
            }
        }
        Spanned {
            tok: Tok::Ident(self.src[start..end].to_string()),
            start,
            end,
        }
    }

    fn scan_number(&mut self, start: usize) -> Result<Spanned, ParseError> {
        let mut end = start;
        let mut is_float = false;

        while let Some((i, c)) = self.peek() {
            if c.is_ascii_digit() {
                end = i + 1;
                self.bump();
            } else {
                break;
            }
        }

        // Fractional part: a '.' counts only when followed by a digit, so
        // `1.` and the two dots of a `1..3` range are left alone.
        if matches!(self.peek(), Some((_, '.'))) {
            let mut lookahead = self.iter.clone();
            lookahead.next();
            if matches!(lookahead.peek(), Some((_, c)) if c.is_ascii_digit()) {
                is_float = true;
                self.bump(); // '.'
                while let Some((i, c)) = self.peek() {
                    if c.is_ascii_digit() {
                        end = i + 1;
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
        }

        // Exponent: `[eE] [+-]? digit+`, only consumed once a digit is confirmed.
        if matches!(self.peek(), Some((_, 'e' | 'E'))) {
            let mut lookahead = self.iter.clone();
            lookahead.next();
            if matches!(lookahead.peek(), Some((_, '+' | '-'))) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some((_, c)) if c.is_ascii_digit()) {
                is_float = true;
                self.bump(); // 'e'/'E'
                if matches!(self.peek(), Some((_, '+' | '-'))) {
                    self.bump();
                }
                while let Some((i, c)) = self.peek() {
                    if c.is_ascii_digit() {
                        end = i + 1;
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
        }

        let text = &self.src[start..end];
        if is_float {
            let value: f64 = text.parse().map_err(|_| ParseError {
                message: format!("invalid float literal '{text}'"),
                offset: start,
            })?;
            if !value.is_finite() {
                return Err(ParseError {
                    message: format!("float literal '{text}' out of range"),
                    offset: start,
                });
            }
            Ok(Spanned {
                tok: Tok::Float(value),
                start,
                end,
            })
        } else {
            // Keep the one extra negative magnitude until the parser sees its sign.
            // Trimming zeros preserves decimal literals with leading zeroes.
            let tok = if text.trim_start_matches('0') == "9223372036854775808" {
                Tok::MinIntMagnitude
            } else {
                Tok::Int(text.parse().map_err(|_| ParseError {
                    message: format!("integer literal '{text}' out of range"),
                    offset: start,
                })?)
            };
            Ok(Spanned { tok, start, end })
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_literal_overflow_is_rejected() {
        for source in ["1e309", "-1e309", "1.9e308"] {
            assert!(tokenize(source).is_err(), "{source}");
        }
        assert!(tokenize("1e308").is_ok());
    }
    #[test]
    fn permits_negative_minimum_magnitude_for_parser_to_resolve() {
        for source in ["-9223372036854775808", "- 0009223372036854775808"] {
            let tokens = tokenize(source).unwrap();
            assert_eq!(tokens[0].tok, Tok::Minus);
            assert_eq!(tokens.len(), 3);
        }
        assert!(tokenize("9223372036854775809").is_err());
    }
}
