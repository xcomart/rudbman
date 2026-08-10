//! What a running query hands the grid: batches, already rendered.
//!
//! [`ResultSource`] is the adapter between `rudbman-jdbc`'s columnar
//! [`Batch`] and `rudbman-grid`'s [`GridSource`]. Everything here is free of
//! gpui and free of the JVM once a batch has been decoded, which is what lets
//! the awkward halves — batch boundaries, null against empty, a `REAL` that
//! must not print the noise of its widening — be asserted without a window.
//!
//! # Rendering happens once, off the UI thread
//!
//! [`GridSource::cell`] is called a few hundred times per frame and must not
//! allocate. A [`Batch`] cannot answer that on its own: an `I64` column would
//! have to format a number, and there is nowhere to put the result
//! (`GridCell::Text` borrows). So a batch is turned into a [`RenderedBatch`]
//! on the background thread that fetched it — every value written once into one
//! contiguous `String` per column, with a `u32` offset table beside it — and
//! `cell` then hands back a slice of that. Per frame the grid allocates
//! nothing.
//!
//! # The kind is read per batch, never per cursor
//!
//! A column that is entirely NULL in one batch arrives as
//! [`ColumnKind::Nulls`] even though the column before it was `I64`
//! (architecture document, §4.6). [`render_batch`] therefore reads the kind off
//! the batch in hand and never off `ColumnInfo::kind`. What *is* read off
//! [`ColumnInfo`] is the presentation: alignment and the `REAL` narrowing come
//! from the logical type, which is stable for the life of the result.
//!
//! # Null is not the empty string
//!
//! Both are a zero-length slice of the column's data, exactly as they are on
//! the wire. The validity bitmap is the only thing that tells them apart, here
//! as there, and it is what decides between [`GridCell::Null`] and
//! `GridCell::Text("")`.
//!
//! # `may_have_more` is a contract, not a field
//!
//! JDBC has no lookahead: asking whether another result exists consumes the
//! current one (architecture document, §4.4). [`advance`] therefore keeps
//! calling `MORE_RESULTS` until the three-part exhaustion holds, and stops the
//! moment a result set still has rows in it — because advancing past it would
//! close the very `ResultSet` the grid is paging. Paging that result to its end
//! resumes the walk, so the later results of a multi-result statement appear
//! when the earlier one is finished with, and never before.
//!
//! The walk lives here rather than beside the pane that first needed it because
//! two panes now need it: the query pane runs whatever was typed, and the data
//! pane (architecture document, §7.9) runs a `SELECT` of its own, and both page
//! the answer the same way.

use rudbman_grid::{GridCell, GridColumn, GridColumnKind, GridSource, GridSourceState};
use rudbman_jdbc::{Batch, ColumnInfo, ColumnKind, Cursor, Error as JdbcError, Value};

/// `java.sql.Types` constants this module branches on.
///
/// Only the ones that decide a [`GridColumnKind`]; the rest fall through to
/// [`GridColumnKind::Text`], which is the safe answer for anything unknown.
mod sql_types {
    pub const BIT: i32 = -7;
    pub const TINYINT: i32 = -6;
    pub const BIGINT: i32 = -5;
    pub const LONGVARBINARY: i32 = -4;
    pub const VARBINARY: i32 = -3;
    pub const BINARY: i32 = -2;
    pub const NUMERIC: i32 = 2;
    pub const DECIMAL: i32 = 3;
    pub const INTEGER: i32 = 4;
    pub const SMALLINT: i32 = 5;
    pub const FLOAT: i32 = 6;
    pub const REAL: i32 = 7;
    pub const DOUBLE: i32 = 8;
    pub const DATE: i32 = 91;
    pub const TIME: i32 = 92;
    pub const TIMESTAMP: i32 = 93;
    pub const BOOLEAN: i32 = 16;
    pub const BLOB: i32 = 2004;
    pub const TIME_WITH_TIMEZONE: i32 = 2013;
    pub const TIMESTAMP_WITH_TIMEZONE: i32 = 2014;
}

/// The shape the grid draws a column in, from its logical JDBC type.
///
/// `BIT` splits on precision for the same reason the codec does: MySQL's
/// `BIT(n)` for `n > 1` is a byte string, not a truth value.
pub fn column_kind(column: &ColumnInfo) -> GridColumnKind {
    use sql_types::*;
    match column.sql_type {
        BIT => {
            if column.precision <= 1 {
                GridColumnKind::Boolean
            } else {
                GridColumnKind::Binary
            }
        }
        BOOLEAN => GridColumnKind::Boolean,
        TINYINT | SMALLINT | INTEGER | BIGINT | FLOAT | REAL | DOUBLE | NUMERIC | DECIMAL => {
            GridColumnKind::Number
        }
        DATE | TIME | TIMESTAMP | TIME_WITH_TIMEZONE | TIMESTAMP_WITH_TIMEZONE => {
            GridColumnKind::Temporal
        }
        BINARY | VARBINARY | LONGVARBINARY | BLOB => GridColumnKind::Binary,
        _ => GridColumnKind::Text,
    }
}

/// Writes `value` into `out` exactly as [`Value::to_text`] would render it.
///
/// A second copy of that rendering, deliberately: `to_text` answers a `String`
/// and this path renders a whole batch into one buffer, so going through it
/// would allocate once per cell. `the_rendering_matches_the_crates_own`
/// asserts the two stay in step.
///
/// `single_precision` is [`ColumnInfo::is_single_precision`] — the one thing
/// about the logical type this rendering turns on.
///
/// NULL and a LOB write nothing — neither has text, and both are carried by the
/// column's own structure rather than by its data.
fn push_value(out: &mut String, value: &Value<'_>, single_precision: bool) {
    use std::fmt::Write as _;

    match value {
        Value::Null | Value::Lob { .. } => {}
        Value::I64(number) => {
            let _ = write!(out, "{number}");
        }
        Value::F64(number) => {
            // A `REAL` arrived widened to `f64`; narrowing it back is what
            // keeps 0.1 from printing as 0.10000000149011612.
            if single_precision {
                let _ = write!(out, "{}", *number as f32);
            } else {
                let _ = write!(out, "{number}");
            }
        }
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Str(text) => out.push_str(text),
        Value::Bin(bytes) => {
            for byte in *bytes {
                let _ = write!(out, "{byte:02X}");
            }
        }
    }
}

/// Marks row `row` of a validity bitmap as holding a value.
///
/// LSB-first, byte `row >> 3`, bit `row & 7` — the codec's convention, kept
/// here so the two never have to be reconciled.
fn set_valid(bits: &mut [u8], row: usize) {
    bits[row >> 3] |= 1 << (row & 7);
}

/// Whether row `row` of a validity bitmap holds a value.
fn is_valid(bits: &[u8], row: usize) -> bool {
    bits.get(row >> 3)
        .is_some_and(|byte| byte & (1 << (row & 7)) != 0)
}

/// Bytes a validity bitmap for `rows` rows takes.
fn bitmap_len(rows: usize) -> usize {
    rows.div_ceil(8)
}

/// One column of one batch, in the form the grid reads it back in.
#[derive(Clone, Debug)]
enum RenderedColumn {
    /// Every row is NULL. No data, no bitmap: the shape says it.
    Nulls,
    /// Text, rendered once. Row `i` is `data[offsets[i]..offsets[i + 1]]`, and
    /// is NULL rather than empty when its validity bit is clear.
    Text {
        /// Every value of the column, end to end.
        data: String,
        /// `rows + 1` boundaries into `data`.
        offsets: Vec<u32>,
        /// Set bit means the row holds a value.
        valid: Vec<u8>,
    },
    /// Large objects, whose bodies were left on the Java side.
    Lob {
        /// `u64::MAX` where the driver would not say how big.
        sizes: Vec<u64>,
        /// Set bit means the row holds a LOB rather than NULL.
        valid: Vec<u8>,
    },
}

/// LOB size the bridge sends when the driver would not answer.
const LOB_SIZE_UNKNOWN: u64 = u64::MAX;

/// One fetched batch, rendered.
#[derive(Clone, Debug)]
pub struct RenderedBatch {
    rows: usize,
    columns: Vec<RenderedColumn>,
}

/// Renders one batch against the result's logical column types.
///
/// Runs on the thread that fetched it — never on the UI thread. `columns` is
/// the `EXECUTE` response's `columns[]`; a batch with more columns than that
/// (which a well-formed bridge never sends) renders the extras as text.
pub fn render_batch(batch: &Batch, columns: &[ColumnInfo]) -> RenderedBatch {
    let rows = batch.rows();
    let rendered = batch
        .columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            if column.kind() == ColumnKind::Nulls {
                return RenderedColumn::Nulls;
            }
            let mut valid = vec![0u8; bitmap_len(rows)];

            if column.kind() == ColumnKind::Lob {
                let mut sizes = Vec::with_capacity(rows);
                for row in 0..rows {
                    match column.value(row) {
                        Some(Value::Lob { size, .. }) => {
                            set_valid(&mut valid, row);
                            sizes.push(size.unwrap_or(LOB_SIZE_UNKNOWN));
                        }
                        // NULL, or a row the batch is too short for; either way
                        // there is no object to size.
                        _ => sizes.push(LOB_SIZE_UNKNOWN),
                    }
                }
                return RenderedColumn::Lob { sizes, valid };
            }

            // The logical type decides the rendering. A column the response did
            // not describe is rendered as though it were not a `REAL`, which is
            // the safe way round: a widened `REAL` prints wide, which is
            // visible, where narrowing a `DOUBLE` would silently drop digits.
            let single_precision = columns
                .get(index)
                .is_some_and(ColumnInfo::is_single_precision);
            let mut data = String::new();
            let mut offsets = Vec::with_capacity(rows + 1);
            offsets.push(0);
            for row in 0..rows {
                if let Some(value) = column.value(row)
                    && !value.is_null()
                {
                    set_valid(&mut valid, row);
                    push_value(&mut data, &value, single_precision);
                }
                offsets.push(data.len() as u32);
            }
            RenderedColumn::Text {
                data,
                offsets,
                valid,
            }
        })
        .collect();

    RenderedBatch {
        rows,
        columns: rendered,
    }
}

/// One thing a statement produced.
#[derive(Debug)]
pub enum Step {
    /// A result set, and its first batch.
    Rows {
        /// The result's logical column types.
        columns: Vec<ColumnInfo>,
        /// The first batch, already rendered.
        batch: RenderedBatch,
        /// Whether the driver had run out of rows.
        complete: bool,
    },
    /// An update count.
    Message {
        /// Rows the statement changed, or zero for a statement that changes
        /// none — a `CREATE TABLE`, say.
        update_count: i64,
    },
}

/// One page of an already-open result.
pub struct Paged {
    /// The rows that came back, already rendered.
    pub batch: RenderedBatch,
    /// Whether that batch was the last of this result set.
    pub complete: bool,
    /// Further results of the same statement, picked up once this one ended.
    pub steps: Vec<Step>,
    /// Whether the cursor is parked on a result set that can still be paged.
    pub pageable: bool,
}

/// Walks a cursor's results, stopping at the first one that can still be paged.
///
/// `fresh` says whether the cursor is already sitting on a result nobody has
/// read — true straight after `EXECUTE`, false when resuming after a result set
/// ran out. Blocks; only ever called from a background thread.
pub fn advance(
    cursor: &mut Cursor,
    mut fresh: bool,
    fetch_rows: u32,
    steps: &mut Vec<Step>,
) -> Result<bool, JdbcError> {
    loop {
        if !fresh && cursor.more_results()?.is_exhausted() {
            return Ok(false);
        }
        fresh = false;

        let (has_result_set, update_count, exhausted, columns) = {
            let result = cursor.result();
            (
                result.has_result_set,
                result.update_count,
                result.is_exhausted(),
                result.columns.clone(),
            )
        };

        if has_result_set {
            let raw = cursor.fetch(fetch_rows)?;
            let complete = raw.is_last();
            let batch = render_batch(&raw, &columns);
            steps.push(Step::Rows {
                columns,
                batch,
                complete,
            });
            if !complete {
                // Advancing now would close the `ResultSet` the grid is about
                // to page. The walk resumes when the rows run out.
                return Ok(true);
            }
        } else if update_count >= 0 {
            steps.push(Step::Message { update_count });
        }

        if exhausted {
            return Ok(false);
        }
    }
}

/// Fetches one more batch of an open result, and walks on if it was the last.
pub fn page(
    cursor: &mut Cursor,
    columns: &[ColumnInfo],
    fetch_rows: u32,
) -> Result<Paged, JdbcError> {
    let raw = cursor.fetch(fetch_rows)?;
    let complete = raw.is_last();
    let batch = render_batch(&raw, columns);
    let mut steps = Vec::new();
    let pageable = if complete {
        advance(cursor, false, fetch_rows, &mut steps)?
    } else {
        true
    };
    Ok(Paged {
        batch,
        complete,
        steps,
        pageable,
    })
}

/// One column's heading, as the grid needs it.
#[derive(Clone, Debug)]
struct SourceColumn {
    name: String,
    kind: GridColumnKind,
    /// Whether the grid emphasises it as part of the primary key. False unless
    /// somebody who knows the key said otherwise; see
    /// [`ResultSource::mark_primary_keys`].
    primary_key: bool,
}

/// The rows of one result, batch by batch.
///
/// Grows rather than being rebuilt: a fetch appends a [`RenderedBatch`] and the
/// grid, now looking at a longer result, redraws. Nothing is copied and no
/// index is rebuilt beyond one `usize` pushed onto [`ResultSource::starts`].
#[derive(Debug, Default)]
pub struct ResultSource {
    columns: Vec<SourceColumn>,
    batches: Vec<RenderedBatch>,
    /// First global row of each batch, plus the total at the end. Always at
    /// least one entry, so `starts.last()` is the row count.
    starts: Vec<usize>,
    state: GridSourceState,
}

impl ResultSource {
    /// An empty source over the columns `EXECUTE` described.
    pub fn new(columns: &[ColumnInfo]) -> Self {
        Self {
            columns: columns
                .iter()
                .map(|column| SourceColumn {
                    name: column.display_name(),
                    kind: column_kind(column),
                    primary_key: false,
                })
                .collect(),
            batches: Vec::new(),
            starts: vec![0],
            state: GridSourceState::Complete,
        }
    }

    /// Appends a batch. Empty batches are dropped rather than stored — an empty
    /// one is how the bridge says "that was the last of them", and keeping it
    /// would put two equal entries in [`ResultSource::starts`].
    pub fn push(&mut self, batch: RenderedBatch) {
        if batch.rows == 0 {
            return;
        }
        self.starts.push(self.row_count() + batch.rows);
        self.batches.push(batch);
    }

    /// Says whether more rows are coming, or one is on its way.
    pub fn set_state(&mut self, state: GridSourceState) {
        self.state = state;
    }

    /// Marks the columns `keys` names as the primary key.
    ///
    /// Nothing here can work this out on its own — a result carries no key
    /// metadata — so it is told, by the one caller that asked the driver first:
    /// the data pane, which reads `DESCRIBE primary_keys` before it runs its
    /// `SELECT` (architecture document, §7.9). A query result never calls this
    /// and keeps the honest answer, which is "no key".
    ///
    /// Names are compared exactly, because both sides come from the same
    /// catalogue and a case-insensitive match would mark the wrong column on
    /// the products where two can differ by case alone.
    pub fn mark_primary_keys(&mut self, keys: &[String]) {
        for column in &mut self.columns {
            column.primary_key = keys.contains(&column.name);
        }
    }

    /// Which batch global row `row` is in, and where it is inside it.
    fn locate(&self, row: usize) -> Option<(&RenderedBatch, usize)> {
        if row >= self.row_count() {
            return None;
        }
        // `starts` is sorted and holds no duplicates — empty batches never get
        // in — so the batch is the last start that is not past `row`.
        let index = self.starts.partition_point(|start| *start <= row) - 1;
        let batch = self.batches.get(index)?;
        Some((batch, row - self.starts[index]))
    }
}

impl GridSource for ResultSource {
    fn column_count(&self) -> usize {
        self.columns.len()
    }

    fn column(&self, index: usize) -> GridColumn<'_> {
        match self.columns.get(index) {
            // The key marking is whatever [`ResultSource::mark_primary_keys`]
            // was told, which for a query result is nothing: it carries no key
            // metadata, and inventing one from the column name would be a guess
            // the user could not see through. See the architecture document,
            // §7.5 and §7.9.
            Some(column) => {
                GridColumn::new(&column.name, column.kind).primary_key(column.primary_key)
            }
            None => GridColumn::new("", GridColumnKind::Text),
        }
    }

    fn row_count(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    fn cell(&self, row: usize, column: usize) -> GridCell<'_> {
        let Some((batch, local)) = self.locate(row) else {
            return GridCell::Null;
        };
        match batch.columns.get(column) {
            None | Some(RenderedColumn::Nulls) => GridCell::Null,
            Some(RenderedColumn::Text {
                data,
                offsets,
                valid,
            }) => {
                if !is_valid(valid, local) {
                    return GridCell::Null;
                }
                let (start, end) = (offsets[local] as usize, offsets[local + 1] as usize);
                GridCell::Text(&data[start..end])
            }
            Some(RenderedColumn::Lob { sizes, valid }) => {
                if !is_valid(valid, local) {
                    return GridCell::Null;
                }
                let size = sizes[local];
                GridCell::Lob {
                    size: (size != LOB_SIZE_UNKNOWN).then_some(size),
                }
            }
        }
    }

    fn state(&self) -> GridSourceState {
        self.state
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes an `RDB1` batch of text columns and decodes it back.
    ///
    /// Goes through the wire format rather than building a [`Batch`] directly —
    /// there is no other way to make one, and it is the right way round anyway:
    /// a test that skipped the codec would not be testing what the bridge
    /// actually sends. A column whose every row is `None` is written as
    /// [`ColumnKind::Nulls`], which is what the encoder does and what the
    /// per-batch kind rule exists for.
    pub(crate) fn encode(columns: &[&[Option<&str>]], last: bool) -> Batch {
        let rows = columns.first().map_or(0, |column| column.len());
        let mut out = Vec::new();
        out.extend_from_slice(b"RDB1");
        out.extend_from_slice(&(columns.len() as u32).to_le_bytes());
        out.extend_from_slice(&(rows as u32).to_le_bytes());
        out.push(u8::from(last));

        for column in columns {
            assert_eq!(column.len(), rows, "columns must be the same length");
            let mut valid = vec![0u8; bitmap_len(rows)];
            let all_null = column.iter().all(Option::is_none);
            let mut payload = Vec::new();
            if all_null {
                payload.extend_from_slice(&valid);
                out.push(ColumnKind::Nulls.as_byte());
            } else {
                let mut data = Vec::new();
                let mut offsets: Vec<u32> = vec![0];
                for (row, value) in column.iter().enumerate() {
                    if let Some(text) = value {
                        set_valid(&mut valid, row);
                        data.extend_from_slice(text.as_bytes());
                    }
                    offsets.push(data.len() as u32);
                }
                payload.extend_from_slice(&valid);
                for offset in &offsets {
                    payload.extend_from_slice(&offset.to_le_bytes());
                }
                payload.extend_from_slice(&data);
                out.push(ColumnKind::Str.as_byte());
            }
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&payload);
        }

        Batch::decode(&out).expect("the encoder and the decoder agree")
    }

    /// A batch that is not the last one.
    pub(crate) fn batch(columns: &[&[Option<&str>]]) -> Batch {
        encode(columns, false)
    }

    /// Builds a `ColumnInfo` of one `java.sql.Types` constant.
    pub(crate) fn info(index: i32, name: &str, sql_type: i32, precision: i32) -> ColumnInfo {
        serde_json::from_str(&format!(
            r#"{{"index":{index},"name":"{name}","label":"{name}","table":null,"schema":null,
                 "catalog":null,"type":{sql_type},"type_name":"T","jdbc_type":"T",
                 "class_name":null,"precision":{precision},"scale":0,"display_size":0,
                 "nullable":2,"auto_increment":false,"signed":true,"read_only":false,"kind":4}}"#
        ))
        .expect("parses")
    }

    #[test]
    fn the_rendering_matches_the_crates_own() {
        // `push_value` is a second copy of `Value::to_text`, written to avoid a
        // `String` per cell. This is what keeps the copy honest.
        let real = info(1, "R", sql_types::REAL, 0);
        let double = info(1, "D", sql_types::DOUBLE, 0);
        let text = info(1, "T", 12, 0);
        let widened = 0.1f32 as f64;

        for (value, column) in [
            (Value::I64(-42), &text),
            (Value::F64(widened), &real),
            (Value::F64(widened), &double),
            (Value::Bool(true), &text),
            (Value::Bool(false), &text),
            (Value::Str("hello"), &text),
            (Value::Str(""), &text),
            (Value::Bin(&[0xde, 0xad, 0x00]), &text),
        ] {
            let mut rendered = String::new();
            push_value(&mut rendered, &value, column.is_single_precision());
            assert_eq!(
                Some(rendered),
                value.to_text(column),
                "{value:?} against {:?}",
                column.type_name
            );
        }
    }

    #[test]
    fn a_null_and_an_empty_string_survive_the_round_trip() {
        // Both are a zero-length slice; only the bitmap tells them apart, and
        // losing that is the failure this whole module is arranged to prevent.
        let columns = vec![info(1, "T", 12, 0)];
        let batch = batch(&[&[None, Some(""), Some("x")]]);
        let mut source = ResultSource::new(&columns);
        source.push(render_batch(&batch, &columns));

        assert_eq!(source.row_count(), 3);
        assert_eq!(source.cell(0, 0), GridCell::Null);
        assert_eq!(source.cell(1, 0), GridCell::Text(""));
        assert_eq!(source.cell(2, 0), GridCell::Text("x"));
    }

    #[test]
    fn an_all_null_batch_is_read_off_its_own_kind() {
        // The column arrives as `NULLS` even though the one before it was text,
        // which is the rule a decoder that cached the kind per cursor breaks.
        let columns = vec![info(1, "T", 12, 0)];
        let mut source = ResultSource::new(&columns);
        source.push(render_batch(&batch(&[&[Some("x")]]), &columns));
        source.push(render_batch(&batch(&[&[None, None]]), &columns));

        assert_eq!(source.row_count(), 3);
        assert_eq!(source.cell(0, 0), GridCell::Text("x"));
        assert_eq!(source.cell(1, 0), GridCell::Null);
        assert_eq!(source.cell(2, 0), GridCell::Null);
    }

    #[test]
    fn a_row_is_found_in_the_batch_it_landed_in() {
        let columns = vec![info(1, "T", 12, 0)];
        let mut source = ResultSource::new(&columns);
        for chunk in [
            &["a0", "a1", "a2"][..],
            &["b0"][..],
            &[][..], // dropped: an empty batch is the end marker, not a row
            &["c0", "c1"][..],
        ] {
            let owned: Vec<Option<&str>> = chunk.iter().map(|value| Some(*value)).collect();
            source.push(render_batch(&batch(&[&owned]), &columns));
        }

        assert_eq!(source.row_count(), 6);
        assert_eq!(source.batches.len(), 3, "the empty batch was not stored");
        for (row, expected) in ["a0", "a1", "a2", "b0", "c0", "c1"].into_iter().enumerate() {
            assert_eq!(source.cell(row, 0), GridCell::Text(expected), "row {row}");
        }
        // Past the end answers rather than panicking: the grid can ask for a row
        // that a re-run took away between the layout and the paint.
        assert_eq!(source.cell(6, 0), GridCell::Null);
        assert_eq!(source.cell(0, 9), GridCell::Null);
    }

    #[test]
    fn alignment_follows_the_logical_type_and_not_the_wire() {
        use sql_types::*;
        for (sql_type, precision, expected) in [
            (INTEGER, 0, GridColumnKind::Number),
            (DECIMAL, 20, GridColumnKind::Number),
            (REAL, 0, GridColumnKind::Number),
            (BOOLEAN, 0, GridColumnKind::Boolean),
            (BIT, 1, GridColumnKind::Boolean),
            // MySQL's BIT(8) is a byte string, not a truth value.
            (BIT, 8, GridColumnKind::Binary),
            (TIMESTAMP, 0, GridColumnKind::Temporal),
            (BLOB, 0, GridColumnKind::Binary),
            (12, 0, GridColumnKind::Text),
            // Anything the table does not name is text, not a panic.
            (9999, 0, GridColumnKind::Text),
        ] {
            assert_eq!(
                column_kind(&info(1, "C", sql_type, precision)),
                expected,
                "type {sql_type}"
            );
        }

        let columns = vec![info(1, "N", INTEGER, 0), info(2, "S", 12, 0)];
        let source = ResultSource::new(&columns);
        assert_eq!(
            source.column(0).align,
            rudbman_grid::GridColumnAlign::Right,
            "digits line up by place value"
        );
        assert_eq!(source.column(1).align, rudbman_grid::GridColumnAlign::Left);
        assert!(!source.column(0).primary_key, "a query result has no key");
    }
}
