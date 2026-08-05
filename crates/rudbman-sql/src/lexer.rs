//! The lexer: bytes in, classified spans out.
//!
//! # Shape of the API
//!
//! There are three entry points and they are the same scanner underneath.
//! [`lex_line`] is the one the editor calls, [`lex`] is the one tests and
//! whole-file work call, and [`Lexer`] is the iterator both are built on — the
//! statement splitter uses it directly so that splitting a 100MB script does not
//! first allocate a vector of every token in it.
//!
//! # Why a line at a time
//!
//! Re-lexing a 100MB buffer on every keystroke is not an option, and the way out
//! is the oldest one in the trade: lex a line at a time, and remember what state
//! each line *ended* in. [`LineState`] is that memory. Give [`lex_line`] the
//! state the previous line ended in and it hands back the tokens of this line and
//! the state this line ends in.
//!
//! That makes the editor's job an easy loop. After an edit on line *n*, re-lex
//! from line *n* downwards and stop at the first line whose *new* end state
//! equals its old one — from there the cached tokens below are still correct.
//! [`LineState`] is `Copy + Eq` precisely so that comparison is the whole
//! termination condition. In the ordinary case — an edit that opens no comment
//! and no quote — the loop stops after one line.
//!
//! What a line can end in the middle of is: a block comment (with its nesting
//! depth, for the dialects that nest), a quoted run of any of the five quoting
//! forms, or a PostgreSQL dollar quote (with its tag). Nothing else crosses a
//! line, so nothing else is in the state.
//!
//! # Guarantees
//!
//! * **Tokens tile the input.** The spans are in order, adjacent, and cover every
//!   byte from `0` to `input.len()` with no gaps — whitespace is a token like any
//!   other. A renderer can walk them and never have to ask what happened between
//!   two of them.
//! * **Every span is on a character boundary**, so slicing the source with one
//!   never panics.
//! * **Line-at-a-time agrees with all-at-once.** Lexing a source line by line and
//!   lexing it in one call classify every byte identically. Multi-line
//!   constructs are one token in the second case and one token per line in the
//!   first, but no byte changes its [`TokenKind`]. The integration test
//!   `line_state_round_trip` holds this down; it is what makes the incremental
//!   path trustworthy.
//!
//! # What it does not do
//!
//! It does not parse. There is no AST, no notion of a `SELECT` list, and no
//! error recovery, because syntax highlighting and statement splitting need
//! neither and every SQL dialect would need its own grammar to get them (the
//! architecture document, §7.4, is where that decision is written down).
//! Anything that looks like classification-by-context here — a word followed by
//! `(` being a function — is one byte of lookahead and nothing more.

use crate::dialect::Dialect;

/// What a run of characters is.
///
/// The names line up with the syntax slots of `rudbman-ui`'s editor palette, so
/// the editor's mapping from token to color is a `match` with no thinking in it.
/// The palette has no `parameter` or `quoted_identifier` slot of its own; the
/// editor decides what to do with those two (`parameter` next to `number`,
/// quoted identifiers next to plain ones, is the obvious reading), and this
/// crate's job is only to keep the distinction available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenKind {
    /// Spaces, tabs, newlines. Emitted rather than skipped so that the token
    /// stream covers the whole input.
    Whitespace,
    /// `-- ...`, `# ...`, `/* ... */`.
    Comment,
    /// A reserved word of the dialect: `SELECT`, `ILIKE`, `NOLOCK`.
    Keyword,
    /// A type name: `VARCHAR`, `NUMBER`, `JSONB`.
    Type,
    /// A word immediately followed by `(` that is not a keyword or a type.
    ///
    /// This catches built-ins and the user's own routines with one rule, at the
    /// price of calling `foo` in `foo (x)` a function when it is a table alias
    /// in a join — a trade every token-level highlighter makes.
    Function,
    /// A bare table, column or alias name.
    Identifier,
    /// An identifier in quotes: `"users"`, `` `users` ``, `[users]`.
    QuotedIdentifier,
    /// A string literal, including `E'...'`, `N'...'`, `X'...'` and dollar
    /// quotes. In MySQL, `"..."` is one of these too.
    String,
    /// A numeric literal: `42`, `1.5e-3`, `0xFF`.
    Number,
    /// A bind parameter: `?`, `$1`, `:name`, `@name`.
    Parameter,
    /// An operator: `=`, `<>`, `||`, `::`, `->>`.
    Operator,
    /// A comma, semicolon, dot, or bracket that is not quoting an identifier.
    Punctuation,
    /// A character that starts nothing this dialect has.
    ///
    /// One character wide, so the scanner always makes progress. Backticks
    /// outside MySQL, stray control characters, a lone `\` — the editor can
    /// mark them or ignore them.
    Error,
}

impl TokenKind {
    /// Whether this token carries no meaning for the statement splitter.
    ///
    /// Whitespace and comments, in other words — the two kinds
    /// [`crate::split_statements`] steps over when deciding whether a fragment
    /// holds any actual SQL.
    pub const fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment)
    }
}

/// One classified span, as a half-open byte range into the lexed input.
///
/// Offsets are relative to the string that was lexed: absolute for [`lex`] and
/// [`Lexer`], relative to the line for [`lex_line`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token {
    /// What the span is.
    pub kind: TokenKind,
    /// First byte of the span.
    pub start: usize,
    /// One past the last byte of the span.
    pub end: usize,
}

impl Token {
    /// Length of the span in bytes. Never zero.
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Always false; a zero-length token is never produced.
    ///
    /// Present because `len` without `is_empty` is a lint, and answering it
    /// honestly is better than allowing the question.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The span as a range, for slicing.
    pub const fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// The text of the span, taken from the string this token was lexed from.
    ///
    /// # Panics
    ///
    /// If `source` is not that string, or a prefix of it long enough to hold the
    /// span.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range()]
    }
}

/// Which of the quoting forms a line ended inside.
///
/// The closing delimiter and the escaping rule, and nothing about what the run
/// *means* — whether `"..."` is a string or an identifier is a question for the
/// dialect at the time the token is emitted, not something to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum QuoteKind {
    /// `'...'`, where `''` is an escaped quote.
    Single,
    /// `'...'` where a backslash escapes as well: MySQL's default, and
    /// PostgreSQL's `E'...'`.
    SingleEscaped,
    /// `"..."`.
    Double,
    /// `` `...` ``.
    Backtick,
    /// `[...]`, closed by `]` and escaped by `]]`.
    Bracket,
}

/// The tag of an open PostgreSQL dollar quote, in a form that is `Copy`.
///
/// A tag is compared by its length and its FNV-1a hash rather than by its bytes,
/// which is what keeps [`LineState`] a fixed sixteen bytes no matter how long
/// the tag is. Two *different* tags of the same length would have to collide in
/// a full 64 bits for a quote to close in the wrong place; the alternative — an
/// inline buffer — would put a hard limit on tag length instead, and a limit
/// that silently mis-lexes is worse than a bound nobody will reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DollarTag {
    /// Length of the tag in bytes, `$` delimiters excluded. Zero for `$$`.
    len: u32,
    /// FNV-1a of the tag.
    hash: u64,
}

impl DollarTag {
    /// Hash a tag.
    fn new(tag: &[u8]) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in tag {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            len: tag.len() as u32,
            hash,
        }
    }
}

/// What the scanner was in the middle of when the input ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum Carry {
    /// Between tokens. The state every buffer starts in.
    #[default]
    None,
    /// Inside `/* ... */`, at the given nesting depth (always at least one).
    ///
    /// The depth is only ever above one for the dialects whose block comments
    /// nest; it saturates rather than overflowing, so a file with 65535 nested
    /// openers loses track of the depth instead of panicking.
    BlockComment(u16),
    /// Inside one of the quoting forms.
    Quoted(QuoteKind),
    /// Inside `$tag$ ... $tag$`.
    DollarQuote(DollarTag),
}

/// What a line ended in the middle of — the whole of what one line of a buffer
/// needs to know about the lines before it.
///
/// Opaque, sixteen bytes, `Copy` and `Eq`. The `Eq` is the point: an editor
/// re-lexing after an edit walks down the buffer and stops at the first line
/// whose end state is unchanged.
///
/// ```
/// use rudbman_sql::{Dialect, LineState, lex_line};
///
/// let pg = Dialect::POSTGRES;
/// let (_, after) = lex_line("create function f() returns int as $body$", LineState::START, &pg);
/// assert_ne!(after, LineState::START);          // the dollar quote is open
///
/// let (_, after) = lex_line("  select 1;", after, &pg);
/// assert_ne!(after, LineState::START);          // still open
///
/// let (_, after) = lex_line("$body$ language sql;", after, &pg);
/// assert_eq!(after, LineState::START);          // and closed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LineState(Carry);

impl LineState {
    /// The state a buffer starts in, and the state a line ends in when every
    /// token on it is closed.
    pub const START: Self = Self(Carry::None);

    /// Whether this is [`Self::START`] — nothing carried over.
    pub const fn is_start(self) -> bool {
        matches!(self.0, Carry::None)
    }

    /// The kind of the token a line beginning in this state opens inside, if
    /// any.
    ///
    /// [`TokenKind::Comment`] for an open block comment, [`TokenKind::String`]
    /// or [`TokenKind::QuotedIdentifier`] for the quoting forms — which of the
    /// two `"..."` is depends on the dialect, hence the argument. Useful to an
    /// editor that wants to fold a block comment or dim a line inside one
    /// without lexing it first.
    pub const fn carried_kind(self, dialect: &Dialect) -> Option<TokenKind> {
        match self.0 {
            Carry::None => None,
            Carry::BlockComment(_) => Some(TokenKind::Comment),
            Carry::DollarQuote(_) => Some(TokenKind::String),
            Carry::Quoted(q) => Some(quote_token_kind(q, dialect)),
        }
    }
}

/// Whether a quoting form yields a string or a quoted identifier here.
const fn quote_token_kind(quote: QuoteKind, dialect: &Dialect) -> TokenKind {
    match quote {
        QuoteKind::Single | QuoteKind::SingleEscaped => TokenKind::String,
        // The one form whose meaning is a dialect question: MySQL reads `"..."`
        // as a string, everyone else as an identifier.
        QuoteKind::Double => {
            if dialect.syntax().double_quoted_strings {
                TokenKind::String
            } else {
                TokenKind::QuotedIdentifier
            }
        }
        QuoteKind::Backtick | QuoteKind::Bracket => TokenKind::QuotedIdentifier,
    }
}

/// Multi-character operators, longest first.
///
/// The union across the dialects rather than a per-dialect table: an operator
/// one vendor does not have cannot appear in a script written for it, and a
/// wrong guess here changes a color and never a span boundary. Order matters —
/// the scanner takes the first match, so `->>` has to be tried before `->`.
const OPERATORS: &[&str] = &[
    // three characters
    "<=>", "->>", "#>>", "!~*", // two characters
    "<>", "!=", "<=", ">=", "||", "::", "->", "=>", ":=", "<<", ">>", "@>", "<@", "&&", "#>", "#-",
    "!~", "~*", "~=", "!<", "!>", "**", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
];

/// The token stream of a string, as an iterator.
///
/// Yields tokens in order until the input is used up, at which point
/// [`Lexer::state`] is the state the input ended in. Both [`lex`] and
/// [`lex_line`] are thin wrappers over this; use it directly to avoid
/// materializing the tokens of a large buffer.
///
/// ```
/// use rudbman_sql::{Dialect, Lexer, TokenKind};
///
/// let sql = "select 1 -- one\n";
/// let kinds: Vec<_> = Lexer::new(sql, &Dialect::GENERIC)
///     .filter(|t| !t.kind.is_trivia())
///     .map(|t| t.kind)
///     .collect();
/// assert_eq!(kinds, [TokenKind::Keyword, TokenKind::Number]);
/// ```
pub struct Lexer<'a> {
    /// The input, kept as a `str` for slicing out token text.
    src: &'a str,
    /// The same input as bytes; every decision the scanner makes is a byte test.
    bytes: &'a [u8],
    /// Offset of the next byte to look at.
    pos: usize,
    /// What the scanner is in the middle of.
    state: LineState,
    /// The rules in force.
    dialect: Dialect,
}

impl<'a> Lexer<'a> {
    /// Lex `source` from the beginning, in [`LineState::START`].
    pub fn new(source: &'a str, dialect: &Dialect) -> Self {
        Self::with_state(source, LineState::START, dialect)
    }

    /// Lex `source` as the continuation of something that ended in `state`.
    pub fn with_state(source: &'a str, state: LineState, dialect: &Dialect) -> Self {
        Self {
            src: source,
            bytes: source.as_bytes(),
            pos: 0,
            state,
            dialect: *dialect,
        }
    }

    /// The state of the scanner: [`LineState::START`] between tokens, and once
    /// the iterator is exhausted, the state the input ended in.
    pub const fn state(&self) -> LineState {
        self.state
    }

    /// Offset of the next byte to be scanned.
    pub const fn offset(&self) -> usize {
        self.pos
    }

    /// Byte at `i`, or `None` past the end.
    fn at(&self, i: usize) -> Option<u8> {
        self.bytes.get(i).copied()
    }

    /// Finish a token that ends at `end`, and leave the scanner there.
    fn emit(&mut self, kind: TokenKind, start: usize, end: usize) -> Token {
        self.pos = end;
        Token { kind, start, end }
    }

    /// Scan one token, or `None` at the end of the input.
    fn scan(&mut self) -> Option<Token> {
        let start = self.pos;
        if start >= self.bytes.len() {
            return None;
        }

        // A carried-over construct swallows the front of the input whole, so it
        // is answered before anything else can look at the first byte.
        match self.state.0 {
            Carry::BlockComment(depth) => return Some(self.block_comment_body(start, depth)),
            Carry::Quoted(quote) => return Some(self.quoted_body(start, quote)),
            Carry::DollarQuote(tag) => return Some(self.dollar_body(start, tag)),
            Carry::None => {}
        }

        let syntax = self.dialect.syntax();
        let b = self.bytes[start];
        let token = match b {
            b if b.is_ascii_whitespace() => {
                let mut i = start;
                while self.at(i).is_some_and(|b| b.is_ascii_whitespace()) {
                    i += 1;
                }
                self.emit(TokenKind::Whitespace, start, i)
            }

            // `--`, unless the dialect wants a space after it and there is none.
            b'-' if self.at(start + 1) == Some(b'-')
                && (!syntax.dash_dash_needs_space
                    || self
                        .at(start + 2)
                        .is_none_or(|b| b.is_ascii_whitespace() || b.is_ascii_control())) =>
            {
                self.line_comment(start)
            }

            b'/' if self.at(start + 1) == Some(b'*') => {
                self.state = LineState(Carry::BlockComment(1));
                self.pos = start + 2;
                self.block_comment_body(start, 1)
            }

            b'#' if syntax.hash_line_comment => self.line_comment(start),

            b'\'' => self.open_quote(start, start + 1, false),
            b'"' => self.open_quote_kind(start, start + 1, QuoteKind::Double),
            b'`' if syntax.backtick_identifiers => {
                self.open_quote_kind(start, start + 1, QuoteKind::Backtick)
            }
            b'[' if syntax.bracket_identifiers => {
                self.open_quote_kind(start, start + 1, QuoteKind::Bracket)
            }

            b'$' => self.dollar(start),
            b'?' => {
                // `?1` is SQLite's numbered form of the same thing.
                let mut i = start + 1;
                while self.at(i).is_some_and(|b| b.is_ascii_digit()) {
                    i += 1;
                }
                self.emit(TokenKind::Parameter, start, i)
            }
            b':' => self.colon(start),
            b'@' if syntax.at_parameters
                && self
                    .at(start + 1)
                    .is_some_and(|b| b == b'@' || self.is_word_start(b)) =>
            {
                let mut i = start + 1;
                while self.at(i) == Some(b'@') {
                    i += 1;
                }
                while self.at(i).is_some_and(|b| self.is_word_cont(b)) {
                    i += 1;
                }
                self.emit(TokenKind::Parameter, start, i)
            }

            b'0'..=b'9' => self.number(start),
            b'.' if self.at(start + 1).is_some_and(|b| b.is_ascii_digit()) => self.number(start),

            b',' | b';' | b'(' | b')' | b'.' | b'[' | b']' => {
                self.emit(TokenKind::Punctuation, start, start + 1)
            }

            b if self.is_word_start(b) => self.word(start),
            b if is_operator_byte(b) => self.operator(start),

            // Whatever it is, it is one character of it: advancing by a whole
            // character is what keeps every offset on a boundary.
            _ => {
                let width = self.src[start..].chars().next().map_or(1, char::len_utf8);
                self.emit(TokenKind::Error, start, start + width)
            }
        };
        Some(token)
    }

    /// `--` or `#` to the end of the line, the newline excluded.
    fn line_comment(&mut self, start: usize) -> Token {
        let end = match self.src[start..].find('\n') {
            Some(offset) => start + offset,
            None => self.bytes.len(),
        };
        self.emit(TokenKind::Comment, start, end)
    }

    /// The body of a block comment, from wherever the scanner is to the `*/`
    /// that closes it or the end of the input.
    ///
    /// `start` is where the *token* begins, which is the `/*` on the line that
    /// opens it and the first byte of the line on every line after. The caller
    /// has already stepped `self.pos` past the `/*` in the first case, so that
    /// `/*/` is not read as opening and closing at once.
    fn block_comment_body(&mut self, start: usize, mut depth: u16) -> Token {
        let nests = self.dialect.syntax().nested_block_comments;
        let mut i = self.pos;
        while i < self.bytes.len() {
            if self.bytes[i] == b'*' && self.at(i + 1) == Some(b'/') {
                i += 2;
                depth -= 1;
                if depth == 0 {
                    self.state = LineState::START;
                    return self.emit(TokenKind::Comment, start, i);
                }
            } else if nests && self.bytes[i] == b'/' && self.at(i + 1) == Some(b'*') {
                i += 2;
                depth = depth.saturating_add(1);
            } else {
                i += 1;
            }
        }
        self.state = LineState(Carry::BlockComment(depth));
        self.emit(TokenKind::Comment, start, self.bytes.len())
    }

    /// Open a `'...'` string, honouring the dialect's backslash rule.
    ///
    /// `escaped` forces backslash escaping on for a `E'...'` prefix.
    fn open_quote(&mut self, start: usize, body: usize, escaped: bool) -> Token {
        let quote = if escaped || self.dialect.syntax().backslash_escapes {
            QuoteKind::SingleEscaped
        } else {
            QuoteKind::Single
        };
        self.open_quote_kind(start, body, quote)
    }

    /// Open a quoted run of any form: record the state, then scan the body.
    fn open_quote_kind(&mut self, start: usize, body: usize, quote: QuoteKind) -> Token {
        self.state = LineState(Carry::Quoted(quote));
        self.pos = body;
        self.quoted_body(start, quote)
    }

    /// The body of a quoted run, from wherever the scanner is to the closing
    /// delimiter or the end of the input.
    fn quoted_body(&mut self, start: usize, quote: QuoteKind) -> Token {
        let (close, escapes) = match quote {
            QuoteKind::Single => (b'\'', false),
            QuoteKind::SingleEscaped => (b'\'', true),
            QuoteKind::Double => (b'"', false),
            QuoteKind::Backtick => (b'`', false),
            QuoteKind::Bracket => (b']', false),
        };
        let kind = quote_token_kind(quote, &self.dialect);
        let mut i = self.pos;
        while i < self.bytes.len() {
            let b = self.bytes[i];
            // A backslash never escapes a newline: MySQL reads `\<newline>` as a
            // newline inside the string either way, and refusing to cross the
            // line here is what keeps line-at-a-time lexing identical to
            // all-at-once.
            if escapes && b == b'\\' && self.at(i + 1).is_some_and(|b| b != b'\n') {
                i += 2;
                continue;
            }
            if b == close {
                // Every one of these forms escapes its delimiter by doubling it,
                // `''` and `""` and `` `` `` and `]]` alike.
                if self.at(i + 1) == Some(close) {
                    i += 2;
                    continue;
                }
                self.state = LineState::START;
                return self.emit(kind, start, i + 1);
            }
            i += 1;
        }
        self.state = LineState(Carry::Quoted(quote));
        self.emit(kind, start, self.bytes.len())
    }

    /// `$` — a numbered parameter, a dollar quote, or part of a name.
    fn dollar(&mut self, start: usize) -> Token {
        let syntax = self.dialect.syntax();
        if syntax.numbered_parameters && self.at(start + 1).is_some_and(|b| b.is_ascii_digit()) {
            let mut i = start + 1;
            while self.at(i).is_some_and(|b| b.is_ascii_digit()) {
                i += 1;
            }
            return self.emit(TokenKind::Parameter, start, i);
        }
        if syntax.dollar_quotes
            && let Some((body, tag)) = self.read_dollar_tag(start)
        {
            self.state = LineState(Carry::DollarQuote(tag));
            self.pos = body;
            return self.dollar_body(start, tag);
        }
        // Not a quote and not a parameter: `$` is a name character in MySQL and
        // Oracle, and a lone one is harmless as an identifier anywhere else.
        self.word(start)
    }

    /// Read `$tag$` at `start`, returning where the tag's body begins and the
    /// tag itself.
    ///
    /// `None` when what follows is not a tag — a digit first, or no closing `$`
    /// on the way. PostgreSQL's rule is that a tag is an identifier or empty,
    /// and this follows it.
    fn read_dollar_tag(&self, start: usize) -> Option<(usize, DollarTag)> {
        debug_assert_eq!(self.at(start), Some(b'$'));
        let mut i = start + 1;
        while self
            .at(i)
            .is_some_and(|b| self.is_word_cont(b) && b != b'$')
        {
            i += 1;
        }
        if self.at(i) != Some(b'$') {
            return None;
        }
        let tag = &self.bytes[start + 1..i];
        if tag.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        Some((i + 1, DollarTag::new(tag)))
    }

    /// The body of a dollar quote, to the matching `$tag$` or the end of the
    /// input.
    fn dollar_body(&mut self, start: usize, tag: DollarTag) -> Token {
        let mut i = self.pos;
        while i < self.bytes.len() {
            if self.bytes[i] == b'$'
                && let Some((end, found)) = self.read_dollar_tag(i)
                && found == tag
            {
                self.state = LineState::START;
                return self.emit(TokenKind::String, start, end);
            }
            // Any other `$` is literal text, and the next one along might be the
            // terminator, so advance by one rather than past the whole run.
            i += 1;
        }
        self.state = LineState(Carry::DollarQuote(tag));
        self.emit(TokenKind::String, start, self.bytes.len())
    }

    /// `:` — a cast, an assignment, a named parameter, or punctuation.
    fn colon(&mut self, start: usize) -> Token {
        match self.at(start + 1) {
            // PostgreSQL's cast, and PL/SQL's assignment.
            Some(b':') | Some(b'=') => self.emit(TokenKind::Operator, start, start + 2),
            Some(b) if self.dialect.syntax().colon_parameters && self.is_word_start(b) => {
                let mut i = start + 1;
                while self.at(i).is_some_and(|b| self.is_word_cont(b)) {
                    i += 1;
                }
                self.emit(TokenKind::Parameter, start, i)
            }
            _ => self.emit(TokenKind::Punctuation, start, start + 1),
        }
    }

    /// A numeric literal.
    fn number(&mut self, start: usize) -> Token {
        let mut i = start;
        if self.dialect.syntax().hex_literals
            && self.bytes[start] == b'0'
            && self.at(start + 1).is_some_and(|b| b | 0x20 == b'x')
            && self.at(start + 2).is_some_and(|b| b.is_ascii_hexdigit())
        {
            i = start + 2;
            while self.at(i).is_some_and(|b| b.is_ascii_hexdigit()) {
                i += 1;
            }
            return self.emit(TokenKind::Number, start, i);
        }
        while self.at(i).is_some_and(|b| b.is_ascii_digit()) {
            i += 1;
        }
        if self.at(i) == Some(b'.') {
            i += 1;
            while self.at(i).is_some_and(|b| b.is_ascii_digit()) {
                i += 1;
            }
        }
        // An exponent counts only if a digit actually follows it, so the `e` of
        // `1e` stays an identifier of its own.
        if self.at(i).is_some_and(|b| b | 0x20 == b'e') {
            let mut j = i + 1;
            if self.at(j).is_some_and(|b| b == b'+' || b == b'-') {
                j += 1;
            }
            if self.at(j).is_some_and(|b| b.is_ascii_digit()) {
                while self.at(j).is_some_and(|b| b.is_ascii_digit()) {
                    j += 1;
                }
                i = j;
            }
        }
        self.emit(TokenKind::Number, start, i)
    }

    /// A word: a keyword, a type, a function name, a plain identifier — or the
    /// one-letter prefix of a string literal.
    fn word(&mut self, start: usize) -> Token {
        // `N'...'`, `X'...'`, `B'...'` are standard; `E'...'` is PostgreSQL's
        // and is the one that turns backslash escaping on.
        if self.at(start + 1) == Some(b'\'') {
            let prefix = self.bytes[start] | 0x20;
            let is_e = prefix == b'e' && self.dialect.syntax().e_strings;
            if is_e || matches!(prefix, b'n' | b'x' | b'b') {
                return self.open_quote(start, start + 2, is_e);
            }
        }

        let mut i = start;
        while self.at(i).is_some_and(|b| self.is_word_cont(b)) {
            i += 1;
        }
        let word = &self.src[start..i];
        let kind = if self.dialect.is_keyword(word) {
            TokenKind::Keyword
        } else if self.dialect.is_type(word) {
            TokenKind::Type
        } else if self.followed_by_call(i) {
            TokenKind::Function
        } else {
            TokenKind::Identifier
        };
        self.emit(kind, start, i)
    }

    /// Whether the next thing on this line, past any spaces, is `(`.
    ///
    /// The lookahead stops at a newline on purpose. Not because `count\n(x)` is
    /// unheard of, but because a lookahead that crossed the line would give
    /// [`lex_line`] an answer the previous line cannot know, and the agreement
    /// between line-at-a-time and all-at-once is worth more than that call.
    fn followed_by_call(&self, mut i: usize) -> bool {
        while let Some(b) = self.at(i) {
            match b {
                b' ' | b'\t' => i += 1,
                b'(' => return true,
                _ => return false,
            }
        }
        false
    }

    /// The longest operator that starts here, or the single character.
    fn operator(&mut self, start: usize) -> Token {
        let rest = &self.src[start..];
        for op in OPERATORS {
            if rest.as_bytes().starts_with(op.as_bytes()) {
                return self.emit(TokenKind::Operator, start, start + op.len());
            }
        }
        self.emit(TokenKind::Operator, start, start + 1)
    }

    /// Whether `b` can begin a name.
    ///
    /// Any byte above ASCII counts, which admits every non-ASCII identifier
    /// PostgreSQL and MySQL allow without decoding a character: a multi-byte
    /// sequence is all continuation bytes after the first, so the whole of it is
    /// taken and the span still lands on a boundary.
    fn is_word_start(&self, b: u8) -> bool {
        b.is_ascii_alphabetic()
            || b == b'_'
            || b >= 0x80
            || (b == b'#' && self.dialect.syntax().hash_identifiers)
    }

    /// Whether `b` can continue a name. `$` is in every dialect's answer:
    /// MySQL and Oracle allow it outright, and elsewhere no other rule claims a
    /// `$` that follows a name character.
    fn is_word_cont(&self, b: u8) -> bool {
        self.is_word_start(b) || b.is_ascii_digit() || b == b'$'
    }
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        self.scan()
    }
}

/// Whether `b` can begin an operator.
///
/// `#` and `@` are here as well as in their own branches of the scanner: they
/// reach this only in the dialects where they are not a comment, a name or a
/// parameter — PostgreSQL's `#>` and `@>`, for instance.
const fn is_operator_byte(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'='
            | b'<'
            | b'>'
            | b'!'
            | b'~'
            | b'^'
            | b'&'
            | b'|'
            | b'#'
            | b'@'
    )
}

/// Lex one line, continuing from the state the line before it ended in.
///
/// Returns the line's tokens — offsets relative to `line`, tiling it exactly —
/// and the state this line ends in, to be handed to the next call. Pass
/// [`LineState::START`] for the first line of a buffer.
///
/// `line` is expected to be a single line without its terminator, but nothing
/// breaks if it carries one or several: a newline is whitespace like any other.
///
/// ```
/// use rudbman_sql::{Dialect, LineState, TokenKind, lex_line};
///
/// let (tokens, end) = lex_line("select /* two", LineState::START, &Dialect::GENERIC);
/// assert_eq!(tokens.last().unwrap().kind, TokenKind::Comment);
/// assert!(!end.is_start());
///
/// let (tokens, end) = lex_line("   lines */ 1", end, &Dialect::GENERIC);
/// assert_eq!(tokens[0].kind, TokenKind::Comment); // the rest of the comment
/// assert!(end.is_start());
/// ```
pub fn lex_line(line: &str, start: LineState, dialect: &Dialect) -> (Vec<Token>, LineState) {
    let mut lexer = Lexer::with_state(line, start, dialect);
    let tokens: Vec<Token> = lexer.by_ref().collect();
    (tokens, lexer.state())
}

/// Lex a whole source string in one call.
///
/// The tokens tile `source` from `0` to `source.len()`. Multi-line constructs
/// are a single token here, where [`lex_line`] would give one per line; every
/// byte still gets the same [`TokenKind`] either way.
pub fn lex(source: &str, dialect: &Dialect) -> Vec<Token> {
    Lexer::new(source, dialect).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lex and return `(kind, text)` pairs with the whitespace dropped, which is
    /// what most of these tests want to talk about.
    fn kinds(source: &str, dialect: &Dialect) -> Vec<(TokenKind, String)> {
        lex(source, dialect)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (t.kind, t.text(source).to_string()))
            .collect()
    }

    /// The tiling guarantee, asserted directly.
    fn assert_tiles(source: &str, tokens: &[Token]) {
        let mut at = 0;
        for t in tokens {
            assert_eq!(t.start, at, "gap or overlap before {t:?}");
            assert!(t.end > t.start, "empty token {t:?}");
            assert!(source.is_char_boundary(t.start) && source.is_char_boundary(t.end));
            at = t.end;
        }
        assert_eq!(at, source.len(), "tokens stop short of the end");
    }

    #[test]
    fn classifies_a_plain_select() {
        let sql = "SELECT count(*) AS n FROM \"users\" u WHERE u.age >= 18 AND u.name LIKE 'a%';";
        assert_tiles(sql, &lex(sql, &Dialect::GENERIC));
        assert_eq!(
            kinds(sql, &Dialect::GENERIC),
            vec![
                (TokenKind::Keyword, "SELECT".into()),
                (TokenKind::Function, "count".into()),
                (TokenKind::Punctuation, "(".into()),
                (TokenKind::Operator, "*".into()),
                (TokenKind::Punctuation, ")".into()),
                (TokenKind::Keyword, "AS".into()),
                (TokenKind::Identifier, "n".into()),
                (TokenKind::Keyword, "FROM".into()),
                (TokenKind::QuotedIdentifier, "\"users\"".into()),
                (TokenKind::Identifier, "u".into()),
                (TokenKind::Keyword, "WHERE".into()),
                (TokenKind::Identifier, "u".into()),
                (TokenKind::Punctuation, ".".into()),
                (TokenKind::Identifier, "age".into()),
                (TokenKind::Operator, ">=".into()),
                (TokenKind::Number, "18".into()),
                (TokenKind::Keyword, "AND".into()),
                (TokenKind::Identifier, "u".into()),
                (TokenKind::Punctuation, ".".into()),
                (TokenKind::Identifier, "name".into()),
                (TokenKind::Keyword, "LIKE".into()),
                (TokenKind::String, "'a%'".into()),
                (TokenKind::Punctuation, ";".into()),
            ]
        );
    }

    #[test]
    fn types_are_their_own_class() {
        assert_eq!(
            kinds("cast(x as varchar2(10))", &Dialect::ORACLE)
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>(),
            vec![
                TokenKind::Keyword,     // cast
                TokenKind::Punctuation, // (
                TokenKind::Identifier,  // x
                TokenKind::Keyword,     // as
                TokenKind::Type,        // varchar2 — a type even before `(`
                TokenKind::Punctuation, // (
                TokenKind::Number,      // 10
                TokenKind::Punctuation, // )
                TokenKind::Punctuation, // )
            ]
        );
    }

    #[test]
    fn numbers() {
        let d = Dialect::GENERIC;
        assert_eq!(kinds("42", &d)[0], (TokenKind::Number, "42".into()));
        assert_eq!(kinds("1.5e-3", &d)[0], (TokenKind::Number, "1.5e-3".into()));
        assert_eq!(kinds(".5", &d)[0], (TokenKind::Number, ".5".into()));
        assert_eq!(kinds("0xFF", &d)[0], (TokenKind::Number, "0xFF".into()));
        // `e` with nothing behind it is not an exponent.
        assert_eq!(
            kinds("1e", &d),
            vec![
                (TokenKind::Number, "1".into()),
                (TokenKind::Identifier, "e".into())
            ]
        );
        // Oracle has no hex literal, so this is a number and a name.
        assert_eq!(
            kinds("0xFF", &Dialect::ORACLE),
            vec![
                (TokenKind::Number, "0".into()),
                (TokenKind::Identifier, "xFF".into())
            ]
        );
    }

    #[test]
    fn string_prefixes_and_escapes() {
        // Doubling closes nothing.
        assert_eq!(
            kinds("'it''s'", &Dialect::GENERIC)[0],
            (TokenKind::String, "'it''s'".into())
        );
        // A backslash escapes in MySQL and does not elsewhere.
        assert_eq!(
            kinds(r"'a\'b'", &Dialect::MYSQL)[0],
            (TokenKind::String, r"'a\'b'".into())
        );
        assert_eq!(
            kinds(r"'a\'", &Dialect::GENERIC)[0],
            (TokenKind::String, r"'a\'".into())
        );
        // PostgreSQL's escape string, and the standard prefixes.
        assert_eq!(
            kinds(r"E'a\'b'", &Dialect::POSTGRES)[0],
            (TokenKind::String, r"E'a\'b'".into())
        );
        assert_eq!(
            kinds("N'hi'", &Dialect::MSSQL)[0],
            (TokenKind::String, "N'hi'".into())
        );
        assert_eq!(
            kinds("X'1f'", &Dialect::GENERIC)[0],
            (TokenKind::String, "X'1f'".into())
        );
        // Without `e_strings`, `E` is just a name.
        assert_eq!(
            kinds(r"E'a'", &Dialect::ORACLE),
            vec![
                (TokenKind::Identifier, "E".into()),
                (TokenKind::String, "'a'".into())
            ]
        );
    }

    #[test]
    fn parameters() {
        assert_eq!(
            kinds("?", &Dialect::GENERIC)[0],
            (TokenKind::Parameter, "?".into())
        );
        assert_eq!(
            kinds("$1", &Dialect::POSTGRES)[0],
            (TokenKind::Parameter, "$1".into())
        );
        assert_eq!(
            kinds(":name", &Dialect::ORACLE)[0],
            (TokenKind::Parameter, ":name".into())
        );
        assert_eq!(
            kinds("@@rowcount", &Dialect::MSSQL)[0],
            (TokenKind::Parameter, "@@rowcount".into())
        );
        // PostgreSQL has no `:name`; `::` is a cast and `:` alone is a slice.
        assert_eq!(
            kinds("x::int", &Dialect::POSTGRES),
            vec![
                (TokenKind::Identifier, "x".into()),
                (TokenKind::Operator, "::".into()),
                (TokenKind::Type, "int".into())
            ]
        );
    }

    #[test]
    fn operators_take_the_longest_match() {
        assert_eq!(
            kinds("a->>'b'", &Dialect::POSTGRES)[1],
            (TokenKind::Operator, "->>".into())
        );
        assert_eq!(
            kinds("a<=>b", &Dialect::MYSQL)[1],
            (TokenKind::Operator, "<=>".into())
        );
        assert_eq!(
            kinds("a<>b", &Dialect::GENERIC)[1],
            (TokenKind::Operator, "<>".into())
        );
    }

    #[test]
    fn unterminated_constructs_carry_state() {
        let (_, end) = lex_line("select 'x", LineState::START, &Dialect::GENERIC);
        assert_eq!(end.carried_kind(&Dialect::GENERIC), Some(TokenKind::String));
        let (_, end) = lex_line("/* x", LineState::START, &Dialect::GENERIC);
        assert_eq!(
            end.carried_kind(&Dialect::GENERIC),
            Some(TokenKind::Comment)
        );
        let (_, end) = lex_line("\"x", LineState::START, &Dialect::GENERIC);
        assert_eq!(
            end.carried_kind(&Dialect::GENERIC),
            Some(TokenKind::QuotedIdentifier)
        );
        assert_eq!(
            end.carried_kind(&Dialect::MYSQL),
            Some(TokenKind::String),
            "MySQL reads the same carried quote as a string"
        );
    }

    #[test]
    fn an_unknown_character_is_one_character_wide() {
        // A backslash outside a string starts nothing in any dialect here, and
        // neither does a brace.
        let sql = r"select \ { 1";
        let tokens = lex(sql, &Dialect::GENERIC);
        assert_tiles(sql, &tokens);
        let errors: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Error)
            .map(|t| t.text(sql))
            .collect();
        assert_eq!(errors, [r"\", "{"]);
    }

    #[test]
    fn non_ascii_identifiers_survive() {
        // Every byte above ASCII is a name character, which is what PostgreSQL
        // and MySQL allow and what costs nothing to decide. The price is that a
        // stray `±` is a name too; it is one token either way, so no span moves.
        let sql = "select 고객명 from 고객";
        let tokens = lex(sql, &Dialect::GENERIC);
        assert_tiles(sql, &tokens);
        assert_eq!(
            kinds(sql, &Dialect::GENERIC)[1],
            (TokenKind::Identifier, "고객명".into())
        );
    }
}
