//! [`LineState`] as a `u32`, for editors that carry one.
//!
//! [`LineState`] is sixteen bytes and opaque, which is the right shape for this
//! crate and the wrong one for a general-purpose editor widget: a widget that
//! holds a highlighter behind `dyn` cannot name the state type, so it stores
//! four bytes per line and hands them back untouched. `ruui-editor` is such a
//! widget. This module is the adapter between the two.
//!
//! # Why it needs a table
//!
//! Three of the four things a line can end inside fit in a `u32` with room to
//! spare: nothing at all, a block comment at a nesting depth of at most 65535,
//! or one of five quoting forms. The fourth does not. A PostgreSQL dollar quote
//! carries the tag it has to be closed by, and that tag is a length and a
//! 64-bit hash — 96 bits, which no encoding puts into 32. So the tags are kept
//! in a side table and the code holds an index into it.
//!
//! That is what makes this a *codec* rather than a pair of functions: the table
//! is the codec, and a code only means anything to the codec that produced it.
//! One codec per editor document, held beside the highlighter, is the intended
//! shape.
//!
//! # The bits
//!
//! ```text
//!  31 30 29                                                             0
//! ┌─────┬─────────────────────────────────────────────────────────────────┐
//! │ tag │                            payload                              │
//! └─────┴─────────────────────────────────────────────────────────────────┘
//!
//!   tag 0  nothing carried        payload 0            — so START encodes to 0
//!   tag 1  inside a block comment payload nesting depth, 1..=65535
//!   tag 2  inside a quoted run    payload the quoting form, 0..=4
//!   tag 3  inside a dollar quote  payload an index into the tag table
//! ```
//!
//! [`LineState::START`] encodes to `0`, which is the value every editor treats
//! as "nothing is open" — the one thing a host may assume about a code.
//!
//! # The pathological input
//!
//! The table grows by one entry per *distinct* dollar-quote tag in a document,
//! so a script that opens `$a$`, `$b$`, `$c$` … forever grows it forever. The
//! index saturates at [`MAX_TAG_INDEX`] rather than overflowing into the tag
//! bits: past a billion distinct tags a dollar quote may be decoded as no
//! dollar quote at all, and a colour goes wrong on a file that cannot exist.
//! Nothing panics.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::lexer::{Carry, DollarTag, LineState, QuoteKind};

/// Which of the four kinds of carry a code holds, in bits 30 and 31.
const TAG_SHIFT: u32 = 30;

/// Everything below the tag.
const PAYLOAD_MASK: u32 = (1 << TAG_SHIFT) - 1;

/// Tag of a code that carries nothing. The whole code is then zero.
const TAG_NONE: u32 = 0;

/// Tag of a code that carries an open block comment.
const TAG_BLOCK_COMMENT: u32 = 1;

/// Tag of a code that carries an open quoted run.
const TAG_QUOTED: u32 = 2;

/// Tag of a code that carries an open dollar quote.
const TAG_DOLLAR: u32 = 3;

/// The largest dollar-quote tag index a code can hold.
///
/// A document with more distinct dollar-quote tags than this saturates here;
/// see the module documentation.
pub const MAX_TAG_INDEX: u32 = PAYLOAD_MASK;

/// The five quoting forms, in the order their payloads number them.
///
/// The array is the encoding: a form's payload is its position here. Adding a
/// form appends to it, so codes already written down keep their meaning.
const QUOTE_KINDS: [QuoteKind; 5] = [
    QuoteKind::Single,
    QuoteKind::SingleEscaped,
    QuoteKind::Double,
    QuoteKind::Backtick,
    QuoteKind::Bracket,
];

/// A [`LineState`] as a `u32`, and back.
///
/// Shared by reference and used from a `&self` method — a highlighter is
/// handed to an editor behind an `Arc` and lexes lines through a shared
/// reference — so the side table is behind a [`Mutex`]. It is touched only when
/// a *dollar quote* is open across a line break, which is rare enough that the
/// lock is never the cost of a keystroke.
///
/// Encoding is deterministic for a given codec: the same [`LineState`] always
/// yields the same code, whatever order the lines are lexed in, and a code that
/// this codec produced always decodes back to the state it came from. That is
/// what an editor's incremental cache needs, since it compares two codes for
/// equality to decide whether to stop re-lexing.
///
/// ```
/// use rudbman_sql::{Dialect, LineState, LineStateCodec, lex_line};
///
/// let codec = LineStateCodec::new();
/// assert_eq!(codec.encode(LineState::START), 0);
///
/// let (_, open) = lex_line("select $body$ a; b", LineState::START, &Dialect::POSTGRES);
/// let code = codec.encode(open);
/// assert_ne!(code, 0);
/// assert_eq!(codec.decode(code), open);
/// ```
#[derive(Debug, Default)]
pub struct LineStateCodec {
    /// The dollar-quote tags seen so far, and where each of them sits.
    tags: Mutex<Tags>,
}

/// The side table: the tags in index order, and the reverse lookup.
#[derive(Debug, Default)]
struct Tags {
    /// `by_index[i]` is the tag encoded as `i`.
    by_index: Vec<DollarTag>,
    /// The inverse of [`Tags::by_index`], so that encoding is not a scan.
    by_tag: HashMap<DollarTag, u32>,
}

impl LineStateCodec {
    /// A codec with an empty tag table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Packs `state` into a `u32`.
    ///
    /// [`LineState::START`] always encodes to `0`. Every other state encodes to
    /// something else, and the same state to the same code every time.
    pub fn encode(&self, state: LineState) -> u32 {
        match state.0 {
            Carry::None => TAG_NONE << TAG_SHIFT,
            Carry::BlockComment(depth) => (TAG_BLOCK_COMMENT << TAG_SHIFT) | u32::from(depth),
            Carry::Quoted(quote) => {
                let index = QUOTE_KINDS
                    .iter()
                    .position(|kind| *kind == quote)
                    .expect("every quoting form is in QUOTE_KINDS");
                (TAG_QUOTED << TAG_SHIFT) | (index as u32)
            }
            Carry::DollarQuote(tag) => (TAG_DOLLAR << TAG_SHIFT) | self.index_of(tag),
        }
    }

    /// Unpacks a code this codec produced.
    ///
    /// A code it did not produce is not an error and never panics: an unknown
    /// tag index, an out-of-range quoting form or a zero block-comment depth
    /// all decode to [`LineState::START`], which is the safe answer — the line
    /// is lexed as if nothing were open.
    pub fn decode(&self, code: u32) -> LineState {
        let payload = code & PAYLOAD_MASK;
        let carry = match code >> TAG_SHIFT {
            TAG_BLOCK_COMMENT => match u16::try_from(payload) {
                // Depth zero is not a state the lexer can be in, so a code
                // holding one did not come from here.
                Ok(0) | Err(_) => Carry::None,
                Ok(depth) => Carry::BlockComment(depth),
            },
            TAG_QUOTED => match QUOTE_KINDS.get(payload as usize) {
                Some(quote) => Carry::Quoted(*quote),
                None => Carry::None,
            },
            TAG_DOLLAR => match self.tag_at(payload) {
                Some(tag) => Carry::DollarQuote(tag),
                None => Carry::None,
            },
            _ => Carry::None,
        };
        LineState(carry)
    }

    /// The index of `tag`, adding it to the table if it is new.
    fn index_of(&self, tag: DollarTag) -> u32 {
        let mut tags = self.tags.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(index) = tags.by_tag.get(&tag) {
            return *index;
        }
        let index = u32::try_from(tags.by_index.len()).unwrap_or(MAX_TAG_INDEX);
        if index >= MAX_TAG_INDEX {
            // The table is full. Every further tag encodes to the same code,
            // which is wrong and is also unreachable: see the module
            // documentation.
            return MAX_TAG_INDEX;
        }
        tags.by_index.push(tag);
        tags.by_tag.insert(tag, index);
        index
    }

    /// The tag at `index`, if the table has one there.
    fn tag_at(&self, index: u32) -> Option<DollarTag> {
        let tags = self.tags.lock().unwrap_or_else(|err| err.into_inner());
        tags.by_index.get(index as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::lexer::lex_line;

    /// The state the lines of `script` leave behind, one per line.
    fn states(script: &str, dialect: &Dialect) -> Vec<LineState> {
        let mut state = LineState::START;
        let mut out = Vec::new();
        for line in script.split('\n') {
            state = lex_line(line, state, dialect).1;
            out.push(state);
        }
        out
    }

    #[test]
    fn the_start_state_is_zero() {
        let codec = LineStateCodec::new();
        assert_eq!(codec.encode(LineState::START), 0);
        assert_eq!(codec.decode(0), LineState::START);
    }

    #[test]
    fn every_carry_the_lexer_can_produce_round_trips() {
        // One script per variant of the carry, in the dialect that produces it:
        // an open block comment, each of the five quoting forms, and a dollar
        // quote.
        let cases: &[(&str, Dialect)] = &[
            ("/* open", Dialect::GENERIC),
            ("/* one /* two /* three", Dialect::POSTGRES),
            ("select 'open", Dialect::GENERIC),
            ("select 'open", Dialect::MYSQL),
            ("select \"open", Dialect::POSTGRES),
            ("select `open", Dialect::MYSQL),
            ("select [open", Dialect::MSSQL),
            ("select $$open", Dialect::POSTGRES),
            ("select $body$open", Dialect::POSTGRES),
        ];

        let codec = LineStateCodec::new();
        let mut codes = Vec::new();
        for (script, dialect) in cases {
            let state = *states(script, dialect).last().expect("one line");
            assert!(!state.is_start(), "{script:?} leaves something open");
            let code = codec.encode(state);
            assert_eq!(codec.decode(code), state, "{script:?}");
            assert_ne!(code, 0, "{script:?} must not encode as START");
            codes.push(code);
        }

        // And every one of them is a code of its own: two carries that differ
        // must not encode alike, or an editor's cache would stop re-lexing a
        // line too early.
        for (left, right) in codes.iter().zip(codes.iter().skip(1)) {
            assert_ne!(left, right);
        }
    }

    #[test]
    fn two_different_dollar_tags_do_not_collide() {
        let codec = LineStateCodec::new();
        let pg = Dialect::POSTGRES;

        let a = *states("select $a$ one; two", &pg).last().expect("one line");
        let b = *states("select $bb$ one; two", &pg)
            .last()
            .expect("one line");
        let plain = *states("select $$ one; two", &pg).last().expect("one line");
        assert_ne!(a, b);

        let (ca, cb, cp) = (codec.encode(a), codec.encode(b), codec.encode(plain));
        assert_ne!(ca, cb);
        assert_ne!(ca, cp);
        assert_ne!(cb, cp);
        assert_eq!(codec.decode(ca), a);
        assert_eq!(codec.decode(cb), b);
        assert_eq!(codec.decode(cp), plain);

        // Encoding is stable: the same tag seen again is the same code, and the
        // table does not grow.
        assert_eq!(codec.encode(a), ca);
        assert_eq!(codec.encode(b), cb);
        assert_eq!(
            codec.tags.lock().expect("the table").by_index.len(),
            3,
            "one entry per distinct tag"
        );
    }

    #[test]
    fn the_block_comment_depth_survives() {
        let codec = LineStateCodec::new();
        let pg = Dialect::POSTGRES;
        let one = *states("/* one", &pg).last().expect("one line");
        let two = *states("/* one /* two", &pg).last().expect("one line");
        assert_ne!(one, two, "PostgreSQL block comments nest");
        assert_ne!(codec.encode(one), codec.encode(two));
        assert_eq!(codec.decode(codec.encode(one)), one);
        assert_eq!(codec.decode(codec.encode(two)), two);
    }

    #[test]
    fn a_code_from_nowhere_decodes_to_the_start_state() {
        let codec = LineStateCodec::new();
        for code in [
            u32::MAX,
            TAG_BLOCK_COMMENT << TAG_SHIFT,              // depth zero
            (TAG_BLOCK_COMMENT << TAG_SHIFT) | 0x1_0000, // depth past a u16
            (TAG_QUOTED << TAG_SHIFT) | 99,              // no such quoting form
            (TAG_DOLLAR << TAG_SHIFT) | 7,               // no such tag
        ] {
            assert_eq!(codec.decode(code), LineState::START, "{code:#x}");
        }
    }

    #[test]
    fn a_whole_script_round_trips_line_by_line() {
        // The states a real multi-line script leaves behind, encoded and
        // decoded one line at a time the way an editor's cache would.
        let script = "create function f() returns int as $body$\n\
                      begin\n\
                      /* a comment ; with a semicolon\n\
                      still commented */ select 'a string\n\
                      still a string';\n\
                      end\n\
                      $body$ language plpgsql;\n";
        let codec = LineStateCodec::new();
        for state in states(script, &Dialect::POSTGRES) {
            assert_eq!(codec.decode(codec.encode(state)), state);
        }
    }
}
