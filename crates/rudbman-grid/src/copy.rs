//! Turning a selection into text on the clipboard.
//!
//! Four formats, because four different things get done with a block of result
//! rows: pasted into a spreadsheet ([`CopyFormat::Tsv`]), fed to something that
//! reads files ([`CopyFormat::Csv`]), pasted into code
//! ([`CopyFormat::Json`]), or replayed against another database
//! ([`CopyFormat::Insert`]).
//!
//! The grid does this itself rather than raising an event for the host to
//! answer: gpui owns the clipboard, the grid owns the selection, and a round
//! trip through the host between them would only be somewhere else for the two
//! to disagree.
//!
//! ## Null survives two of the four
//!
//! There is no way to write a null in a tab- or comma-separated field that an
//! empty string could not also be — the formats have one hole and two things to
//! put in it. So **[`CopyFormat::Tsv`] and [`CopyFormat::Csv`] write both as an
//! empty field**, and the distinction the rest of this crate is careful about
//! is lost on the way out. [`CopyFormat::Json`] writes `null`, and
//! [`CopyFormat::Insert`] writes `NULL`; those two are faithful, and are what to
//! reach for when it matters.
//!
//! ## What a non-rectangular selection copies
//!
//! `Ctrl`-click can pick blocks that no rectangle covers, and no format has a
//! shape for that. The copy runs over [`Selection::bounds`] — the smallest box
//! containing everything picked — and the cells inside the box but outside the
//! selection are copied as nulls. Nothing is silently dropped, and nothing
//! unpicked is silently included as a value.

use std::borrow::Cow;

use crate::selection::Selection;
use crate::source::{GridCell, GridColumnKind, GridSource, lob_label};

/// The table name written into an `INSERT` when the host has not set one.
///
/// Deliberately not valid SQL: a statement aimed at the wrong table is worse
/// than one that will not parse until the name is filled in.
pub const DEFAULT_INSERT_TABLE: &str = "?table?";

/// How a copied block is written out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyFormat {
    /// Tab separated, one line per row. What a spreadsheet expects from the
    /// clipboard, and so the default.
    ///
    /// A field holding a tab, a newline or a quote is wrapped in double quotes
    /// with inner quotes doubled, which is what spreadsheets read back. Nulls
    /// and empty strings both come out empty; see the module docs.
    #[default]
    Tsv,
    /// Comma separated, quoted per RFC 4180. Nulls come out empty, as in
    /// [`CopyFormat::Tsv`].
    Csv,
    /// A JSON array of objects, one per row, keyed by column name. Nulls come
    /// out as `null`.
    ///
    /// Numeric and boolean columns are written as JSON numbers and booleans
    /// when their text really is one, and as strings when it is not — a numeric
    /// column holding `1,234` is text, whatever the driver called it.
    Json,
    /// One `INSERT` statement per row. Nulls come out as `NULL`.
    ///
    /// Numbers and booleans are bare, everything else is a quoted string with
    /// inner quotes doubled. Note that MySQL reads a backslash inside a string
    /// literal as an escape unless `NO_BACKSLASH_ESCAPES` is set, so a value
    /// containing one may need that mode on the far end.
    Insert,
}

impl CopyFormat {
    /// Every format, in menu order.
    pub const ALL: [CopyFormat; 4] = [
        CopyFormat::Tsv,
        CopyFormat::Csv,
        CopyFormat::Json,
        CopyFormat::Insert,
    ];

    /// A short name for a menu.
    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::Tsv => "TSV",
            CopyFormat::Csv => "CSV",
            CopyFormat::Json => "JSON",
            CopyFormat::Insert => "INSERT",
        }
    }
}

/// One copied cell, once it is known whether it has a value at all.
enum Copied<'a> {
    /// No value: either the cell is null, or it lies in the copied box but
    /// outside the selection.
    Null,
    /// The value's text, borrowed from the source unless it had to be built —
    /// which only a LOB placeholder does.
    Text(Cow<'a, str>),
}

/// Writes the picked cells of `source` in `format`.
///
/// `columns` maps display position to source column, which is what makes hidden
/// columns absent from the copy and (once reordering lands) reordered ones come
/// out in the order they are seen. `table` is the name written into an
/// `INSERT`; pass [`DEFAULT_INSERT_TABLE`] when the host has not set one.
///
/// An empty selection copies the empty string, so that `Ctrl+C` over nothing
/// leaves the clipboard alone rather than blanking it with punctuation.
pub fn copy_payload<S>(
    source: &S,
    columns: &[usize],
    selection: &Selection,
    format: CopyFormat,
    table: &str,
) -> String
where
    S: GridSource + ?Sized,
{
    let rows = source.row_count();
    let Some(bounds) = selection.bounds() else {
        return String::new();
    };
    if rows == 0 || bounds.top >= rows {
        return String::new();
    }

    let top = bounds.top;
    let bottom = bounds.bottom.min(rows - 1);
    // Display position kept beside the source column: the first is what the
    // selection is written in, the second is what the source is asked in.
    let picked: Vec<(usize, usize)> = bounds
        .columns()
        .filter_map(|display| columns.get(display).map(|column| (display, *column)))
        .collect();
    if picked.is_empty() {
        return String::new();
    }

    match format {
        CopyFormat::Tsv => delimited(source, &picked, selection, top, bottom, '\t'),
        CopyFormat::Csv => delimited(source, &picked, selection, top, bottom, ','),
        CopyFormat::Json => json(source, &picked, selection, top, bottom),
        CopyFormat::Insert => inserts(source, &picked, selection, top, bottom, table),
    }
}

/// The value at `row` and `column`, or nothing when the cell is null or was
/// never picked.
fn read<'a, S>(
    source: &'a S,
    row: usize,
    column: usize,
    display: usize,
    selection: &Selection,
) -> Copied<'a>
where
    S: GridSource + ?Sized,
{
    if !selection.contains(row, display) {
        return Copied::Null;
    }
    match source.cell(row, column) {
        // A cell nobody has typed into yet copies as nothing at all. It has no
        // value to carry, and writing the word `DEFAULT` into a TSV column
        // would put a plausible-looking string where the absence of one is the
        // truth.
        GridCell::Null | GridCell::Default => Copied::Null,
        GridCell::Text(text) => Copied::Text(Cow::Borrowed(text)),
        GridCell::Lob { size } => Copied::Text(Cow::Owned(lob_label(size))),
    }
}

/// TSV and CSV, which differ only in the character between fields and therefore
/// in which characters force a field to be quoted.
fn delimited<S>(
    source: &S,
    picked: &[(usize, usize)],
    selection: &Selection,
    top: usize,
    bottom: usize,
    separator: char,
) -> String
where
    S: GridSource + ?Sized,
{
    let mut out = String::new();
    for row in top..=bottom {
        if row > top {
            out.push('\n');
        }
        for (n, (display, column)) in picked.iter().enumerate() {
            if n > 0 {
                out.push(separator);
            }
            if let Copied::Text(text) = read(source, row, *column, *display, selection) {
                out.push_str(&quoted_field(&text, separator));
            }
        }
    }
    out
}

/// A field as the delimited formats carry it: bare unless it holds something
/// that would break the row apart, and double quoted with inner quotes doubled
/// when it does.
fn quoted_field<'a>(value: &'a str, separator: char) -> Cow<'a, str> {
    let awkward = value.contains([separator, '"', '\n', '\r']);
    if !awkward {
        return Cow::Borrowed(value);
    }

    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    Cow::Owned(out)
}

/// A JSON array of one object per row.
fn json<S>(
    source: &S,
    picked: &[(usize, usize)],
    selection: &Selection,
    top: usize,
    bottom: usize,
) -> String
where
    S: GridSource + ?Sized,
{
    let mut out = String::from("[\n");
    for row in top..=bottom {
        out.push_str("  {\n");
        for (n, (display, column)) in picked.iter().enumerate() {
            let heading = source.column(*column);
            out.push_str("    ");
            json_string(heading.name, &mut out);
            out.push_str(": ");
            match read(source, row, *column, *display, selection) {
                Copied::Null => out.push_str("null"),
                Copied::Text(text) => match json_literal(heading.kind, &text) {
                    Some(literal) => out.push_str(&literal),
                    None => json_string(&text, &mut out),
                },
            }
            if n + 1 < picked.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  }");
        if row < bottom {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(']');
    out
}

/// `text` written as a bare JSON literal, or `None` when it has to be a string
/// — either because the column is not of a kind JSON has a literal for, or
/// because the text is not one after all.
fn json_literal<'a>(kind: GridColumnKind, text: &'a str) -> Option<Cow<'a, str>> {
    match kind {
        GridColumnKind::Number => is_number(text).then_some(Cow::Borrowed(text)),
        GridColumnKind::Boolean => bool_literal(text).map(Cow::Borrowed),
        _ => None,
    }
}

/// The `true` or `false` a boolean column's text stands for.
///
/// Wider than the two words, because drivers hand booleans back as `t`, as `1`
/// and as `Y` depending on how the column was declared, and all of them mean
/// the same thing.
fn bool_literal(text: &str) -> Option<&'static str> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "y" | "yes" => Some("true"),
        "false" | "f" | "0" | "n" | "no" => Some("false"),
        _ => None,
    }
}

/// Whether `text` is something JSON and SQL would both read as a number.
///
/// `f64::from_str` accepts `inf` and `NaN`, which neither of them does, and
/// accepts a leading `+`, which JSON does not; so the characters are checked
/// before the parse rather than trusting it alone.
fn is_number(text: &str) -> bool {
    if text.is_empty() || text.starts_with('+') {
        return false;
    }
    if !text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E'))
    {
        return false;
    }
    text.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Appends `value` as a quoted JSON string.
fn json_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// One `INSERT` statement per row.
fn inserts<S>(
    source: &S,
    picked: &[(usize, usize)],
    selection: &Selection,
    top: usize,
    bottom: usize,
    table: &str,
) -> String
where
    S: GridSource + ?Sized,
{
    let mut names = String::new();
    for (n, (_, column)) in picked.iter().enumerate() {
        if n > 0 {
            names.push_str(", ");
        }
        names.push_str(source.column(*column).name);
    }

    let mut out = String::new();
    for row in top..=bottom {
        out.push_str("INSERT INTO ");
        out.push_str(table);
        out.push_str(" (");
        out.push_str(&names);
        out.push_str(") VALUES (");
        for (n, (display, column)) in picked.iter().enumerate() {
            if n > 0 {
                out.push_str(", ");
            }
            let kind = source.column(*column).kind;
            match read(source, row, *column, *display, selection) {
                Copied::Null => out.push_str("NULL"),
                Copied::Text(text) => sql_literal(kind, &text, &mut out),
            }
        }
        out.push_str(");");
        if row < bottom {
            out.push('\n');
        }
    }
    out
}

/// Appends `text` as an SQL literal of `kind`.
///
/// The same question [`json_literal`] answers, for the other grammar: a value
/// that is a number in one had better be a number in the other, so both go
/// through [`is_number`] and [`bool_literal`].
fn sql_literal(kind: GridColumnKind, text: &str, out: &mut String) {
    let bare = match kind {
        GridColumnKind::Number => is_number(text).then_some(text),
        GridColumnKind::Boolean => bool_literal(text),
        _ => None,
    };
    if let Some(bare) = bare {
        out.push_str(bare);
        return;
    }

    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::CellAddress;
    use crate::source::GridColumn;

    /// A result held as plain strings, so a copy test states its rows inline.
    struct Fixture {
        headings: Vec<(&'static str, GridColumnKind)>,
        rows: Vec<Vec<Option<String>>>,
    }

    impl GridSource for Fixture {
        fn column_count(&self) -> usize {
            self.headings.len()
        }

        fn column(&self, index: usize) -> GridColumn<'_> {
            let (name, kind) = self.headings[index];
            GridColumn::new(name, kind)
        }

        fn row_count(&self) -> usize {
            self.rows.len()
        }

        fn cell(&self, row: usize, column: usize) -> GridCell<'_> {
            match self.rows[row][column].as_deref() {
                Some(text) => GridCell::Text(text),
                None => GridCell::Null,
            }
        }
    }

    /// Two rows of three columns, one of each kind, holding every character
    /// that any of the four formats has to escape.
    fn awkward() -> Fixture {
        Fixture {
            headings: vec![
                ("id", GridColumnKind::Number),
                ("note", GridColumnKind::Text),
                ("ok", GridColumnKind::Boolean),
            ],
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("a\tb\"c\nd".into()),
                    Some("true".into()),
                ],
                vec![Some("2".into()), None, Some("false".into())],
            ],
        }
    }

    /// Everything picked, in source order.
    fn all(fixture: &Fixture) -> (Vec<usize>, Selection) {
        let columns: Vec<usize> = (0..fixture.column_count()).collect();
        let mut selection = Selection::new();
        selection.select_all(fixture.row_count(), columns.len());
        (columns, selection)
    }

    /// Tabs, quotes and newlines are quoted the way a spreadsheet reads them
    /// back, and null is indistinguishable from empty — which is the documented
    /// hole in the format, not an oversight.
    #[test]
    fn tsv_quotes_what_would_break_a_row() {
        let fixture = awkward();
        let (columns, selection) = all(&fixture);

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Tsv,
                DEFAULT_INSERT_TABLE
            ),
            "1\t\"a\tb\"\"c\nd\"\ttrue\n2\t\tfalse"
        );
    }

    /// A comma forces a quote in CSV and does not in TSV; a tab does the
    /// reverse.
    #[test]
    fn csv_quotes_commas_and_leaves_tabs_alone() {
        let fixture = Fixture {
            headings: vec![("a", GridColumnKind::Text), ("b", GridColumnKind::Text)],
            rows: vec![vec![Some("x,y".into()), Some("p\tq".into())]],
        };
        let (columns, selection) = all(&fixture);

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Csv,
                DEFAULT_INSERT_TABLE
            ),
            "\"x,y\",p\tq"
        );
        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Tsv,
                DEFAULT_INSERT_TABLE
            ),
            "x,y\t\"p\tq\""
        );
    }

    /// JSON is where null survives, and where the control characters are
    /// escaped rather than quoted around.
    #[test]
    fn json_writes_null_and_escapes_control_characters() {
        let fixture = awkward();
        let (columns, selection) = all(&fixture);

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Json,
                DEFAULT_INSERT_TABLE
            ),
            concat!(
                "[\n",
                "  {\n",
                "    \"id\": 1,\n",
                "    \"note\": \"a\\tb\\\"c\\nd\",\n",
                "    \"ok\": true\n",
                "  },\n",
                "  {\n",
                "    \"id\": 2,\n",
                "    \"note\": null,\n",
                "    \"ok\": false\n",
                "  }\n",
                "]"
            )
        );
    }

    /// A numeric column holding something that is not a number comes out as a
    /// string in both grammars, rather than as an unparseable literal.
    #[test]
    fn a_number_that_is_not_one_is_written_as_text() {
        let fixture = Fixture {
            headings: vec![("n", GridColumnKind::Number)],
            rows: vec![vec![Some("1,234".into())], vec![Some("-1.5e3".into())]],
        };
        let (columns, selection) = all(&fixture);

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Json,
                DEFAULT_INSERT_TABLE
            ),
            "[\n  {\n    \"n\": \"1,234\"\n  },\n  {\n    \"n\": -1.5e3\n  }\n]"
        );
        assert_eq!(
            copy_payload(&fixture, &columns, &selection, CopyFormat::Insert, "t"),
            "INSERT INTO t (n) VALUES ('1,234');\nINSERT INTO t (n) VALUES (-1.5e3);"
        );
    }

    /// The `INSERT` doubles quotes, keeps numbers and booleans bare, and says
    /// `NULL` where there is no value.
    #[test]
    fn inserts_quote_strings_and_name_the_table() {
        let fixture = Fixture {
            headings: vec![("id", GridColumnKind::Number), ("s", GridColumnKind::Text)],
            rows: vec![
                vec![Some("7".into()), Some("it's\ttab".into())],
                vec![Some("8".into()), None],
            ],
        };
        let (columns, selection) = all(&fixture);

        assert_eq!(
            copy_payload(&fixture, &columns, &selection, CopyFormat::Insert, "app.t"),
            concat!(
                "INSERT INTO app.t (id, s) VALUES (7, 'it''s\ttab');\n",
                "INSERT INTO app.t (id, s) VALUES (8, NULL);"
            )
        );

        // And with no table set, a name that will not parse until it is filled
        // in — better than one aimed at the wrong table.
        assert!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Insert,
                DEFAULT_INSERT_TABLE
            )
            .starts_with("INSERT INTO ?table? (id, s)")
        );
    }

    /// A hidden column is not in `columns`, so it is not in any format.
    #[test]
    fn a_hidden_column_is_not_copied() {
        let fixture = awkward();
        let columns = vec![0, 2];
        let mut selection = Selection::new();
        selection.select_all(2, columns.len());

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Tsv,
                DEFAULT_INSERT_TABLE
            ),
            "1\ttrue\n2\tfalse"
        );
    }

    /// The hole in a selection no rectangle covers is copied as a null, so the
    /// block keeps its shape and nothing unpicked arrives as a value.
    #[test]
    fn a_gap_in_a_ragged_selection_copies_as_null() {
        let fixture = Fixture {
            headings: vec![("a", GridColumnKind::Text), ("b", GridColumnKind::Text)],
            rows: vec![
                vec![Some("a0".into()), Some("b0".into())],
                vec![Some("a1".into()), Some("b1".into())],
            ],
        };
        let columns = vec![0, 1];
        let mut selection = Selection::new();
        selection.replace(CellAddress::new(0, 0));
        selection.add(CellAddress::new(1, 1));

        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Tsv,
                DEFAULT_INSERT_TABLE
            ),
            "a0\t\n\tb1"
        );
        assert_eq!(
            copy_payload(
                &fixture,
                &columns,
                &selection,
                CopyFormat::Json,
                DEFAULT_INSERT_TABLE
            ),
            concat!(
                "[\n",
                "  {\n    \"a\": \"a0\",\n    \"b\": null\n  },\n",
                "  {\n    \"a\": null,\n    \"b\": \"b1\"\n  }\n",
                "]"
            )
        );
    }

    /// A LOB has no body to copy, so every format gets the placeholder rather
    /// than something that could pass for data.
    #[test]
    fn a_lob_copies_as_a_placeholder() {
        struct Lobs;
        impl GridSource for Lobs {
            fn column_count(&self) -> usize {
                1
            }
            fn column(&self, _: usize) -> GridColumn<'_> {
                GridColumn::new("blob", GridColumnKind::Binary)
            }
            fn row_count(&self) -> usize {
                1
            }
            fn cell(&self, _: usize, _: usize) -> GridCell<'_> {
                GridCell::Lob { size: Some(1024) }
            }
        }

        let columns = vec![0];
        let mut selection = Selection::new();
        selection.select_all(1, 1);

        assert_eq!(
            copy_payload(
                &Lobs,
                &columns,
                &selection,
                CopyFormat::Tsv,
                DEFAULT_INSERT_TABLE
            ),
            "[LOB 1024]"
        );
        assert_eq!(
            copy_payload(&Lobs, &columns, &selection, CopyFormat::Insert, "t"),
            "INSERT INTO t (blob) VALUES ('[LOB 1024]');"
        );
    }

    /// Nothing picked copies nothing at all, so `Ctrl+C` over an untouched grid
    /// leaves the clipboard as it was rather than blanking it with brackets.
    #[test]
    fn an_empty_selection_copies_nothing() {
        let fixture = awkward();
        let columns = vec![0, 1, 2];
        let selection = Selection::new();

        for format in CopyFormat::ALL {
            assert_eq!(
                copy_payload(&fixture, &columns, &selection, format, DEFAULT_INSERT_TABLE),
                "",
                "{format:?}"
            );
        }
    }
}
