//! CSIL parser implementation

use crate::ast::*;
use crate::lexer::{Lexer, LexerError, Token, TokenType};
use anyhow::{Context, Result, bail};
use std::fmt;
use std::io::BufRead;

/// Parse CSIL interface definition from string
pub fn parse_csil(input: &str) -> Result<CsilSpec> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize().context("Failed to tokenize input")?;

    let mut parser = Parser::new(tokens, input);
    parser.parse()
}

/// Parse CSIL interface definition from file
pub fn parse_csil_file<P: AsRef<std::path::Path>>(path: P) -> Result<CsilSpec> {
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read file: {:?}", path.as_ref()))?;
    parse_csil(&content)
}

/// Parse CSIL interface definition from file with streaming for large files
pub fn parse_csil_file_streaming<P: AsRef<std::path::Path>>(path: P) -> Result<CsilSpec> {
    use std::fs::File;
    use std::io::BufReader;

    let file =
        File::open(&path).with_context(|| format!("Failed to open file: {:?}", path.as_ref()))?;

    let file_size = file
        .metadata()
        .with_context(|| format!("Failed to get file metadata: {:?}", path.as_ref()))?
        .len();

    // Use streaming parser for large files (>10MB)
    if file_size > 10 * 1024 * 1024 {
        parse_csil_streaming(BufReader::new(file))
    } else {
        // Use regular parsing for smaller files
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read file: {:?}", path.as_ref()))?;
        parse_csil(&content)
    }
}

/// Streaming parser for large CSIL files
pub fn parse_csil_streaming<R: BufRead>(mut reader: R) -> Result<CsilSpec> {
    // For streaming large files, read to string and parse (memory optimization can be enhanced later)
    let mut content = String::new();
    reader
        .read_to_string(&mut content)
        .context("Failed to read from input stream")?;

    parse_csil(&content)
}

/// Parser for CDDL/CSIL specifications
pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
    errors: Vec<ParseError>,
    _source_code: String,
}

impl Parser {
    pub fn new(tokens: Vec<Token>, source_code: &str) -> Self {
        Self {
            tokens,
            current: 0,
            errors: Vec::new(),
            _source_code: source_code.to_string(),
        }
    }

    pub fn parse(&mut self) -> Result<CsilSpec> {
        let mut imports = Vec::new();
        let mut options = None;
        let mut rules = Vec::new();

        self.skip_whitespace_and_comments();

        // Parse imports first
        while self.check(&TokenType::Include) || self.check(&TokenType::From) {
            match self.parse_import_statement() {
                Ok(import) => imports.push(import),
                Err(error) => {
                    self.errors.push(error);
                    self.synchronize();
                }
            }
            self.skip_whitespace_and_comments();
        }

        // Check for options block
        if self.check(&TokenType::Options) {
            match self.parse_options_block() {
                Ok(opts) => options = Some(opts),
                Err(error) => {
                    self.errors.push(error);
                    self.synchronize();
                }
            }
            self.skip_whitespace_and_comments();
        }

        // Parse remaining rules
        while !self.is_at_end() {
            self.skip_whitespace_and_comments();

            if self.is_at_end() {
                break;
            }

            match self.parse_rule() {
                Ok(rule) => rules.push(rule),
                Err(error) => {
                    self.errors.push(error);
                    if self.errors.len() > 10 {
                        break;
                    }
                    self.synchronize();
                }
            }
        }

        if !self.errors.is_empty() {
            let error_messages: Vec<String> = self.errors.iter().map(|e| e.to_string()).collect();
            bail!("Parse errors:\n{}", error_messages.join("\n"));
        }

        Ok(CsilSpec {
            imports,
            options,
            rules,
        })
    }

    fn parse_rule(&mut self) -> Result<Rule, ParseError> {
        // Check for service definitions first
        if self.check(&TokenType::Service) {
            return self.parse_service_rule();
        }

        let identifier_token = self.consume_identifier()?;
        let name = match &identifier_token.token_type {
            TokenType::Identifier(name) => name.clone(),
            _ => {
                return Err(ParseError::ExpectedIdentifier {
                    found: identifier_token.clone(),
                    context: "rule name".to_string(),
                });
            }
        };

        let position = identifier_token.position;

        self.skip_whitespace_and_comments();

        let rule_type = match self.peek().token_type {
            TokenType::Assign => {
                self.advance(); // consume =
                self.skip_whitespace_and_comments();

                let type_expr = self.parse_type_expression()?;
                RuleType::TypeDef(type_expr)
            }
            TokenType::TypeChoice => {
                self.advance(); // consume /=
                self.skip_whitespace_and_comments();

                let first_type = self.parse_type_expression()?;
                let mut types = vec![first_type];

                while self.match_token(&TokenType::Choice) {
                    self.skip_whitespace_and_comments();
                    types.push(self.parse_type_expression()?);
                }

                RuleType::TypeChoice(types)
            }
            TokenType::GroupChoice => {
                self.advance(); // consume //=
                self.skip_whitespace_and_comments();

                let first_group = self.parse_group_expression()?;
                let mut groups = vec![first_group];

                while self.match_token(&TokenType::Choice) {
                    self.skip_whitespace_and_comments();
                    groups.push(self.parse_group_expression()?);
                }

                RuleType::GroupChoice(groups)
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "assignment operator (=, /=, or //=)".to_string(),
                    found: self.peek().clone(),
                });
            }
        };

        Ok(Rule {
            name,
            rule_type,
            position,
        })
    }

    fn parse_type_expression(&mut self) -> Result<TypeExpression, ParseError> {
        let mut expr = self.parse_primary_type()?;

        // Handle choices (type1 / type2 / type3)
        if self.match_token(&TokenType::Choice) {
            let mut choices = vec![expr];

            loop {
                self.skip_whitespace_and_comments();
                choices.push(self.parse_primary_type()?);

                if !self.match_token(&TokenType::Choice) {
                    break;
                }
            }

            expr = TypeExpression::Choice(choices);
        }

        Ok(expr)
    }

    fn parse_primary_type(&mut self) -> Result<TypeExpression, ParseError> {
        let token = self.peek().clone();

        let base_type = match &token.token_type {
            TokenType::Builtin(name) => {
                let name = name.clone();
                self.advance();
                TypeExpression::Builtin(name)
            }
            TokenType::Identifier(name) => {
                let mut name = name.clone();
                self.advance();

                // Handle dotted identifiers (e.g., namespace.Type)
                while self.peek().token_type == TokenType::Dot {
                    self.advance(); // consume '.'
                    if let TokenType::Identifier(next_part) = &self.peek().token_type {
                        name.push('.');
                        name.push_str(next_part);
                        self.advance();
                    } else {
                        return Err(ParseError::ExpectedIdentifier {
                            found: self.peek().clone(),
                            context: "identifier after '.'".to_string(),
                        });
                    }
                }

                TypeExpression::Reference(name)
            }
            TokenType::LeftBrace => {
                // Need to peek ahead to see if this is a map or group
                if self.check_for_map_syntax() {
                    self.parse_map_type()?
                } else {
                    let group = self.parse_group_expression()?;
                    TypeExpression::Group(group)
                }
            }
            TokenType::LeftBracket => self.parse_array_type()?,
            TokenType::Socket => {
                let name = token
                    .lexeme
                    .strip_prefix('$')
                    .unwrap_or(&token.lexeme)
                    .to_string();
                self.advance();
                TypeExpression::Socket(name)
            }
            TokenType::Plug => {
                let name = token
                    .lexeme
                    .strip_prefix("$$")
                    .unwrap_or(&token.lexeme)
                    .to_string();
                self.advance();
                TypeExpression::Plug(name)
            }
            TokenType::Integer(value) => {
                let value = *value;
                self.advance();
                TypeExpression::Literal(LiteralValue::Integer(value))
            }
            TokenType::Float(value) => {
                let value = *value;
                self.advance();
                TypeExpression::Literal(LiteralValue::Float(value))
            }
            TokenType::TextString(value) => {
                let value = value.clone();
                self.advance();
                TypeExpression::Literal(LiteralValue::Text(value))
            }
            TokenType::ByteString(value) => {
                let value = value.clone();
                self.advance();
                TypeExpression::Literal(LiteralValue::Bytes(value))
            }
            TokenType::LeftParen => {
                self.advance(); // consume (
                self.skip_whitespace_and_comments();
                let expr = self.parse_type_expression()?;
                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::RightParen)?;
                return Ok(expr);
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "type expression".to_string(),
                    found: token.clone(),
                });
            }
        };

        // Skip whitespace and comments before looking for control operators
        self.skip_whitespace_and_comments();

        // Parse any control operators that follow the base type
        let constraints = self.parse_control_operators()?;

        if constraints.is_empty() {
            Ok(base_type)
        } else {
            Ok(TypeExpression::Constrained {
                base_type: Box::new(base_type),
                constraints,
            })
        }
    }

    fn parse_array_type(&mut self) -> Result<TypeExpression, ParseError> {
        self.consume_token(&TokenType::LeftBracket)?;
        self.skip_whitespace_and_comments();

        // Parse occurrence if present
        let occurrence = if self.match_occurrence_indicator() {
            Some(self.parse_occurrence()?)
        } else {
            None
        };

        self.skip_whitespace_and_comments();
        let element_type = Box::new(self.parse_type_expression()?);

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightBracket)?;

        Ok(TypeExpression::Array {
            element_type,
            occurrence,
        })
    }

    fn parse_map_type(&mut self) -> Result<TypeExpression, ParseError> {
        self.consume_token(&TokenType::LeftBrace)?;
        self.skip_whitespace_and_comments();

        // Parse occurrence indicator for the entire map
        let map_occurrence = if self.match_occurrence_indicator() {
            Some(self.parse_occurrence()?)
        } else {
            None
        };

        self.skip_whitespace_and_comments();

        // Parse key type
        let key_type = Box::new(self.parse_type_expression()?);

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::Arrow)?; // consume =>
        self.skip_whitespace_and_comments();

        // Parse value type
        let value_type = Box::new(self.parse_type_expression()?);

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightBrace)?;

        Ok(TypeExpression::Map {
            key: key_type,
            value: value_type,
            occurrence: map_occurrence,
        })
    }

    fn parse_group_expression(&mut self) -> Result<GroupExpression, ParseError> {
        self.consume_token(&TokenType::LeftBrace)?;
        self.skip_whitespace_and_comments();

        let mut entries = Vec::new();

        while !self.check(&TokenType::RightBrace) && !self.is_at_end() {
            let entry = self.parse_group_entry()?;
            entries.push(entry);

            self.skip_whitespace_and_comments();

            if self.match_token(&TokenType::Comma) {
                self.skip_whitespace_and_comments();
                continue;
            }

            if !self.check(&TokenType::RightBrace) {
                return Err(ParseError::ExpectedToken {
                    expected: "comma or }".to_string(),
                    found: self.peek().clone(),
                });
            }
        }

        self.consume_token(&TokenType::RightBrace)?;

        Ok(GroupExpression { entries })
    }

    fn check_for_map_syntax(&self) -> bool {
        // Look ahead for => pattern indicating a map
        let mut i = self.current;
        let mut paren_depth = 0;

        while i < self.tokens.len() {
            match &self.tokens[i].token_type {
                TokenType::Arrow => return true,
                TokenType::LeftParen => paren_depth += 1,
                TokenType::RightParen => paren_depth -= 1,
                TokenType::RightBrace if paren_depth == 0 => return false,
                TokenType::Comma if paren_depth == 0 => return false,
                _ => {}
            }
            i += 1;
        }

        false
    }

    fn parse_group_entry(&mut self) -> Result<GroupEntry, ParseError> {
        self.skip_whitespace_and_comments();

        // Parse metadata annotations first
        let metadata = self.parse_metadata_annotations()?;

        self.skip_whitespace_and_comments();

        // Check for optional field prefix (?)
        let mut is_optional = false;
        if self.match_token(&TokenType::Optional) {
            is_optional = true;
            self.skip_whitespace_and_comments();
        }

        // Try to parse key first
        let has_key = self.check_for_group_key();

        let (key, mut occurrence) = if has_key {
            let (k, occ) = self.parse_group_key()?;
            (Some(k), occ)
        } else {
            (None, None)
        };

        // If we found a ? prefix, set the occurrence to optional
        if is_optional {
            occurrence = Some(Occurrence::Optional);
        }

        self.skip_whitespace_and_comments();
        let value_type = self.parse_type_expression()?;

        // Parse postfix optional marker and inline annotations
        self.skip_whitespace_and_comments();

        // Check for postfix ? operator
        if self.match_token(&TokenType::Optional) {
            occurrence = Some(Occurrence::Optional);
            self.skip_whitespace_and_comments();
        }

        // Parse inline metadata annotations (after the type)
        let postfix_metadata = self.parse_metadata_annotations()?;

        // Merge prefix and postfix metadata
        let mut all_metadata = metadata;
        all_metadata.extend(postfix_metadata);

        Ok(GroupEntry {
            key,
            value_type,
            occurrence,
            metadata: all_metadata,
        })
    }

    fn parse_group_key(&mut self) -> Result<(GroupKey, Option<Occurrence>), ParseError> {
        let token = self.peek().clone();

        match &token.token_type {
            TokenType::Identifier(name) => {
                let name = name.clone();
                self.advance();
                self.skip_whitespace_and_comments();

                // Check for optional indicator
                let occurrence = if self.match_token(&TokenType::Optional) {
                    Some(Occurrence::Optional)
                } else {
                    None
                };

                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::Colon)?;
                Ok((GroupKey::Bare(name), occurrence))
            }
            TokenType::TextString(value) => {
                let value = value.clone();
                self.advance();
                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::Colon)?;
                Ok((GroupKey::Literal(LiteralValue::Text(value)), None))
            }
            TokenType::Integer(value) => {
                let value = *value;
                self.advance();
                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::Colon)?;
                Ok((GroupKey::Literal(LiteralValue::Integer(value)), None))
            }
            _ => {
                // Try parsing as a type expression
                let type_expr = self.parse_type_expression()?;
                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::Colon)?;
                Ok((GroupKey::Type(type_expr), None))
            }
        }
    }

    fn parse_occurrence(&mut self) -> Result<Occurrence, ParseError> {
        match &self.peek().token_type {
            TokenType::Optional => {
                self.advance();
                Ok(Occurrence::Optional)
            }
            TokenType::ZeroOrMore => {
                self.advance();
                Ok(Occurrence::ZeroOrMore)
            }
            TokenType::OneOrMore => {
                self.advance();
                Ok(Occurrence::OneOrMore)
            }
            TokenType::Integer(count) => {
                let count = *count as u64;
                self.advance();

                // Check for range (5*10)
                if self.match_token(&TokenType::ZeroOrMore) {
                    if let TokenType::Integer(max) = &self.peek().token_type {
                        let max = *max as u64;
                        self.advance();
                        Ok(Occurrence::Range {
                            min: Some(count),
                            max: Some(max),
                        })
                    } else {
                        Ok(Occurrence::Range {
                            min: Some(count),
                            max: None,
                        })
                    }
                } else {
                    Ok(Occurrence::Exact(count))
                }
            }
            _ => Err(ParseError::ExpectedToken {
                expected: "occurrence indicator".to_string(),
                found: self.peek().clone(),
            }),
        }
    }

    fn check_for_group_key(&self) -> bool {
        let mut i = self.current;

        // Skip the potential key token (identifier, string, integer, or bracketed type)
        if i < self.tokens.len() {
            match &self.tokens[i].token_type {
                TokenType::Identifier(_) | TokenType::TextString(_) | TokenType::Integer(_) => {
                    i += 1;
                }
                TokenType::LeftBracket => {
                    // Skip bracketed type expression: [type_expression]
                    i += 1;
                    let mut bracket_depth = 1;
                    while i < self.tokens.len() && bracket_depth > 0 {
                        match &self.tokens[i].token_type {
                            TokenType::LeftBracket => bracket_depth += 1,
                            TokenType::RightBracket => bracket_depth -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    if bracket_depth != 0 {
                        return false; // Unmatched brackets
                    }
                }
                _ => return false,
            }
        } else {
            return false;
        }

        // Skip whitespace
        while i < self.tokens.len() {
            match &self.tokens[i].token_type {
                TokenType::Whitespace(_) | TokenType::Newline => i += 1,
                TokenType::Optional => i += 1, // Skip optional indicator
                TokenType::Colon => return true,
                TokenType::Arrow => return true, // => is also a key indicator for maps
                _ => return false,
            }
        }

        false
    }

    fn match_occurrence_indicator(&self) -> bool {
        matches!(
            self.peek().token_type,
            TokenType::Optional
                | TokenType::ZeroOrMore
                | TokenType::OneOrMore
                | TokenType::Integer(_)
        )
    }

    fn skip_whitespace_and_comments(&mut self) {
        while matches!(
            self.peek().token_type,
            TokenType::Whitespace(_) | TokenType::Newline | TokenType::Comment(_)
        ) {
            self.advance();
        }
    }

    fn synchronize(&mut self) {
        self.advance();

        while !self.is_at_end() {
            match self.peek().token_type {
                TokenType::Newline => {
                    self.advance();
                    return;
                }
                TokenType::Identifier(_) => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn consume_identifier(&mut self) -> Result<Token, ParseError> {
        if matches!(self.peek().token_type, TokenType::Identifier(_)) {
            Ok(self.advance().clone())
        } else {
            Err(ParseError::ExpectedIdentifier {
                found: self.peek().clone(),
                context: "identifier".to_string(),
            })
        }
    }

    fn consume_token(&mut self, expected: &TokenType) -> Result<Token, ParseError> {
        if std::mem::discriminant(&self.peek().token_type) == std::mem::discriminant(expected) {
            Ok(self.advance().clone())
        } else {
            Err(ParseError::ExpectedToken {
                expected: format!("{expected:?}"),
                found: self.peek().clone(),
            })
        }
    }

    fn match_token(&mut self, token_type: &TokenType) -> bool {
        if std::mem::discriminant(&self.peek().token_type) == std::mem::discriminant(token_type) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, token_type: &TokenType) -> bool {
        std::mem::discriminant(&self.peek().token_type) == std::mem::discriminant(token_type)
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    fn is_at_end(&self) -> bool {
        matches!(self.peek().token_type, TokenType::Eof)
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.current - 1]
    }

    fn parse_control_operators(&mut self) -> Result<Vec<ControlOperator>, ParseError> {
        let mut constraints = Vec::new();

        while matches!(
            self.peek().token_type,
            TokenType::DotSize | TokenType::DotRegex | TokenType::DotDefault |
            TokenType::DotGe | TokenType::DotLe | TokenType::DotGt | TokenType::DotLt | TokenType::DotEq |
            // Unsupported tokens (will produce helpful error messages)
            TokenType::DotNe | TokenType::DotBits | TokenType::DotAnd | TokenType::DotWithin |
            TokenType::DotJson | TokenType::DotCbor | TokenType::DotCborseq
        ) {
            match self.peek().token_type {
                TokenType::DotSize => {
                    self.advance(); // consume .size
                    let size_constraint = self.parse_size_constraint()?;
                    constraints.push(ControlOperator::Size(size_constraint));
                }
                TokenType::DotRegex => {
                    self.advance(); // consume .regex
                    self.skip_whitespace_and_comments();

                    if let TokenType::TextString(pattern) = &self.peek().token_type {
                        let pattern = pattern.clone();
                        self.advance();
                        constraints.push(ControlOperator::Regex(pattern));
                    } else {
                        return Err(ParseError::ExpectedToken {
                            expected: "string literal for regex pattern".to_string(),
                            found: self.peek().clone(),
                        });
                    }
                }
                TokenType::DotDefault => {
                    self.advance(); // consume .default
                    self.skip_whitespace_and_comments();

                    let default_value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::Default(default_value));
                }
                TokenType::DotGe => {
                    self.advance(); // consume .ge
                    self.skip_whitespace_and_comments();

                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::GreaterEqual(value));
                }
                TokenType::DotLe => {
                    self.advance(); // consume .le
                    self.skip_whitespace_and_comments();

                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::LessEqual(value));
                }
                TokenType::DotGt => {
                    self.advance(); // consume .gt
                    self.skip_whitespace_and_comments();

                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::GreaterThan(value));
                }
                TokenType::DotLt => {
                    self.advance(); // consume .lt
                    self.skip_whitespace_and_comments();

                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::LessThan(value));
                }
                TokenType::DotEq => {
                    self.advance(); // consume .eq
                    self.skip_whitespace_and_comments();

                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::Equal(value));
                }
                // Unsupported CDDL control operators with helpful error messages
                TokenType::DotNe => {
                    self.advance(); // consume .ne
                    self.skip_whitespace_and_comments();
                    let value = self.parse_literal_value()?;
                    constraints.push(ControlOperator::NotEqual(value));
                }
                TokenType::DotBits => {
                    self.advance(); // consume .bits
                    self.skip_whitespace_and_comments();
                    // .bits expects a bit expression (for now, we'll treat it as a string)
                    let bits_expr = if let TokenType::TextString(s) = &self.peek().token_type {
                        let expr = s.clone();
                        self.advance();
                        expr
                    } else if let TokenType::Identifier(s) = &self.peek().token_type {
                        let expr = s.clone();
                        self.advance();
                        expr
                    } else {
                        return Err(ParseError::ExpectedToken {
                            expected: "bits expression".to_string(),
                            found: self.peek().clone(),
                        });
                    };
                    constraints.push(ControlOperator::Bits(bits_expr));
                }
                TokenType::DotAnd => {
                    self.advance(); // consume .and
                    self.skip_whitespace_and_comments();
                    // .and expects a type expression
                    let type_expr = self.parse_type_expression()?;
                    constraints.push(ControlOperator::And(Box::new(type_expr)));
                }
                TokenType::DotWithin => {
                    self.advance(); // consume .within
                    self.skip_whitespace_and_comments();
                    // .within expects a type expression
                    let type_expr = self.parse_type_expression()?;
                    constraints.push(ControlOperator::Within(Box::new(type_expr)));
                }
                TokenType::DotJson => {
                    self.advance(); // consume .json
                    constraints.push(ControlOperator::Json);
                }
                TokenType::DotCbor => {
                    self.advance(); // consume .cbor
                    constraints.push(ControlOperator::Cbor);
                }
                TokenType::DotCborseq => {
                    self.advance(); // consume .cborseq
                    constraints.push(ControlOperator::Cborseq);
                }
                _ => break,
            }
            self.skip_whitespace_and_comments();
        }

        Ok(constraints)
    }

    fn parse_size_constraint(&mut self) -> Result<SizeConstraint, ParseError> {
        self.skip_whitespace_and_comments();

        match &self.peek().token_type {
            TokenType::Integer(value) => {
                let value = *value as u64;
                self.advance();
                Ok(SizeConstraint::Exact(value))
            }
            TokenType::LeftParen => {
                self.advance(); // consume (
                self.skip_whitespace_and_comments();

                let constraint = if let TokenType::Integer(min) = &self.peek().token_type {
                    let min = *min as u64;
                    self.advance();
                    self.skip_whitespace_and_comments();

                    if self.match_token(&TokenType::Range) {
                        self.skip_whitespace_and_comments();
                        if let TokenType::Integer(max) = &self.peek().token_type {
                            let max = *max as u64;
                            self.advance();
                            SizeConstraint::Range { min, max }
                        } else {
                            SizeConstraint::Min(min)
                        }
                    } else {
                        SizeConstraint::Exact(min)
                    }
                } else if self.match_token(&TokenType::Range) {
                    self.skip_whitespace_and_comments();
                    if let TokenType::Integer(max) = &self.peek().token_type {
                        let max = *max as u64;
                        self.advance();
                        SizeConstraint::Max(max)
                    } else {
                        return Err(ParseError::ExpectedToken {
                            expected: "integer for range maximum".to_string(),
                            found: self.peek().clone(),
                        });
                    }
                } else {
                    return Err(ParseError::ExpectedToken {
                        expected: "integer or range for size constraint".to_string(),
                        found: self.peek().clone(),
                    });
                };

                self.skip_whitespace_and_comments();
                self.consume_token(&TokenType::RightParen)?;
                Ok(constraint)
            }
            _ => Err(ParseError::ExpectedToken {
                expected: "integer or parenthesized range for size constraint".to_string(),
                found: self.peek().clone(),
            }),
        }
    }

    fn parse_service_rule(&mut self) -> Result<Rule, ParseError> {
        let service_token = self.consume_token(&TokenType::Service)?;
        let position = service_token.position;

        self.skip_whitespace_and_comments();

        let name_token = self.consume_identifier()?;
        let name = match &name_token.token_type {
            TokenType::Identifier(name) => name.clone(),
            _ => {
                return Err(ParseError::ExpectedIdentifier {
                    found: name_token.clone(),
                    context: "service name".to_string(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::LeftBrace)?;
        self.skip_whitespace_and_comments();

        let mut operations = Vec::new();

        while !self.check(&TokenType::RightBrace) && !self.is_at_end() {
            let operation = self.parse_service_operation()?;
            operations.push(operation);

            self.skip_whitespace_and_comments();

            if self.match_token(&TokenType::Comma) {
                self.skip_whitespace_and_comments();
                continue;
            }

            if !self.check(&TokenType::RightBrace) {
                return Err(ParseError::ExpectedToken {
                    expected: "comma or }".to_string(),
                    found: self.peek().clone(),
                });
            }
        }

        self.consume_token(&TokenType::RightBrace)?;

        let service_def = ServiceDefinition { operations };
        let rule_type = RuleType::ServiceDef(service_def);

        Ok(Rule {
            name,
            rule_type,
            position,
        })
    }

    fn parse_service_operation(&mut self) -> Result<ServiceOperation, ParseError> {
        let name_token = self.consume_identifier()?;
        let name = match &name_token.token_type {
            TokenType::Identifier(name) => name.clone(),
            _ => {
                return Err(ParseError::ExpectedIdentifier {
                    found: name_token.clone(),
                    context: "operation name".to_string(),
                });
            }
        };

        let position = name_token.position;

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::Colon)?;
        self.skip_whitespace_and_comments();

        let input_type = self.parse_type_expression()?;

        self.skip_whitespace_and_comments();

        let direction = match self.peek().token_type {
            TokenType::ServiceArrow => {
                self.advance();
                ServiceDirection::Unidirectional
            }
            TokenType::ServiceBackArrow => {
                self.advance();
                ServiceDirection::Reverse
            }
            TokenType::ServiceBidirectional => {
                self.advance();
                ServiceDirection::Bidirectional
            }
            _ => {
                // Check if user might have used regular arrow (=>) instead
                if self.peek().token_type == TokenType::Arrow {
                    return Err(ParseError::ServiceDefinitionError {
                        message:
                            "Service operations use '->' not '=>'. The '=>' arrow is for map types"
                                .to_string(),
                        token: self.peek().clone(),
                    });
                }

                return Err(ParseError::ServiceDefinitionError {
                    message: "Service operations require a direction arrow: -> (unidirectional), <- (reverse), or <-> (bidirectional)".to_string(),
                    token: self.peek().clone(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        let output_type = self.parse_type_expression()?;

        Ok(ServiceOperation {
            name,
            input_type,
            output_type,
            direction,
            position,
        })
    }

    fn parse_metadata_annotations(&mut self) -> Result<Vec<FieldMetadata>, ParseError> {
        let mut metadata = Vec::new();

        while self.is_metadata_token() {
            let annotation = self.parse_single_metadata_annotation()?;
            metadata.push(annotation);
            self.skip_whitespace_and_comments();
        }

        Ok(metadata)
    }

    fn is_metadata_token(&self) -> bool {
        matches!(
            self.peek().token_type,
            TokenType::AtSendOnly
                | TokenType::AtReceiveOnly
                | TokenType::AtBidirectional
                | TokenType::AtDependsOn
                | TokenType::AtDescription
                | TokenType::AtMinLength
                | TokenType::AtMaxLength
                | TokenType::AtMinItems
                | TokenType::AtMaxItems
                | TokenType::AtMinValue
                | TokenType::AtMaxValue
                | TokenType::AtCustom
        )
    }

    fn parse_single_metadata_annotation(&mut self) -> Result<FieldMetadata, ParseError> {
        let token = self.peek().clone();

        match &token.token_type {
            TokenType::AtSendOnly => {
                self.advance();
                Ok(FieldMetadata::Visibility(FieldVisibility::SendOnly))
            }
            TokenType::AtReceiveOnly => {
                self.advance();
                Ok(FieldMetadata::Visibility(FieldVisibility::ReceiveOnly))
            }
            TokenType::AtBidirectional => {
                self.advance();
                Ok(FieldMetadata::Visibility(FieldVisibility::Bidirectional))
            }
            TokenType::AtDependsOn => {
                self.advance();
                self.parse_depends_on_annotation()
            }
            TokenType::AtDescription => {
                self.advance();
                self.parse_description_annotation()
            }
            TokenType::AtMinLength => {
                self.advance();
                self.parse_constraint_annotation(ValidationConstraint::MinLength)
            }
            TokenType::AtMaxLength => {
                self.advance();
                self.parse_constraint_annotation(ValidationConstraint::MaxLength)
            }
            TokenType::AtMinItems => {
                self.advance();
                self.parse_constraint_annotation(ValidationConstraint::MinItems)
            }
            TokenType::AtMaxItems => {
                self.advance();
                self.parse_constraint_annotation(ValidationConstraint::MaxItems)
            }
            TokenType::AtMinValue => {
                self.advance();
                self.parse_value_constraint_annotation(ValidationConstraint::MinValue)
            }
            TokenType::AtMaxValue => {
                self.advance();
                self.parse_value_constraint_annotation(ValidationConstraint::MaxValue)
            }
            TokenType::AtCustom => {
                self.advance();
                self.parse_custom_annotation(&token.lexeme)
            }
            _ => Err(ParseError::ExpectedToken {
                expected: "metadata annotation".to_string(),
                found: token,
            }),
        }
    }

    fn parse_depends_on_annotation(&mut self) -> Result<FieldMetadata, ParseError> {
        self.skip_whitespace_and_comments();

        if !self.match_token(&TokenType::LeftParen) {
            return Err(ParseError::MetadataError {
                message: "@depends-on requires parentheses with field name: @depends-on(field_name) or @depends-on(field_name = value)".to_string(),
                token: self.peek().clone(),
            });
        }

        self.skip_whitespace_and_comments();

        let field_token = self.consume_identifier()?;
        let mut field = match &field_token.token_type {
            TokenType::Identifier(name) => name.clone(),
            _ => {
                return Err(ParseError::MetadataError {
                    message: "@depends-on requires a field name as first parameter: @depends-on(field_name)".to_string(),
                    token: field_token,
                });
            }
        };

        // Handle dotted field paths like permissions.can_read
        while self.match_token(&TokenType::Dot) {
            self.skip_whitespace_and_comments();
            let next_token = self.consume_identifier()?;
            match &next_token.token_type {
                TokenType::Identifier(name) => {
                    field.push('.');
                    field.push_str(name);
                }
                _ => {
                    return Err(ParseError::MetadataError {
                        message: "Expected identifier after '.' in field path".to_string(),
                        token: next_token,
                    });
                }
            }
        }

        self.skip_whitespace_and_comments();

        let value = if self.match_token(&TokenType::Assign) {
            self.skip_whitespace_and_comments();
            Some(self.parse_literal_value()?)
        } else {
            None
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightParen)?;

        Ok(FieldMetadata::DependsOn { field, value })
    }

    fn parse_description_annotation(&mut self) -> Result<FieldMetadata, ParseError> {
        self.skip_whitespace_and_comments();

        if !self.match_token(&TokenType::LeftParen) {
            return Err(ParseError::ExpectedToken {
                expected: "(".to_string(),
                found: self.peek().clone(),
            });
        }

        self.skip_whitespace_and_comments();

        let description = match &self.peek().token_type {
            TokenType::TextString(desc) => {
                let desc = desc.clone();
                self.advance();
                desc
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "string literal".to_string(),
                    found: self.peek().clone(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightParen)?;

        Ok(FieldMetadata::Description(description))
    }

    fn parse_constraint_annotation<F>(
        &mut self,
        constraint_constructor: F,
    ) -> Result<FieldMetadata, ParseError>
    where
        F: Fn(u64) -> ValidationConstraint,
    {
        self.skip_whitespace_and_comments();

        if !self.match_token(&TokenType::LeftParen) {
            return Err(ParseError::ExpectedToken {
                expected: "(".to_string(),
                found: self.peek().clone(),
            });
        }

        self.skip_whitespace_and_comments();

        let value = match &self.peek().token_type {
            TokenType::Integer(val) if *val >= 0 => {
                let val = *val as u64;
                self.advance();
                val
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "positive integer".to_string(),
                    found: self.peek().clone(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightParen)?;

        Ok(FieldMetadata::Constraint(constraint_constructor(value)))
    }

    fn parse_value_constraint_annotation<F>(
        &mut self,
        constraint_constructor: F,
    ) -> Result<FieldMetadata, ParseError>
    where
        F: Fn(LiteralValue) -> ValidationConstraint,
    {
        self.skip_whitespace_and_comments();

        if !self.match_token(&TokenType::LeftParen) {
            return Err(ParseError::ExpectedToken {
                expected: "(".to_string(),
                found: self.peek().clone(),
            });
        }

        self.skip_whitespace_and_comments();

        let value = self.parse_literal_value()?;

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::RightParen)?;

        Ok(FieldMetadata::Constraint(constraint_constructor(value)))
    }

    fn parse_custom_annotation(&mut self, lexeme: &str) -> Result<FieldMetadata, ParseError> {
        let name = lexeme.strip_prefix('@').unwrap_or(lexeme).to_string();

        let parameters = if self.check(&TokenType::LeftParen) {
            self.parse_metadata_parameters()?
        } else {
            Vec::new()
        };

        Ok(FieldMetadata::Custom { name, parameters })
    }

    fn parse_metadata_parameters(&mut self) -> Result<Vec<MetadataParameter>, ParseError> {
        self.consume_token(&TokenType::LeftParen)?;
        self.skip_whitespace_and_comments();

        let mut parameters = Vec::new();

        while !self.check(&TokenType::RightParen) && !self.is_at_end() {
            let parameter = self.parse_metadata_parameter()?;
            parameters.push(parameter);

            self.skip_whitespace_and_comments();

            if self.match_token(&TokenType::Comma) {
                self.skip_whitespace_and_comments();
                continue;
            }

            if !self.check(&TokenType::RightParen) {
                return Err(ParseError::ExpectedToken {
                    expected: "comma or )".to_string(),
                    found: self.peek().clone(),
                });
            }
        }

        self.consume_token(&TokenType::RightParen)?;
        Ok(parameters)
    }

    fn parse_metadata_parameter(&mut self) -> Result<MetadataParameter, ParseError> {
        // Check if this is a named parameter (name = value)
        if self.check_for_named_parameter() {
            let name_token = self.consume_identifier()?;
            let name = match &name_token.token_type {
                TokenType::Identifier(n) => n.clone(),
                _ => {
                    return Err(ParseError::ExpectedIdentifier {
                        found: name_token,
                        context: "parameter name".to_string(),
                    });
                }
            };

            self.skip_whitespace_and_comments();
            self.consume_token(&TokenType::Assign)?;
            self.skip_whitespace_and_comments();

            let value = self.parse_literal_value()?;

            Ok(MetadataParameter {
                name: Some(name),
                value,
            })
        } else {
            let value = self.parse_literal_value()?;
            Ok(MetadataParameter { name: None, value })
        }
    }

    fn check_for_named_parameter(&self) -> bool {
        let mut i = self.current;

        // Skip identifier
        if i < self.tokens.len() && matches!(self.tokens[i].token_type, TokenType::Identifier(_)) {
            i += 1;
        } else {
            return false;
        }

        // Skip whitespace
        while i < self.tokens.len() {
            match &self.tokens[i].token_type {
                TokenType::Whitespace(_) | TokenType::Newline => i += 1,
                TokenType::Assign => return true,
                _ => return false,
            }
        }

        false
    }

    fn parse_import_statement(&mut self) -> Result<ImportStatement, ParseError> {
        if self.match_token(&TokenType::Include) {
            self.parse_include_statement()
        } else if self.match_token(&TokenType::From) {
            self.parse_from_statement()
        } else {
            Err(ParseError::ExpectedToken {
                expected: "include or from".to_string(),
                found: self.peek().clone(),
            })
        }
    }

    fn parse_include_statement(&mut self) -> Result<ImportStatement, ParseError> {
        let start_pos = self.previous().position;
        self.skip_whitespace_and_comments();

        // Parse file path string
        let path = match &self.peek().token_type {
            TokenType::TextString(path) => {
                let path = path.clone();
                self.advance();
                path
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "file path string".to_string(),
                    found: self.peek().clone(),
                });
            }
        };

        self.skip_whitespace_and_comments();

        // Check for optional "as alias"
        let alias = if self.match_token(&TokenType::As) {
            self.skip_whitespace_and_comments();
            let alias_token = self.consume_identifier()?;
            match &alias_token.token_type {
                TokenType::Identifier(name) => Some(name.clone()),
                _ => {
                    return Err(ParseError::ExpectedIdentifier {
                        found: alias_token,
                        context: "import alias".to_string(),
                    });
                }
            }
        } else {
            None
        };

        Ok(ImportStatement::Include {
            path,
            alias,
            position: start_pos,
        })
    }

    fn parse_from_statement(&mut self) -> Result<ImportStatement, ParseError> {
        let start_pos = self.previous().position;
        self.skip_whitespace_and_comments();

        // Parse file path
        let path = match &self.peek().token_type {
            TokenType::TextString(path) => {
                let path = path.clone();
                self.advance();
                path
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "file path string".to_string(),
                    found: self.peek().clone(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::Include)?; // "include" keyword
        self.skip_whitespace_and_comments();

        // Parse comma-separated list of items
        let mut items = Vec::new();
        loop {
            let item_token = self.consume_identifier()?;
            match &item_token.token_type {
                TokenType::Identifier(name) => items.push(name.clone()),
                _ => {
                    return Err(ParseError::ExpectedIdentifier {
                        found: item_token,
                        context: "import item".to_string(),
                    });
                }
            }

            self.skip_whitespace_and_comments();
            if !self.match_token(&TokenType::Comma) {
                break;
            }
            self.skip_whitespace_and_comments();
        }

        Ok(ImportStatement::SelectiveImport {
            path,
            items,
            position: start_pos,
        })
    }

    fn parse_options_block(&mut self) -> Result<FileOptions, ParseError> {
        let options_token = self.consume_token(&TokenType::Options)?;
        let position = options_token.position;

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::LeftBrace)?;

        let mut entries = Vec::new();

        while !self.check(&TokenType::RightBrace) && !self.is_at_end() {
            self.skip_whitespace_and_comments();

            if self.check(&TokenType::RightBrace) {
                break;
            }

            entries.push(self.parse_option_entry()?);

            self.skip_whitespace_and_comments();
            if self.match_token(&TokenType::Comma) {
                self.skip_whitespace_and_comments();
                continue;
            }
            break;
        }

        self.consume_token(&TokenType::RightBrace)?;

        Ok(FileOptions { entries, position })
    }

    fn parse_option_entry(&mut self) -> Result<OptionEntry, ParseError> {
        let key_token = self.consume_identifier()?;
        let key = match &key_token.token_type {
            TokenType::Identifier(name) => name.clone(),
            _ => {
                return Err(ParseError::ExpectedIdentifier {
                    found: key_token,
                    context: "option key".to_string(),
                });
            }
        };

        self.skip_whitespace_and_comments();
        self.consume_token(&TokenType::Colon)?;
        self.skip_whitespace_and_comments();

        let value = self.parse_literal_value()?;

        Ok(OptionEntry {
            key,
            value,
            position: key_token.position,
        })
    }

    fn parse_literal_value(&mut self) -> Result<LiteralValue, ParseError> {
        let token = self.peek().clone();

        match &token.token_type {
            TokenType::Integer(value) => {
                let value = *value;
                self.advance();
                Ok(LiteralValue::Integer(value))
            }
            TokenType::Float(value) => {
                let value = *value;
                self.advance();
                Ok(LiteralValue::Float(value))
            }
            TokenType::TextString(value) => {
                let value = value.clone();
                self.advance();
                Ok(LiteralValue::Text(value))
            }
            TokenType::ByteString(value) => {
                let value = value.clone();
                self.advance();
                Ok(LiteralValue::Bytes(value))
            }
            TokenType::Builtin(name) if name == "true" => {
                self.advance();
                Ok(LiteralValue::Bool(true))
            }
            TokenType::Builtin(name) if name == "false" => {
                self.advance();
                Ok(LiteralValue::Bool(false))
            }
            TokenType::Builtin(name) if name == "null" => {
                self.advance();
                Ok(LiteralValue::Null)
            }
            TokenType::LeftBracket => {
                self.advance(); // consume '['
                self.skip_whitespace_and_comments();

                let mut elements = Vec::new();

                while !self.check(&TokenType::RightBracket) && !self.is_at_end() {
                    self.skip_whitespace_and_comments();

                    if self.check(&TokenType::RightBracket) {
                        break;
                    }

                    elements.push(self.parse_literal_value()?);

                    self.skip_whitespace_and_comments();
                    if self.match_token(&TokenType::Comma) {
                        self.skip_whitespace_and_comments();
                        continue;
                    }
                    break;
                }

                self.consume_token(&TokenType::RightBracket)?;
                Ok(LiteralValue::Array(elements))
            }
            _ => Err(ParseError::ExpectedToken {
                expected: "literal value".to_string(),
                found: token,
            }),
        }
    }
}

/// Parser error types
#[derive(Debug, Clone)]
pub enum ParseError {
    ExpectedToken {
        expected: String,
        found: Token,
    },
    ExpectedIdentifier {
        found: Token,
        context: String,
    },
    UnexpectedToken {
        token: Token,
    },
    LexerError(LexerError),
    ServiceDefinitionError {
        message: String,
        token: Token,
    },
    MetadataError {
        message: String,
        token: Token,
    },
    UnsupportedFeature {
        feature: String,
        token: Token,
        suggestion: Option<Box<str>>,
        coming_soon: bool,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::ExpectedToken { expected, found } => {
                write!(
                    f,
                    "Expected {} but found '{}' at {}",
                    expected, found.lexeme, found.position
                )
            }
            ParseError::ExpectedIdentifier { found, context } => {
                write!(
                    f,
                    "Expected identifier for {} but found '{}' at {}",
                    context, found.lexeme, found.position
                )
            }
            ParseError::UnexpectedToken { token } => {
                write!(
                    f,
                    "Unexpected token '{}' at {}",
                    token.lexeme, token.position
                )
            }
            ParseError::ServiceDefinitionError { message, token } => {
                write!(
                    f,
                    "Service definition error: {} at {}",
                    message, token.position
                )
            }
            ParseError::MetadataError { message, token } => {
                write!(f, "Field metadata error: {} at {}", message, token.position)
            }
            ParseError::UnsupportedFeature {
                feature,
                token,
                suggestion,
                coming_soon,
            } => {
                let status = if *coming_soon {
                    "planned feature"
                } else {
                    "unsupported feature"
                };
                if let Some(suggestion) = suggestion {
                    write!(
                        f,
                        "Unsupported CDDL syntax: {} ({}) at {}. {}",
                        feature, status, token.position, suggestion
                    )
                } else {
                    write!(
                        f,
                        "Unsupported CDDL syntax: {} ({}) at {}",
                        feature, status, token.position
                    )
                }
            }
            ParseError::LexerError(err) => write!(f, "Lexer error: {err}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<LexerError> for ParseError {
    fn from(err: LexerError) -> Self {
        ParseError::LexerError(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_type_definition() {
        let input = "name = text";
        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.rules.len(), 1);
        let rule = &spec.rules[0];
        assert_eq!(rule.name, "name");

        match &rule.rule_type {
            RuleType::TypeDef(TypeExpression::Builtin(type_name)) => {
                assert_eq!(type_name, "text");
            }
            _ => panic!("Expected builtin type definition"),
        }
    }

    #[test]
    fn test_parse_multiple_rules() {
        let input = r#"
        name = text
        age = int
        "#;
        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.rules.len(), 2);
        assert_eq!(spec.rules[0].name, "name");
        assert_eq!(spec.rules[1].name, "age");
    }

    #[test]
    fn test_parse_simple_array() {
        let input = "names = [text]";
        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.rules.len(), 1);
        let rule = &spec.rules[0];

        match &rule.rule_type {
            RuleType::TypeDef(TypeExpression::Array { element_type, .. }) => {
                match element_type.as_ref() {
                    TypeExpression::Builtin(type_name) => {
                        assert_eq!(type_name, "text");
                    }
                    _ => panic!("Expected builtin element type"),
                }
            }
            _ => panic!("Expected array type definition"),
        }
    }

    #[test]
    fn test_parse_comments() {
        let input = r#"
        ; This is a comment
        name = text ; inline comment
        "#;
        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.rules.len(), 1);
        assert_eq!(spec.rules[0].name, "name");
    }

    #[test]
    fn test_parse_simple_group() {
        let input = "person = { name: text }";

        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];
                assert_eq!(rule.name, "person");

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 1);
                    }
                    _ => panic!("Expected group type definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_group_definition() {
        let input = r#"
        person = {
          name: text,
          age: int,
          email?: text,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];
                assert_eq!(rule.name, "person");

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 3);
                    }
                    _ => panic!("Expected group type definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_simple_service() {
        let input = r#"
        service UserService {
          create-user: CreateUserRequest -> UserProfile
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];
                assert_eq!(rule.name, "UserService");

                match &rule.rule_type {
                    RuleType::ServiceDef(service) => {
                        assert_eq!(service.operations.len(), 1);
                        let op = &service.operations[0];
                        assert_eq!(op.name, "create-user");
                        assert!(matches!(op.direction, ServiceDirection::Unidirectional));

                        match (&op.input_type, &op.output_type) {
                            (
                                TypeExpression::Reference(input),
                                TypeExpression::Reference(output),
                            ) => {
                                assert_eq!(input, "CreateUserRequest");
                                assert_eq!(output, "UserProfile");
                            }
                            _ => panic!("Expected reference types for input and output"),
                        }
                    }
                    _ => panic!("Expected service definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_service_multiple_operations() {
        let input = r#"
        service UserService {
          create-user: CreateUserRequest -> UserProfile,
          get-user: { id: int } -> UserProfile,
          delete-user: { id: int } -> { success: bool }
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];
                assert_eq!(rule.name, "UserService");

                match &rule.rule_type {
                    RuleType::ServiceDef(service) => {
                        assert_eq!(service.operations.len(), 3);
                        assert_eq!(service.operations[0].name, "create-user");
                        assert_eq!(service.operations[1].name, "get-user");
                        assert_eq!(service.operations[2].name, "delete-user");
                    }
                    _ => panic!("Expected service definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_service_bidirectional() {
        let input = r#"
        service ChatService {
          chat-message: Message <-> Message
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::ServiceDef(service) => {
                        assert_eq!(service.operations.len(), 1);
                        let op = &service.operations[0];
                        assert_eq!(op.name, "chat-message");
                        assert!(matches!(op.direction, ServiceDirection::Bidirectional));
                    }
                    _ => panic!("Expected service definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_service_reverse() {
        let input = r#"
        service NotificationService {
          notify: Notification <- Event
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::ServiceDef(service) => {
                        assert_eq!(service.operations.len(), 1);
                        let op = &service.operations[0];
                        assert_eq!(op.name, "notify");
                        assert!(matches!(op.direction, ServiceDirection::Reverse));
                    }
                    _ => panic!("Expected service definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_service_with_inline_types() {
        let input = r#"
        service ApiService {
          get-status: {} -> { status: text, uptime: int },
          echo: { message: text } -> { message: text }
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::ServiceDef(service) => {
                        assert_eq!(service.operations.len(), 2);

                        let op1 = &service.operations[0];
                        assert_eq!(op1.name, "get-status");
                        assert!(matches!(op1.input_type, TypeExpression::Group(_)));
                        assert!(matches!(op1.output_type, TypeExpression::Group(_)));

                        let op2 = &service.operations[1];
                        assert_eq!(op2.name, "echo");
                        assert!(matches!(op2.input_type, TypeExpression::Group(_)));
                        assert!(matches!(op2.output_type, TypeExpression::Group(_)));
                    }
                    _ => panic!("Expected service definition, got {:?}", rule.rule_type),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_mixed_rules_and_services() {
        let input = r#"
        CreateUserRequest = {
          name: text,
          email: text
        }
        
        UserProfile = {
          id: int,
          name: text,
          email: text
        }
        
        service UserService {
          create-user: CreateUserRequest -> UserProfile,
          get-user: { id: int } -> UserProfile
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 3);

                assert_eq!(spec.rules[0].name, "CreateUserRequest");
                assert!(matches!(spec.rules[0].rule_type, RuleType::TypeDef(_)));

                assert_eq!(spec.rules[1].name, "UserProfile");
                assert!(matches!(spec.rules[1].rule_type, RuleType::TypeDef(_)));

                assert_eq!(spec.rules[2].name, "UserService");
                assert!(matches!(spec.rules[2].rule_type, RuleType::ServiceDef(_)));
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_service_parse_error_missing_colon() {
        let input = r#"
        service TestService {
          operation CreateRequest -> Response
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_service_parse_error_missing_arrow() {
        let input = r#"
        service TestService {
          operation: CreateRequest Response
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_service_parse_error_missing_brace() {
        let input = r#"
        service TestService 
          operation: CreateRequest -> Response
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_field_visibility_metadata() {
        let input = r#"
        Person = {
          @send-only
          password: text,
          @receive-only
          id: int,
          @bidirectional
          name: text,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];
                assert_eq!(rule.name, "Person");

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 3);

                        // Check password field has @send-only
                        let password_entry = &group.entries[0];
                        assert_eq!(password_entry.metadata.len(), 1);
                        assert!(matches!(
                            password_entry.metadata[0],
                            FieldMetadata::Visibility(FieldVisibility::SendOnly)
                        ));

                        // Check id field has @receive-only
                        let id_entry = &group.entries[1];
                        assert_eq!(id_entry.metadata.len(), 1);
                        assert!(matches!(
                            id_entry.metadata[0],
                            FieldMetadata::Visibility(FieldVisibility::ReceiveOnly)
                        ));

                        // Check name field has @bidirectional
                        let name_entry = &group.entries[2];
                        assert_eq!(name_entry.metadata.len(), 1);
                        assert!(matches!(
                            name_entry.metadata[0],
                            FieldMetadata::Visibility(FieldVisibility::Bidirectional)
                        ));
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_depends_on_metadata() {
        let input = r#"
        Order = {
          type: text,
          @depends-on(type = "express")
          expedite_fee?: int,
          @depends-on(type)
          processing_time: int,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 3);

                        // Check expedite_fee has depends-on with value
                        let expedite_entry = &group.entries[1];
                        assert_eq!(expedite_entry.metadata.len(), 1);
                        match &expedite_entry.metadata[0] {
                            FieldMetadata::DependsOn { field, value } => {
                                assert_eq!(field, "type");
                                assert!(
                                    matches!(value, Some(LiteralValue::Text(s)) if s == "express")
                                );
                            }
                            _ => panic!("Expected DependsOn metadata"),
                        }

                        // Check processing_time has depends-on without value
                        let processing_entry = &group.entries[2];
                        assert_eq!(processing_entry.metadata.len(), 1);
                        match &processing_entry.metadata[0] {
                            FieldMetadata::DependsOn { field, value } => {
                                assert_eq!(field, "type");
                                assert!(value.is_none());
                            }
                            _ => panic!("Expected DependsOn metadata"),
                        }
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_validation_constraint_metadata() {
        let input = r#"
        User = {
          @min-length(3)
          @max-length(50)
          username: text,
          @min-items(1)
          @max-items(10)
          tags: [text],
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 2);

                        // Check username field has length constraints
                        let username_entry = &group.entries[0];
                        assert_eq!(username_entry.metadata.len(), 2);
                        let min_constraint = &username_entry.metadata[0];
                        let max_constraint = &username_entry.metadata[1];

                        assert!(matches!(
                            min_constraint,
                            FieldMetadata::Constraint(ValidationConstraint::MinLength(3))
                        ));
                        assert!(matches!(
                            max_constraint,
                            FieldMetadata::Constraint(ValidationConstraint::MaxLength(50))
                        ));

                        // Check tags field has item constraints
                        let tags_entry = &group.entries[1];
                        assert_eq!(tags_entry.metadata.len(), 2);
                        let min_items = &tags_entry.metadata[0];
                        let max_items = &tags_entry.metadata[1];

                        assert!(matches!(
                            min_items,
                            FieldMetadata::Constraint(ValidationConstraint::MinItems(1))
                        ));
                        assert!(matches!(
                            max_items,
                            FieldMetadata::Constraint(ValidationConstraint::MaxItems(10))
                        ));
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_description_metadata() {
        let input = r#"
        User = {
          @description("The user's unique identifier")
          id: int,
          @description("The user's display name")
          name: text,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 2);

                        // Check id field description
                        let id_entry = &group.entries[0];
                        assert_eq!(id_entry.metadata.len(), 1);
                        match &id_entry.metadata[0] {
                            FieldMetadata::Description(desc) => {
                                assert_eq!(desc, "The user's unique identifier");
                            }
                            _ => panic!("Expected Description metadata"),
                        }

                        // Check name field description
                        let name_entry = &group.entries[1];
                        assert_eq!(name_entry.metadata.len(), 1);
                        match &name_entry.metadata[0] {
                            FieldMetadata::Description(desc) => {
                                assert_eq!(desc, "The user's display name");
                            }
                            _ => panic!("Expected Description metadata"),
                        }
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_custom_metadata() {
        let input = r#"
        User = {
          @rust(skip_serializing_if = "Option::is_none")
          optional_field?: text,
          @validation-rule("email")
          email: text,
          @db-index(unique = true, name = "user_email_idx")
          email_normalized: text,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 3);

                        // Check rust custom annotation
                        let optional_entry = &group.entries[0];
                        assert_eq!(optional_entry.metadata.len(), 1);
                        match &optional_entry.metadata[0] {
                            FieldMetadata::Custom { name, parameters } => {
                                assert_eq!(name, "rust");
                                assert_eq!(parameters.len(), 1);
                                assert_eq!(
                                    parameters[0].name,
                                    Some("skip_serializing_if".to_string())
                                );
                                assert!(
                                    matches!(&parameters[0].value, LiteralValue::Text(s) if s == "Option::is_none")
                                );
                            }
                            _ => panic!("Expected Custom metadata"),
                        }

                        // Check validation rule (single parameter)
                        let email_entry = &group.entries[1];
                        assert_eq!(email_entry.metadata.len(), 1);
                        match &email_entry.metadata[0] {
                            FieldMetadata::Custom { name, parameters } => {
                                assert_eq!(name, "validation-rule");
                                assert_eq!(parameters.len(), 1);
                                assert_eq!(parameters[0].name, None);
                                assert!(
                                    matches!(&parameters[0].value, LiteralValue::Text(s) if s == "email")
                                );
                            }
                            _ => panic!("Expected Custom metadata"),
                        }

                        // Check db-index (multiple parameters)
                        let email_norm_entry = &group.entries[2];
                        assert_eq!(email_norm_entry.metadata.len(), 1);
                        match &email_norm_entry.metadata[0] {
                            FieldMetadata::Custom { name, parameters } => {
                                assert_eq!(name, "db-index");
                                assert_eq!(parameters.len(), 2);

                                // Find the parameters by name
                                let unique_param = parameters
                                    .iter()
                                    .find(|p| p.name == Some("unique".to_string()))
                                    .unwrap();
                                let name_param = parameters
                                    .iter()
                                    .find(|p| p.name == Some("name".to_string()))
                                    .unwrap();

                                assert!(matches!(unique_param.value, LiteralValue::Bool(true)));
                                assert!(
                                    matches!(&name_param.value, LiteralValue::Text(s) if s == "user_email_idx")
                                );
                            }
                            _ => panic!("Expected Custom metadata"),
                        }
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_mixed_metadata() {
        let input = r#"
        User = {
          @send-only
          @description("User's secret password")
          @min-length(8)
          password: text,
          @receive-only
          @custom-validation("uuid-v4")
          id: text,
        }
        "#;
        let result = parse_csil(input);

        match result {
            Ok(spec) => {
                assert_eq!(spec.rules.len(), 1);
                let rule = &spec.rules[0];

                match &rule.rule_type {
                    RuleType::TypeDef(TypeExpression::Group(group)) => {
                        assert_eq!(group.entries.len(), 2);

                        // Check password field has all three metadata types
                        let password_entry = &group.entries[0];
                        assert_eq!(password_entry.metadata.len(), 3);

                        // Check each metadata type is present
                        let has_visibility = password_entry
                            .metadata
                            .iter()
                            .any(|m| matches!(m, FieldMetadata::Visibility(_)));
                        let has_description = password_entry
                            .metadata
                            .iter()
                            .any(|m| matches!(m, FieldMetadata::Description(_)));
                        let has_constraint = password_entry
                            .metadata
                            .iter()
                            .any(|m| matches!(m, FieldMetadata::Constraint(_)));

                        assert!(has_visibility, "Should have visibility metadata");
                        assert!(has_description, "Should have description metadata");
                        assert!(has_constraint, "Should have constraint metadata");

                        // Check id field has visibility and custom metadata
                        let id_entry = &group.entries[1];
                        assert_eq!(id_entry.metadata.len(), 2);

                        let has_visibility = id_entry
                            .metadata
                            .iter()
                            .any(|m| matches!(m, FieldMetadata::Visibility(_)));
                        let has_custom = id_entry
                            .metadata
                            .iter()
                            .any(|m| matches!(m, FieldMetadata::Custom { .. }));

                        assert!(has_visibility, "Should have visibility metadata");
                        assert!(has_custom, "Should have custom metadata");
                    }
                    _ => panic!("Expected group type definition"),
                }
            }
            Err(e) => panic!("Parse failed: {e}"),
        }
    }

    #[test]
    fn test_parse_metadata_error_missing_paren() {
        let input = r#"
        User = {
          @description "missing parentheses"
          name: text,
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_metadata_error_invalid_depends_on() {
        let input = r#"
        User = {
          @depends-on(123)
          name: text,
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_metadata_error_invalid_constraint_value() {
        let input = r#"
        User = {
          @min-length("invalid")
          name: text,
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_options_block() {
        let input = r#"
        options {
          version: "1.0.0",
          namespace: "com.example"
        }
        User = { name: text }
        "#;

        let spec = parse_csil(input).unwrap();

        assert!(spec.options.is_some());
        let options = spec.options.unwrap();
        assert_eq!(options.entries.len(), 2);

        // Verify specific options
        let version_entry = options.entries.iter().find(|e| e.key == "version").unwrap();
        assert!(matches!(version_entry.value, LiteralValue::Text(ref s) if s == "1.0.0"));

        let namespace_entry = options
            .entries
            .iter()
            .find(|e| e.key == "namespace")
            .unwrap();
        assert!(matches!(namespace_entry.value, LiteralValue::Text(ref s) if s == "com.example"));

        // Verify we still have the rule
        assert_eq!(spec.rules.len(), 1);
        assert_eq!(spec.rules[0].name, "User");
    }

    #[test]
    fn test_parse_without_options() {
        let input = "User = { name: text }";
        let spec = parse_csil(input).unwrap();
        assert!(spec.options.is_none());
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_parse_cddl_control_operators() {
        // Test size constraint
        let input = "Name = text .size (3..50)";
        let ast = parse_csil(input).expect("Failed to parse CSIL with size constraint");

        assert_eq!(ast.rules.len(), 1);
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "Name");

        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 1);
            assert!(matches!(
                constraints[0],
                ControlOperator::Size(SizeConstraint::Range { min: 3, max: 50 })
            ));
        } else {
            panic!("Expected Constrained type expression");
        }

        // Test regex constraint
        let input = r#"Pattern = text .regex "^[A-Za-z]+$""#;
        let ast = parse_csil(input).expect("Failed to parse CSIL with regex constraint");

        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 1);
            assert!(
                matches!(constraints[0], ControlOperator::Regex(ref pattern) if pattern == "^[A-Za-z]+$")
            );
        } else {
            panic!("Expected Constrained type expression");
        }

        // Test default constraint
        let input = "Status = text .default \"active\"";
        let ast = parse_csil(input).expect("Failed to parse CSIL with default constraint");

        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 1);
            assert!(
                matches!(constraints[0], ControlOperator::Default(LiteralValue::Text(ref s)) if s == "active")
            );
        } else {
            panic!("Expected Constrained type expression");
        }

        // Test multiple constraints
        let input =
            r#"Username = text .size (3..20) .regex "^[a-zA-Z0-9_]+$" .default "anonymous""#;
        let ast = parse_csil(input).expect("Failed to parse CSIL with multiple constraints");

        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 3);

            assert!(matches!(
                constraints[0],
                ControlOperator::Size(SizeConstraint::Range { min: 3, max: 20 })
            ));
            assert!(
                matches!(constraints[1], ControlOperator::Regex(ref pattern) if pattern == "^[a-zA-Z0-9_]+$")
            );
            assert!(
                matches!(constraints[2], ControlOperator::Default(LiteralValue::Text(ref s)) if s == "anonymous")
            );
        } else {
            panic!("Expected Constrained type expression");
        }
    }

    #[test]
    fn test_parse_optional_fields() {
        let input = "User = { id: int, ? name: text }";
        let ast = parse_csil(input).expect("Failed to parse CSIL");

        assert_eq!(ast.rules.len(), 1);
        let rule = &ast.rules[0];

        if let RuleType::TypeDef(TypeExpression::Group(group)) = &rule.rule_type {
            assert_eq!(group.entries.len(), 2);

            // First field should not be optional
            assert!(group.entries[0].occurrence.is_none());

            // Second field should be optional
            assert!(matches!(
                group.entries[1].occurrence,
                Some(Occurrence::Optional)
            ));
        } else {
            panic!("Expected Group type expression");
        }
    }

    #[test]
    fn test_parse_options_with_different_types() {
        let input = r#"
        options {
          version: "1.0.0",
          port: 8080,
          debug: true,
          timeout: 30.5
        }
        "#;

        let spec = parse_csil(input).unwrap();

        assert!(spec.options.is_some());
        let options = spec.options.unwrap();
        assert_eq!(options.entries.len(), 4);

        let version = options.entries.iter().find(|e| e.key == "version").unwrap();
        assert!(matches!(version.value, LiteralValue::Text(ref s) if s == "1.0.0"));

        let port = options.entries.iter().find(|e| e.key == "port").unwrap();
        assert!(matches!(port.value, LiteralValue::Integer(8080)));

        let debug = options.entries.iter().find(|e| e.key == "debug").unwrap();
        assert!(matches!(debug.value, LiteralValue::Bool(true)));

        let timeout = options.entries.iter().find(|e| e.key == "timeout").unwrap();
        assert!(matches!(timeout.value, LiteralValue::Float(f) if (f - 30.5).abs() < f64::EPSILON));
    }

    #[test]
    fn test_options_parse_error_missing_brace() {
        let input = "options version = \"1.0.0\"";
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_options_parse_error_missing_colon() {
        let input = r#"
        options {
          version "1.0.0"
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_options_parse_error_missing_value() {
        let input = r#"
        options {
          version:
        }
        "#;
        let result = parse_csil(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_options_empty_block() {
        let input = r#"
        options {
        }
        User = { name: text }
        "#;

        let spec = parse_csil(input).unwrap();

        assert!(spec.options.is_some());
        let options = spec.options.unwrap();
        assert_eq!(options.entries.len(), 0);
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_options_trailing_comma() {
        let input = r#"
        options {
          version: "1.0.0",
          namespace: "com.example",
        }
        User = { name: text }
        "#;

        let spec = parse_csil(input).unwrap();

        assert!(spec.options.is_some());
        let options = spec.options.unwrap();
        assert_eq!(options.entries.len(), 2);
    }

    #[test]
    fn test_parse_include_statement() {
        let input = r#"
        include "common/types.csil"
        
        User = { name: text }
        "#;

        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.imports.len(), 1);
        match &spec.imports[0] {
            ImportStatement::Include { path, alias, .. } => {
                assert_eq!(path, "common/types.csil");
                assert!(alias.is_none());
            }
            _ => panic!("Expected Include statement"),
        }
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_parse_include_with_alias() {
        let input = r#"
        include "user/types.csil" as user
        
        service TestService {
            test: user.Request -> user.Response
        }
        "#;

        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.imports.len(), 1);
        match &spec.imports[0] {
            ImportStatement::Include { path, alias, .. } => {
                assert_eq!(path, "user/types.csil");
                assert_eq!(alias.as_ref().unwrap(), "user");
            }
            _ => panic!("Expected Include statement"),
        }
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_parse_selective_import() {
        let input = r#"
        from "types.csil" include Request, Response
        
        service TestService {
            test: Request -> Response
        }
        "#;

        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.imports.len(), 1);
        match &spec.imports[0] {
            ImportStatement::SelectiveImport { path, items, .. } => {
                assert_eq!(path, "types.csil");
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], "Request");
                assert_eq!(items[1], "Response");
            }
            _ => panic!("Expected SelectiveImport statement"),
        }
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_parse_multiple_imports() {
        let input = r#"
        include "common.csil"
        from "errors.csil" include ErrorResponse
        include "user/types.csil" as user
        
        User = { name: text }
        "#;

        let spec = parse_csil(input).unwrap();

        assert_eq!(spec.imports.len(), 3);
        assert_eq!(spec.rules.len(), 1);
    }

    #[test]
    fn test_parse_dotted_identifier() {
        let input = r#"
        User = { profile: user.Profile }
        "#;

        let spec = parse_csil(input).unwrap();
        assert_eq!(spec.rules.len(), 1);

        if let RuleType::TypeDef(TypeExpression::Group(group)) = &spec.rules[0].rule_type {
            if let Some(entry) = group.entries.first() {
                if let TypeExpression::Reference(name) = &entry.value_type {
                    assert_eq!(name, "user.Profile");
                } else {
                    panic!("Expected Reference type");
                }
            } else {
                panic!("Expected group entry");
            }
        } else {
            panic!("Expected TypeDef with Group");
        }
    }

    #[test]
    fn test_parse_ne_constraint() {
        let input = "Status = int .ne 0";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .ne constraint");
        
        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "int"));
            assert_eq!(constraints.len(), 1);
            assert!(
                matches!(constraints[0], ControlOperator::NotEqual(LiteralValue::Integer(0)))
            );
        } else {
            panic!("Expected Constrained type expression with .ne");
        }
    }

    #[test]
    fn test_parse_bits_constraint() {
        let input = "Flags = int .bits \"0x00FF\"";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .bits constraint");
        
        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "int"));
            assert_eq!(constraints.len(), 1);
            assert!(
                matches!(constraints[0], ControlOperator::Bits(ref expr) if expr == "0x00FF")
            );
        } else {
            panic!("Expected Constrained type expression with .bits");
        }
    }

    #[test]
    fn test_parse_and_constraint() {
        let input = "Combined = text .and (text .size (3..10))";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .and constraint");
        
        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 1);
            
            if let ControlOperator::And(type_expr) = &constraints[0] {
                assert!(matches!(**type_expr, TypeExpression::Constrained { .. }));
            } else {
                panic!("Expected And constraint");
            }
        } else {
            panic!("Expected Constrained type expression with .and");
        }
    }

    #[test]
    fn test_parse_within_constraint() {
        let input = "Subset = int .within (1 / 2 / 3)";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .within constraint");
        
        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "int"));
            assert_eq!(constraints.len(), 1);
            
            if let ControlOperator::Within(type_expr) = &constraints[0] {
                assert!(matches!(**type_expr, TypeExpression::Choice(_)));
            } else {
                panic!("Expected Within constraint");
            }
        } else {
            panic!("Expected Constrained type expression with .within");
        }
    }

    #[test]
    fn test_parse_multiple_new_constraints() {
        let input = "Complex = int .ne 0 .ge 1 .le 100";
        let ast = parse_csil(input).expect("Failed to parse CSIL with multiple constraints");
        
        let rule = &ast.rules[0];
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "int"));
            assert_eq!(constraints.len(), 3);
            assert!(matches!(constraints[0], ControlOperator::NotEqual(_)));
            assert!(matches!(constraints[1], ControlOperator::GreaterEqual(_)));
            assert!(matches!(constraints[2], ControlOperator::LessEqual(_)));
        } else {
            panic!("Expected Constrained type expression with multiple constraints");
        }
    }

    #[test]
    fn test_parse_json_constraint() {
        let input = "JsonData = text .json";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .json constraint");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "JsonData");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 1);
            assert!(matches!(constraints[0], ControlOperator::Json));
        } else {
            panic!("Expected Constrained type expression with .json constraint");
        }
    }

    #[test]
    fn test_parse_json_constraint_with_size() {
        let input = "ApiResponse = text .size (10..1000) .json";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .json and .size constraints");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "ApiResponse");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "text"));
            assert_eq!(constraints.len(), 2);
            assert!(matches!(constraints[0], ControlOperator::Size(_)));
            assert!(matches!(constraints[1], ControlOperator::Json));
        } else {
            panic!("Expected Constrained type expression with .json and .size constraints");
        }
    }

    #[test]
    fn test_parse_json_constraint_on_bytes() {
        let input = "BinaryJson = bytes .json";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .json constraint on bytes");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "BinaryJson");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "bytes"));
            assert_eq!(constraints.len(), 1);
            assert!(matches!(constraints[0], ControlOperator::Json));
        } else {
            panic!("Expected Constrained type expression with .json constraint on bytes");
        }
    }

    #[test]
    fn test_parse_cbor_constraint() {
        let input = "CborData = bytes .cbor";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .cbor constraint");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "CborData");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "bytes"));
            assert_eq!(constraints.len(), 1);
            assert!(matches!(constraints[0], ControlOperator::Cbor));
        } else {
            panic!("Expected Constrained type expression with .cbor constraint");
        }
    }

    #[test]
    fn test_parse_cborseq_constraint() {
        let input = "CborSequence = bytes .cborseq";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .cborseq constraint");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "CborSequence");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "bytes"));
            assert_eq!(constraints.len(), 1);
            assert!(matches!(constraints[0], ControlOperator::Cborseq));
        } else {
            panic!("Expected Constrained type expression with .cborseq constraint");
        }
    }

    #[test]
    fn test_parse_cbor_constraint_with_size() {
        let input = "CompactData = bytes .size (10..1000) .cbor";
        let ast = parse_csil(input).expect("Failed to parse CSIL with .cbor and .size constraints");
        
        let rule = &ast.rules[0];
        assert_eq!(rule.name, "CompactData");
        
        if let RuleType::TypeDef(TypeExpression::Constrained {
            base_type,
            constraints,
        }) = &rule.rule_type
        {
            assert!(matches!(**base_type, TypeExpression::Builtin(ref name) if name == "bytes"));
            assert_eq!(constraints.len(), 2);
            assert!(matches!(constraints[0], ControlOperator::Size(_)));
            assert!(matches!(constraints[1], ControlOperator::Cbor));
        } else {
            panic!("Expected Constrained type expression with .cbor and .size constraints");
        }
    }
}
