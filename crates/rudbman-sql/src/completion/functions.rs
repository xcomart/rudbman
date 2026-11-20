//! Frequently used built-ins, independent of database metadata. Function names
//! stay separate from reserved words: completing COUNT must not make a column
//! called count require identifier quotes or change syntax classification.
use crate::Dialect;

const COMMON: &[&str] = &[
    "ABS", "AVG", "COALESCE", "COUNT", "LOWER", "LTRIM", "MAX", "MIN", "NULLIF", "REPLACE",
    "ROUND", "RTRIM", "SUM", "TRIM", "UPPER",
];

/// A deliberately small list of widely available built-ins, not a claim that
/// these are every function available on every version of a database product.
/// Names are inserted alone, preserving call arguments and special syntax.
/// The vendor half is each dialect's [`functions`](crate::dialect::Vendor::functions).
pub(super) fn for_dialect(dialect: Dialect) -> impl Iterator<Item = &'static str> {
    COMMON.iter().chain(dialect.vendor().functions()).copied()
}
