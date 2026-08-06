//! The diagram as a file: one self-contained SVG, written by hand.
//!
//! Not a screenshot. gpui can only render into a window it owns, and an
//! off-screen path for it is uncharted (architecture document, §7.6 and §12.5),
//! so the export is a second renderer over the same [`crate::layout`]
//! primitives: the same [`measure`](crate::layout::measure) sizes the boxes,
//! the same [`route`] draws the lines, and the same crow's foot marks the many
//! end. Two renderers over one geometry is the only arrangement in which the
//! picture on screen and the picture in the file cannot drift apart.
//!
//! ## Self-contained, and colour-injected
//!
//! The output references nothing: no stylesheet, no font file, no image. It
//! names `sans-serif` and lets the viewer resolve it, which is the one external
//! thing an SVG cannot avoid and the one every viewer has.
//!
//! The colours arrive as a [`SvgPalette`] of CSS strings rather than being read
//! from a theme, which is what keeps this module pure: the view converts the
//! active [`Theme`](rudbman_ui::Theme) with
//! [`to_hex`](rudbman_ui::to_hex) and hands the result in, and a test hands in
//! whatever it likes.
//!
//! ## Escaping
//!
//! Every string that reaches the output goes through [`escape`]. Table and
//! column names come from a catalog, and a catalog will happily hand back
//! `a&b`, `"quoted"` or `<odd>` — an unescaped one of those does not produce a
//! wrong picture, it produces a file no viewer will open.

use std::fmt::Write as _;

use crate::layout::{
    BOX_PADDING, HEADER_HEIGHT, NodeRect, ROW_HEIGHT, crow_foot, elide, extent, head_direction,
    key_bar, route, row_top, split_row, tail_direction,
};
use crate::model::ErdModel;

/// Blank space left around the diagram, in logical units.
const MARGIN: f32 = 24.;

/// Text size of a column row.
const FONT_SIZE: f32 = 12.;

/// Text size of a box's title.
const TITLE_SIZE: f32 = 13.;

/// Where a row's baseline sits inside its row, as a fraction of the row height.
const BASELINE: f32 = 0.72;

/// The colours an exported diagram is drawn in.
///
/// Every field is a CSS colour — in practice the `#rrggbb` that
/// [`to_hex`](rudbman_ui::to_hex) produces — because this module has no opinion
/// about themes and no way to read one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvgPalette {
    /// The page behind the diagram.
    pub background: String,
    /// The body of an entity box.
    pub box_fill: String,
    /// The title band of an entity box.
    pub header_fill: String,
    /// Box outlines.
    pub border: String,
    /// Titles and column names.
    pub text: String,
    /// Type names, drawn quieter than the column names beside them.
    pub text_muted: String,
    /// Relation lines and their cardinality marks.
    pub line: String,
    /// Column names that take part in a primary key.
    pub pk: String,
}

/// The diagram as an SVG document.
///
/// `rects` is expected to be parallel to `model.tables`, as every layout in
/// [`crate::layout`] produces; a table without a rect is skipped rather than
/// panicked over, because the export should still write what it can when a
/// model and a layout have fallen out of step.
pub fn to_svg(model: &ErdModel, rects: &[NodeRect], palette: &SvgPalette) -> String {
    let bounds = extent(rects).unwrap_or(NodeRect {
        x: 0.,
        y: 0.,
        w: 200.,
        h: 100.,
    });
    let left = bounds.x - MARGIN;
    let top = bounds.y - MARGIN;
    let width = bounds.w + 2. * MARGIN;
    let height = bounds.h + 2. * MARGIN;

    let mut out = String::with_capacity(1024 + model.tables.len() * 512);
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" \
         viewBox=\"{} {} {} {}\">",
        num(width),
        num(height),
        num(left),
        num(top),
        num(width),
        num(height)
    );
    let _ = writeln!(
        out,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
        num(left),
        num(top),
        num(width),
        num(height),
        escape(&palette.background)
    );

    // Lines first: a box is opaque, and a relation that ends *behind* a box
    // reads better than one that ends on top of it.
    for relation in model.valid_relations() {
        let (Some(from), Some(to)) = (rects.get(relation.from), rects.get(relation.to)) else {
            continue;
        };
        write_relation(&mut out, from, to, palette);
    }

    for (index, table) in model.tables.iter().enumerate() {
        let Some(rect) = rects.get(index) else {
            continue;
        };
        write_table(&mut out, table, rect, palette);
    }

    out.push_str("</svg>\n");
    out
}

/// One relation: the polyline, the crow's foot at the many end and the bar at
/// the one end.
fn write_relation(out: &mut String, from: &NodeRect, to: &NodeRect, palette: &SvgPalette) {
    let points = route(from, to);
    if points.is_empty() {
        return;
    }

    let mut d = String::with_capacity(points.len() * 16);
    for (index, (x, y)) in points.iter().enumerate() {
        let _ = write!(
            d,
            "{}{} {}",
            if index == 0 { "M" } else { " L" },
            num(*x),
            num(*y)
        );
    }
    let _ = writeln!(
        out,
        "<path d=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1\"/>",
        d,
        escape(&palette.line)
    );

    for prong in crow_foot(points[0], head_direction(&points)) {
        write_segment(out, prong[0], prong[1], &palette.line);
    }
    let bar = key_bar(points[points.len() - 1], tail_direction(&points));
    write_segment(out, bar[0], bar[1], &palette.line);
}

/// One straight stroke.
fn write_segment(out: &mut String, from: (f32, f32), to: (f32, f32), stroke: &str) {
    let _ = writeln!(
        out,
        "<path d=\"M{} {} L{} {}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1\"/>",
        num(from.0),
        num(from.1),
        num(to.0),
        num(to.1),
        escape(stroke)
    );
}

/// One entity box: the body, the title band, the title and the column rows.
fn write_table(
    out: &mut String,
    table: &crate::model::ErdTable,
    rect: &NodeRect,
    palette: &SvgPalette,
) {
    let _ = writeln!(
        out,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\" stroke=\"{}\" \
         stroke-width=\"1\"/>",
        num(rect.x),
        num(rect.y),
        num(rect.w),
        num(rect.h),
        escape(&palette.box_fill),
        escape(&palette.border)
    );
    let _ = writeln!(
        out,
        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
        num(rect.x),
        num(rect.y),
        num(rect.w),
        num(HEADER_HEIGHT.min(rect.h)),
        escape(&palette.header_fill)
    );

    let room = rect.w - 2. * BOX_PADDING;
    let _ = writeln!(
        out,
        "<text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"{}\" \
         font-weight=\"600\" fill=\"{}\">{}</text>",
        num(rect.x + BOX_PADDING),
        num(rect.y + HEADER_HEIGHT * BASELINE),
        num(TITLE_SIZE),
        escape(&palette.text),
        escape(&elide(&table.name, room))
    );

    for (index, column) in table.columns.iter().enumerate() {
        let baseline = row_top(rect, index) + ROW_HEIGHT * BASELINE;
        let (name_text, type_text) = split_row(&column.name, &column.type_name, room);
        let name_colour = if column.primary_key {
            &palette.pk
        } else {
            &palette.text
        };

        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"{}\" \
             fill=\"{}\">{}</text>",
            num(rect.x + BOX_PADDING),
            num(baseline),
            num(FONT_SIZE),
            escape(name_colour),
            escape(&name_text)
        );
        let _ = writeln!(
            out,
            "<text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"{}\" \
             text-anchor=\"end\" fill=\"{}\">{}</text>",
            num(rect.right() - BOX_PADDING),
            num(baseline),
            num(FONT_SIZE),
            escape(&palette.text_muted),
            escape(&type_text)
        );
    }
}

/// A coordinate, written short.
///
/// Two decimals is a hundredth of a pixel, which is finer than any viewer
/// resolves, and dropping the trailing zeros keeps a diagram of fifty tables
/// from carrying a kilobyte of `.00`.
fn num(value: f32) -> String {
    let rounded = (value * 100.).round() / 100.;
    if rounded == rounded.trunc() && rounded.abs() < 1e9 {
        format!("{}", rounded as i64)
    } else {
        let mut text = format!("{rounded:.2}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

/// `text` with the five characters XML reserves replaced by their entities.
///
/// All five, including the two that are only strictly required inside an
/// attribute: this function is used for attribute values as well as for
/// character data, and one escaper that is right everywhere beats two that have
/// to be chosen between.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{auto_layout, grid_layout};
    use crate::model::{ErdColumn, ErdRelation, ErdTable};

    /// A palette whose values are recognisable in the output.
    fn palette() -> SvgPalette {
        SvgPalette {
            background: "#101010".into(),
            box_fill: "#202020".into(),
            header_fill: "#303030".into(),
            border: "#404040".into(),
            text: "#f0f0f0".into(),
            text_muted: "#909090".into(),
            line: "#707070".into(),
            pk: "#e5c07b".into(),
        }
    }

    /// Two tables and the key between them.
    fn model() -> ErdModel {
        ErdModel {
            tables: vec![
                ErdTable::new("orders")
                    .column(ErdColumn::new("id", "NUMBER(19)").primary_key())
                    .column(ErdColumn::new("customer_id", "NUMBER(19)").foreign_key()),
                ErdTable::new("customers")
                    .column(ErdColumn::new("id", "NUMBER(19)").primary_key())
                    .column(ErdColumn::new("name", "VARCHAR2(120)")),
            ],
            relations: vec![ErdRelation {
                name: Some("fk_orders_customer".into()),
                from: 0,
                to: 1,
                columns: vec![("customer_id".into(), "id".into())],
            }],
        }
    }

    #[test]
    fn the_document_is_an_svg_and_names_everything_in_it() {
        let model = model();
        let out = to_svg(&model, &grid_layout(&model), &palette());

        assert!(out.starts_with("<svg"), "{}", &out[..40.min(out.len())]);
        assert!(out.trim_end().ends_with("</svg>"));
        for name in [
            "orders",
            "customers",
            "customer_id",
            "NUMBER(19)",
            "VARCHAR2(120)",
        ] {
            assert!(out.contains(name), "{name} is missing from the export");
        }
        // The injected colours, and no others hard-coded.
        assert!(out.contains("#101010"));
        assert!(out.contains("#e5c07b"));
    }

    #[test]
    fn a_relation_is_drawn_with_both_of_its_marks() {
        let model = model();
        let out = to_svg(&model, &auto_layout(&model), &palette());
        // The polyline, three prongs and one bar: five strokes at least.
        assert!(out.matches("stroke=\"#707070\"").count() >= 5, "{out}");
    }

    #[test]
    fn the_document_references_nothing_outside_itself() {
        let model = model();
        let out = to_svg(&model, &grid_layout(&model), &palette());
        for forbidden in ["url(", "<image", "@import", "<script", "xlink:href", "<use"] {
            assert!(
                !out.contains(forbidden),
                "{forbidden} leaked into the export"
            );
        }
        // The one URL in the document is the SVG namespace, which names a
        // standard rather than fetching anything.
        assert_eq!(out.matches("http").count(), 1);
        assert!(out.contains("xmlns=\"http://www.w3.org/2000/svg\""));
    }

    #[test]
    fn the_characters_xml_reserves_are_escaped() {
        let model = ErdModel {
            tables: vec![
                ErdTable::new("a<b&c").column(ErdColumn::new("say \"hi\"", "it's a type")),
            ],
            relations: Vec::new(),
        };
        let out = to_svg(&model, &grid_layout(&model), &palette());

        assert!(out.contains("a&lt;b&amp;c"), "{out}");
        assert!(out.contains("&quot;hi&quot;"), "{out}");
        assert!(out.contains("it&apos;s"), "{out}");
        // Nothing raw survived: every `<` is the start of a tag, so no `<` is
        // ever followed by a letter that is not a tag name we wrote.
        assert!(!out.contains("a<b"));
        assert!(!out.contains("\"hi\""));
    }

    #[test]
    fn an_empty_model_still_produces_a_document() {
        let out = to_svg(&ErdModel::default(), &[], &palette());
        assert!(out.starts_with("<svg"));
        assert!(out.contains("#101010"));
    }

    #[test]
    fn numbers_are_written_short() {
        assert_eq!(num(12.), "12");
        assert_eq!(num(-4.5), "-4.5");
        assert_eq!(num(1.0004), "1");
        assert_eq!(num(0.126), "0.13");
        assert_eq!(num(140.4), "140.4");
    }
}
