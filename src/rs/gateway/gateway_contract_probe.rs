//! Read-only, offline inspection of Hermes gateway admission RPC source.
//!
//! This reports static hints, not proof of on-wire schema or cancellation guarantees.
//! No Hermes modules are imported, and no gateway process is started.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use crate::util::bin_main::parse_args;

use crate::util::{pretty_json, resolve_strict};

use self::pysource::{FuncDef, function_defs, literal_dict_keys, return_count, string_constants};

/// Handler file inspected inside an explicitly supplied checkout.
pub const SOURCE_RELATIVE: &str = "gateway/session_controls.py";
/// Handler names that must each be defined exactly once.
pub const TARGETS: [&str; 3] = ["submit", "receipt", "cancel"];
/// Largest source file accepted for offline inspection.
pub const MAX_SOURCE_BYTES: u64 = 2_000_000;

/// Raised when source cannot be inspected safely or unambiguously.
#[derive(Debug, thiserror::Error)]
pub enum ContractInspectionError {
    /// Source cannot be inspected safely or unambiguously.
    #[error("{0}")]
    Inspection(String),
    /// Underlying filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Static facts collected about one handler definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MethodHint {
    /// Function name.
    pub name: String,
    /// One-based line of the definition.
    pub line: usize,
    /// Positional-or-keyword and keyword-only parameter names in source order.
    pub parameters: Vec<String>,
    /// Sorted string-literal dictionary keys found anywhere in the body.
    pub literal_dict_keys: Vec<String>,
    /// Sorted `prompt.*` / `session.*` string literals.
    pub rpc_names: Vec<String>,
    /// Number of `return` statements.
    pub returns: usize,
}

/// Result of inspecting a gateway source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectionReport {
    /// Name of the inspected source.
    pub source: String,
    /// One hint per target handler, in [`TARGETS`] order.
    pub methods: Vec<MethodHint>,
    /// Always false: hints are not proof.
    pub verified: bool,
    /// Always false: this module never dispatches.
    pub live_dispatch_enabled: bool,
}

fn rpc_names(tokens: &[pysource::Tok]) -> Vec<String> {
    let names: BTreeSet<String> = string_constants(tokens)
        .into_iter()
        .map(|(value, _)| value)
        .filter(|value| value.starts_with("prompt.") || value.starts_with("session."))
        .collect();
    names.into_iter().collect()
}

fn hint(function: &FuncDef) -> MethodHint {
    MethodHint {
        name: function.name.clone(),
        line: function.line,
        parameters: function.parameters(),
        literal_dict_keys: literal_dict_keys(&function.tokens).into_iter().collect(),
        rpc_names: rpc_names(&function.tokens),
        returns: return_count(&function.tokens),
    }
}

/// Collect source hints; reject absent or duplicate target handlers.
///
/// # Errors
/// Source files cannot be read or the source scanner cannot inspect the checkout.
pub fn inspect_source(source: &str) -> Result<InspectionReport, ContractInspectionError> {
    inspect_source_named(source, "<fixture>")
}

/// Like [`inspect_source`] with an explicit `source_name` for the report.
///
/// # Errors
/// Source files cannot be read or the source scanner cannot inspect the checkout.
pub fn inspect_source_named(
    source: &str,
    source_name: &str,
) -> Result<InspectionReport, ContractInspectionError> {
    let defs = function_defs(source).map_err(|exc| {
        ContractInspectionError::Inspection(format!(
            "Cannot parse Hermes gateway source: {}",
            exc.msg
        ))
    })?;
    let mut methods = Vec::new();
    let mut wrong = Vec::new();
    for name in TARGETS {
        let found: Vec<&FuncDef> = defs.iter().filter(|def| def.name == name).collect();
        match found.as_slice() {
            [only] => methods.push(hint(only)),
            _ => wrong.push(name),
        }
    }
    if !wrong.is_empty() {
        return Err(ContractInspectionError::Inspection(format!(
            "Expected exactly one definition of each handler; ambiguous: {}",
            wrong.join(", ")
        )));
    }
    Ok(InspectionReport {
        source: source_name.to_owned(),
        methods,
        verified: false,
        live_dispatch_enabled: false,
    })
}

/// Collect hints for every function and async function in source order.
///
/// This parity oracle supports comparison with `CPython`'s AST on arbitrary source.
///
/// # Errors
/// Returns an inspection error when the scanner cannot parse the supplied source.
pub fn all_method_hints(source: &str) -> Result<Vec<MethodHint>, ContractInspectionError> {
    let defs = function_defs(source).map_err(|exc| {
        ContractInspectionError::Inspection(format!(
            "Cannot parse Hermes gateway source: {}",
            exc.msg
        ))
    })?;
    Ok(defs.iter().map(hint).collect())
}

fn fail(message: &str) -> ContractInspectionError {
    ContractInspectionError::Inspection(message.to_owned())
}

/// Read only the expected handler file in an explicitly supplied checkout.
///
/// # Errors
/// Source files cannot be read or the source scanner cannot inspect the checkout.
pub fn inspect_checkout(checkout: &Path) -> Result<InspectionReport, ContractInspectionError> {
    let base = resolve_strict(checkout)?;
    let target = base.join(SOURCE_RELATIVE);
    let is_symlink = crate::util::lstat(&target).is_ok_and(|meta| meta.is_symlink());
    if is_symlink || !target.is_file() {
        return Err(fail(&format!(
            "Missing or symlinked source: {SOURCE_RELATIVE}"
        )));
    }
    if !resolve_strict(&target)?.starts_with(&base) {
        return Err(fail("Source path escaped checkout"));
    }
    if fs::metadata(&target)?.len() > MAX_SOURCE_BYTES {
        return Err(fail("Source file exceeds offline inspection limit"));
    }
    inspect_source_named(&fs::read_to_string(&target)?, SOURCE_RELATIVE)
}

#[derive(Parser)]
#[command(args_override_self = true, infer_long_args = true)]
struct Args {
    checkout: PathBuf,
}

/// Command-line entry point; `args` excludes the program name. Returns the exit code.
pub fn run(args: &[impl AsRef<OsStr>], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let parsed: Args = match parse_args(
        "gateway_contract_probe",
        args,
        "Inspect Hermes RPC handlers without running Hermes",
        "usage: gateway_contract_probe [-h] checkout",
        stdout,
        stderr,
    ) {
        Ok(parsed) => parsed,
        Err(status) => return status,
    };
    let report = match inspect_checkout(&parsed.checkout) {
        Ok(report) => report,
        Err(exc) => {
            let _ = writeln!(stderr, "Inspection failed: {exc}");
            return 2;
        }
    };
    match pretty_json(&report) {
        Ok(text) => {
            let _ = writeln!(stdout, "{text}");
            0
        }
        Err(exc) => {
            let _ = writeln!(stderr, "Inspection failed: {exc}");
            2
        }
    }
}

/// Purpose-built Python token/indentation scanner for contract probing, not a parser.
///
/// `tests/pysource_parity.rs` checks it against `CPython` `ast` over the frozen corpus.
///
/// The scanner finds function
/// definitions, string constants, literal dict keys and identifier references; exotic
/// syntax (parenthesised dict keys, soft keywords, `import` aliases) is approximated.
pub(crate) mod pysource {
    use crate::util::PyStr as _;
    use std::collections::BTreeSet;
    use std::ops::{Deref, Range};
    use std::rc::Rc;

    /// Token category.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Kind {
        Name,
        Op,
        Num,
        Str,
        Bytes,
        FPart,
    }

    /// One lexical token.
    #[derive(Debug, Clone)]
    pub(crate) struct Tok {
        pub(crate) kind: Kind,
        pub(crate) text: String,
        pub(crate) line: usize,
        pub(crate) synth: bool,
        /// Inside the pattern of a `case` clause, where `{...}` is a mapping pattern.
        pub(crate) pattern: bool,
    }

    impl Tok {
        fn new(kind: Kind, text: impl Into<String>, line: usize) -> Self {
            Self {
                kind,
                text: text.into(),
                line,
                synth: false,
                pattern: false,
            }
        }

        pub(crate) fn is_op(&self, text: &str) -> bool {
            self.kind == Kind::Op && self.text == text
        }

        pub(crate) fn is_name(&self, text: &str) -> bool {
            self.kind == Kind::Name && self.text == text
        }
    }

    /// A syntax problem found while scanning.
    #[derive(Debug)]
    pub(crate) struct ScanError {
        pub(crate) line: usize,
        pub(crate) msg: String,
    }

    fn scan_error<T>(line: usize, msg: impl Into<String>) -> Result<T, ScanError> {
        Err(ScanError {
            line,
            msg: msg.into(),
        })
    }

    const OPS3: [&str; 5] = ["**=", "//=", ">>=", "<<=", "..."];
    const OPS2: [&str; 22] = [
        "==", "!=", "<=", ">=", "->", ":=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "@=",
        "**", "//", "<<", ">>", "<>", "~=", "|>",
    ];
    const OPS1: &str = "()[]{},:;.+-*/%@&|^~<>=";
    const KEYWORDS: [&str; 31] = [
        "and", "as", "assert", "async", "await", "case", "class", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
        "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
    ];

    struct Lexer<'a> {
        chars: &'a [char],
        pos: usize,
        line: usize,
        brackets: Vec<(char, usize)>,
        lines: Vec<(usize, Vec<Tok>)>,
        current: Vec<Tok>,
        indent: usize,
        at_start: bool,
    }

    impl Lexer<'_> {
        fn peek(&self, offset: usize) -> Option<char> {
            self.chars.get(self.pos + offset).copied()
        }

        fn take_str(&self, count: usize) -> String {
            self.chars.iter().skip(self.pos).take(count).collect()
        }

        fn push(&mut self, kind: Kind, text: impl Into<String>) {
            let line = self.line;
            self.current.push(Tok::new(kind, text, line));
        }

        fn end_line(&mut self) {
            if !self.current.is_empty() {
                let mut tokens = std::mem::take(&mut self.current);
                mark_case_pattern(&mut tokens);
                self.lines.push((self.indent, tokens));
            }
        }

        fn measure_indent(&mut self) {
            let mut width = 0;
            while let Some(c) = self.peek(0) {
                match c {
                    ' ' => width += 1,
                    '\t' => width = (width / 8 + 1) * 8,
                    '\x0c' => width = 0,
                    _ => break,
                }
                self.pos += 1;
            }
            self.indent = width;
            self.at_start = false;
        }

        fn run(&mut self) -> Result<(), ScanError> {
            while let Some(c) = self.peek(0) {
                if self.at_start && self.brackets.is_empty() {
                    self.measure_indent();
                    continue;
                }
                match c {
                    '\n' => {
                        self.pos += 1;
                        self.line += 1;
                        if self.brackets.is_empty() {
                            self.end_line();
                            self.at_start = true;
                        }
                    }
                    ' ' | '\t' | '\r' | '\x0c' => self.pos += 1,
                    '#' => {
                        while self.peek(0).is_some_and(|ch| ch != '\n') {
                            self.pos += 1;
                        }
                    }
                    '\\' => self.continuation()?,
                    _ => self.token(c)?,
                }
            }
            if let Some(&(open, line)) = self.brackets.last() {
                return scan_error(line, format!("'{open}' was never closed"));
            }
            self.end_line();
            Ok(())
        }

        fn continuation(&mut self) -> Result<(), ScanError> {
            self.pos += 1;
            if self.peek(0) == Some('\r') {
                self.pos += 1;
            }
            if self.peek(0) == Some('\n') {
                self.pos += 1;
                self.line += 1;
                return Ok(());
            }
            scan_error(
                self.line,
                "unexpected character after line continuation character",
            )
        }

        fn token(&mut self, c: char) -> Result<(), ScanError> {
            if c.is_alphabetic() || c == '_' {
                return self.word();
            }
            if c == '\'' || c == '"' {
                return self.string("");
            }
            if c.is_ascii_digit() || (c == '.' && self.peek(1).is_some_and(|n| n.is_ascii_digit()))
            {
                self.number();
                return Ok(());
            }
            self.operator(c)
        }

        fn word(&mut self) -> Result<(), ScanError> {
            let start = self.pos;
            while self
                .peek(0)
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
            {
                self.pos += 1;
            }
            let word: String = self
                .chars
                .get(start..self.pos)
                .unwrap_or_default()
                .iter()
                .collect();
            let prefixed = matches!(self.peek(0), Some('\'' | '"'))
                && matches!(
                    word.to_ascii_lowercase().as_str(),
                    "r" | "u" | "b" | "f" | "br" | "rb" | "fr" | "rf"
                );
            if prefixed {
                return self.string(&word);
            }
            self.push(Kind::Name, word);
            Ok(())
        }

        fn number(&mut self) {
            let start = self.pos;
            let hex = self.take_str(2).eq_ignore_ascii_case("0x");
            let mut prev = '\0';
            while let Some(c) = self.peek(0) {
                let exponent_sign = (c == '+' || c == '-') && !hex && matches!(prev, 'e' | 'E');
                if !(c.is_alphanumeric() || c == '_' || c == '.' || exponent_sign) {
                    break;
                }
                prev = c;
                self.pos += 1;
            }
            let text: String = self
                .chars
                .get(start..self.pos)
                .unwrap_or_default()
                .iter()
                .collect();
            self.push(Kind::Num, text);
        }

        fn operator(&mut self, c: char) -> Result<(), ScanError> {
            let three = self.chars.get(self.pos..self.pos + 3);
            let two = self.chars.get(self.pos..self.pos + 2);
            let chosen = OPS3
                .iter()
                .find(|op| three.is_some_and(|chars| chars.iter().copied().eq(op.chars())))
                .or_else(|| {
                    OPS2.iter()
                        .find(|op| two.is_some_and(|chars| chars.iter().copied().eq(op.chars())))
                });
            let text = if let Some(op) = chosen {
                (*op).to_owned()
            } else if OPS1.contains(c) {
                c.to_string()
            } else if matches!(c, '?' | '$' | '`' | '!') {
                return scan_error(self.line, "invalid syntax");
            } else {
                return scan_error(
                    self.line,
                    format!("invalid character '{c}' (U+{:04X})", u32::from(c)),
                );
            };
            self.pos += text.chars().count();
            match c {
                '(' | '[' | '{' if text.len() == 1 => self.brackets.push((c, self.line)),
                ')' | ']' | '}' if text.len() == 1 => self.close(c)?,
                _ => {}
            }
            self.push(Kind::Op, text);
            Ok(())
        }

        fn close(&mut self, c: char) -> Result<(), ScanError> {
            let expected = match c {
                ')' => '(',
                ']' => '[',
                _ => '{',
            };
            match self.brackets.pop() {
                None => scan_error(self.line, format!("unmatched '{c}'")),
                Some((open, _)) if open != expected => scan_error(
                    self.line,
                    format!(
                        "closing parenthesis '{c}' does not match opening parenthesis '{open}'"
                    ),
                ),
                Some(_) => Ok(()),
            }
        }

        fn unterminated(&self, start_line: usize, triple: bool) -> ScanError {
            let kind = if triple {
                "triple-quoted string"
            } else {
                "string"
            };
            ScanError {
                line: start_line,
                msg: format!(
                    "unterminated {kind} literal (detected at line {})",
                    self.line
                ),
            }
        }

        /// Consume an f-string whose fields may contain the same quote (PEP 701).
        fn fstring(
            &mut self,
            prefix: &str,
            quote: char,
            triple: bool,
            start_line: usize,
        ) -> Result<(), ScanError> {
            let width = if triple { 3 } else { 1 };
            let Some(end) = fstring_end(self.chars, self.pos + width, quote, triple) else {
                self.line += self
                    .chars
                    .iter()
                    .skip(self.pos)
                    .filter(|c| **c == '\n')
                    .count();
                return Err(self.unterminated(start_line, triple));
            };
            let body: String = self
                .chars
                .get(self.pos + width..end - width)
                .unwrap_or_default()
                .iter()
                .collect();
            self.line += body.matches('\n').count();
            self.pos = end;
            self.finish_string(prefix, body, start_line)
        }

        fn string(&mut self, prefix: &str) -> Result<(), ScanError> {
            let start_line = self.line;
            let Some(quote) = self.peek(0) else {
                return scan_error(start_line, "invalid syntax");
            };
            let triple = self.peek(1) == Some(quote) && self.peek(2) == Some(quote);
            if prefix.to_ascii_lowercase().contains('f') {
                return self.fstring(prefix, quote, triple, start_line);
            }
            let width = if triple { 3 } else { 1 };
            self.pos += width;
            let mut body = String::new();
            loop {
                let Some(c) = self.peek(0) else {
                    return Err(self.unterminated(start_line, triple));
                };
                let closes = c == quote
                    && (!triple || (self.peek(1) == Some(quote) && self.peek(2) == Some(quote)));
                if closes {
                    self.pos += width;
                    break;
                }
                if c == '\n' {
                    if !triple {
                        return Err(self.unterminated(start_line, false));
                    }
                    self.line += 1;
                }
                body.push(c);
                self.pos += 1;
                if c == '\\'
                    && let Some(next) = self.peek(0)
                {
                    if next == '\n' {
                        self.line += 1;
                    }
                    body.push(next);
                    self.pos += 1;
                }
            }
            self.finish_string(prefix, body, start_line)
        }

        fn finish_string(
            &mut self,
            prefix: &str,
            body: String,
            start_line: usize,
        ) -> Result<(), ScanError> {
            let lower = prefix.to_ascii_lowercase();
            let raw = lower.contains('r');
            if lower.contains('b') {
                self.current.push(Tok::new(Kind::Bytes, body, start_line));
            } else if lower.contains('f') {
                let tokens = expand_fstring(&body, raw, start_line)?;
                self.current.extend(tokens);
            } else {
                let text = if raw { body } else { decode_escapes(&body) };
                self.current.push(Tok::new(Kind::Str, text, start_line));
            }
            Ok(())
        }
    }

    /// Flag the pattern of a `case` clause (`case` is a soft keyword: only a line with a
    /// top-level colon whose second token cannot continue an expression statement counts).
    fn mark_case_pattern(tokens: &mut [Tok]) {
        let starts_case = tokens.first().is_some_and(|tok| tok.is_name("case"))
            && tokens.get(1).is_some_and(|tok| {
                let continues = tok.kind == Kind::Op
                    && (tok.text.ends_with('=')
                        || matches!(tok.text.as_str(), ":" | "." | "," | ";" | ")" | "]" | "}"));
                !continues && !tok.is_name("in") && !tok.is_name("is")
            });
        if !starts_case || top_level_index(tokens, |tok| tok.is_op(":")).is_none() {
            return;
        }
        let end = top_level_index(tokens, |tok| tok.is_op(":") || tok.is_name("if"))
            .unwrap_or(tokens.len());
        for tok in tokens.iter_mut().take(end) {
            tok.pattern = true;
        }
    }

    fn closes_at(chars: &[char], i: usize, quote: char, triple: bool) -> bool {
        chars.get(i) == Some(&quote)
            && (!triple || (chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote)))
    }

    /// Index just after the closing quote of a plain string whose body starts at `start`.
    fn plain_end(chars: &[char], start: usize, quote: char, triple: bool) -> Option<usize> {
        let mut i = start;
        while let Some(&c) = chars.get(i) {
            if closes_at(chars, i, quote, triple) {
                return Some(i + if triple { 3 } else { 1 });
            }
            if c == '\n' && !triple {
                return None;
            }
            i += if c == '\\' { 2 } else { 1 };
        }
        None
    }

    /// Index just after the closing quote of an f-string whose body starts at `start`.
    fn fstring_end(chars: &[char], start: usize, quote: char, triple: bool) -> Option<usize> {
        let mut i = start;
        while let Some(&c) = chars.get(i) {
            if closes_at(chars, i, quote, triple) {
                return Some(i + if triple { 3 } else { 1 });
            }
            match c {
                '\\' => i += 2,
                '{' if chars.get(i + 1) == Some(&'{') => i += 2,
                '{' => i = field_end(chars, i + 1, quote, triple)?,
                '\n' if !triple => return None,
                _ => i += 1,
            }
        }
        None
    }

    /// Index just after the `}` closing the replacement field whose body starts at `start`.
    fn field_end(chars: &[char], start: usize, quote: char, triple: bool) -> Option<usize> {
        let mut i = start;
        let mut depth = 0_usize;
        let mut in_spec = false;
        while let Some(&c) = chars.get(i) {
            if in_spec {
                match c {
                    '{' => i = field_end(chars, i + 1, quote, triple)?,
                    '}' => return Some(i + 1),
                    '\\' => i += 2,
                    _ if closes_at(chars, i, quote, triple) => return None,
                    _ => i += 1,
                }
                continue;
            }
            match c {
                '(' | '[' | '{' => depth += 1,
                '}' if depth == 0 => return Some(i + 1),
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                ':' if depth == 0 => in_spec = true,
                '\'' | '"' => {
                    let nested_triple =
                        chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
                    let width = if nested_triple { 3 } else { 1 };
                    let is_f = chars
                        .get(..i)?
                        .iter()
                        .rev()
                        .take_while(|p| p.is_alphabetic())
                        .any(|p| p.eq_ignore_ascii_case(&'f'));
                    i = if is_f {
                        fstring_end(chars, i + width, c, nested_triple)?
                    } else {
                        plain_end(chars, i + width, c, nested_triple)?
                    };
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn logical_lines(src: &str, first_line: usize) -> Result<Vec<(usize, Vec<Tok>)>, ScanError> {
        let normalized = src.replace("\r\n", "\n").replace('\r', "\n");
        let chars: Vec<char> = normalized.chars().collect();
        let mut lexer = Lexer {
            chars: &chars,
            pos: 0,
            line: first_line,
            brackets: Vec::new(),
            lines: Vec::new(),
            current: Vec::new(),
            indent: 0,
            at_start: true,
        };
        lexer.run()?;
        Ok(lexer.lines)
    }

    fn read_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>, count: usize) -> String {
        let mut digits = String::new();
        while digits.len() < count {
            match it.peek() {
                Some(c) if c.is_ascii_hexdigit() => {
                    digits.push(*c);
                    it.next();
                }
                _ => break,
            }
        }
        digits
    }

    fn decode_numeric(it: &mut std::iter::Peekable<std::str::Chars<'_>>, marker: char) -> String {
        let count = match marker {
            'x' => 2,
            'u' => 4,
            _ => 8,
        };
        let digits = read_digits(it, count);
        let decoded = if digits.len() == count {
            u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
        } else {
            None
        };
        decoded.map_or_else(|| format!("\\{marker}{digits}"), |c| c.to_string())
    }

    fn decode_octal(it: &mut std::iter::Peekable<std::str::Chars<'_>>, first: char) -> String {
        let mut digits = first.to_string();
        while digits.len() < 3 {
            match it.peek() {
                Some(c) if ('0'..='7').contains(c) => {
                    digits.push(*c);
                    it.next();
                }
                _ => break,
            }
        }
        u32::from_str_radix(&digits, 8)
            .ok()
            .and_then(char::from_u32)
            .map_or_else(|| format!("\\{digits}"), |c| c.to_string())
    }

    /// Decode the common Python string escapes (unknown escapes are kept verbatim).
    fn decode_escapes(body: &str) -> String {
        let mut out = String::with_capacity(body.len());
        let mut it = body.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(next) = it.next() else {
                out.push('\\');
                break;
            };
            match next {
                '\n' => {}
                '\\' | '\'' | '"' => out.push(next),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'a' => out.push('\x07'),
                'b' => out.push('\x08'),
                'f' => out.push('\x0c'),
                'v' => out.push('\x0b'),
                '0'..='7' => out.push_str(&decode_octal(&mut it, next)),
                'x' | 'u' | 'U' => out.push_str(&decode_numeric(&mut it, next)),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
        out
    }

    fn flush_literal(out: &mut Vec<Tok>, literal: &mut String, raw: bool, line: usize) {
        if literal.is_empty() {
            return;
        }
        let text = if raw {
            literal.clone()
        } else {
            decode_escapes(literal)
        };
        out.push(Tok::new(Kind::FPart, text, line));
        literal.clear();
    }

    fn skip_quoted(chars: &[char], start: usize) -> usize {
        let quote = chars.get(start).copied();
        let mut i = start + 1;
        while let Some(&c) = chars.get(i) {
            if Some(c) == quote {
                return i + 1;
            }
            i += if c == '\\' { 2 } else { 1 };
        }
        i
    }

    fn is_debug_equals(chars: &[char], i: usize) -> bool {
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        if chars.get(i + 1) == Some(&'=') || matches!(prev, Some('=' | '!' | '<' | '>')) {
            return false;
        }
        let mut j = i + 1;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        matches!(chars.get(j), Some('}' | '!' | ':'))
    }

    fn ends_field_expression(chars: &[char], i: usize, c: char) -> bool {
        match c {
            '}' | ':' => true,
            '!' => chars.get(i + 1) != Some(&'='),
            '=' => is_debug_equals(chars, i),
            _ => false,
        }
    }

    fn field_expression_end(chars: &[char], start: usize) -> usize {
        let mut i = start;
        let mut depth = 0_usize;
        while let Some(&c) = chars.get(i) {
            if depth == 0 && ends_field_expression(chars, i, c) {
                break;
            }
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                '\'' | '"' => {
                    i = skip_quoted(chars, i);
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        i
    }

    fn fstring_field(
        chars: &[char],
        start: usize,
        raw: bool,
        line: usize,
    ) -> Result<(usize, Vec<Tok>), ScanError> {
        let mut i = field_expression_end(chars, start);
        let expr: String = chars.get(start..i).unwrap_or_default().iter().collect();
        if expr.py_strip().is_empty() {
            return scan_error(line, "f-string: valid expression required before '}'");
        }
        let mut tokens = logical_lines(&format!("({expr})"), line)?
            .into_iter()
            .next()
            .map(|(_, toks)| toks)
            .unwrap_or_default();
        if let Some(first) = tokens.first_mut() {
            first.synth = true;
        }
        if let Some(last) = tokens.last_mut() {
            last.synth = true;
        }
        if chars.get(i) == Some(&'=') {
            i += 1;
            while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                i += 1;
            }
        }
        if chars.get(i) == Some(&'!') {
            while chars.get(i).is_some_and(|c| *c != ':' && *c != '}') {
                i += 1;
            }
        }
        if chars.get(i) == Some(&':') {
            let mut j = i + 1;
            let mut nest = 0_usize;
            while let Some(&c) = chars.get(j) {
                match c {
                    '{' => nest += 1,
                    '}' if nest == 0 => break,
                    '}' => nest -= 1,
                    _ => {}
                }
                j += 1;
            }
            let spec: String = chars.get(i + 1..j).unwrap_or_default().iter().collect();
            tokens.extend(expand_fstring(&spec, raw, line)?);
            i = j;
        }
        if chars.get(i) != Some(&'}') {
            return scan_error(line, "f-string: expecting '}'");
        }
        Ok((i + 1, tokens))
    }

    fn expand_fstring(body: &str, raw: bool, line: usize) -> Result<Vec<Tok>, ScanError> {
        let chars: Vec<char> = body.chars().collect();
        let mut out = Vec::new();
        let mut literal = String::new();
        let mut i = 0;
        while let Some(&c) = chars.get(i) {
            let next = chars.get(i + 1).copied();
            match c {
                '{' if next == Some('{') => {
                    literal.push('{');
                    i += 2;
                }
                '}' if next == Some('}') => {
                    literal.push('}');
                    i += 2;
                }
                '{' => {
                    flush_literal(&mut out, &mut literal, raw, line);
                    let (end, tokens) = fstring_field(&chars, i + 1, raw, line)?;
                    out.extend(tokens);
                    i = end;
                }
                '\\' if !raw && next == Some('N') && chars.get(i + 2) == Some(&'{') => {
                    while chars.get(i).is_some_and(|ch| *ch != '}') {
                        literal.push(chars.get(i).copied().unwrap_or_default());
                        i += 1;
                    }
                    literal.push('}');
                    i += 1;
                }
                _ => {
                    literal.push(c);
                    i += 1;
                }
            }
        }
        flush_literal(&mut out, &mut literal, raw, line);
        Ok(out)
    }

    /// A view into the shared source tokens.
    #[derive(Debug, Clone)]
    pub(crate) struct TokenRange {
        source: Rc<[Tok]>,
        range: Range<usize>,
    }

    impl Deref for TokenRange {
        type Target = [Tok];

        fn deref(&self) -> &Self::Target {
            self.source.get(self.range.clone()).unwrap_or_default()
        }
    }

    /// A function definition with every token it spans (decorators, header, body).
    #[derive(Debug, Clone)]
    pub(crate) struct FuncDef {
        /// Function name.
        pub(crate) name: String,
        /// Line of the `def` (or `async`) keyword.
        pub(crate) line: usize,
        /// Enclosing class/function names followed by this function's own name.
        pub(crate) symbols: Vec<String>,
        /// Decorator, header and body tokens.
        pub(crate) tokens: TokenRange,
        def_index: usize,
    }

    struct Open {
        indent: usize,
        name: String,
        func: Option<usize>,
    }

    fn declaration(tokens: &[Tok]) -> Option<(bool, usize)> {
        let first = tokens.first()?;
        let (is_class, name_idx) = if first.is_name("class") {
            (true, 1)
        } else if first.is_name("def") {
            (false, 1)
        } else if first.is_name("async") && tokens.get(1)?.is_name("def") {
            (false, 2)
        } else {
            return None;
        };
        tokens
            .get(name_idx)
            .filter(|tok| tok.kind == Kind::Name)
            .map(|_| (is_class, name_idx))
    }

    /// Find every function definition (at any nesting depth) in `src`.
    pub(crate) fn function_defs(src: &str) -> Result<Vec<FuncDef>, ScanError> {
        let mut defs: Vec<FuncDef> = Vec::new();
        let mut stack: Vec<Open> = Vec::new();
        let mut tokens = Vec::new();
        let mut lines = Vec::new();
        for (indent, toks) in logical_lines(src, 1)? {
            let start = tokens.len();
            tokens.extend(toks);
            lines.push((indent, start..tokens.len()));
        }
        let source: Rc<[Tok]> = tokens.into();
        let mut decorators = None;
        let mut decorator_indent = 0;
        for (indent, range) in lines {
            let toks = source.get(range.clone()).unwrap_or_default();
            while stack.last().is_some_and(|open| open.indent >= indent) {
                if let Some(def) = stack
                    .pop()
                    .and_then(|open| open.func)
                    .and_then(|index| defs.get_mut(index))
                {
                    def.tokens.range.end = range.start;
                }
            }
            if toks.first().is_some_and(|tok| tok.is_op("@")) {
                decorator_indent = indent;
                decorators.get_or_insert(range.start);
                continue;
            }
            if let Some((is_class, name_idx)) = declaration(toks) {
                let name = toks
                    .get(name_idx)
                    .map(|t| t.text.clone())
                    .unwrap_or_default();
                let mut symbols: Vec<String> = stack.iter().map(|o| o.name.clone()).collect();
                symbols.push(name.clone());
                let func = if is_class {
                    None
                } else {
                    let start = if decorator_indent == indent {
                        decorators.take().unwrap_or(range.start)
                    } else {
                        range.start
                    };
                    let def_index = range.start - start + name_idx.saturating_sub(1);
                    let line = toks.first().map_or(0, |t| t.line);
                    defs.push(FuncDef {
                        name: name.clone(),
                        line,
                        symbols,
                        tokens: TokenRange {
                            source: Rc::clone(&source),
                            range: start..source.len(),
                        },
                        def_index,
                    });
                    Some(defs.len().saturating_sub(1))
                };
                stack.push(Open { indent, name, func });
            }
            decorators = None;
        }
        Ok(defs)
    }

    fn is_open(tok: &Tok) -> bool {
        tok.kind == Kind::Op && matches!(tok.text.as_str(), "(" | "[" | "{")
    }

    fn is_close(tok: &Tok) -> bool {
        tok.kind == Kind::Op && matches!(tok.text.as_str(), ")" | "]" | "}")
    }

    fn matching_close(tokens: &[Tok], open: usize) -> Option<usize> {
        if !is_open(tokens.get(open)?) {
            return None;
        }
        let mut depth = 0_usize;
        for (offset, tok) in tokens.iter().enumerate().skip(open) {
            if is_open(tok) {
                depth += 1;
            } else if is_close(tok) {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
        }
        None
    }

    fn matching_open(tokens: &[Tok], close: usize) -> Option<usize> {
        let mut depth = 0_usize;
        for offset in (0..=close).rev() {
            let tok = tokens.get(offset)?;
            if is_close(tok) {
                depth += 1;
            } else if is_open(tok) {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
        }
        None
    }

    fn split_top_level<'t>(tokens: &'t [Tok], separator: &str) -> Vec<&'t [Tok]> {
        let mut parts = Vec::new();
        let mut depth = 0_usize;
        let mut start = 0;
        for (index, tok) in tokens.iter().enumerate() {
            if is_open(tok) {
                depth += 1;
            } else if is_close(tok) {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && tok.is_op(separator) {
                parts.push(tokens.get(start..index).unwrap_or_default());
                start = index + 1;
            }
        }
        parts.push(tokens.get(start..).unwrap_or_default());
        parts
    }

    /// Like `split_top_level(.., ",")` but commas in a `lambda` default's parameter list stay.
    fn split_parameters(tokens: &[Tok]) -> Vec<&[Tok]> {
        let mut parts = Vec::new();
        let mut depth = 0_usize;
        let mut lambdas = 0_usize;
        let mut start = 0;
        for (index, tok) in tokens.iter().enumerate() {
            if is_open(tok) {
                depth += 1;
            } else if is_close(tok) {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && tok.is_name("lambda") {
                lambdas += 1;
            } else if depth == 0 && lambdas > 0 && tok.is_op(":") {
                lambdas -= 1;
            } else if depth == 0 && lambdas == 0 && tok.is_op(",") {
                parts.push(tokens.get(start..index).unwrap_or_default());
                start = index + 1;
            }
        }
        parts.push(tokens.get(start..).unwrap_or_default());
        parts
    }

    fn top_level_index(tokens: &[Tok], matches: impl Fn(&Tok) -> bool) -> Option<usize> {
        let mut depth = 0_usize;
        for (index, tok) in tokens.iter().enumerate() {
            if is_open(tok) {
                depth += 1;
            } else if is_close(tok) {
                depth = depth.saturating_sub(1);
            } else if depth == 0 && matches(tok) {
                return Some(index);
            }
        }
        None
    }

    fn parameter_name(chunk: &[Tok]) -> Option<String> {
        let first = chunk.first()?;
        match first.kind {
            Kind::Name => Some(first.text.clone()),
            _ => None,
        }
    }

    impl FuncDef {
        /// Names of positional-only, positional and keyword-only parameters in order.
        pub(crate) fn parameters(&self) -> Vec<String> {
            let mut open = self.def_index + 2;
            if self.tokens.get(open).is_some_and(|tok| tok.is_op("[")) {
                open = matching_close(&self.tokens, open).map_or(open, |close| close + 1);
            }
            let Some(close) = matching_close(&self.tokens, open) else {
                return Vec::new();
            };
            let inner = self.tokens.get(open + 1..close).unwrap_or_default();
            split_parameters(inner)
                .into_iter()
                .filter_map(parameter_name)
                .collect()
        }

        /// Symbol path joined with dots.
        pub(crate) fn qualified_name(&self) -> String {
            self.symbols.join(".")
        }
    }

    fn constant_key(element: &[Tok]) -> Option<String> {
        let colon = top_level_index(element, |tok| tok.is_op(":"))?;
        let key = element.get(..colon)?;
        if key.is_empty() || key.iter().any(|tok| tok.kind != Kind::Str) {
            return None;
        }
        Some(key.iter().map(|tok| tok.text.as_str()).collect())
    }

    fn dict_literal_keys(inner: &[Tok]) -> Vec<String> {
        if top_level_index(inner, |tok| tok.is_name("for")).is_some() {
            return Vec::new();
        }
        let elements = split_top_level(inner, ",");
        let is_dict = elements.first().is_some_and(|element| {
            element.first().is_some_and(|tok| tok.is_op("**"))
                || top_level_index(element, |tok| tok.is_op(":")).is_some()
        });
        if !is_dict {
            return Vec::new();
        }
        elements.into_iter().filter_map(constant_key).collect()
    }

    /// String-literal keys of every dict display in `tokens`.
    pub(crate) fn literal_dict_keys(tokens: &[Tok]) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut closes = vec![None; tokens.len()];
        let mut stack = Vec::new();
        for (index, tok) in tokens.iter().enumerate() {
            if is_open(tok) {
                stack.push(index);
                continue;
            }
            if !is_close(tok) {
                continue;
            }
            if let Some(slot) = stack.pop().and_then(|open| closes.get_mut(open)) {
                *slot = Some(index);
            }
        }
        for (index, tok) in tokens.iter().enumerate() {
            if !tok.is_op("{") || tok.pattern {
                continue;
            }
            let Some(close) = closes.get(index).copied().flatten() else {
                continue;
            };
            keys.extend(dict_literal_keys(
                tokens.get(index + 1..close).unwrap_or_default(),
            ));
        }
        keys
    }

    /// Every string constant (adjacent literals merged) with its starting line.
    pub(crate) fn string_constants(tokens: &[Tok]) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let mut previous_was_str = false;
        for tok in tokens {
            match tok.kind {
                Kind::Str if previous_was_str => {
                    if let Some(last) = out.last_mut() {
                        last.0.push_str(&tok.text);
                    }
                }
                Kind::Str | Kind::FPart => out.push((tok.text.clone(), tok.line)),
                _ => {}
            }
            previous_was_str = tok.kind == Kind::Str;
        }
        out
    }

    /// Number of `return` statements.
    pub(crate) fn return_count(tokens: &[Tok]) -> usize {
        tokens.iter().filter(|tok| tok.is_name("return")).count()
    }

    fn is_keyword(text: &str) -> bool {
        KEYWORDS.contains(&text) || text == "yield"
    }

    fn is_call_paren(tokens: &[Tok], index: usize) -> bool {
        let Some(tok) = tokens.get(index) else {
            return false;
        };
        if !tok.is_op("(") || tok.synth {
            return false;
        }
        let Some(prev) = index.checked_sub(1).and_then(|p| tokens.get(p)) else {
            return false;
        };
        match prev.kind {
            Kind::Name => {
                let after_def = index
                    .checked_sub(2)
                    .and_then(|p| tokens.get(p))
                    .is_some_and(|before| before.is_name("def"));
                !is_keyword(&prev.text) && !after_def
            }
            Kind::Str | Kind::Bytes | Kind::FPart => true,
            Kind::Op => prev.text == ")" || prev.text == "]",
            Kind::Num => false,
        }
    }

    fn ends_primary(tok: &Tok) -> bool {
        match tok.kind {
            Kind::Name => !is_keyword(&tok.text),
            Kind::Str | Kind::Bytes | Kind::FPart => true,
            Kind::Op => is_close(tok),
            Kind::Num => false,
        }
    }

    fn primary_start(tokens: &[Tok], mut end: usize) -> usize {
        while tokens.get(end).is_some_and(is_close) {
            let Some(open) = matching_open(tokens, end) else {
                return end;
            };
            match open.checked_sub(1) {
                Some(prev) if tokens.get(prev).is_some_and(ends_primary) => end = prev,
                _ => return open,
            }
        }
        end
    }

    fn chain_start_line(tokens: &[Tok], attribute: usize) -> usize {
        let mut index = attribute;
        while index >= 2 && tokens.get(index - 1).is_some_and(|tok| tok.is_op(".")) {
            index = primary_start(tokens, index - 2);
        }
        tokens.get(index).map_or(0, |tok| tok.line)
    }

    fn is_keyword_argument(tokens: &[Tok], index: usize, in_call: bool) -> bool {
        if !in_call || !tokens.get(index + 1).is_some_and(|tok| tok.is_op("=")) {
            return false;
        }
        index
            .checked_sub(1)
            .and_then(|p| tokens.get(p))
            .is_some_and(|prev| prev.is_op("(") || prev.is_op(","))
    }

    fn declares_name(tokens: &[Tok], index: usize) -> bool {
        index
            .checked_sub(1)
            .and_then(|p| tokens.get(p))
            .is_some_and(|prev| prev.is_name("def") || prev.is_name("class"))
    }

    fn name_line(tokens: &[Tok], index: usize) -> usize {
        let after_dot = index
            .checked_sub(1)
            .and_then(|p| tokens.get(p))
            .is_some_and(|prev| prev.is_op("."));
        if after_dot {
            chain_start_line(tokens, index)
        } else {
            tokens.get(index).map_or(0, |tok| tok.line)
        }
    }

    /// Names, parameters and attributes accepted by `is_term`, with the line Python reports.
    pub(crate) fn identifier_hits(
        tokens: &[Tok],
        is_term: impl Fn(&str) -> bool,
    ) -> Vec<(String, usize)> {
        let mut hits = Vec::new();
        let mut parens: Vec<bool> = Vec::new();
        for (index, tok) in tokens.iter().enumerate() {
            if is_open(tok) {
                parens.push(is_call_paren(tokens, index));
            } else if is_close(tok) {
                parens.pop();
            } else if tok.kind == Kind::Name && is_term(&tok.text) {
                let in_call = parens.last().copied().unwrap_or(false);
                if !declares_name(tokens, index) && !is_keyword_argument(tokens, index, in_call) {
                    hits.push((tok.text.clone(), name_line(tokens, index)));
                }
            }
        }
        hits
    }
}
