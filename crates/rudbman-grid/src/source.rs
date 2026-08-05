//! Where a grid gets its rows, and the shape it insists on seeing them in.
//!
//! The grid never touches a result set. It is handed one through [`GridSource`],
//! exactly as the tree is handed its nodes through
//! [`TreeSource`](rudbman_ui::TreeSource) — and for a sharper reason than reuse:
//! the thing a real grid is showing is a `rudbman-jdbc` batch, and a crate that
//! knew that type would need a JVM to run its own unit tests. Behind this trait
//! a test source is twenty lines and a million rows cost nothing, which is what
//! makes "does it still only touch the visible rows?" a thing that can be
//! asserted rather than eyeballed.
//!
//! ## Values are already strings
//!
//! [`GridCell::Text`] borrows from the source rather than owning, so drawing a
//! screenful of cells allocates nothing per cell for the value itself. That is
//! only possible because the values *are* strings by the time they reach here:
//! the binary codec (architecture document, §4.6) hands over its `STR` family
//! already decoded, and a grid is a view of text. A source that would have to
//! format a number on the way out has nowhere to put the result, which is the
//! constraint saying so.
//!
//! ## Null is not empty
//!
//! [`GridCell::Null`] and `GridCell::Text("")` are different values and are
//! drawn differently — the marker `NULL` in [`Theme::grid_null`], against a cell
//! with nothing in it. Too many tools cannot tell you which one is in front of
//! you (architecture document, §7.5), and a source that flattens the two here
//! has already lost the distinction whatever the widget does.
//!
//! [`Theme::grid_null`]: rudbman_ui::Theme#structfield.grid_null

use gpui::SharedString;

/// The text drawn in a cell that holds no value.
pub const NULL_TEXT: &str = "NULL";

/// Which way the values of a column line up in their cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridColumnAlign {
    /// Against the left-hand edge: text, dates, anything read from the start.
    Left,
    /// Against the right-hand edge, so that digits line up by place value.
    Right,
}

/// The rough shape of a column's values, as the source understands them.
///
/// A hint and not a type: the grid uses it to decide which way a column lines
/// up and whether a value needs quoting in generated SQL, and nothing else. A
/// source that cannot tell says [`GridColumnKind::Text`], which is the safe
/// answer to both questions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridColumnKind {
    /// Character data, and the fallback for anything unrecognised.
    #[default]
    Text,
    /// Numeric data of any width or scale.
    Number,
    /// A truth value.
    Boolean,
    /// A date, a time, a timestamp or an interval.
    Temporal,
    /// Bytes: `BLOB`, `BYTEA`, `VARBINARY`.
    Binary,
}

impl GridColumnKind {
    /// Which edge values of this kind line up against.
    ///
    /// Only numbers go right, and for the one reason that matters: a column of
    /// right-aligned digits can be read down for magnitude, and a left-aligned
    /// one cannot.
    pub fn align(self) -> GridColumnAlign {
        match self {
            GridColumnKind::Number => GridColumnAlign::Right,
            _ => GridColumnAlign::Left,
        }
    }

    /// Whether a value of this kind is quoted when it is written into SQL.
    ///
    /// Numbers and booleans are literals; everything else — text, dates, bytes
    /// — is quoted, because a bare `2024-01-01` is arithmetic in more dialects
    /// than it is a date.
    pub fn quoted_in_sql(self) -> bool {
        !matches!(self, GridColumnKind::Number | GridColumnKind::Boolean)
    }
}

/// One column's heading, as the grid needs to draw and use it.
///
/// Borrowed from the source, so that asking about a column allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridColumn<'a> {
    /// The label drawn in the header, which is the name the query gave it.
    pub name: &'a str,
    /// What sort of values it holds.
    pub kind: GridColumnKind,
    /// Which edge those values line up against.
    ///
    /// Defaults to [`GridColumnKind::align`]; a source that knows better — a
    /// numeric column it means to show as an identifier, say — overrides it
    /// with [`GridColumn::aligned`].
    pub align: GridColumnAlign,
    /// Whether the column is part of the table's primary key.
    ///
    /// Drawn in [`Theme::grid_pk`](rudbman_ui::Theme#structfield.grid_pk), and
    /// later the thing that decides whether a cell can be edited at all — an
    /// `UPDATE` needs a key to aim at (architecture document, §7.5).
    pub primary_key: bool,
}

impl<'a> GridColumn<'a> {
    /// A column of `kind` named `name`, aligned as that kind is usually
    /// aligned and not part of any key.
    pub fn new(name: &'a str, kind: GridColumnKind) -> Self {
        Self {
            name,
            kind,
            align: kind.align(),
            primary_key: false,
        }
    }

    /// Marks the column as part of the primary key.
    pub fn primary_key(mut self, primary_key: bool) -> Self {
        self.primary_key = primary_key;
        self
    }

    /// Overrides which edge the values line up against.
    pub fn aligned(mut self, align: GridColumnAlign) -> Self {
        self.align = align;
        self
    }
}

/// What one cell holds.
///
/// Three variants and not more, because a grid draws text: the codec decodes on
/// the way in, and what is left to decide here is only how a value is *shown*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridCell<'a> {
    /// No value at all. Drawn as [`NULL_TEXT`] in the null colour, which is
    /// what tells it apart from `Text("")` — an empty cell that really does
    /// hold the empty string.
    Null,
    /// The value, already a string. May be empty, and an empty one is not null.
    Text(&'a str),
    /// A large object, whose body is not here.
    ///
    /// Only the size travels with the row; the bytes are fetched in chunks when
    /// the cell is opened, which is what [`GridEvent::CellActivated`] is for.
    ///
    /// [`GridEvent::CellActivated`]: crate::GridEvent::CellActivated
    Lob {
        /// How many bytes the object runs to, when the driver said.
        size: Option<u64>,
    },
}

/// Whether the source has everything, or is still filling up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridSourceState {
    /// Every row of the result is in the source. Scrolling to the bottom asks
    /// for nothing.
    #[default]
    Complete,
    /// The server has more rows than the source holds. Approaching the bottom
    /// raises [`GridEvent::NearEnd`](crate::GridEvent::NearEnd).
    HasMore,
    /// A batch is on its way. The grid asks for nothing while one is, which is
    /// what keeps a fast scroll from firing a fetch per frame.
    Loading,
}

/// Where a [`GridView`](crate::GridView) gets its columns and rows.
///
/// Implemented on whatever the host already keeps the result in, so that there
/// is one copy of the data rather than two that can disagree. Every method is
/// asked only about what is on screen, and none of them may block: a grid over
/// a million rows calls [`GridSource::cell`] a few hundred times per frame and
/// never once for a row nobody can see.
pub trait GridSource: 'static {
    /// How many columns the result has, hidden ones included.
    fn column_count(&self) -> usize;

    /// The heading of column `index`.
    ///
    /// Asked once per visible column per frame, so it must be cheap; a source
    /// that would have to build the name should keep it.
    fn column(&self, index: usize) -> GridColumn<'_>;

    /// How many rows the source holds *now*.
    ///
    /// Not how many the query will return: a result being paged in grows this
    /// number batch by batch, and the grid follows it.
    fn row_count(&self) -> usize;

    /// The value at `row` and `column`.
    ///
    /// `column` is an index into the source's own columns, unaffected by which
    /// of them are hidden or how wide they have been dragged.
    fn cell(&self, row: usize, column: usize) -> GridCell<'_>;

    /// Whether more rows are coming.
    ///
    /// Defaults to [`GridSourceState::Complete`], which is right for a source
    /// that was handed a finished list.
    fn state(&self) -> GridSourceState {
        GridSourceState::Complete
    }
}

/// How a large object is written where its bytes cannot go: in a cell, and in
/// every copied format.
///
/// Deliberately not a valid value in any of them. A LOB's body is not in the
/// grid, so it cannot be copied out of one, and a placeholder that could be
/// mistaken for data would be worse than one that cannot.
pub fn lob_label(size: Option<u64>) -> String {
    match size {
        Some(size) => format!("[LOB {size}]"),
        None => "[LOB]".to_string(),
    }
}

/// What one cell draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellLabel {
    /// The text itself, which is empty for a cell holding the empty string.
    pub text: SharedString,
    /// Whether the text stands in for a value rather than being one — the null
    /// marker, or a LOB placeholder — and is therefore drawn in
    /// [`Theme::grid_null`](rudbman_ui::Theme#structfield.grid_null) instead of
    /// the ordinary foreground.
    pub muted: bool,
}

/// What `cell` draws, which is the whole of how null is told from empty.
///
/// Split out of the widget so that the distinction can be asserted without a
/// window: `cell_label(&GridCell::Null)` and `cell_label(&GridCell::Text(""))`
/// differ in both fields.
pub fn cell_label(cell: &GridCell<'_>) -> CellLabel {
    match cell {
        GridCell::Null => CellLabel {
            text: SharedString::new_static(NULL_TEXT),
            muted: true,
        },
        GridCell::Text(text) => CellLabel {
            text: SharedString::from(text.to_string()),
            muted: false,
        },
        GridCell::Lob { size } => CellLabel {
            text: SharedString::from(lob_label(*size)),
            muted: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the whole crate exists to keep: a cell with no value and
    /// a cell with an empty value do not draw the same thing.
    #[test]
    fn null_and_the_empty_string_draw_differently() {
        let null = cell_label(&GridCell::Null);
        let empty = cell_label(&GridCell::Text(""));

        assert_eq!(null.text, NULL_TEXT);
        assert!(null.muted, "the null marker is not a value");
        assert_eq!(empty.text, "");
        assert!(!empty.muted, "the empty string is a value");
        assert_ne!(null, empty);
    }

    /// And a cell holding the *string* `NULL` is not the null marker either:
    /// the text matches, the colour does not.
    #[test]
    fn the_string_null_is_not_the_null_marker() {
        let marker = cell_label(&GridCell::Null);
        let text = cell_label(&GridCell::Text(NULL_TEXT));

        assert_eq!(marker.text, text.text);
        assert_ne!(marker.muted, text.muted);
        assert_ne!(marker, text);
    }

    /// A LOB says how big it is and nothing else, because nothing else came.
    #[test]
    fn a_lob_shows_its_size() {
        assert_eq!(
            cell_label(&GridCell::Lob { size: Some(4096) }).text,
            "[LOB 4096]"
        );
        assert_eq!(cell_label(&GridCell::Lob { size: None }).text, "[LOB]");
        assert!(cell_label(&GridCell::Lob { size: None }).muted);
    }

    /// Numbers line up on the right and are written bare into SQL; everything
    /// else does neither.
    #[test]
    fn only_numbers_are_right_aligned_and_unquoted() {
        assert_eq!(GridColumnKind::Number.align(), GridColumnAlign::Right);
        assert!(!GridColumnKind::Number.quoted_in_sql());
        assert!(!GridColumnKind::Boolean.quoted_in_sql());

        for kind in [
            GridColumnKind::Text,
            GridColumnKind::Boolean,
            GridColumnKind::Temporal,
            GridColumnKind::Binary,
        ] {
            assert_eq!(kind.align(), GridColumnAlign::Left, "{kind:?}");
        }
        assert!(GridColumnKind::Temporal.quoted_in_sql());
        assert!(GridColumnKind::Binary.quoted_in_sql());
    }

    /// A column takes its alignment from its kind unless the source says
    /// otherwise.
    #[test]
    fn a_column_can_override_the_alignment_of_its_kind() {
        let id = GridColumn::new("id", GridColumnKind::Number).primary_key(true);
        assert_eq!(id.align, GridColumnAlign::Right);
        assert!(id.primary_key);

        let code = GridColumn::new("code", GridColumnKind::Number).aligned(GridColumnAlign::Left);
        assert_eq!(code.align, GridColumnAlign::Left);
        assert!(!code.primary_key);
    }
}
