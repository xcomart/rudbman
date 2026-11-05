//! The explorer's filter: the box above the tree, and the folders it narrows.
//!
//! Pure data and pure functions, so that what a filter lets through can be
//! tested without a window; [`crate::explorer`] is where it meets the tree.
//!
//! # What the box understands
//!
//! A name is matched without regard to case, against terms separated by commas:
//!
//! - `cust` — the name *contains* it. What a user typing three letters expects,
//!   and the rule rudbgen's explorer already follows.
//! - `cust*`, `*_seq`, `v_???` — a term with `*` or `?` in it is a wildcard
//!   over the *whole* name instead, so `*_seq` is "ends in `_seq`".
//! - `orders, items` — a comma is "or": a name passes when it matches any term.
//! - `!tmp*` — a leading `!` takes away: a name that matches it is left out,
//!   whatever else it matches. A box of nothing but these keeps everything else.
//!
//! Or, switched to it, a regular expression: matched anywhere in the name and
//! without regard to case, with the commas and `!`s its own rather than the
//! box's. One that does not compile narrows nothing, and the box is marked.
//!
//! # Why the filter has a set of folders
//!
//! The same text rarely means something for every kind of object at once:
//! `*_seq` is about sequences, `v_*` about views. The filter therefore narrows
//! only the folders in its [`FolderSet`] and leaves the others whole — a folder
//! that is not being filtered is not hiding anything, which is easier to read
//! than a folder emptied by a pattern that was never meant for it.
//!
//! # Conditions
//!
//! Under the box, a panel of [`Condition`]s for what one line of text cannot
//! say: a comment that mentions a word, a schema of a given name, a regular
//! expression. Each reads one [`Field`] of the object and compares it one
//! [`Op`] way, and the rows are put together with "all" or "any"
//! ([`Combine`]). The box and the rows are always *both* asked: an object
//! passes when it passes the box **and** the rows.
//!
//! A row still being filled in — a comparison with no value yet — narrows
//! nothing, and neither does a regular expression that does not compile,
//! which the panel marks instead. Half-typed input never empties the tree.

use gpui::SharedString;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::explorer::{FOLDERS, Folder};
use crate::i18n::ts;

/// A set of the explorer's folders.
///
/// A bit per entry of [`FOLDERS`], in that order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FolderSet(u8);

impl FolderSet {
    /// Every folder.
    pub const ALL: Self = Self((1 << FOLDERS.len()) - 1);

    /// No folder at all.
    pub const NONE: Self = Self(0);

    /// The bit `folder` is stored in.
    fn bit(folder: Folder) -> u8 {
        let index = FOLDERS
            .iter()
            .position(|known| *known == folder)
            .expect("every folder is one of FOLDERS");
        1 << index
    }

    /// Whether `folder` is in the set.
    pub fn contains(self, folder: Folder) -> bool {
        self.0 & Self::bit(folder) != 0
    }

    /// The set with `folder` added when it was missing and taken out when it
    /// was there.
    pub fn toggled(self, folder: Folder) -> Self {
        Self(self.0 ^ Self::bit(folder))
    }

    /// Whether every folder is in the set.
    pub fn is_all(self) -> bool {
        self == Self::ALL
    }

    /// Whether no folder is.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// The folders in the set, in [`FOLDERS`] order.
    pub fn iter(self) -> impl Iterator<Item = Folder> {
        FOLDERS
            .into_iter()
            .filter(move |folder| self.contains(*folder))
    }
}

impl Default for FolderSet {
    /// Every folder: a filter typed without opening the menu narrows them all.
    fn default() -> Self {
        Self::ALL
    }
}

/// One term of the box, already in lower case.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pattern {
    /// The name contains this.
    Contains(String),
    /// The whole name matches this, `*` and `?` being wildcards.
    Wildcard(Vec<char>),
}

impl Pattern {
    /// The pattern a term spells, which is never empty.
    fn of(term: &str) -> Self {
        let term = term.to_lowercase();
        if term.contains(['*', '?']) {
            Self::Wildcard(term.chars().collect())
        } else {
            Self::Contains(term)
        }
    }

    /// Whether `name`, already in lower case, matches.
    fn matches(&self, name: &str, chars: &[char]) -> bool {
        match self {
            Self::Contains(needle) => name.contains(needle.as_str()),
            Self::Wildcard(pattern) => wildcard(pattern, chars),
        }
    }
}

/// Whether the whole of `name` matches `pattern`, `*` standing for any run of
/// characters and `?` for exactly one.
///
/// The greedy walk with one remembered `*`: on a mismatch it goes back to the
/// last star and lets it swallow one more character. Linear in practice and
/// never worse than the product of the two lengths, so a name list of
/// thousands stays instant.
fn wildcard(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // The last `*` seen, and the name position it is currently matched up to.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((star_p, star_n)) => {
                    p = star_p + 1;
                    n = star_n + 1;
                    star = Some((star_p, star_n + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// What the box holds, parsed: the terms a name must match and the ones it
/// must not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameFilter {
    /// A name passes when it matches any of these, or when there are none.
    include: Vec<Pattern>,
    /// A name that matches any of these is left out.
    exclude: Vec<Pattern>,
}

impl NameFilter {
    /// Parses the box's text; see the module documentation.
    ///
    /// Blank terms, and a `!` with nothing after it, are dropped rather than
    /// matched: a trailing comma is the user about to type the next term, not
    /// asking for every name.
    pub fn parse(text: &str) -> Self {
        let mut filter = Self::default();
        for term in text.split(',') {
            let term = term.trim();
            let (negated, body) = match term.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, term),
            };
            if body.is_empty() {
                continue;
            }
            let pattern = Pattern::of(body);
            if negated {
                filter.exclude.push(pattern);
            } else {
                filter.include.push(pattern);
            }
        }
        filter
    }

    /// Whether the filter lets everything through.
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether `name` passes.
    pub fn matches(&self, name: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        let name = name.to_lowercase();
        let chars: Vec<char> = name.chars().collect();
        let included = self.include.is_empty()
            || self
                .include
                .iter()
                .any(|pattern| pattern.matches(&name, &chars));
        included
            && !self
                .exclude
                .iter()
                .any(|pattern| pattern.matches(&name, &chars))
    }
}

/// What a condition looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// The object's name.
    Name,
    /// The catalog's comment on it — only tables and views carry one.
    Remarks,
    /// The schema it is in, or its database on a product with no schema
    /// level (MySQL, MariaDB).
    Schema,
}

impl Field {
    /// Every field, in the order the menu lists them.
    pub const ALL: [Self; 3] = [Self::Name, Self::Remarks, Self::Schema];

    /// The field's name in the active language.
    pub fn label(self) -> SharedString {
        match self {
            Self::Name => ts!("explorer.field_name"),
            Self::Remarks => ts!("explorer.field_remarks"),
            Self::Schema => ts!("explorer.field_schema"),
        }
    }
}

/// How a condition compares its field with its value.
///
/// Every comparison ignores case, as the box does: a catalog that folds names
/// to upper case is not something a user filtering it should have to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// The field contains the value.
    Contains,
    /// The field does not contain the value.
    NotContains,
    /// The field starts with the value.
    StartsWith,
    /// The field ends with the value.
    EndsWith,
    /// The field is the value.
    Equals,
    /// The whole field matches the value, `*` and `?` being wildcards.
    Wildcard,
    /// The field matches the value as a regular expression, anywhere in it
    /// unless the expression anchors itself.
    Regex,
    /// The field is empty — an object with no comment, say.
    Empty,
    /// The field is not empty.
    NotEmpty,
}

impl Op {
    /// Every comparison, in the order the menu lists them.
    pub const ALL: [Self; 9] = [
        Self::Contains,
        Self::NotContains,
        Self::StartsWith,
        Self::EndsWith,
        Self::Equals,
        Self::Wildcard,
        Self::Regex,
        Self::Empty,
        Self::NotEmpty,
    ];

    /// The comparison's name in the active language.
    pub fn label(self) -> SharedString {
        match self {
            Self::Contains => ts!("explorer.op_contains"),
            Self::NotContains => ts!("explorer.op_not_contains"),
            Self::StartsWith => ts!("explorer.op_starts_with"),
            Self::EndsWith => ts!("explorer.op_ends_with"),
            Self::Equals => ts!("explorer.op_equals"),
            Self::Wildcard => ts!("explorer.op_wildcard"),
            Self::Regex => ts!("explorer.op_regex"),
            Self::Empty => ts!("explorer.op_empty"),
            Self::NotEmpty => ts!("explorer.op_not_empty"),
        }
    }

    /// Whether the comparison needs a value to compare with.
    pub fn takes_value(self) -> bool {
        !matches!(self, Self::Empty | Self::NotEmpty)
    }
}

/// How the conditions are put together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Combine {
    /// An object passes when every condition holds.
    #[default]
    All,
    /// An object passes when any one does.
    Any,
}

/// One row of the conditions panel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Condition {
    /// What it looks at.
    pub field: Field,
    /// How it compares.
    pub op: Op,
    /// What it compares with; ignored by the comparisons that take none.
    #[serde(default)]
    pub value: String,
}

impl Default for Condition {
    /// What "add a condition" puts in the panel: the name, contains, nothing
    /// yet.
    fn default() -> Self {
        Self {
            field: Field::Name,
            op: Op::Contains,
            value: String::new(),
        }
    }
}

impl Condition {
    /// Whether the condition says anything yet: one that takes a value and
    /// has none is a row being filled in, and narrows nothing.
    pub fn is_live(&self) -> bool {
        !self.op.takes_value() || !self.value.trim().is_empty()
    }
}

/// A condition ready to be asked: its value in lower case, or compiled.
#[derive(Clone, Debug)]
enum Test {
    Contains(String),
    NotContains(String),
    StartsWith(String),
    EndsWith(String),
    Equals(String),
    Wildcard(Vec<char>),
    Regex(Regex),
    Empty,
    NotEmpty,
}

impl Test {
    /// The test `condition` asks for, or `Err` for a regular expression that
    /// does not compile.
    fn of(condition: &Condition) -> Result<Self, regex::Error> {
        let value = condition.value.trim();
        let lower = value.to_lowercase();
        Ok(match condition.op {
            Op::Contains => Self::Contains(lower),
            Op::NotContains => Self::NotContains(lower),
            Op::StartsWith => Self::StartsWith(lower),
            Op::EndsWith => Self::EndsWith(lower),
            Op::Equals => Self::Equals(lower),
            Op::Wildcard => Self::Wildcard(lower.chars().collect()),
            Op::Regex => Self::Regex(RegexBuilder::new(value).case_insensitive(true).build()?),
            Op::Empty => Self::Empty,
            Op::NotEmpty => Self::NotEmpty,
        })
    }

    /// Whether `text` passes; `lower` is the same text in lower case.
    fn passes(&self, text: &str, lower: &str) -> bool {
        match self {
            Self::Contains(value) => lower.contains(value.as_str()),
            Self::NotContains(value) => !lower.contains(value.as_str()),
            Self::StartsWith(value) => lower.starts_with(value.as_str()),
            Self::EndsWith(value) => lower.ends_with(value.as_str()),
            Self::Equals(value) => lower == value,
            Self::Wildcard(pattern) => wildcard(pattern, &lower.chars().collect::<Vec<_>>()),
            Self::Regex(regex) => regex.is_match(text),
            Self::Empty => text.trim().is_empty(),
            Self::NotEmpty => !text.trim().is_empty(),
        }
    }
}

/// What a filter is asked about one object.
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidate<'a> {
    /// Its name.
    pub name: &'a str,
    /// The catalog's comment on it, if it carries one.
    pub remarks: Option<&'a str>,
    /// Its schema, or its database where there is no schema level.
    pub schema: Option<&'a str>,
}

impl<'a> Candidate<'a> {
    /// An object known by its name alone.
    #[cfg(test)]
    pub fn named(name: &'a str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// The text `field` reads, empty where the object has none.
    fn field(&self, field: Field) -> &'a str {
        match field {
            Field::Name => self.name,
            Field::Remarks => self.remarks.unwrap_or_default(),
            Field::Schema => self.schema.unwrap_or_default(),
        }
    }
}

/// A filter as it is kept in the history: everything that decides what it
/// lets through, and nothing compiled.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilterSnapshot {
    /// What the box held.
    #[serde(default)]
    pub text: String,
    /// The folders it narrowed, or `None` for every folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<Folder>>,
    /// How the conditions were put together.
    #[serde(default)]
    pub combine: Combine,
    /// The conditions that said something, in the panel's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Whether it narrowed every database, rather than only the one the
    /// connection landed in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_databases: bool,
    /// Whether the text was a regular expression rather than the box's own
    /// terms.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub regex: bool,
}

impl FilterSnapshot {
    /// Whether the snapshot lets everything through, which is not worth
    /// keeping.
    pub fn is_empty(&self) -> bool {
        let box_empty = if self.regex {
            self.text.trim().is_empty()
        } else {
            NameFilter::parse(&self.text).is_empty()
        };
        box_empty && self.conditions.is_empty()
    }

    /// The conditions in a line, as the history menu lists them: `Name starts
    /// with cust and Comment contains customer`.
    pub fn conditions_summary(&self) -> String {
        let joint = match self.combine {
            Combine::All => ts!("explorer.combine_and"),
            Combine::Any => ts!("explorer.combine_or"),
        };
        self.conditions
            .iter()
            .map(|condition| {
                let mut words = format!("{} {}", condition.field.label(), condition.op.label());
                if condition.op.takes_value() {
                    words.push(' ');
                    words.push_str(condition.value.trim());
                }
                words
            })
            .collect::<Vec<_>>()
            .join(&format!(" {joint} "))
    }
}

/// One connection's filter: the text in the box, the conditions under it, and
/// the folders it narrows.
#[derive(Clone, Debug, Default)]
pub struct ExplorerFilter {
    /// What the box holds, as typed.
    text: String,
    /// The same, parsed as the box's terms — unless [`ExplorerFilter::regex`].
    name: NameFilter,
    /// Whether the text is a regular expression instead.
    regex: bool,
    /// The text compiled as one, in that mode; `None` while it is blank or
    /// does not compile.
    pattern: Option<Regex>,
    /// The folders the filter narrows.
    targets: FolderSet,
    /// How the conditions are put together.
    combine: Combine,
    /// The panel's rows, as they stand — filled in or not.
    conditions: Vec<Condition>,
    /// The rows that say something, compiled.
    tests: Vec<(Field, Test)>,
    /// The rows whose regular expression does not compile, by index.
    invalid: Vec<usize>,
    /// Whether the filter narrows every database the tree shows, rather than
    /// only the one the connection landed in — see [`ExplorerFilter::all_databases`].
    all_databases: bool,
}

impl PartialEq for ExplorerFilter {
    /// Equal when they say the same thing; what they compiled it to follows.
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.targets == other.targets
            && self.combine == other.combine
            && self.conditions == other.conditions
            && self.all_databases == other.all_databases
            && self.regex == other.regex
    }
}

impl ExplorerFilter {
    /// What the box holds, as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text, and the terms parsed from it.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.parse_text();
    }

    /// Whether the text is a regular expression, matched anywhere in a name
    /// and without regard to case, instead of the box's own terms.
    pub fn regex(&self) -> bool {
        self.regex
    }

    /// Reads the text as a regular expression, or as the box's terms.
    pub fn set_regex(&mut self, regex: bool) {
        self.regex = regex;
        self.parse_text();
    }

    /// Whether the text is meant as a regular expression and is not one,
    /// which the box marks and the filter ignores.
    pub fn text_invalid(&self) -> bool {
        self.regex && !self.text.trim().is_empty() && self.pattern.is_none()
    }

    /// Parses the text the way the mode says.
    fn parse_text(&mut self) {
        if self.regex {
            self.name = NameFilter::default();
            let text = self.text.trim();
            self.pattern = (!text.is_empty())
                .then(|| RegexBuilder::new(text).case_insensitive(true).build().ok())
                .flatten();
        } else {
            self.name = NameFilter::parse(&self.text);
            self.pattern = None;
        }
    }

    /// Whether the box lets every name through.
    fn box_is_empty(&self) -> bool {
        if self.regex {
            self.pattern.is_none()
        } else {
            self.name.is_empty()
        }
    }

    /// The folders the filter narrows.
    pub fn targets(&self) -> FolderSet {
        self.targets
    }

    /// Replaces the folders the filter narrows.
    pub fn set_targets(&mut self, targets: FolderSet) {
        self.targets = targets;
    }

    /// Whether the filter narrows every database the tree shows.
    ///
    /// Off by default: the database a connection lands in is the one its user
    /// is working in, and a server's other databases — dozens of them, often —
    /// are drawn whole until they are asked for. The explorer decides which
    /// folders are in that database; this only says whether to ask.
    pub fn all_databases(&self) -> bool {
        self.all_databases
    }

    /// Narrows every database, or only the landed one.
    pub fn set_all_databases(&mut self, all: bool) {
        self.all_databases = all;
    }

    /// How the conditions are put together.
    pub fn combine(&self) -> Combine {
        self.combine
    }

    /// The panel's rows, filled in or not.
    pub fn conditions(&self) -> &[Condition] {
        &self.conditions
    }

    /// Replaces the conditions, and compiles the ones that say something.
    pub fn set_conditions(&mut self, combine: Combine, conditions: Vec<Condition>) {
        self.combine = combine;
        self.conditions = conditions;
        self.tests.clear();
        self.invalid.clear();
        for (index, condition) in self.conditions.iter().enumerate() {
            if !condition.is_live() {
                continue;
            }
            match Test::of(condition) {
                Ok(test) => self.tests.push((condition.field, test)),
                Err(_) => self.invalid.push(index),
            }
        }
    }

    /// How many conditions are narrowing: filled in, and compiled.
    pub fn live_conditions(&self) -> usize {
        self.tests.len()
    }

    /// Whether row `index`'s regular expression fails to compile, which the
    /// panel marks and the filter ignores.
    pub fn is_invalid(&self, index: usize) -> bool {
        self.invalid.contains(&index)
    }

    /// Whether the filter narrows anything at all.
    ///
    /// Not when the box is blank and no condition says anything, and not when
    /// no folder is chosen: either way the tree is whole.
    pub fn is_active(&self) -> bool {
        (!self.box_is_empty() || !self.tests.is_empty()) && !self.targets.is_empty()
    }

    /// Whether the filter narrows `folder`.
    pub fn narrows(&self, folder: Folder) -> bool {
        self.is_active() && self.targets.contains(folder)
    }

    /// Whether `object` passes: the box, *and* the conditions put together as
    /// [`ExplorerFilter::combine`] says.
    ///
    /// Says nothing about the folder: ask [`ExplorerFilter::narrows`] first.
    pub fn admits(&self, object: &Candidate<'_>) -> bool {
        let passes_box = match &self.pattern {
            Some(pattern) => pattern.is_match(object.name),
            None => self.name.matches(object.name),
        };
        if !passes_box {
            return false;
        }
        if self.tests.is_empty() {
            return true;
        }
        let mut results = self.tests.iter().map(|(field, test)| {
            let text = object.field(*field);
            test.passes(text, &text.to_lowercase())
        });
        match self.combine {
            Combine::All => results.all(|passed| passed),
            Combine::Any => results.any(|passed| passed),
        }
    }

    /// What the history keeps of this filter: the text trimmed, the folders
    /// unless they are all of them, and only the conditions that say
    /// something — so that two filters that let the same things through are
    /// one entry.
    pub fn snapshot(&self) -> FilterSnapshot {
        FilterSnapshot {
            text: self.text.trim().to_string(),
            targets: (!self.targets.is_all()).then(|| self.targets.iter().collect()),
            combine: self.combine,
            all_databases: self.all_databases,
            regex: self.regex,
            conditions: self
                .conditions
                .iter()
                .filter(|condition| condition.is_live())
                .map(|condition| Condition {
                    value: condition.value.trim().to_string(),
                    ..condition.clone()
                })
                .collect(),
        }
    }

    /// The filter a history entry describes.
    pub fn from_snapshot(snapshot: &FilterSnapshot) -> Self {
        let mut filter = Self::default();
        filter.set_regex(snapshot.regex);
        filter.set_text(snapshot.text.clone());
        if let Some(folders) = &snapshot.targets {
            filter.set_targets(
                folders
                    .iter()
                    .fold(FolderSet::NONE, |set, folder| set.toggled(*folder)),
            );
        }
        filter.set_conditions(snapshot.combine, snapshot.conditions.clone());
        filter.set_all_databases(snapshot.all_databases);
        filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passes(text: &str, name: &str) -> bool {
        NameFilter::parse(text).matches(name)
    }

    #[test]
    fn a_plain_term_is_a_substring_in_any_case() {
        assert!(passes("cust", "CUSTOMER"));
        assert!(passes("CUST", "order_customer"));
        assert!(!passes("cust", "orders"));
    }

    #[test]
    fn a_blank_box_and_stray_commas_let_everything_through() {
        assert!(NameFilter::parse("").is_empty());
        assert!(NameFilter::parse("  , ,!").is_empty());
        assert!(passes("", "anything"));
        // A trailing comma is the next term on its way, not "every name".
        assert!(!passes("cust,", "orders"));
    }

    #[test]
    fn a_wildcard_term_matches_the_whole_name() {
        assert!(passes("*_seq", "orders_id_SEQ"));
        assert!(!passes("*_seq", "orders_seq_backup"));
        assert!(passes("cust*", "customer"));
        assert!(!passes("cust*", "order_customer"));
        assert!(passes("v_???", "v_abc"));
        assert!(!passes("v_???", "v_abcd"));
        assert!(passes("*", "anything"));
        assert!(passes("a*b*c", "a_x_b_y_c"));
        assert!(!passes("a*b*c", "a_x_b_y_d"));
        assert!(passes("**", ""));
    }

    #[test]
    fn a_comma_is_or() {
        let filter = NameFilter::parse("orders, item");
        assert!(filter.matches("orders"));
        assert!(filter.matches("order_item"));
        assert!(!filter.matches("customer"));
    }

    #[test]
    fn a_negated_term_takes_away_whatever_else_matched() {
        assert!(passes("order, !tmp*", "order_item"));
        assert!(!passes("order, !tmp*", "tmp_order"));
        // Exclusions alone keep everything they do not name.
        assert!(passes("!tmp*", "customer"));
        assert!(!passes("!tmp*", "TMP_import"));
        assert!(!passes("! audit", "audit_log"));
    }

    #[test]
    fn names_outside_ascii_match_as_typed() {
        assert!(passes("고객", "고객_마스터"));
        assert!(passes("Ä*", "äpfel"));
    }

    #[test]
    fn the_folder_set_starts_full_and_toggles_one_folder_at_a_time() {
        let all = FolderSet::default();
        assert!(all.is_all());
        assert_eq!(all.iter().collect::<Vec<_>>(), FOLDERS.to_vec());

        let no_views = all.toggled(Folder::Views);
        assert!(!no_views.contains(Folder::Views));
        assert!(no_views.contains(Folder::Tables));
        assert!(!no_views.is_all());
        assert_eq!(no_views.toggled(Folder::Views), all);

        let none = FOLDERS
            .into_iter()
            .fold(all, |set, folder| set.toggled(folder));
        assert!(none.is_empty());
        assert_eq!(none, FolderSet::NONE);
    }

    #[test]
    fn a_filter_narrows_only_its_folders_and_only_with_text() {
        let mut filter = ExplorerFilter::default();
        assert!(!filter.is_active());
        assert!(!filter.narrows(Folder::Tables));

        filter.set_text("*_seq");
        assert!(filter.narrows(Folder::Tables));
        assert!(filter.admits(&Candidate::named("orders_id_seq")));
        assert!(!filter.admits(&Candidate::named("orders")));

        filter.set_targets(FolderSet::NONE.toggled(Folder::Sequences));
        assert!(filter.narrows(Folder::Sequences));
        assert!(!filter.narrows(Folder::Tables));

        filter.set_targets(FolderSet::NONE);
        assert!(!filter.is_active());
    }

    #[test]
    fn the_box_can_hold_a_regular_expression_instead_of_its_terms() {
        let mut filter = ExplorerFilter::default();
        filter.set_text("^cust|_seq$");
        // As terms, that is one substring nothing contains.
        assert!(!filter.admits(&Candidate::named("customer")));

        filter.set_regex(true);
        assert!(filter.is_active());
        assert!(filter.admits(&Candidate::named("CUSTOMER")));
        assert!(filter.admits(&Candidate::named("orders_seq")));
        assert!(!filter.admits(&Candidate::named("orders")));
        // A comma or a `!` is the expression's own, not the box's.
        filter.set_text("a,b");
        assert!(filter.admits(&Candidate::named("xa,by")));
        assert!(!filter.admits(&Candidate::named("a")));

        // Half-typed: marked, and nothing narrowed.
        filter.set_text("cust(");
        assert!(filter.text_invalid());
        assert!(!filter.is_active());
        assert!(filter.admits(&Candidate::named("orders")));

        let snapshot = {
            filter.set_text("_seq$");
            filter.snapshot()
        };
        assert!(snapshot.regex);
        assert!(!snapshot.is_empty());
        let back = ExplorerFilter::from_snapshot(&snapshot);
        assert!(back.regex());
        assert!(back.admits(&Candidate::named("a_seq")));
        assert!(!back.admits(&Candidate::named("a_seq_old")));
    }

    fn condition(field: Field, op: Op, value: &str) -> Condition {
        Condition {
            field,
            op,
            value: value.to_string(),
        }
    }

    fn table<'a>(name: &'a str, remarks: Option<&'a str>, schema: &'a str) -> Candidate<'a> {
        Candidate {
            name,
            remarks,
            schema: Some(schema),
        }
    }

    #[test]
    fn every_comparison_ignores_case() {
        let object = table("Customer_Address", Some("Customer ADDRESSES"), "Sales");
        let passes = |op, value: &str, field| {
            let mut filter = ExplorerFilter::default();
            filter.set_conditions(Combine::All, vec![condition(field, op, value)]);
            filter.admits(&object)
        };
        assert!(passes(Op::Contains, "address", Field::Remarks));
        assert!(!passes(Op::NotContains, "ADDRESS", Field::Name));
        assert!(passes(Op::NotContains, "order", Field::Name));
        assert!(passes(Op::StartsWith, "CUST", Field::Name));
        assert!(passes(Op::EndsWith, "address", Field::Name));
        assert!(!passes(Op::EndsWith, "cust", Field::Name));
        assert!(passes(Op::Equals, "sales", Field::Schema));
        assert!(!passes(Op::Equals, "sale", Field::Schema));
        assert!(passes(Op::Wildcard, "cust*_?ddress", Field::Name));
        assert!(passes(Op::Regex, "^cust.*s$", Field::Name));
        assert!(!passes(Op::Regex, "^address", Field::Name));
        assert!(passes(Op::NotEmpty, "", Field::Remarks));
        assert!(!passes(Op::Empty, "", Field::Remarks));
    }

    #[test]
    fn a_missing_comment_or_schema_reads_as_empty() {
        let object = Candidate::named("orders");
        let mut filter = ExplorerFilter::default();
        filter.set_conditions(Combine::All, vec![condition(Field::Remarks, Op::Empty, "")]);
        assert!(filter.admits(&object));
        filter.set_conditions(
            Combine::All,
            vec![condition(Field::Schema, Op::Contains, "x")],
        );
        assert!(!filter.admits(&object));
    }

    #[test]
    fn conditions_go_together_as_all_or_any_and_always_with_the_box() {
        let customer = table("customer", Some("고객 마스터"), "public");
        let orders = table("orders", Some("주문"), "public");
        let rows = vec![
            condition(Field::Name, Op::StartsWith, "cust"),
            condition(Field::Remarks, Op::Contains, "주문"),
        ];
        let mut filter = ExplorerFilter::default();
        filter.set_conditions(Combine::All, rows.clone());
        assert!(filter.is_active());
        assert_eq!(filter.live_conditions(), 2);
        assert!(!filter.admits(&customer));
        assert!(!filter.admits(&orders));

        filter.set_conditions(Combine::Any, rows);
        assert!(filter.admits(&customer));
        assert!(filter.admits(&orders));

        // The box is asked as well, whichever way the rows go together.
        filter.set_text("!ord*");
        assert!(filter.admits(&customer));
        assert!(!filter.admits(&orders));
    }

    #[test]
    fn a_row_being_filled_in_or_a_broken_expression_narrows_nothing() {
        let mut filter = ExplorerFilter::default();
        filter.set_conditions(
            Combine::All,
            vec![
                condition(Field::Name, Op::Contains, "  "),
                condition(Field::Name, Op::Regex, "cust("),
            ],
        );
        assert_eq!(filter.live_conditions(), 0);
        assert!(!filter.is_active());
        assert!(!filter.is_invalid(0));
        assert!(filter.is_invalid(1));
        assert!(filter.admits(&Candidate::named("anything")));
    }

    #[test]
    fn a_snapshot_keeps_what_decides_the_filter_and_round_trips() {
        let mut filter = ExplorerFilter::default();
        filter.set_text("  cust ");
        filter.set_targets(
            FolderSet::NONE
                .toggled(Folder::Views)
                .toggled(Folder::Tables),
        );
        filter.set_conditions(
            Combine::Any,
            vec![
                condition(Field::Remarks, Op::Contains, " 고객 "),
                condition(Field::Name, Op::Equals, ""),
                condition(Field::Remarks, Op::Empty, "ignored"),
            ],
        );
        let snapshot = filter.snapshot();
        assert_eq!(snapshot.text, "cust");
        assert_eq!(snapshot.targets, Some(vec![Folder::Tables, Folder::Views]));
        assert_eq!(snapshot.combine, Combine::Any);
        // The row with no value is not kept; the rest are, trimmed.
        assert_eq!(
            snapshot.conditions,
            vec![
                condition(Field::Remarks, Op::Contains, "고객"),
                condition(Field::Remarks, Op::Empty, "ignored"),
            ]
        );

        let json = serde_json::to_string(&snapshot).expect("serializes");
        let back: FilterSnapshot = serde_json::from_str(&json).expect("reads back");
        assert_eq!(back, snapshot);
        let restored = ExplorerFilter::from_snapshot(&back);
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.targets(), filter.targets());

        let mut everywhere = ExplorerFilter::default();
        everywhere.set_text("cust");
        everywhere.set_all_databases(true);
        let snapshot = everywhere.snapshot();
        assert!(snapshot.all_databases);
        assert_eq!(
            ExplorerFilter::from_snapshot(&snapshot).snapshot(),
            snapshot
        );
        assert!(ExplorerFilter::from_snapshot(&snapshot).all_databases());

        // Every folder is written as no folders at all, and read back so.
        let everything = ExplorerFilter::default().snapshot();
        assert_eq!(everything.targets, None);
        assert!(everything.is_empty());
        assert_eq!(
            serde_json::to_string(&everything).unwrap(),
            r#"{"text":"","combine":"all"}"#
        );
        assert!(
            ExplorerFilter::from_snapshot(&everything)
                .targets()
                .is_all()
        );
    }

    #[test]
    fn the_history_menu_spells_the_conditions_out() {
        let snapshot = FilterSnapshot {
            text: String::new(),
            targets: None,
            combine: Combine::All,
            conditions: vec![
                condition(Field::Name, Op::StartsWith, "cust"),
                condition(Field::Remarks, Op::NotEmpty, ""),
            ],
            all_databases: false,
            regex: false,
        };
        assert_eq!(
            snapshot.conditions_summary(),
            format!(
                "{} {} cust {} {} {}",
                Field::Name.label(),
                Op::StartsWith.label(),
                ts!("explorer.combine_and"),
                Field::Remarks.label(),
                Op::NotEmpty.label()
            )
        );
    }
}
