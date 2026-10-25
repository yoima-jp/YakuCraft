use std::collections::HashSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnbtDocument {
    pub entries: Vec<(String, SnbtValue)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnbtValue {
    String(String),
    StringArray(Vec<String>),
}

pub fn parse_snbt(input: &str) -> Result<SnbtDocument, (usize, String)> {
    Parser { input, offset: 0 }.parse_document()
}

pub fn write_snbt(document: &SnbtDocument) -> String {
    let mut output = String::from("{\n");
    for (key, value) in &document.entries {
        output.push('\t');
        write_key(&mut output, key);
        output.push_str(": ");
        match value {
            SnbtValue::String(text) => write_string(&mut output, text),
            SnbtValue::StringArray(values) => {
                output.push_str("[\n");
                for value in values {
                    output.push_str("\t\t");
                    write_string(&mut output, value);
                    output.push('\n');
                }
                output.push_str("\t]");
            }
        }
        output.push('\n');
    }
    output.push_str("}\n");
    output
}

fn write_key(output: &mut String, key: &str) {
    if !key.is_empty()
        && key.chars().all(|character| {
            !character.is_whitespace()
                && !matches!(
                    character,
                    ':' | '{' | '}' | '[' | ']' | ',' | '#' | '"' | '\''
                )
        })
    {
        output.push_str(key);
    } else {
        write_string(output, key);
    }
}

fn write_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            other => output.push(other),
        }
    }
    output.push('"');
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
}

impl Parser<'_> {
    fn parse_document(mut self) -> Result<SnbtDocument, (usize, String)> {
        self.skip_trivia();
        self.expect('{')?;
        let mut entries = Vec::new();
        let mut seen_keys = HashSet::new();
        loop {
            let had_newline = self.skip_trivia();
            if self.consume('}') {
                break;
            }
            if self.offset >= self.input.len() {
                return self.error("expected a key or closing brace");
            }
            let key = self.parse_key()?;
            self.skip_horizontal_space();
            self.expect(':')?;
            self.skip_trivia();
            let value = self.parse_value()?;
            if !seen_keys.insert(key.clone()) {
                return self.error(format!("duplicate key {key:?}"));
            }
            entries.push((key, value));

            let separated_by_newline = self.skip_trivia();
            if self.consume(',') {
                self.skip_trivia();
                if self.consume('}') {
                    break;
                }
                continue;
            }
            if self.consume('}') {
                break;
            }
            if !separated_by_newline {
                return self.error("expected a comma, newline, or closing brace");
            }
            let _ = had_newline;
        }
        self.skip_trivia();
        if self.offset != self.input.len() {
            return self.error("unexpected content after the top-level compound");
        }
        Ok(SnbtDocument { entries })
    }

    fn parse_value(&mut self) -> Result<SnbtValue, (usize, String)> {
        match self.peek() {
            Some('"' | '\'') => self.parse_quoted().map(SnbtValue::String),
            Some('[') => {
                self.offset += 1;
                let mut values = Vec::new();
                loop {
                    let _ = self.skip_trivia();
                    if self.consume(']') {
                        break;
                    }
                    if self.offset >= self.input.len() {
                        return self.error("expected a string or closing bracket");
                    }
                    match self.peek() {
                        Some('"' | '\'') => values.push(self.parse_quoted()?),
                        _ => return self.error("language arrays may contain only strings"),
                    }
                    let separated_by_newline = self.skip_trivia();
                    if self.consume(',') {
                        self.skip_trivia();
                        if self.consume(']') {
                            break;
                        }
                    } else if self.consume(']') {
                        break;
                    } else if !separated_by_newline {
                        return self.error("expected a comma, newline, or closing bracket");
                    }
                }
                Ok(SnbtValue::StringArray(values))
            }
            _ => self.error("language values must be quoted strings or arrays of strings"),
        }
    }

    fn parse_key(&mut self) -> Result<String, (usize, String)> {
        match self.peek() {
            Some('"' | '\'') => self.parse_quoted(),
            _ => {
                let start = self.offset;
                while let Some(character) = self.peek() {
                    if character == ':' || character.is_whitespace() || character == '#' {
                        break;
                    }
                    self.advance();
                }
                if self.offset == start {
                    return self.error("expected an unquoted or quoted key");
                }
                Ok(self.input[start..self.offset].to_owned())
            }
        }
    }

    fn parse_quoted(&mut self) -> Result<String, (usize, String)> {
        let quote = self
            .peek()
            .ok_or((self.offset, "expected quoted string".into()))?;
        if quote != '"' && quote != '\'' {
            return self.error("expected quoted string");
        }
        self.advance();
        let mut output = String::new();
        while let Some(character) = self.peek() {
            self.advance();
            if character == quote {
                return Ok(output);
            }
            if character == '\\' {
                let Some(escaped) = self.peek() else {
                    return self.error("unterminated escape sequence");
                };
                self.advance();
                if escaped == quote || escaped == '\\' {
                    output.push(escaped);
                } else {
                    // FTB Quests text can use escapes such as `\n` and `\u2022`
                    // after SNBT parsing, so preserve unknown escapes verbatim.
                    output.push('\\');
                    output.push(escaped);
                }
            } else {
                output.push(character);
            }
        }
        self.error("unterminated quoted string")
    }

    fn skip_trivia(&mut self) -> bool {
        let mut had_newline = false;
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\u{000c}') => self.advance(),
                Some('\r') => {
                    self.advance();
                    if self.peek() == Some('\n') {
                        self.advance();
                    }
                    had_newline = true;
                }
                Some('\n') => {
                    self.advance();
                    had_newline = true;
                }
                Some('#') => {
                    while let Some(character) = self.peek() {
                        self.advance();
                        if character == '\n' || character == '\r' {
                            had_newline = true;
                            break;
                        }
                    }
                }
                _ => return had_newline,
            }
        }
    }

    fn skip_horizontal_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\u{000c}')) {
            self.advance();
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), (usize, String)> {
        if self.consume(expected) {
            Ok(())
        } else {
            self.error(format!("expected {expected:?}"))
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.offset..)?.chars().next()
    }

    fn advance(&mut self) {
        if let Some(character) = self.peek() {
            self.offset += character.len_utf8();
        }
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, (usize, String)> {
        Err((self.offset, message.into()))
    }
}
