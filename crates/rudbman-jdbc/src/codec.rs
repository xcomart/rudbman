//! Decoder for the `RDB1` result batch format (architecture document, §4.6).
//!
//! ```text
//! Batch  := Header Column*
//! Header := "RDB1"(4B) | u32 col_count | u32 row_count | u8 flags
//!           flags bit0 = this is the last batch
//! Column := u8 kind | u32 payload_len | payload
//! ```
//!
//! Everything is little-endian, and every `payload` starts with a validity
//! bitmap of `ceil(row_count/8)` bytes in which a **set bit means non-null**.
//!
//! # The rules that are easy to get quietly wrong
//!
//! These are the points where the format leaves room for two readings, and
//! where disagreeing with the encoder draws wrong data instead of failing:
//!
//! * **Bits are LSB-first.** Row `i` is byte `i >> 3`, bit `i & 7`. Packed
//!   [`ColumnKind::Bool`] values use the same order.
//! * **The validity bitmap is always there**, [`ColumnKind::Nulls`] included.
//!   Only the value area is omitted.
//! * **A column's kind changes from batch to batch.** Any column whose values
//!   are all NULL in a given batch is shortened to [`ColumnKind::Nulls`],
//!   whatever its declared type. The kind byte therefore has to be read per
//!   batch — the `kind` in the `EXECUTE` response's `columns[]` is a hint about
//!   what a *full* batch would use, nothing more.
//! * **NULL rows still occupy their slot** in every fixed-width value area,
//!   zero-filled, so indexes line up with the bitmap without counting ranks.
//! * **NULL and the empty string are both zero-length slices** in `STR` and
//!   `BIN`. Only the bitmap tells them apart.
//! * **`row_count` may be 0**, in which case every column is `NULLS` with an
//!   empty payload. That is what keeps `STR` from needing an `offsets[1]`.
//! * **The last-batch flag is only set when the driver ran out of rows.** A
//!   batch that filled its row limit exactly reports `flags = 0` and the next
//!   `FETCH` answers with 0 rows and bit 0 set.
//!
//! # Trust
//!
//! These bytes were produced from data a driver handed the bridge, so nothing
//! here is taken on faith. Every length, offset and bitmap is checked against
//! the buffer, no allocation happens before the length that would size it has
//! been validated, and a batch that does not add up comes back as a
//! [`CodecError`] rather than a panic.

use std::fmt;

/// Magic bytes every batch starts with.
pub const MAGIC: [u8; 4] = *b"RDB1";

/// Header flag bit 0: no further batches follow on this cursor.
const FLAG_LAST: u8 = 0x01;

/// Sentinel `size` meaning the driver would not report a LOB length.
const LOB_SIZE_UNKNOWN: u64 = u64::MAX;

/// Smallest possible encoding of one column record: kind byte plus length.
const COLUMN_HEADER_LEN: usize = 5;

/// Why a batch could not be decoded.
///
/// Every variant means the bytes are not a batch this version can read. None of
/// them are recoverable; they are reported so the caller can log the batch and
/// fail the query instead of drawing something wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    /// The buffer ended in the middle of a field.
    #[error("truncated batch: {needed} bytes needed at offset {at}, only {available} left")]
    Truncated {
        /// Byte offset the read started at.
        at: usize,
        /// Bytes the read wanted.
        needed: usize,
        /// Bytes actually left in the buffer.
        available: usize,
    },

    /// The buffer does not start with `RDB1`.
    #[error("not an RDB1 batch: magic bytes are {found:02x?}")]
    BadMagic {
        /// The four bytes found where the magic was expected.
        found: [u8; 4],
    },

    /// A column announced a kind this version does not know.
    ///
    /// Means the bridge JAR is newer than this crate, not that the data is bad.
    #[error("column {column}: unknown kind byte {kind}")]
    UnknownKind {
        /// Zero-based column index.
        column: usize,
        /// The unrecognised kind byte.
        kind: u8,
    },

    /// A column's `payload_len` does not match the bytes its kind needs.
    #[error(
        "column {column}: kind {kind} needs {expected} bytes for {rows} rows, \
         but payload_len is {declared}"
    )]
    PayloadLength {
        /// Zero-based column index.
        column: usize,
        /// The kind the column announced.
        kind: u8,
        /// Rows the batch header declared.
        rows: usize,
        /// Bytes that kind needs for that many rows.
        expected: u64,
        /// Bytes the column said it had.
        declared: usize,
    },

    /// A variable-length column's offset array is not usable.
    #[error("column {column}: offset[{index}] = {offset} is not valid for a {len}-byte value area")]
    BadOffset {
        /// Zero-based column index.
        column: usize,
        /// Index into the offset array.
        index: usize,
        /// The offending offset.
        offset: u64,
        /// Length of the value area the offsets index into.
        len: usize,
    },

    /// A `STR` column's bytes are not valid UTF-8, or an offset lands inside a
    /// multi-byte character.
    ///
    /// The bridge writes `String.getBytes(UTF_8)`, so this can only be
    /// corruption.
    #[error("column {column}: value area is not valid UTF-8")]
    InvalidUtf8 {
        /// Zero-based column index.
        column: usize,
    },

    /// A `NULLS` column carried a set validity bit, which contradicts itself:
    /// the row is marked non-null but there is no value area to read it from.
    #[error("column {column}: kind NULLS has a non-null bit set at row {row}")]
    NullsWithValidBit {
        /// Zero-based column index.
        column: usize,
        /// The row whose bit was set.
        row: usize,
    },

    /// Bytes are left over after the last column.
    ///
    /// The format is fixed for the `RDB1` magic; a longer payload means the
    /// batch was mis-parsed or truncated in the middle.
    #[error("{extra} trailing bytes after the last column")]
    TrailingBytes {
        /// Bytes left unread.
        extra: usize,
    },
}

/// Physical encoding of one column *in one batch*.
///
/// This is a transport decision only. How a value is presented — right
/// alignment, NULL rendering, copy format — follows from the logical JDBC type
/// in [`ColumnInfo`](crate::ColumnInfo), not from this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    /// Every row is NULL; no value area follows the bitmap.
    Nulls,
    /// `row_count` signed 64-bit integers.
    I64,
    /// `row_count` IEEE-754 doubles, raw bits, so NaN and the infinities
    /// survive the trip.
    F64,
    /// Packed bits, LSB-first, `ceil(row_count/8)` bytes.
    Bool,
    /// `u32 offsets[row_count + 1]` followed by UTF-8 bytes.
    Str,
    /// Same layout as [`ColumnKind::Str`], raw bytes.
    Bin,
    /// `row_count` pairs of `(u64 lob_id, u64 size)`; the body is fetched
    /// separately.
    Lob,
}

impl ColumnKind {
    /// Maps a kind byte, or `None` when the byte is not one this version knows.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => ColumnKind::Nulls,
            1 => ColumnKind::I64,
            2 => ColumnKind::F64,
            3 => ColumnKind::Bool,
            4 => ColumnKind::Str,
            5 => ColumnKind::Bin,
            6 => ColumnKind::Lob,
            _ => return None,
        })
    }

    /// The wire byte for this kind.
    pub fn as_byte(self) -> u8 {
        match self {
            ColumnKind::Nulls => 0,
            ColumnKind::I64 => 1,
            ColumnKind::F64 => 2,
            ColumnKind::Bool => 3,
            ColumnKind::Str => 4,
            ColumnKind::Bin => 5,
            ColumnKind::Lob => 6,
        }
    }
}

/// One cell, borrowed from the batch that holds it.
///
/// [`Value::Null`] is a SQL NULL. "No such row or column" is expressed by the
/// `Option` the accessors return, so the two are never confused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    /// SQL NULL.
    Null,
    /// An integer type (`TINYINT` through `BIGINT`).
    I64(i64),
    /// A floating point type. `REAL` arrives here too, widened — see
    /// [`Value::to_text`].
    F64(f64),
    /// A boolean.
    Bool(bool),
    /// Text. Also carries `DECIMAL`, the date and time types, `UUID`,
    /// `INTERVAL`, arrays and vendor types, as the driver rendered them.
    Str(&'a str),
    /// Raw bytes.
    Bin(&'a [u8]),
    /// A reference to a LOB that was deliberately left on the Java side.
    Lob {
        /// Opaque identifier, unique within the cursor.
        id: u64,
        /// Octets for a binary LOB, characters for a character LOB, `None` when
        /// the driver would not say.
        size: Option<u64>,
    },
}

impl Value<'_> {
    /// Whether this cell is SQL NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// One decoded column of one batch.
#[derive(Clone, Debug)]
pub struct Column {
    kind: ColumnKind,
    rows: usize,
    validity: Vec<u8>,
    values: ColumnValues,
}

/// The value area of a column, once decoded.
#[derive(Clone, Debug)]
enum ColumnValues {
    /// No value area: the column was shortened because every row is NULL.
    Nulls,
    I64(Vec<i64>),
    F64(Vec<f64>),
    /// Packed bits, LSB-first, exactly like the validity bitmap.
    Bool(Vec<u8>),
    Str {
        offsets: Vec<u32>,
        data: String,
    },
    Bin {
        offsets: Vec<u32>,
        data: Vec<u8>,
    },
    Lob {
        ids: Vec<u64>,
        sizes: Vec<u64>,
    },
}

impl Column {
    /// The physical encoding this column used **in this batch**.
    ///
    /// Not stable across batches of the same cursor: an all-NULL batch reports
    /// [`ColumnKind::Nulls`] for a column that was `I64` in the batch before.
    pub fn kind(&self) -> ColumnKind {
        self.kind
    }

    /// Number of rows in the batch this column belongs to.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Whether row `row` is SQL NULL. Out-of-range rows are reported as `false`
    /// — use [`Column::value`], which answers `None` for them.
    pub fn is_null(&self, row: usize) -> bool {
        row < self.rows && !self.bit_set(row)
    }

    /// The value of row `row`, or `None` when the row is out of range.
    pub fn value(&self, row: usize) -> Option<Value<'_>> {
        if row >= self.rows {
            return None;
        }
        if !self.bit_set(row) {
            return Some(Value::Null);
        }
        Some(match &self.values {
            // Unreachable in a well-formed batch: decoding rejects a NULLS
            // column with a set bit. Answering NULL keeps this total anyway.
            ColumnValues::Nulls => Value::Null,
            ColumnValues::I64(values) => Value::I64(values[row]),
            ColumnValues::F64(values) => Value::F64(values[row]),
            ColumnValues::Bool(bits) => Value::Bool(bit_set(bits, row)),
            ColumnValues::Str { offsets, data } => {
                let (start, end) = (offsets[row] as usize, offsets[row + 1] as usize);
                Value::Str(&data[start..end])
            }
            ColumnValues::Bin { offsets, data } => {
                let (start, end) = (offsets[row] as usize, offsets[row + 1] as usize);
                Value::Bin(&data[start..end])
            }
            ColumnValues::Lob { ids, sizes } => Value::Lob {
                id: ids[row],
                size: (sizes[row] != LOB_SIZE_UNKNOWN).then_some(sizes[row]),
            },
        })
    }

    /// Reads the validity bitmap: set bit means non-null, LSB-first.
    fn bit_set(&self, row: usize) -> bool {
        bit_set(&self.validity, row)
    }
}

/// LSB-first bit test shared by the validity bitmap and packed booleans.
///
/// Row `i` lives in byte `i >> 3` at bit `i & 7`. Getting this backwards is the
/// classic way to read a batch that decodes without complaint and is wrong.
fn bit_set(bits: &[u8], index: usize) -> bool {
    bits.get(index >> 3)
        .is_some_and(|byte| byte & (1 << (index & 7)) != 0)
}

/// One decoded `RDB1` batch.
#[derive(Clone, Debug)]
pub struct Batch {
    rows: usize,
    last: bool,
    columns: Vec<Column>,
}

impl Batch {
    /// Decodes a batch.
    ///
    /// # Errors
    ///
    /// Returns a [`CodecError`] for anything that does not add up. It never
    /// panics, whatever the bytes are.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut reader = Reader::new(bytes);

        let magic = reader.take(MAGIC.len())?;
        if magic != MAGIC {
            let mut found = [0u8; 4];
            found.copy_from_slice(magic);
            return Err(CodecError::BadMagic { found });
        }

        let col_count = reader.u32()? as usize;
        let rows = reader.u32()? as usize;
        let last = reader.u8()? & FLAG_LAST != 0;

        // Guard the `Vec` allocation before making it: a corrupt col_count of
        // four billion would otherwise reserve gigabytes for a batch that
        // cannot possibly hold that many column records.
        let smallest = (col_count as u64).saturating_mul(COLUMN_HEADER_LEN as u64);
        if smallest > reader.remaining() as u64 {
            return Err(CodecError::Truncated {
                at: reader.position(),
                needed: smallest.min(usize::MAX as u64) as usize,
                available: reader.remaining(),
            });
        }

        let mut columns = Vec::with_capacity(col_count);
        for index in 0..col_count {
            columns.push(Column::decode(&mut reader, index, rows)?);
        }

        if reader.remaining() != 0 {
            return Err(CodecError::TrailingBytes {
                extra: reader.remaining(),
            });
        }

        Ok(Batch {
            rows,
            last,
            columns,
        })
    }

    /// Number of rows in this batch.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Whether the cursor is exhausted.
    ///
    /// Only true once the driver ran out of rows. A batch that filled its row
    /// limit exactly answers `false` even when it happens to be the last one
    /// with data in it; the next `FETCH` then returns an empty batch with this
    /// set.
    pub fn is_last(&self) -> bool {
        self.last
    }

    /// The columns, in result order.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Number of columns. Zero for a statement that produced no result set.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// The value at `(row, column)`, or `None` when either index is out of
    /// range.
    pub fn value(&self, row: usize, column: usize) -> Option<Value<'_>> {
        self.columns.get(column)?.value(row)
    }
}

impl Column {
    /// Decodes one `u8 kind | u32 payload_len | payload` record.
    fn decode(reader: &mut Reader<'_>, index: usize, rows: usize) -> Result<Self, CodecError> {
        let kind_byte = reader.u8()?;
        let payload_len = reader.u32()? as usize;
        let payload = reader.take(payload_len)?;

        let kind = ColumnKind::from_byte(kind_byte).ok_or(CodecError::UnknownKind {
            column: index,
            kind: kind_byte,
        })?;

        // The bitmap comes first for every kind, NULLS included.
        let bitmap_len = rows.div_ceil(8);
        if payload.len() < bitmap_len {
            return Err(CodecError::PayloadLength {
                column: index,
                kind: kind_byte,
                rows,
                expected: bitmap_len as u64,
                declared: payload_len,
            });
        }
        let (validity, values) = payload.split_at(bitmap_len);

        let decoded = match kind {
            ColumnKind::Nulls => {
                expect_len(index, kind_byte, rows, values.len(), 0)?;
                // A set bit here would claim a value that was never encoded.
                if let Some(row) = (0..rows).find(|row| bit_set(validity, *row)) {
                    return Err(CodecError::NullsWithValidBit { column: index, row });
                }
                ColumnValues::Nulls
            }
            ColumnKind::I64 => {
                expect_len(index, kind_byte, rows, values.len(), 8 * rows as u64)?;
                ColumnValues::I64(
                    values
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|chunk| i64::from_le_bytes(*chunk))
                        .collect(),
                )
            }
            ColumnKind::F64 => {
                expect_len(index, kind_byte, rows, values.len(), 8 * rows as u64)?;
                ColumnValues::F64(
                    values
                        .as_chunks::<8>()
                        .0
                        .iter()
                        // Raw bits, not a parsed decimal: NaN payloads and the
                        // infinities have to survive unchanged.
                        .map(|chunk| f64::from_le_bytes(*chunk))
                        .collect(),
                )
            }
            ColumnKind::Bool => {
                expect_len(index, kind_byte, rows, values.len(), bitmap_len as u64)?;
                ColumnValues::Bool(values.to_vec())
            }
            ColumnKind::Str | ColumnKind::Bin => {
                let (offsets, data) = decode_var(index, kind_byte, rows, values)?;
                if kind == ColumnKind::Bin {
                    ColumnValues::Bin {
                        offsets,
                        data: data.to_vec(),
                    }
                } else {
                    let text = std::str::from_utf8(data)
                        .map_err(|_| CodecError::InvalidUtf8 { column: index })?;
                    // Slicing by byte offset is only sound if every offset sits
                    // on a character boundary; a valid UTF-8 blob alone does not
                    // guarantee that.
                    if offsets
                        .iter()
                        .any(|offset| !text.is_char_boundary(*offset as usize))
                    {
                        return Err(CodecError::InvalidUtf8 { column: index });
                    }
                    ColumnValues::Str {
                        offsets,
                        data: text.to_string(),
                    }
                }
            }
            ColumnKind::Lob => {
                expect_len(index, kind_byte, rows, values.len(), 16 * rows as u64)?;
                let mut ids = Vec::with_capacity(rows);
                let mut sizes = Vec::with_capacity(rows);
                for pair in values.as_chunks::<16>().0 {
                    ids.push(u64::from_le_bytes(pair[..8].try_into().expect("8 bytes")));
                    sizes.push(u64::from_le_bytes(pair[8..].try_into().expect("8 bytes")));
                }
                ColumnValues::Lob { ids, sizes }
            }
        };

        Ok(Column {
            kind,
            rows,
            validity: validity.to_vec(),
            values: decoded,
        })
    }
}

/// Reads the `u32 offsets[rows + 1]` header shared by `STR` and `BIN` and
/// returns it with the data area it describes.
fn decode_var(
    column: usize,
    kind: u8,
    rows: usize,
    values: &[u8],
) -> Result<(Vec<u32>, &[u8]), CodecError> {
    // Checked in u64 so that a huge row count cannot wrap the multiplication
    // and pass a length test it should have failed.
    let offsets_len = 4u64 * (rows as u64 + 1);
    if (values.len() as u64) < offsets_len {
        return Err(CodecError::PayloadLength {
            column,
            kind,
            rows,
            expected: offsets_len,
            declared: values.len(),
        });
    }
    let (raw, data) = values.split_at(offsets_len as usize);

    let offsets: Vec<u32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect();

    // Monotonic, starting at 0 and ending exactly at the end of the data area:
    // anything else and a row would borrow bytes from its neighbour.
    let mut previous = 0u32;
    for (index, offset) in offsets.iter().copied().enumerate() {
        if offset < previous || offset as usize > data.len() {
            return Err(CodecError::BadOffset {
                column,
                index,
                offset: offset as u64,
                len: data.len(),
            });
        }
        previous = offset;
    }
    if previous as usize != data.len() {
        return Err(CodecError::BadOffset {
            column,
            index: rows,
            offset: previous as u64,
            len: data.len(),
        });
    }

    Ok((offsets, data))
}

/// Asserts that a value area is exactly as long as its kind requires.
fn expect_len(
    column: usize,
    kind: u8,
    rows: usize,
    actual: usize,
    expected: u64,
) -> Result<(), CodecError> {
    if actual as u64 == expected {
        return Ok(());
    }
    Err(CodecError::PayloadLength {
        column,
        kind,
        rows,
        expected,
        declared: actual,
    })
}

/// A bounds-checked little-endian cursor over the batch buffer.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn position(&self) -> usize {
        self.pos
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        if len > self.remaining() {
            return Err(CodecError::Truncated {
                at: self.pos,
                needed: len,
                available: self.remaining(),
            });
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
}

impl fmt::Display for ColumnKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ColumnKind::Nulls => "NULLS",
            ColumnKind::I64 => "I64",
            ColumnKind::F64 => "F64",
            ColumnKind::Bool => "BOOL",
            ColumnKind::Str => "STR",
            ColumnKind::Bin => "BIN",
            ColumnKind::Lob => "LOB",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal encoder, written from the format description rather than from
    /// the decoder, so the tests below are not simply the decoder agreeing with
    /// itself. The Java `FetchRoundTripTest` does the same in the other
    /// direction.
    struct Encoder {
        rows: usize,
        last: bool,
        columns: Vec<u8>,
        column_count: u32,
    }

    impl Encoder {
        fn new(rows: usize, last: bool) -> Self {
            Encoder {
                rows,
                last,
                columns: Vec::new(),
                column_count: 0,
            }
        }

        /// Appends a column from a validity bitmap and a value area.
        fn column(mut self, kind: u8, valid: &[bool], values: &[u8]) -> Self {
            let mut payload = vec![0u8; self.rows.div_ceil(8)];
            for (row, set) in valid.iter().enumerate() {
                if *set {
                    payload[row >> 3] |= 1 << (row & 7);
                }
            }
            payload.extend_from_slice(values);
            self.columns.push(kind);
            self.columns
                .extend_from_slice(&(payload.len() as u32).to_le_bytes());
            self.columns.extend_from_slice(&payload);
            self.column_count += 1;
            self
        }

        fn finish(self) -> Vec<u8> {
            let mut out = Vec::from(MAGIC);
            out.extend_from_slice(&self.column_count.to_le_bytes());
            out.extend_from_slice(&(self.rows as u32).to_le_bytes());
            out.push(if self.last { FLAG_LAST } else { 0 });
            out.extend_from_slice(&self.columns);
            out
        }
    }

    /// `u32 offsets[n+1]` + data, as `STR` and `BIN` lay it out.
    fn var_values(items: &[&[u8]]) -> Vec<u8> {
        let mut offsets = Vec::new();
        let mut data = Vec::new();
        offsets.extend_from_slice(&0u32.to_le_bytes());
        for item in items {
            data.extend_from_slice(item);
            offsets.extend_from_slice(&(data.len() as u32).to_le_bytes());
        }
        offsets.extend_from_slice(&data);
        offsets
    }

    fn i64_values(items: &[i64]) -> Vec<u8> {
        items.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn header_flags_and_shape() {
        let batch = Batch::decode(&Encoder::new(0, true).finish()).expect("empty batch");
        assert_eq!(batch.rows(), 0);
        assert_eq!(batch.column_count(), 0);
        assert!(batch.is_last());
        assert_eq!(batch.value(0, 0), None);

        let batch = Batch::decode(&Encoder::new(0, false).finish()).expect("empty batch");
        assert!(!batch.is_last());
    }

    #[test]
    fn bitmap_is_lsb_first() {
        // Rows 0 and 9 non-null: byte 0 bit 0, byte 1 bit 1 => 0x01, 0x02.
        let mut valid = [false; 10];
        valid[0] = true;
        valid[9] = true;
        let bytes = Encoder::new(10, true)
            .column(ColumnKind::I64.as_byte(), &valid, &i64_values(&[7; 10]))
            .finish();
        // Assert the encoding really is what the norm describes before trusting
        // the decoder's reading of it.
        let bitmap_at = 4 + 4 + 4 + 1 + 1 + 4;
        assert_eq!(&bytes[bitmap_at..bitmap_at + 2], &[0x01, 0x02]);

        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.value(0, 0), Some(Value::I64(7)));
        assert_eq!(batch.value(9, 0), Some(Value::I64(7)));
        for row in 1..9 {
            assert_eq!(batch.value(row, 0), Some(Value::Null), "row {row}");
        }
    }

    #[test]
    fn null_rows_keep_their_slot_in_a_fixed_width_column() {
        let bytes = Encoder::new(3, true)
            .column(
                ColumnKind::I64.as_byte(),
                &[true, false, true],
                // The middle slot is present and zero-filled, so no rank
                // computation is needed to find row 2.
                &i64_values(&[10, 0, 30]),
            )
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.value(0, 0), Some(Value::I64(10)));
        assert_eq!(batch.value(1, 0), Some(Value::Null));
        assert_eq!(batch.value(2, 0), Some(Value::I64(30)));
    }

    #[test]
    fn nulls_kind_still_carries_a_bitmap_and_omits_only_the_values() {
        let bytes = Encoder::new(9, true)
            .column(ColumnKind::Nulls.as_byte(), &[false; 9], &[])
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.columns()[0].kind(), ColumnKind::Nulls);
        assert_eq!(batch.rows(), 9);
        for row in 0..9 {
            assert!(batch.columns()[0].is_null(row));
            assert_eq!(batch.value(row, 0), Some(Value::Null));
        }
    }

    #[test]
    fn a_nulls_column_that_claims_a_non_null_row_is_rejected() {
        let mut valid = [false; 3];
        valid[2] = true;
        let bytes = Encoder::new(3, true)
            .column(ColumnKind::Nulls.as_byte(), &valid, &[])
            .finish();
        assert_eq!(
            Batch::decode(&bytes).unwrap_err(),
            CodecError::NullsWithValidBit { column: 0, row: 2 }
        );
    }

    #[test]
    fn null_and_the_empty_string_differ_only_in_the_bitmap() {
        let bytes = Encoder::new(2, true)
            .column(
                ColumnKind::Str.as_byte(),
                &[false, true],
                &var_values(&[b"", b""]),
            )
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.value(0, 0), Some(Value::Null));
        assert_eq!(batch.value(1, 0), Some(Value::Str("")));
    }

    #[test]
    fn packed_booleans_use_the_same_bit_order_as_the_bitmap() {
        // Rows 0..3 valid, values true, false, true, true => 0b1101 = 0x0d.
        let bytes = Encoder::new(4, true)
            .column(ColumnKind::Bool.as_byte(), &[true; 4], &[0x0d])
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.value(0, 0), Some(Value::Bool(true)));
        assert_eq!(batch.value(1, 0), Some(Value::Bool(false)));
        assert_eq!(batch.value(2, 0), Some(Value::Bool(true)));
        assert_eq!(batch.value(3, 0), Some(Value::Bool(true)));
    }

    #[test]
    fn multi_byte_text_survives() {
        let bytes = Encoder::new(2, true)
            .column(
                ColumnKind::Str.as_byte(),
                &[true, true],
                &var_values(&["안녕".as_bytes(), "ünïcode ✓".as_bytes()]),
            )
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(batch.value(0, 0), Some(Value::Str("안녕")));
        assert_eq!(batch.value(1, 0), Some(Value::Str("ünïcode ✓")));
    }

    #[test]
    fn lob_cells_carry_an_id_and_an_optional_size() {
        let mut values = Vec::new();
        values.extend_from_slice(&7u64.to_le_bytes());
        values.extend_from_slice(&1024u64.to_le_bytes());
        values.extend_from_slice(&8u64.to_le_bytes());
        values.extend_from_slice(&LOB_SIZE_UNKNOWN.to_le_bytes());
        let bytes = Encoder::new(2, true)
            .column(ColumnKind::Lob.as_byte(), &[true, true], &values)
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        assert_eq!(
            batch.value(0, 0),
            Some(Value::Lob {
                id: 7,
                size: Some(1024)
            })
        );
        assert_eq!(
            batch.value(1, 0),
            Some(Value::Lob { id: 8, size: None }),
            "the -1 sentinel means the driver would not report a length"
        );
    }

    #[test]
    fn floats_keep_their_raw_bits() {
        let values: Vec<u8> = [f64::NAN, f64::INFINITY, -0.0f64]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let bytes = Encoder::new(3, true)
            .column(ColumnKind::F64.as_byte(), &[true; 3], &values)
            .finish();
        let batch = Batch::decode(&bytes).expect("decodes");
        let Some(Value::F64(nan)) = batch.value(0, 0) else {
            panic!("expected a float")
        };
        assert!(nan.is_nan());
        assert_eq!(batch.value(1, 0), Some(Value::F64(f64::INFINITY)));
        let Some(Value::F64(zero)) = batch.value(2, 0) else {
            panic!("expected a float")
        };
        assert!(zero.is_sign_negative());
    }

    // --- corruption: every one of these must be an error, never a panic -----

    #[test]
    fn a_foreign_buffer_is_not_a_batch() {
        assert!(matches!(
            Batch::decode(b"XXXX\0\0\0\0\0\0\0\0\0"),
            Err(CodecError::BadMagic { .. })
        ));
        assert!(matches!(
            Batch::decode(b""),
            Err(CodecError::Truncated { .. })
        ));
        assert!(matches!(
            Batch::decode(b"RDB1\x01"),
            Err(CodecError::Truncated { .. })
        ));
    }

    #[test]
    fn a_truncated_payload_is_an_error() {
        let full = Encoder::new(3, true)
            .column(
                ColumnKind::I64.as_byte(),
                &[true, true, true],
                &i64_values(&[1, 2, 3]),
            )
            .finish();
        // Every prefix of a valid batch has to be rejected rather than read.
        for cut in 0..full.len() {
            let result = Batch::decode(&full[..cut]);
            assert!(result.is_err(), "prefix of {cut} bytes decoded");
        }
        assert!(Batch::decode(&full).is_ok());
    }

    #[test]
    fn a_payload_length_that_runs_past_the_buffer_is_an_error() {
        let mut bytes = Encoder::new(1, true)
            .column(ColumnKind::I64.as_byte(), &[true], &i64_values(&[1]))
            .finish();
        // Patch payload_len to 4GB - 1 without adding any bytes.
        let len_at = 4 + 4 + 4 + 1 + 1;
        bytes[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Batch::decode(&bytes),
            Err(CodecError::Truncated { .. })
        ));
    }

    #[test]
    fn a_column_count_no_buffer_could_hold_is_an_error() {
        let mut bytes = Encoder::new(0, true).finish();
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Batch::decode(&bytes),
            Err(CodecError::Truncated { .. })
        ));
    }

    #[test]
    fn a_value_area_of_the_wrong_size_is_an_error() {
        let bytes = Encoder::new(3, true)
            .column(
                ColumnKind::I64.as_byte(),
                &[true, true, true],
                &i64_values(&[1, 2]),
            )
            .finish();
        assert!(matches!(
            Batch::decode(&bytes),
            Err(CodecError::PayloadLength { .. })
        ));
    }

    #[test]
    fn offsets_outside_the_value_area_are_an_error() {
        let mut values = var_values(&[b"ab", b"cd"]);
        // Push the last offset past the end of the data.
        values[8..12].copy_from_slice(&99u32.to_le_bytes());
        let bytes = Encoder::new(2, true)
            .column(ColumnKind::Str.as_byte(), &[true, true], &values)
            .finish();
        assert!(matches!(
            Batch::decode(&bytes),
            Err(CodecError::BadOffset { .. })
        ));
    }

    #[test]
    fn offsets_that_go_backwards_are_an_error() {
        let mut values = var_values(&[b"ab", b"cd"]);
        values[4..8].copy_from_slice(&3u32.to_le_bytes());
        values[8..12].copy_from_slice(&1u32.to_le_bytes());
        let bytes = Encoder::new(2, true)
            .column(ColumnKind::Bin.as_byte(), &[true, true], &values)
            .finish();
        assert!(matches!(
            Batch::decode(&bytes),
            Err(CodecError::BadOffset { .. })
        ));
    }

    #[test]
    fn text_that_is_not_utf8_is_an_error_not_a_panic() {
        let bytes = Encoder::new(1, true)
            .column(
                ColumnKind::Str.as_byte(),
                &[true],
                &var_values(&[&[0xff, 0xfe]]),
            )
            .finish();
        assert_eq!(
            Batch::decode(&bytes).unwrap_err(),
            CodecError::InvalidUtf8 { column: 0 }
        );
    }

    #[test]
    fn an_offset_inside_a_character_is_an_error_not_a_panic() {
        // "안" is three bytes; cutting it at one byte is valid UTF-8 overall but
        // an impossible slice boundary.
        let mut values = var_values(&["안".as_bytes(), b""]);
        values[4..8].copy_from_slice(&1u32.to_le_bytes());
        let bytes = Encoder::new(2, true)
            .column(ColumnKind::Str.as_byte(), &[true, true], &values)
            .finish();
        assert_eq!(
            Batch::decode(&bytes).unwrap_err(),
            CodecError::InvalidUtf8 { column: 0 }
        );
    }

    #[test]
    fn an_unknown_kind_is_reported_rather_than_guessed() {
        let bytes = Encoder::new(1, true).column(9, &[true], &[]).finish();
        assert_eq!(
            Batch::decode(&bytes).unwrap_err(),
            CodecError::UnknownKind { column: 0, kind: 9 }
        );
    }

    #[test]
    fn trailing_bytes_are_reported() {
        let mut bytes = Encoder::new(0, true).finish();
        bytes.push(0);
        assert_eq!(
            Batch::decode(&bytes).unwrap_err(),
            CodecError::TrailingBytes { extra: 1 }
        );
    }
}
