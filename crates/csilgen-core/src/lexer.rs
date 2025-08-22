//! CSIL lexer implementation for tokenizing CDDL and CSIL extensions

use std::fmt;

/// Position information for tokens
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

impl Position {
    pub fn new(line: usize, column: usize, offset: usize) -> Self {
        Self {
            line,
            column,
            offset,
        }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Token types for CDDL and CSIL
#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    // CDDL Operators
    Assign,      // =
    GroupChoice, // //=
    TypeChoice,  // /=

    // CDDL Delimiters
    LeftBrace,    // {
    RightBrace,   // }
    LeftBracket,  // [
    RightBracket, // ]
    LeftParen,    // (
    RightParen,   // )

    // CDDL Punctuation
    Comma,     // ,
    Colon,     // :
    Semicolon, // ;
    Arrow,     // =>
    Dot,       // .

    // CDDL Choice and Ranges
    Choice,         // /
    Range,          // ..
    RangeInclusive, // ...

    // CDDL Occurrence Indicators
    Optional,   // ?
    ZeroOrMore, // *
    OneOrMore,  // +

    // CSIL Service Keywords
    Service,              // service
    ServiceArrow,         // ->
    ServiceBackArrow,     // <-
    ServiceBidirectional, // <->

    // CSIL File Options
    Options, // options

    // CSIL Import Keywords
    Include, // include
    From,    // from
    As,      // as

    // CSIL Metadata Annotations
    AtSendOnly,      // @send-only
    AtReceiveOnly,   // @receive-only
    AtBidirectional, // @bidirectional
    AtDependsOn,     // @depends-on
    AtDescription,   // @description
    AtMinLength,     // @min-length
    AtMaxLength,     // @max-length
    AtMinItems,      // @min-items
    AtMaxItems,      // @max-items
    AtMinValue,      // @min-value
    AtMaxValue,      // @max-value
    AtCustom,        // @<identifier>

    // CDDL Control Operators - Supported
    DotSize,    // .size
    DotRegex,   // .regex
    DotDefault, // .default
    DotGe,      // .ge (greater than or equal)
    DotLe,      // .le (less than or equal)
    DotGt,      // .gt (greater than)
    DotLt,      // .lt (less than)
    DotEq,      // .eq (equal to)

    // CDDL Control Operators - Unsupported (planned features)
    DotNe,      // .ne (not equal) - coming soon
    DotBits,    // .bits (bit control) - coming soon
    DotAnd,     // .and (type intersection) - coming soon
    DotWithin,  // .within (subset constraint) - coming soon
    DotJson,    // .json (JSON encoding) - coming soon
    DotCbor,    // .cbor (CBOR encoding) - coming soon
    DotCborseq, // .cborseq (CBOR sequence) - coming soon

    // CDDL Socket/Plug
    Socket, // $<identifier>
    Plug,   // $$<identifier>

    // Literals
    Integer(i64),
    Float(f64),
    TextString(String),
    ByteString(Vec<u8>),

    // Identifiers and Types
    Identifier(String),
    Builtin(String), // int, text, bool, bytes, etc.

    // Comments
    Comment(String),

    // Whitespace and EOF
    Whitespace(String),
    Newline,
    Eof,
}

/// A token with its type, value, and position
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub token_type: TokenType,
    pub position: Position,
    pub lexeme: String,
}

impl Token {
    pub fn new(token_type: TokenType, position: Position, lexeme: String) -> Self {
        Self {
            token_type,
            position,
            lexeme,
        }
    }
}

/// Lexer for CDDL and CSIL
pub struct Lexer {
    input: Vec<char>,
    current: usize,
    line: usize,
    column: usize,
    start_column: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            current: 0,
            line: 1,
            column: 1,
            start_column: 1,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();

        while !self.is_at_end() {
            let token = self.next_token()?;
            tokens.push(token);
        }

        tokens.push(Token::new(
            TokenType::Eof,
            Position::new(self.line, self.column, self.current),
            String::new(),
        ));

        Ok(tokens)
    }

    fn next_token(&mut self) -> Result<Token, LexerError> {
        self.start_column = self.column;
        let start_pos = Position::new(self.line, self.start_column, self.current);
        let ch = self.advance();

        match ch {
            // Whitespace
            ' ' | '\t' | '\r' => {
                let mut whitespace = String::from(ch);
                while self.peek().is_ascii_whitespace() && self.peek() != '\n' {
                    whitespace.push(self.advance());
                }
                Ok(Token::new(
                    TokenType::Whitespace(whitespace.clone()),
                    start_pos,
                    whitespace,
                ))
            }

            '\n' => {
                let token = Token::new(TokenType::Newline, start_pos, String::from(ch));
                self.line += 1;
                self.column = 1;
                Ok(token)
            }

            // Comments or semicolon
            ';' => {
                // Check for CDDL-style comment ;;
                if self.peek() == ';' {
                    self.advance(); // consume second ;
                    self.comment(start_pos)
                } else if self.peek().is_ascii_whitespace() || self.is_at_end() {
                    // If followed by whitespace or end of input, treat as comment
                    self.comment(start_pos)
                } else {
                    Ok(Token::new(
                        TokenType::Semicolon,
                        start_pos,
                        String::from(ch),
                    ))
                }
            }

            // Single character tokens
            '{' => Ok(Token::new(
                TokenType::LeftBrace,
                start_pos,
                String::from(ch),
            )),
            '}' => Ok(Token::new(
                TokenType::RightBrace,
                start_pos,
                String::from(ch),
            )),
            '[' => Ok(Token::new(
                TokenType::LeftBracket,
                start_pos,
                String::from(ch),
            )),
            ']' => Ok(Token::new(
                TokenType::RightBracket,
                start_pos,
                String::from(ch),
            )),
            '(' => Ok(Token::new(
                TokenType::LeftParen,
                start_pos,
                String::from(ch),
            )),
            ')' => Ok(Token::new(
                TokenType::RightParen,
                start_pos,
                String::from(ch),
            )),
            ',' => Ok(Token::new(TokenType::Comma, start_pos, String::from(ch))),
            ':' => Ok(Token::new(TokenType::Colon, start_pos, String::from(ch))),
            '?' => Ok(Token::new(TokenType::Optional, start_pos, String::from(ch))),
            '*' => Ok(Token::new(
                TokenType::ZeroOrMore,
                start_pos,
                String::from(ch),
            )),
            '+' => Ok(Token::new(
                TokenType::OneOrMore,
                start_pos,
                String::from(ch),
            )),

            // Multi-character operators
            '=' => {
                if self.peek() == '>' {
                    self.advance();
                    Ok(Token::new(TokenType::Arrow, start_pos, "=>".to_string()))
                } else {
                    Ok(Token::new(TokenType::Assign, start_pos, String::from(ch)))
                }
            }

            '/' => {
                if self.peek() == '/' && self.peek_next() == '=' {
                    self.advance(); // consume /
                    self.advance(); // consume =
                    Ok(Token::new(
                        TokenType::GroupChoice,
                        start_pos,
                        "//=".to_string(),
                    ))
                } else if self.peek() == '=' {
                    self.advance(); // consume =
                    Ok(Token::new(
                        TokenType::TypeChoice,
                        start_pos,
                        "/=".to_string(),
                    ))
                } else {
                    Ok(Token::new(TokenType::Choice, start_pos, String::from(ch)))
                }
            }

            '-' => {
                if self.peek() == '>' {
                    self.advance();
                    Ok(Token::new(
                        TokenType::ServiceArrow,
                        start_pos,
                        "->".to_string(),
                    ))
                } else {
                    self.number_or_identifier(ch, start_pos)
                }
            }

            '<' => {
                if self.peek() == '-' {
                    self.advance();
                    if self.peek() == '>' {
                        self.advance();
                        Ok(Token::new(
                            TokenType::ServiceBidirectional,
                            start_pos,
                            "<->".to_string(),
                        ))
                    } else {
                        Ok(Token::new(
                            TokenType::ServiceBackArrow,
                            start_pos,
                            "<-".to_string(),
                        ))
                    }
                } else {
                    Err(LexerError::UnexpectedCharacter {
                        ch,
                        position: start_pos,
                    })
                }
            }

            '.' => {
                if self.peek() == '.' {
                    self.advance();
                    if self.peek() == '.' {
                        self.advance();
                        Ok(Token::new(
                            TokenType::RangeInclusive,
                            start_pos,
                            "...".to_string(),
                        ))
                    } else {
                        Ok(Token::new(TokenType::Range, start_pos, "..".to_string()))
                    }
                } else if self.peek_identifier_start() {
                    // Check for .size, .regex, .default control operators
                    let saved_current = self.current;
                    let saved_line = self.line;
                    let saved_column = self.column;

                    let ident = self.read_identifier();

                    match ident.as_str() {
                        // Supported control operators
                        "size" => Ok(Token::new(
                            TokenType::DotSize,
                            start_pos,
                            ".size".to_string(),
                        )),
                        "regex" => Ok(Token::new(
                            TokenType::DotRegex,
                            start_pos,
                            ".regex".to_string(),
                        )),
                        "default" => Ok(Token::new(
                            TokenType::DotDefault,
                            start_pos,
                            ".default".to_string(),
                        )),
                        "ge" => Ok(Token::new(TokenType::DotGe, start_pos, ".ge".to_string())),
                        "le" => Ok(Token::new(TokenType::DotLe, start_pos, ".le".to_string())),
                        "gt" => Ok(Token::new(TokenType::DotGt, start_pos, ".gt".to_string())),
                        "lt" => Ok(Token::new(TokenType::DotLt, start_pos, ".lt".to_string())),
                        "eq" => Ok(Token::new(TokenType::DotEq, start_pos, ".eq".to_string())),

                        // Unsupported control operators (planned features)
                        "ne" => Ok(Token::new(TokenType::DotNe, start_pos, ".ne".to_string())),
                        "bits" => Ok(Token::new(
                            TokenType::DotBits,
                            start_pos,
                            ".bits".to_string(),
                        )),
                        "and" => Ok(Token::new(TokenType::DotAnd, start_pos, ".and".to_string())),
                        "within" => Ok(Token::new(
                            TokenType::DotWithin,
                            start_pos,
                            ".within".to_string(),
                        )),
                        "json" => Ok(Token::new(
                            TokenType::DotJson,
                            start_pos,
                            ".json".to_string(),
                        )),
                        "cbor" => Ok(Token::new(
                            TokenType::DotCbor,
                            start_pos,
                            ".cbor".to_string(),
                        )),
                        "cborseq" => Ok(Token::new(
                            TokenType::DotCborseq,
                            start_pos,
                            ".cborseq".to_string(),
                        )),
                        _ => {
                            // Not a recognized control operator, backtrack
                            self.current = saved_current;
                            self.line = saved_line;
                            self.column = saved_column;
                            Ok(Token::new(TokenType::Dot, start_pos, String::from(ch)))
                        }
                    }
                } else {
                    Ok(Token::new(TokenType::Dot, start_pos, String::from(ch)))
                }
            }

            // Strings
            '"' => self.text_string(start_pos),
            '\'' => self.byte_string(start_pos),

            // Socket/Plug
            '$' => {
                if self.peek() == '$' {
                    self.advance();
                    let name = self.identifier_chars();
                    let lexeme = format!("$${name}");
                    Ok(Token::new(TokenType::Plug, start_pos, lexeme))
                } else {
                    let name = self.identifier_chars();
                    let lexeme = format!("${name}");
                    Ok(Token::new(TokenType::Socket, start_pos, lexeme))
                }
            }

            // Metadata annotations
            '@' => self.metadata_annotation(start_pos),

            // Hash comments
            '#' => self.comment(start_pos),

            // Numbers and identifiers
            _ if ch.is_ascii_digit() => self.number_or_identifier(ch, start_pos),
            _ if ch.is_ascii_alphabetic() || ch == '_' => self.identifier_or_keyword(ch, start_pos),

            _ => Err(LexerError::UnexpectedCharacter {
                ch,
                position: start_pos,
            }),
        }
    }

    fn comment(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let comment_start = if start_pos.offset > 0 {
            self.input[start_pos.offset - 1]
        } else {
            '#'
        };

        let mut comment = String::new();
        while self.peek() != '\n' && !self.is_at_end() {
            comment.push(self.advance());
        }

        let prefix = if comment_start == '#' { "#" } else { ";" };
        Ok(Token::new(
            TokenType::Comment(comment.clone()),
            start_pos,
            format!("{prefix}{comment}"),
        ))
    }

    fn text_string(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let mut value = String::new();
        let mut lexeme = String::from('"');

        while self.peek() != '"' && !self.is_at_end() {
            let ch = self.advance();
            lexeme.push(ch);

            if ch == '\\' && !self.is_at_end() {
                let escaped = self.advance();
                lexeme.push(escaped);
                match escaped {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    'r' => value.push('\r'),
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    _ => {
                        value.push('\\');
                        value.push(escaped);
                    }
                }
            } else {
                value.push(ch);
            }
        }

        if self.is_at_end() {
            return Err(LexerError::UnterminatedString {
                position: start_pos,
            });
        }

        self.advance(); // consume closing "
        lexeme.push('"');

        Ok(Token::new(TokenType::TextString(value), start_pos, lexeme))
    }

    fn byte_string(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let mut bytes = Vec::new();
        let mut lexeme = String::from('\'');

        while self.peek() != '\'' && !self.is_at_end() {
            let ch = self.advance();
            lexeme.push(ch);

            if ch.is_ascii_hexdigit() {
                if let Some(next_ch) = self.peek().to_digit(16) {
                    if ch.is_ascii_hexdigit() {
                        let byte_val = (ch.to_digit(16).unwrap() * 16 + next_ch) as u8;
                        bytes.push(byte_val);
                        lexeme.push(self.advance());
                    }
                }
            }
        }

        if self.is_at_end() {
            return Err(LexerError::UnterminatedString {
                position: start_pos,
            });
        }

        self.advance(); // consume closing '
        lexeme.push('\'');

        Ok(Token::new(TokenType::ByteString(bytes), start_pos, lexeme))
    }

    fn metadata_annotation(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let name = self.identifier_chars();
        let lexeme = format!("@{name}");

        let token_type = match name.as_str() {
            "send-only" => TokenType::AtSendOnly,
            "receive-only" => TokenType::AtReceiveOnly,
            "bidirectional" => TokenType::AtBidirectional,
            "depends-on" => TokenType::AtDependsOn,
            "description" => TokenType::AtDescription,
            "min-length" => TokenType::AtMinLength,
            "max-length" => TokenType::AtMaxLength,
            "min-items" => TokenType::AtMinItems,
            "max-items" => TokenType::AtMaxItems,
            "min-value" => TokenType::AtMinValue,
            "max-value" => TokenType::AtMaxValue,
            _ => TokenType::AtCustom,
        };

        Ok(Token::new(token_type, start_pos, lexeme))
    }

    fn number_or_identifier(
        &mut self,
        first_char: char,
        start_pos: Position,
    ) -> Result<Token, LexerError> {
        let mut lexeme = String::from(first_char);

        if first_char.is_ascii_digit() || (first_char == '-' && self.peek().is_ascii_digit()) {
            // Collect all digits first
            while self.peek().is_ascii_digit() {
                lexeme.push(self.advance());
            }

            // Check if there are non-numeric characters following - if so, treat as identifier
            if self.peek().is_ascii_alphabetic() || self.peek() == '_' {
                while self.peek().is_ascii_alphanumeric()
                    || self.peek() == '_'
                    || self.peek() == '-'
                {
                    lexeme.push(self.advance());
                }
                return Ok(Token::new(
                    TokenType::Identifier(lexeme.clone()),
                    start_pos,
                    lexeme,
                ));
            }

            // Check for float
            if self.peek() == '.' && self.peek_next().is_ascii_digit() {
                lexeme.push(self.advance()); // consume .
                while self.peek().is_ascii_digit() {
                    lexeme.push(self.advance());
                }

                let value: f64 = lexeme.parse().map_err(|_| LexerError::InvalidNumber {
                    lexeme: lexeme.clone(),
                    position: start_pos,
                })?;
                Ok(Token::new(TokenType::Float(value), start_pos, lexeme))
            } else {
                let value: i64 = lexeme.parse().map_err(|_| LexerError::InvalidNumber {
                    lexeme: lexeme.clone(),
                    position: start_pos,
                })?;
                Ok(Token::new(TokenType::Integer(value), start_pos, lexeme))
            }
        } else {
            while self.peek().is_ascii_alphanumeric() || self.peek() == '_' || self.peek() == '-' {
                lexeme.push(self.advance());
            }
            Ok(Token::new(
                TokenType::Identifier(lexeme.clone()),
                start_pos,
                lexeme,
            ))
        }
    }

    fn identifier_or_keyword(
        &mut self,
        first_char: char,
        start_pos: Position,
    ) -> Result<Token, LexerError> {
        let mut lexeme = String::from(first_char);

        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' || self.peek() == '-' {
            lexeme.push(self.advance());
        }

        let token_type = match lexeme.as_str() {
            // CSIL keywords
            "service" => TokenType::Service,
            "options" => TokenType::Options,
            "include" => TokenType::Include,
            "from" => TokenType::From,
            "as" => TokenType::As,

            // CDDL built-in types
            "int" | "uint" | "nint" | "text" | "tstr" | "bytes" | "bstr" | "bool" | "true"
            | "false" | "null" | "undefined" | "float" | "float16" | "float32" | "float64"
            | "any" => TokenType::Builtin(lexeme.clone()),

            _ => TokenType::Identifier(lexeme.clone()),
        };

        Ok(Token::new(token_type, start_pos, lexeme))
    }

    fn identifier_chars(&mut self) -> String {
        let mut name = String::new();
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' || self.peek() == '-' {
            name.push(self.advance());
        }
        name
    }

    fn advance(&mut self) -> char {
        let ch = self.input[self.current];
        self.current += 1;
        self.column += 1;
        ch
    }

    fn peek(&self) -> char {
        if self.is_at_end() {
            '\0'
        } else {
            self.input[self.current]
        }
    }

    fn peek_next(&self) -> char {
        if self.current + 1 >= self.input.len() {
            '\0'
        } else {
            self.input[self.current + 1]
        }
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.input.len()
    }

    fn peek_identifier_start(&self) -> bool {
        let ch = self.peek();
        ch.is_alphabetic() || ch == '_'
    }

    fn read_identifier(&mut self) -> String {
        let mut ident = String::new();
        while !self.is_at_end()
            && (self.peek().is_alphanumeric() || self.peek() == '_' || self.peek() == '-')
        {
            ident.push(self.advance());
        }
        ident
    }
}

/// Lexer error types
#[derive(Debug, Clone, PartialEq)]
pub enum LexerError {
    UnexpectedCharacter { ch: char, position: Position },
    UnterminatedString { position: Position },
    InvalidNumber { lexeme: String, position: Position },
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexerError::UnexpectedCharacter { ch, position } => {
                write!(f, "Unexpected character '{ch}' at {position}")
            }
            LexerError::UnterminatedString { position } => {
                write!(f, "Unterminated string at {position}")
            }
            LexerError::InvalidNumber { lexeme, position } => {
                write!(f, "Invalid number '{lexeme}' at {position}")
            }
        }
    }
}

impl std::error::Error for LexerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_cddl_tokens() {
        let input = "name = text";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens.len(), 6); // identifier, whitespace, assign, whitespace, builtin, eof
        assert!(matches!(tokens[0].token_type, TokenType::Identifier(_)));
        assert!(matches!(tokens[2].token_type, TokenType::Assign));
        assert!(matches!(tokens[4].token_type, TokenType::Builtin(_)));
    }

    #[test]
    fn test_all_cddl_operators() {
        let input = "= //= /= => / .. ... ? * +";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let non_whitespace_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| !matches!(t.token_type, TokenType::Whitespace(_) | TokenType::Eof))
            .collect();

        assert!(matches!(
            non_whitespace_tokens[0].token_type,
            TokenType::Assign
        ));
        assert!(matches!(
            non_whitespace_tokens[1].token_type,
            TokenType::GroupChoice
        ));
        assert!(matches!(
            non_whitespace_tokens[2].token_type,
            TokenType::TypeChoice
        ));
        assert!(matches!(
            non_whitespace_tokens[3].token_type,
            TokenType::Arrow
        ));
        assert!(matches!(
            non_whitespace_tokens[4].token_type,
            TokenType::Choice
        ));
        assert!(matches!(
            non_whitespace_tokens[5].token_type,
            TokenType::Range
        ));
        assert!(matches!(
            non_whitespace_tokens[6].token_type,
            TokenType::RangeInclusive
        ));
        assert!(matches!(
            non_whitespace_tokens[7].token_type,
            TokenType::Optional
        ));
        assert!(matches!(
            non_whitespace_tokens[8].token_type,
            TokenType::ZeroOrMore
        ));
        assert!(matches!(
            non_whitespace_tokens[9].token_type,
            TokenType::OneOrMore
        ));
    }

    #[test]
    fn test_cddl_delimiters() {
        let input = "{ } [ ] ( )";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let delimiters: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type,
                    TokenType::LeftBrace
                        | TokenType::RightBrace
                        | TokenType::LeftBracket
                        | TokenType::RightBracket
                        | TokenType::LeftParen
                        | TokenType::RightParen
                )
            })
            .collect();

        assert_eq!(delimiters.len(), 6);
    }

    #[test]
    fn test_cddl_punctuation() {
        let input = ",:;."; // Test punctuation without spaces
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let punctuation: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type,
                    TokenType::Comma | TokenType::Colon | TokenType::Semicolon | TokenType::Dot
                )
            })
            .collect();

        assert_eq!(punctuation.len(), 4);
    }

    #[test]
    fn test_all_builtin_types() {
        let input = "int uint nint text tstr bytes bstr bool true false null undefined float float16 float32 float64 any";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let builtins: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Builtin(_)))
            .collect();

        assert_eq!(builtins.len(), 17);
    }

    #[test]
    fn test_service_definition() {
        let input = "service UserService {\n  create-user: Request -> Response\n}";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let service_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Service));
        let arrow_token = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::ServiceArrow));

        assert!(service_token.is_some());
        assert!(arrow_token.is_some());
    }

    #[test]
    fn test_bidirectional_service() {
        let input = "operation: Request <-> Response";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let bidirectional = tokens
            .iter()
            .find(|t| matches!(t.token_type, TokenType::ServiceBidirectional));
        assert!(bidirectional.is_some());
    }

    #[test]
    fn test_all_metadata_annotations() {
        let input = "@send-only @receive-only @bidirectional @depends-on @description @min-length @max-length @min-items @max-items";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let metadata_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type,
                    TokenType::AtSendOnly
                        | TokenType::AtReceiveOnly
                        | TokenType::AtBidirectional
                        | TokenType::AtDependsOn
                        | TokenType::AtDescription
                        | TokenType::AtMinLength
                        | TokenType::AtMaxLength
                        | TokenType::AtMinItems
                        | TokenType::AtMaxItems
                )
            })
            .collect();

        assert_eq!(metadata_tokens.len(), 9);
    }

    #[test]
    fn test_custom_metadata() {
        let input = "@custom-annotation @rust-specific @validation-rule";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let custom_annotations: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::AtCustom))
            .collect();

        assert_eq!(custom_annotations.len(), 3);
    }

    #[test]
    fn test_text_strings_with_escapes() {
        let input = r#""hello world" "with\nescapes" "quotes\"inside""#;
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.token_type {
                TokenType::TextString(s) => Some(s),
                _ => None,
            })
            .collect();

        assert_eq!(strings.len(), 3);
        assert_eq!(strings[0], "hello world");
        assert_eq!(strings[1], "with\nescapes");
        assert_eq!(strings[2], r#"quotes"inside"#);
    }

    #[test]
    fn test_byte_strings() {
        let input = "'deadbeef' 'abcd'";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let byte_strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.token_type {
                TokenType::ByteString(b) => Some(b),
                _ => None,
            })
            .collect();

        assert_eq!(byte_strings.len(), 2);
    }

    #[test]
    fn test_integer_literals() {
        let input = "0 42 -17 1000";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let integers: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.token_type {
                TokenType::Integer(i) => Some(*i),
                _ => None,
            })
            .collect();

        assert_eq!(integers, vec![0, 42, -17, 1000]);
    }

    #[test]
    fn test_float_literals() {
        let input = "2.71 -2.5 0.001";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let floats: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.token_type {
                TokenType::Float(f) => Some(*f),
                _ => None,
            })
            .collect();

        assert_eq!(floats.len(), 3);
        assert!((floats[0] - 2.71).abs() < f64::EPSILON);
        assert_eq!(floats[1], -2.5);
        assert_eq!(floats[2], 0.001);
    }

    #[test]
    fn test_socket_plug_references() {
        let input = "$my-socket $$my-plug $123 $$abc-def";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let sockets: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Socket))
            .collect();
        let plugs: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Plug))
            .collect();

        assert_eq!(sockets.len(), 2);
        assert_eq!(plugs.len(), 2);
    }

    #[test]
    fn test_comments_preservation() {
        let input = "; Main comment\nname = text ; inline comment\n; End comment";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let comments: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.token_type {
                TokenType::Comment(c) => Some(c),
                _ => None,
            })
            .collect();

        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0], " Main comment");
        assert_eq!(comments[1], " inline comment");
        assert_eq!(comments[2], " End comment");
    }

    #[test]
    fn test_position_tracking_multiline() {
        let input = "first\nsecond\nthird";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let identifiers: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Identifier(_)))
            .collect();

        assert_eq!(identifiers[0].position.line, 1);
        assert_eq!(identifiers[0].position.column, 1);

        assert_eq!(identifiers[1].position.line, 2);
        assert_eq!(identifiers[1].position.column, 1);

        assert_eq!(identifiers[2].position.line, 3);
        assert_eq!(identifiers[2].position.column, 1);
    }

    #[test]
    fn test_unicode_support() {
        let input = r#""Unicode: 🚀 한글 العربية""#;
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::TextString(s) = &tokens[0].token_type {
            assert!(s.contains("🚀"));
            assert!(s.contains("한글"));
            assert!(s.contains("العربية"));
        } else {
            panic!("Expected text string with Unicode content");
        }
    }

    #[test]
    fn test_error_unterminated_string() {
        let input = r#""unterminated string"#;
        let mut lexer = Lexer::new(input);
        let result = lexer.tokenize();

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LexerError::UnterminatedString { .. }
        ));
    }

    #[test]
    fn test_error_unexpected_character() {
        let input = "valid & invalid";
        let mut lexer = Lexer::new(input);
        let result = lexer.tokenize();

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LexerError::UnexpectedCharacter { ch: '&', .. }
        ));
    }

    #[test]
    fn test_error_invalid_number() {
        // This would be caught by parsing logic, but we can test malformed numbers
        let input = "123abc";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        // Should be treated as identifier, not invalid number
        assert!(matches!(tokens[0].token_type, TokenType::Identifier(_)));
    }

    #[test]
    fn test_whitespace_handling() {
        let input = "  \t  name \t = \t text  \t ";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let whitespace_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Whitespace(_)))
            .collect();

        assert_eq!(whitespace_tokens.len(), 4);
    }

    #[test]
    fn test_real_world_csil_example() {
        let input = r#"
; User management service
CreateUserRequest = {
  name: text,
  email: text,
  @send-only
  password: text,
}

UserProfile = {
  id: int,
  name: text,
  email: text,
  @receive-only
  created_at: text,
}

service UserService {
  create-user: CreateUserRequest -> UserProfile,
  get-user: { id: int } -> UserProfile,
}
"#;
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        // Verify we have service tokens
        let service_count = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Service))
            .count();
        assert_eq!(service_count, 1);

        // Verify metadata annotations
        let metadata_count = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type,
                    TokenType::AtSendOnly | TokenType::AtReceiveOnly
                )
            })
            .count();
        assert_eq!(metadata_count, 2);

        // Verify service arrows
        let arrow_count = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::ServiceArrow))
            .count();
        assert_eq!(arrow_count, 2);

        // Verify all tokens parse without errors
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.token_type, TokenType::Eof))
        );
    }

    #[test]
    fn test_cddl_control_operators() {
        let input = "text .size (3..50) .regex \"pattern\" bool .default true int .eq 42";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let control_ops: Vec<_> = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type,
                    TokenType::DotSize
                        | TokenType::DotRegex
                        | TokenType::DotDefault
                        | TokenType::DotEq
                )
            })
            .collect();

        assert_eq!(control_ops.len(), 4);

        // Check specific tokens
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.token_type, TokenType::DotSize))
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.token_type, TokenType::DotRegex))
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.token_type, TokenType::DotDefault))
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.token_type, TokenType::DotEq))
        );
    }

    #[test]
    fn test_cddl_comment_syntax() {
        let input = ";; This is a CDDL comment\n# This is also a comment";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        let comments: Vec<_> = tokens
            .iter()
            .filter(|t| matches!(t.token_type, TokenType::Comment(_)))
            .collect();

        assert_eq!(comments.len(), 2);
    }
}
