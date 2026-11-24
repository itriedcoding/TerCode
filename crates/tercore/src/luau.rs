//! Luau analysis: tokenizer, block/bracket balance and convention diagnostics.
//!
//! This is a real (if intentionally focused) static pass over the source, not a
//! regex sweep: strings, long strings and comments are skipped before keywords
//! are counted, so `end` inside a comment never balances a block.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub line: u32,
    pub column: u32,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    String,
    Comment,
    Number,
    Symbol,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub line: u32,
    pub column: u32,
}

const KEYWORDS: [&str; 21] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Tokenizes Luau source, skipping comments and handling long strings.
pub fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut column = 1u32;

    let bump = |i: &mut usize, line: &mut u32, column: &mut u32| {
        if chars[*i] == '\n' {
            *line += 1;
            *column = 1;
        } else {
            *column += 1;
        }
        *i += 1;
    };

    while i < chars.len() {
        let c = chars[i];

        if c == '\n' {
            bump(&mut i, &mut line, &mut column);
            continue;
        }
        if c.is_whitespace() {
            bump(&mut i, &mut line, &mut column);
            continue;
        }

        // Comments: --[[ long ]] or -- line
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            let start_line = line;
            let start_col = column;
            let mut text = String::new();
            bump(&mut i, &mut line, &mut column);
            bump(&mut i, &mut line, &mut column);
            if i + 1 < chars.len() && chars[i] == '[' && (chars[i + 1] == '[' || chars[i + 1] == '=') {
                let level = count_long_bracket_level(&chars[i..]);
                let mut closed = false;
                while i < chars.len() {
                    if is_long_bracket_close(&chars[i..], level) {
                        for _ in 0..(level + 2) {
                            bump(&mut i, &mut line, &mut column);
                        }
                        closed = true;
                        break;
                    }
                    text.push(chars[i]);
                    bump(&mut i, &mut line, &mut column);
                }
                if !closed {
                    tokens.push(Token {
                        kind: TokenKind::Comment,
                        text,
                        line: start_line,
                        column: start_col,
                    });
                    continue;
                }
            } else {
                while i < chars.len() && chars[i] != '\n' {
                    text.push(chars[i]);
                    bump(&mut i, &mut line, &mut column);
                }
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                text,
                line: start_line,
                column: start_col,
            });
            continue;
        }

        // Long strings [[ ... ]]
        if c == '[' && i + 1 < chars.len() && (chars[i + 1] == '[' || chars[i + 1] == '=') {
            let start_line = line;
            let start_col = column;
            let level = count_long_bracket_level(&chars[i..]);
            let mut text = String::new();
            for _ in 0..(level + 2) {
                bump(&mut i, &mut line, &mut column);
            }
            while i < chars.len() {
                if is_long_bracket_close(&chars[i..], level) {
                    for _ in 0..(level + 2) {
                        bump(&mut i, &mut line, &mut column);
                    }
                    break;
                }
                text.push(chars[i]);
                bump(&mut i, &mut line, &mut column);
            }
            tokens.push(Token {
                kind: TokenKind::String,
                text,
                line: start_line,
                column: start_col,
            });
            continue;
        }

        // Quoted strings (including escapes)
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start_line = line;
            let start_col = column;
            let mut text = String::new();
            bump(&mut i, &mut line, &mut column);
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' {
                    text.push(ch);
                    bump(&mut i, &mut line, &mut column);
                    if i < chars.len() {
                        text.push(chars[i]);
                        bump(&mut i, &mut line, &mut column);
                    }
                    continue;
                }
                if ch == quote {
                    bump(&mut i, &mut line, &mut column);
                    break;
                }
                if ch == '\n' && quote != '`' {
                    // Unterminated single-line string; stop at the newline so
                    // the rest of the file is still analysed.
                    break;
                }
                text.push(ch);
                bump(&mut i, &mut line, &mut column);
            }
            tokens.push(Token {
                kind: TokenKind::String,
                text,
                line: start_line,
                column: start_col,
            });
            continue;
        }

        if c.is_ascii_digit() {
            let start_line = line;
            let start_col = column;
            let mut text = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                text.push(chars[i]);
                bump(&mut i, &mut line, &mut column);
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                text,
                line: start_line,
                column: start_col,
            });
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let start_line = line;
            let start_col = column;
            let mut text = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                text.push(chars[i]);
                bump(&mut i, &mut line, &mut column);
            }
            let kind = if KEYWORDS.contains(&text.as_str()) {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            tokens.push(Token {
                kind,
                text,
                line: start_line,
                column: start_col,
            });
            continue;
        }

        let start_line = line;
        let start_col = column;
        let text = c.to_string();
        bump(&mut i, &mut line, &mut column);
        tokens.push(Token {
            kind: TokenKind::Symbol,
            text,
            line: start_line,
            column: start_col,
        });
    }

    tokens
}

fn count_long_bracket_level(chars: &[char]) -> usize {
    // `[`, then zero or more `=`, then `[`
    let mut level = 0;
    let mut idx = 1;
    while idx < chars.len() && chars[idx] == '=' {
        level += 1;
        idx += 1;
    }
    level
}

fn is_long_bracket_close(chars: &[char], level: usize) -> bool {
    if chars.len() < level + 2 || chars[0] != ']' {
        return false;
    }
    for i in 0..level {
        if chars[1 + i] != '=' {
            return false;
        }
    }
    chars[level + 1] == ']'
}

const DEPRECATED: [(&str, &str); 5] = [
    ("wait", "use task.wait instead of the deprecated wait()"),
    ("spawn", "use task.spawn instead of the deprecated spawn()"),
    ("delay", "use task.delay instead of the deprecated delay()"),
    ("elapsedTime", "use os.clock() instead of the deprecated elapsedTime()"),
    ("tick", "use os.clock() for durations or DateTime.now() for wall time"),
];

/// Runs the analyser over a Luau source file.
pub fn analyze(source: &str, path: &str) -> Vec<Diagnostic> {
    let tokens = tokenize(source);
    let mut diagnostics = Vec::new();

    // 1. Bracket balance.
    let mut stack: Vec<&Token> = Vec::new();
    for token in &tokens {
        if token.kind != TokenKind::Symbol {
            continue;
        }
        match token.text.as_str() {
            "(" | "[" | "{" => stack.push(token),
            ")" | "]" | "}" => {
                let expected = match token.text.as_str() {
                    ")" => "(",
                    "]" => "[",
                    _ => "{",
                };
                match stack.pop() {
                    Some(open) if open.text == expected => {}
                    Some(open) => diagnostics.push(Diagnostic {
                        line: token.line,
                        column: token.column,
                        severity: Severity::Error,
                        code: "unbalanced-bracket",
                        message: format!(
                            "`{}` does not match `{}` opened on line {}",
                            token.text, open.text, open.line
                        ),
                    }),
                    None => diagnostics.push(Diagnostic {
                        line: token.line,
                        column: token.column,
                        severity: Severity::Error,
                        code: "unbalanced-bracket",
                        message: format!("unexpected closing `{}`", token.text),
                    }),
                }
            }
            _ => {}
        }
    }
    for open in stack {
        diagnostics.push(Diagnostic {
            line: open.line,
            column: open.column,
            severity: Severity::Error,
            code: "unbalanced-bracket",
            message: format!("`{}` is never closed", open.text),
        });
    }

    // 2. Block balance. `if`/`for`/`while` open a block at `then`/`do`;
    //    `function` and `repeat` open directly.
    let mut blocks: Vec<(String, u32)> = Vec::new();
    for (idx, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Keyword {
            continue;
        }
        match token.text.as_str() {
            "function" => blocks.push(("function".into(), token.line)),
            "repeat" => blocks.push(("repeat".into(), token.line)),
            "if" => {
                // `elseif` is its own keyword; only `then` opens the block.
                if let Some(next) = tokens[idx..].iter().find(|t| {
                    t.kind == TokenKind::Keyword
                        && (t.text == "then" || t.text == "end" || t.text == "do")
                }) {
                    if next.text == "then" {
                        blocks.push(("if".into(), token.line));
                    }
                }
            }
            "for" | "while" => {
                if let Some(next) = tokens[idx..].iter().find(|t| {
                    t.kind == TokenKind::Keyword && (t.text == "do" || t.text == "end")
                }) {
                    if next.text == "do" {
                        blocks.push((token.text.clone(), token.line));
                    }
                }
            }
            "do" => {
                // A `do` that belongs to a for/while was already counted.
                let belongs = tokens[..idx].iter().rev().any(|t| {
                    t.kind == TokenKind::Keyword
                        && (t.text == "for" || t.text == "while" || t.text == "do")
                        && t.line == token.line
                });
                if !belongs {
                    blocks.push(("do".into(), token.line));
                }
            }
            "end" => {
                if blocks.pop().is_none() {
                    diagnostics.push(Diagnostic {
                        line: token.line,
                        column: token.column,
                        severity: Severity::Error,
                        code: "unbalanced-block",
                        message: "`end` without a matching block".into(),
                    });
                }
            }
            "until" => {
                match blocks.pop() {
                    Some((kind, _)) if kind == "repeat" => {}
                    Some((kind, line)) => diagnostics.push(Diagnostic {
                        line: token.line,
                        column: token.column,
                        severity: Severity::Error,
                        code: "unbalanced-block",
                        message: format!("`until` closes a `{kind}` opened on line {line}"),
                    }),
                    None => diagnostics.push(Diagnostic {
                        line: token.line,
                        column: token.column,
                        severity: Severity::Error,
                        code: "unbalanced-block",
                        message: "`until` without a matching `repeat`".into(),
                    }),
                }
            }
            _ => {}
        }
    }
    for (kind, line) in blocks {
        diagnostics.push(Diagnostic {
            line,
            column: 1,
            severity: Severity::Error,
            code: "unbalanced-block",
            message: format!("`{kind}` block is missing its closing `end`"),
        });
    }

    // 3. Deprecated globals used as calls.
    for (idx, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Identifier {
            continue;
        }
        for (name, hint) in DEPRECATED {
            if token.text != name {
                continue;
            }
            let is_call = tokens
                .get(idx + 1)
                .map(|t| t.kind == TokenKind::Symbol && t.text == "(")
                .unwrap_or(false);
            let is_local = idx > 0
                && tokens[idx - 1].kind == TokenKind::Keyword
                && tokens[idx - 1].text == "local";
            if is_call && !is_local {
                diagnostics.push(Diagnostic {
                    line: token.line,
                    column: token.column,
                    severity: Severity::Warning,
                    code: "deprecated-api",
                    message: hint.to_string(),
                });
            }
        }
    }

    // 4. Studio conventions.
    let is_server = path.contains("server") || path.ends_with(".server.luau");
    if is_server && source.contains("print(") {
        diagnostics.push(Diagnostic {
            line: line_of(source, "print("),
            column: 1,
            severity: Severity::Info,
            code: "server-print",
            message: "print() in server code is kept in production logs; consider a logger".into(),
        });
    }
    if source.trim().is_empty() {
        diagnostics.push(Diagnostic {
            line: 1,
            column: 1,
            severity: Severity::Warning,
            code: "empty-file",
            message: "this script is empty".into(),
        });
    }
    if !source.contains("--!strict") && !source.trim().is_empty() {
        diagnostics.push(Diagnostic {
            line: 1,
            column: 1,
            severity: Severity::Info,
            code: "no-strict",
            message: "add `--!strict` for Luau type checking in Studio".into(),
        });
    }
    if source.contains("game.Workspace") {
        diagnostics.push(Diagnostic {
            line: line_of(source, "game.Workspace"),
            column: 1,
            severity: Severity::Info,
            code: "use-workspace",
            message: "prefer the `workspace` global over `game.Workspace`".into(),
        });
    }

    diagnostics.sort_by_key(|d| (d.line, d.column));
    diagnostics
}

fn line_of(source: &str, needle: &str) -> u32 {
    source
        .lines()
        .position(|l| l.contains(needle))
        .map(|i| (i + 1) as u32)
        .unwrap_or(1)
}

/// Strips comments and strings — used when generating a compact context for AI.
pub fn strip_comments(source: &str) -> String {
    let tokens = tokenize(source);
    let mut out = String::new();
    let mut last_line = 0;
    for token in tokens {
        if token.kind == TokenKind::Comment {
            continue;
        }
        if token.line != last_line {
            if !out.is_empty() {
                out.push('\n');
            }
            last_line = token.line;
        } else if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_script_has_no_errors() {
        let source = r#"--!strict
local function greet(name: string): string
	if name == "" then
		return "nobody"
	elseif name == "admin" then
		return "boss"
	else
		return name
	end
end

for i = 1, 3 do
	print(greet("player" .. i))
end

local t = { a = 1, b = { 2, 3 } }
return t
"#;
        let diagnostics = analyze(source, "src/shared/util.luau");
        assert!(
            diagnostics.iter().all(|d| d.severity != Severity::Error),
            "unexpected errors: {diagnostics:?}"
        );
    }

    #[test]
    fn detects_missing_end() {
        let source = "local function f()\n\tif true then\n\t\tprint(1)\nend\n";
        let diagnostics = analyze(source, "src/a.luau");
        assert!(diagnostics
            .iter()
            .any(|d| d.code == "unbalanced-block" && d.severity == Severity::Error));
    }

    #[test]
    fn ignores_keywords_inside_comments_and_strings() {
        let source = "-- end end end\nlocal s = \"if then end\"\nprint(s)\n";
        let diagnostics = analyze(source, "src/a.luau");
        assert!(!diagnostics.iter().any(|d| d.code == "unbalanced-block"));
    }

    #[test]
    fn handles_long_strings_and_comments() {
        let source = "--[==[ end end ]==]\nlocal s = [=[ if then ]=]\nprint(s)\n";
        let diagnostics = analyze(source, "src/a.luau");
        assert!(!diagnostics.iter().any(|d| d.severity == Severity::Error));
    }

    #[test]
    fn detects_unbalanced_brackets() {
        let diagnostics = analyze("local x = (1 + 2\n", "src/a.luau");
        assert!(diagnostics
            .iter()
            .any(|d| d.code == "unbalanced-bracket" && d.message.contains("never closed")));
    }

    #[test]
    fn flags_deprecated_globals_but_not_locals() {
        let diagnostics = analyze(
            "wait(1)\nlocal wait = function() end\nwait(2)\n",
            "src/a.luau",
        );
        let hits: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.code == "deprecated-api")
            .collect();
        // Calls on lines 1 and 3 are flagged; the local declaration on line 2
        // is not a call and must never be reported.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[1].line, 3);
        assert!(hits.iter().all(|d| d.line != 2));
    }

    #[test]
    fn reports_studio_conventions() {
        let diagnostics = analyze("game.Workspace.Part:Destroy()\n", "src/server/A.server.luau");
        assert!(diagnostics.iter().any(|d| d.code == "use-workspace"));
        assert!(diagnostics.iter().any(|d| d.code == "no-strict"));
    }

    #[test]
    fn repeat_until_pairs() {
        let diagnostics = analyze("repeat\n\tprint(1)\nuntil true\n", "src/a.luau");
        assert!(!diagnostics.iter().any(|d| d.severity == Severity::Error));
    }

    #[test]
    fn tokenizes_identifiers_and_numbers() {
        let tokens = tokenize("local x = 42.5\n");
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword,
                TokenKind::Identifier,
                TokenKind::Symbol,
                TokenKind::Number
            ]
        );
    }

    #[test]
    fn strip_comments_keeps_code() {
        let stripped = strip_comments("local a = 1 -- note\nprint(a)\n");
        assert!(stripped.contains("local"));
        assert!(!stripped.contains("note"));
    }
}
