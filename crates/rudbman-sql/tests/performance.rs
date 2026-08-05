//! A smoke test with a number attached.
//!
//! Not a benchmark — it is a guard against the kind of change that turns the
//! scanner from linear into quadratic, or drops an allocation into the inner
//! loop, without anybody noticing until an editor stutters on a real script. The
//! budget is deliberately loose (a debug build on a slow machine has to fit
//! under it) and deliberately present (something six times slower will not).

use std::time::Instant;

use rudbman_sql::{Dialect, LineState, TokenKind, lex_line, split_statements, statement_at};

/// About ten megabytes of plausible SQL: every construct the scanner has a
/// branch for, so no branch is optimized away by the shape of the input.
fn generate(target: usize) -> String {
    const UNIT: &str = "\
-- customer rollup for the daily report
insert into report.daily (id, name, total, note, made_at)
select c.id,
       coalesce(c.name, '(unnamed)') as name,      /* nulls become a label */
       sum(o.total * 1.075e0) as total,
       'it''s a note; with a semicolon',
       current_timestamp
  from customers c
  join orders o on o.customer_id = c.id and o.status <> 'CANCELLED'
 where c.created_at >= ?
   and c.region in ('KR', 'JP', 'US')
   and c.tier = :tier
 group by c.id, c.name
having sum(o.total) > 0x3e8
 order by total desc;

update report.daily set note = '#not a comment' where id = 1; -- and a tail comment
";
    let mut out = String::with_capacity(target + UNIT.len());
    while out.len() < target {
        out.push_str(UNIT);
    }
    out
}

/// Lexing line by line — the editor's path — over ten megabytes.
#[test]
fn ten_megabytes_lex_in_seconds() {
    let source = generate(10 * 1024 * 1024);
    let dialect = Dialect::GENERIC;

    let started = Instant::now();
    let mut state = LineState::START;
    let mut tokens = 0usize;
    let mut keywords = 0usize;
    for line in source.lines() {
        let (line_tokens, next) = lex_line(line, state, &dialect);
        tokens += line_tokens.len();
        keywords += line_tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Keyword)
            .count();
        state = next;
    }
    let elapsed = started.elapsed();

    assert!(tokens > 1_000_000, "the input should be substantial");
    assert!(keywords > 100_000, "and should be full of keywords");
    assert!(state.is_start(), "and should end cleanly");
    // An unoptimized build lexes this in about 0.8 seconds — some 12 MB/s —
    // on the machine it was written on. Five seconds leaves room for a slower
    // one and still catches a sixfold regression.
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "lexing 10MB took {elapsed:?} ({:.1} MB/s)",
        source.len() as f64 / 1_048_576.0 / elapsed.as_secs_f64()
    );
    println!(
        "lex_line: {} tokens over {:.1} MB in {:?} ({:.1} MB/s)",
        tokens,
        source.len() as f64 / 1_048_576.0,
        elapsed,
        source.len() as f64 / 1_048_576.0 / elapsed.as_secs_f64()
    );
}

/// Splitting the same ten megabytes, then finding the statement in the middle.
#[test]
fn ten_megabytes_split_in_seconds() {
    let source = generate(10 * 1024 * 1024);
    let dialect = Dialect::GENERIC;

    let started = Instant::now();
    let spans = split_statements(&source, &dialect);
    let split = started.elapsed();

    assert!(spans.len() > 10_000, "{} statements", spans.len());
    // The `;` inside the string literal must not have made an extra one: each
    // repetition of the unit is exactly two statements.
    assert_eq!(spans.len() % 2, 0);

    let started = Instant::now();
    let middle = statement_at(&source, source.len() / 2, &dialect).unwrap();
    let lookup = started.elapsed();
    assert!(middle.sql(&source).starts_with("--") || !middle.sql(&source).is_empty());

    // Both are one pass of the same scanner: about 0.8 seconds to split the
    // whole script and 0.3 to reach the middle of it, unoptimized.
    assert!(
        split.as_secs_f64() < 5.0 && lookup.as_secs_f64() < 5.0,
        "split took {split:?}, lookup took {lookup:?}"
    );
    println!(
        "split: {} statements in {split:?}; statement_at in {lookup:?}",
        spans.len()
    );
}
