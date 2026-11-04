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
//! # Why the filter has a set of folders
//!
//! The same text rarely means something for every kind of object at once:
//! `*_seq` is about sequences, `v_*` about views. The filter therefore narrows
//! only the folders in its [`FolderSet`] and leaves the others whole — a folder
//! that is not being filtered is not hiding anything, which is easier to read
//! than a folder emptied by a pattern that was never meant for it.

use crate::explorer::{FOLDERS, Folder};

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

/// One connection's filter: the text in the box and the folders it narrows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplorerFilter {
    /// What the box holds, as typed.
    text: String,
    /// The same, parsed.
    name: NameFilter,
    /// The folders the filter narrows.
    targets: FolderSet,
}

impl ExplorerFilter {
    /// What the box holds, as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text, and the terms parsed from it.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.name = NameFilter::parse(&self.text);
    }

    /// The folders the filter narrows.
    pub fn targets(&self) -> FolderSet {
        self.targets
    }

    /// Replaces the folders the filter narrows.
    pub fn set_targets(&mut self, targets: FolderSet) {
        self.targets = targets;
    }

    /// Whether the filter narrows anything at all.
    ///
    /// Not when the box is blank, and not when no folder is chosen: either way
    /// the tree is whole.
    pub fn is_active(&self) -> bool {
        !self.name.is_empty() && !self.targets.is_empty()
    }

    /// Whether the filter narrows `folder`.
    pub fn narrows(&self, folder: Folder) -> bool {
        self.is_active() && self.targets.contains(folder)
    }

    /// Whether an object called `name` passes the box.
    ///
    /// Says nothing about the folder: ask [`ExplorerFilter::narrows`] first.
    pub fn admits(&self, name: &str) -> bool {
        self.name.matches(name)
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
        assert!(filter.admits("orders_id_seq"));
        assert!(!filter.admits("orders"));

        filter.set_targets(FolderSet::NONE.toggled(Folder::Sequences));
        assert!(filter.narrows(Folder::Sequences));
        assert!(!filter.narrows(Folder::Tables));

        filter.set_targets(FolderSet::NONE);
        assert!(!filter.is_active());
    }
}
