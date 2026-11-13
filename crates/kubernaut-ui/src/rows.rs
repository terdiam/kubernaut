//! The rows a resource table shows, and how a watch keeps them current.
//!
//! Pure data: no window system, so the batch, sort and filter rules are
//! unit-tested directly. The table delegate in `table.rs` is a thin layer over
//! this.

use std::{cmp::Ordering, collections::HashMap};

use gpui_kit::SharedString;
use k8s_app::present::{Tone, parse_timestamp, tone_for, write_age};
use k8s_core::{ColumnSpec, Row, RowHealth, WatchBatch};

/// One object as the table keeps it. Strings are `SharedString` so rendering a
/// cell clones an `Arc`, not the text.
pub struct StoredRow {
    pub uid: SharedString,
    pub name: SharedString,
    /// Empty for cluster-scoped objects.
    pub namespace: SharedString,
    /// Aligned with the watch's `TableSpec::columns`.
    pub cells: Vec<SharedString>,
    /// Aligned with `cells`; `None` for columns that are not a status.
    pub tones: Vec<Option<Tone>>,
    pub health: RowHealth,
    pub terminating: bool,
    created: Option<i64>,
    pub age: SharedString,
}

impl StoredRow {
    fn from_row(row: &Row, columns: &[ColumnSpec], now: i64) -> Self {
        let created = row.created.as_deref().and_then(parse_timestamp);
        let mut age = String::new();
        if let Some(created) = created {
            write_age(&mut age, now - created);
        }
        Self {
            uid: row.uid.as_str().into(),
            name: row.name.as_str().into(),
            namespace: row.namespace.as_deref().unwrap_or_default().into(),
            tones: row
                .cells
                .iter()
                .zip(columns)
                .map(|(value, column)| tone_for(&column.name, value))
                .collect(),
            cells: row.cells.iter().map(|cell| cell.as_str().into()).collect(),
            health: row.health,
            terminating: row.terminating,
            created,
            age: age.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Namespace,
    /// Index into the watch's columns, not into the visible ones.
    Cell(usize),
    Age,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

#[derive(Default)]
pub struct RowStore {
    rows: Vec<StoredRow>,
    index: HashMap<SharedString, usize>,
    /// Positions in `rows`, filtered and sorted. `u32` halves the cost of the
    /// view against `usize`; no table gets near four billion rows.
    order: Vec<u32>,
    epoch: u64,
}

impl RowStore {
    /// Apply one batch. Returns whether the set of rows changed.
    ///
    /// A batch from an older epoch is dropped: it predates a re-list, and
    /// applying a slow delta from before it would resurrect a deleted row.
    pub fn apply(&mut self, batch: &WatchBatch, columns: &[ColumnSpec], now: i64) -> bool {
        if batch.epoch < self.epoch {
            return false;
        }
        self.epoch = batch.epoch;

        if batch.snapshot {
            self.rows.clear();
            self.rows.reserve(batch.upserts.len());
            self.index.clear();
            self.index.reserve(batch.upserts.len());
            for row in &batch.upserts {
                self.upsert(StoredRow::from_row(row, columns, now));
            }
            return true;
        }

        if batch.upserts.is_empty() && batch.deletes.is_empty() {
            return false;
        }
        for row in &batch.upserts {
            self.upsert(StoredRow::from_row(row, columns, now));
        }
        for uid in &batch.deletes {
            self.remove(uid);
        }
        true
    }

    fn upsert(&mut self, row: StoredRow) {
        match self.index.get(&row.uid) {
            Some(&at) => {
                if let Some(slot) = self.rows.get_mut(at) {
                    *slot = row;
                }
            }
            None => {
                self.index.insert(row.uid.clone(), self.rows.len());
                self.rows.push(row);
            }
        }
    }

    fn remove(&mut self, uid: &str) {
        let Some(at) = self.index.remove(uid) else {
            return;
        };
        self.rows.swap_remove(at);
        // `swap_remove` moved the last row into `at`; point its index there.
        if let Some(moved) = self.rows.get(at) {
            self.index.insert(moved.uid.clone(), at);
        }
    }

    /// Recompute every age against `now`, keeping the existing string when the
    /// text did not change so an idle table allocates nothing.
    pub fn refresh_ages(&mut self, now: i64) {
        let mut text = String::new();
        for row in &mut self.rows {
            let Some(created) = row.created else {
                continue;
            };
            text.clear();
            write_age(&mut text, now - created);
            if *row.age != *text {
                row.age = text.as_str().into();
            }
        }
    }

    /// Rebuild the filtered, sorted view.
    ///
    /// `needle` is matched case-insensitively against the name, namespace and
    /// the visible cells. `visible` lists the column indices that are shown.
    pub fn rebuild_view(&mut self, sort: Option<Sort>, needle: &str, visible: &[usize]) {
        self.order.clear();
        self.order.extend(
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row_matches(row, needle, visible))
                .filter_map(|(at, _)| u32::try_from(at).ok()),
        );

        let rows = &self.rows;
        let sort = sort.unwrap_or(Sort {
            key: SortKey::Namespace,
            descending: false,
        });
        self.order.sort_unstable_by(|&a, &b| {
            let (Some(a), Some(b)) = (rows.get(a as usize), rows.get(b as usize)) else {
                return Ordering::Equal;
            };
            let ordering = compare(a, b, sort.key);
            if sort.descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Objects the watch holds, before the filter.
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// The row at a position in the current view.
    pub fn row(&self, view_ix: usize) -> Option<&StoredRow> {
        let at = *self.order.get(view_ix)?;
        self.rows.get(at as usize)
    }
}

fn row_matches(row: &StoredRow, needle: &str, visible: &[usize]) -> bool {
    needle.is_empty()
        || contains_ignore_case(&row.name, needle)
        || contains_ignore_case(&row.namespace, needle)
        || visible
            .iter()
            .filter_map(|&column| row.cells.get(column))
            .any(|cell| contains_ignore_case(cell, needle))
}

/// ASCII case-insensitive substring test that allocates nothing. Kubernetes
/// names are ASCII; any other character must match exactly.
pub(crate) fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    // `windows(0)` panics, and an empty needle matches everything anyway.
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

fn compare(a: &StoredRow, b: &StoredRow, key: SortKey) -> Ordering {
    let primary = match key {
        SortKey::Name => natural_cmp(&a.name, &b.name),
        SortKey::Namespace => natural_cmp(&a.namespace, &b.namespace),
        SortKey::Cell(column) => natural_cmp(
            a.cells.get(column).map_or("", |cell| &**cell),
            b.cells.get(column).map_or("", |cell| &**cell),
        ),
        // Youngest first: a later creation time is a smaller age. Objects with
        // no creation time sort after those that have one.
        SortKey::Age => match (a.created, b.created) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    // A total order keeps the view from shuffling between batches.
    primary
        .then_with(|| natural_cmp(&a.namespace, &b.namespace))
        .then_with(|| natural_cmp(&a.name, &b.name))
        .then_with(|| a.uid.cmp(&b.uid))
}

/// Compare so that `pod-2` sorts before `pod-10` and case is ignored.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.split_first(), b.split_first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some((x, _)), Some((y, _))) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (digits_a, rest_a) = split_digits(a);
                let (digits_b, rest_b) = split_digits(b);
                let (digits_a, digits_b) = (trim_zeros(digits_a), trim_zeros(digits_b));
                // More digits means a bigger number once leading zeros are gone.
                let ordering = digits_a
                    .len()
                    .cmp(&digits_b.len())
                    .then_with(|| digits_a.cmp(digits_b));
                if ordering != Ordering::Equal {
                    return ordering;
                }
                (a, b) = (rest_a, rest_b);
            }
            (Some((x, rest_a)), Some((y, rest_b))) => {
                let ordering = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                (a, b) = (rest_a, rest_b);
            }
        }
    }
}

fn split_digits(bytes: &[u8]) -> (&[u8], &[u8]) {
    let end = bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len());
    bytes.split_at(end)
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let start = digits
        .iter()
        .position(|&byte| byte != b'0')
        .unwrap_or(digits.len());
    digits.get(start..).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{RowStore, Sort, SortKey, contains_ignore_case, natural_cmp};
    use k8s_core::{ColumnSpec, Row, RowHealth, WatchBatch, WatchState};
    use std::cmp::Ordering;

    fn columns() -> Vec<ColumnSpec> {
        ["Name", "Status"]
            .iter()
            .map(|name| ColumnSpec {
                name: (*name).into(),
                kind: "string".into(),
                priority: 0,
                description: None,
            })
            .collect()
    }

    fn row(uid: &str, name: &str, status: &str) -> Row {
        Row {
            uid: uid.into(),
            name: name.into(),
            namespace: Some("ns".into()),
            created: Some("2026-01-01T00:00:00Z".into()),
            resource_version: None,
            cells: vec![name.into(), status.into()],
            health: RowHealth::Ok,
            terminating: false,
        }
    }

    fn batch(epoch: u64, snapshot: bool, upserts: Vec<Row>, deletes: &[&str]) -> WatchBatch {
        WatchBatch {
            epoch,
            snapshot,
            upserts,
            deletes: deletes.iter().map(|uid| (*uid).into()).collect(),
            state: WatchState::Ready,
        }
    }

    fn names(store: &RowStore) -> Vec<String> {
        (0..store.len())
            .filter_map(|ix| store.row(ix).map(|row| row.name.to_string()))
            .collect()
    }

    #[test]
    fn a_snapshot_replaces_everything_and_deltas_patch_it() {
        let cols = columns();
        let mut store = RowStore::default();
        let all = [0usize, 1];

        assert!(store.apply(
            &batch(
                1,
                true,
                vec![row("a", "alpha", "Running"), row("b", "beta", "Running")],
                &[]
            ),
            &cols,
            0
        ));
        store.rebuild_view(None, "", &all);
        assert_eq!(names(&store), ["alpha", "beta"]);

        // An upsert of an existing uid updates in place; a new uid is added.
        store.apply(
            &batch(
                1,
                false,
                vec![row("a", "alpha", "Failed"), row("c", "gamma", "Running")],
                &["b"],
            ),
            &cols,
            0,
        );
        store.rebuild_view(None, "", &all);
        assert_eq!(names(&store), ["alpha", "gamma"]);
        assert_eq!(
            &*store.row(0).map(|r| r.cells[1].clone()).unwrap(),
            "Failed"
        );
        assert_eq!(store.total(), 2);

        // A later snapshot wipes rows the cluster no longer has.
        store.apply(
            &batch(2, true, vec![row("z", "zeta", "Running")], &[]),
            &cols,
            0,
        );
        store.rebuild_view(None, "", &all);
        assert_eq!(names(&store), ["zeta"]);
    }

    #[test]
    fn a_batch_from_an_older_epoch_is_ignored() {
        let cols = columns();
        let mut store = RowStore::default();
        store.apply(
            &batch(5, true, vec![row("a", "alpha", "Running")], &[]),
            &cols,
            0,
        );

        let stale = batch(4, false, vec![row("ghost", "ghost", "Running")], &["a"]);
        assert!(!store.apply(&stale, &cols, 0));
        store.rebuild_view(None, "", &[0, 1]);
        assert_eq!(
            names(&store),
            ["alpha"],
            "the stale delta must not resurrect or delete"
        );
    }

    #[test]
    fn deleting_keeps_the_index_of_the_row_that_moved_into_the_gap() {
        let cols = columns();
        let mut store = RowStore::default();
        store.apply(
            &batch(
                1,
                true,
                vec![row("a", "a", "x"), row("b", "b", "x"), row("c", "c", "x")],
                &[],
            ),
            &cols,
            0,
        );
        // Removing the first row swaps the last one into its slot.
        store.apply(&batch(1, false, vec![], &["a"]), &cols, 0);
        store.apply(
            &batch(1, false, vec![row("c", "c", "Failed")], &[]),
            &cols,
            0,
        );
        store.rebuild_view(None, "", &[0, 1]);
        assert_eq!(names(&store), ["b", "c"]);
        assert_eq!(
            &*store.row(1).map(|r| r.cells[1].clone()).unwrap(),
            "Failed",
            "the update must land on the moved row, not a stale slot"
        );
    }

    #[test]
    fn filter_matches_name_namespace_and_visible_cells_case_insensitively() {
        let cols = columns();
        let mut store = RowStore::default();
        store.apply(
            &batch(
                1,
                true,
                vec![
                    row("a", "web-1", "Running"),
                    row("b", "db-1", "CrashLoopBackOff"),
                ],
                &[],
            ),
            &cols,
            0,
        );
        store.rebuild_view(None, "WEB", &[0, 1]);
        assert_eq!(names(&store), ["web-1"]);
        store.rebuild_view(None, "crashloop", &[0, 1]);
        assert_eq!(names(&store), ["db-1"]);
        store.rebuild_view(None, "crashloop", &[0]);
        assert!(names(&store).is_empty(), "a hidden column is not searched");
        store.rebuild_view(None, "NS", &[0]);
        assert_eq!(store.len(), 2, "the namespace matches");
    }

    #[test]
    fn natural_order_sorts_numbers_by_value_and_ignores_case() {
        assert_eq!(natural_cmp("pod-2", "pod-10"), Ordering::Less);
        assert_eq!(natural_cmp("pod-010", "pod-10"), Ordering::Equal);
        assert_eq!(natural_cmp("Alpha", "alpha"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn sorting_by_name_descending_and_by_age() {
        let cols = columns();
        let mut store = RowStore::default();
        let mut older = row("a", "pod-2", "x");
        older.created = Some("2026-01-01T00:00:00Z".into());
        let mut newer = row("b", "pod-10", "x");
        newer.created = Some("2026-06-01T00:00:00Z".into());
        store.apply(&batch(1, true, vec![older, newer], &[]), &cols, 0);

        store.rebuild_view(
            Some(Sort {
                key: SortKey::Name,
                descending: true,
            }),
            "",
            &[0, 1],
        );
        assert_eq!(names(&store), ["pod-10", "pod-2"]);

        store.rebuild_view(
            Some(Sort {
                key: SortKey::Age,
                descending: false,
            }),
            "",
            &[0, 1],
        );
        assert_eq!(names(&store), ["pod-10", "pod-2"], "youngest first");
    }

    #[test]
    fn age_text_is_kept_when_it_does_not_change() {
        let cols = columns();
        let mut store = RowStore::default();
        store.apply(
            &batch(1, true, vec![row("a", "a", "x")], &[]),
            &cols,
            1_767_225_600 + 3 * 86_400,
        );
        store.rebuild_view(None, "", &[0, 1]);
        let before = store.row(0).map(|r| r.age.clone()).unwrap();
        store.refresh_ages(1_767_225_600 + 3 * 86_400 + 5);
        store.rebuild_view(None, "", &[0, 1]);
        let after = store.row(0).map(|r| r.age.clone()).unwrap();
        assert_eq!(&*before, "3d0h");
        assert_eq!(&*before, &*after);
    }

    #[test]
    fn substring_search_handles_empty_and_oversized_needles() {
        assert!(contains_ignore_case("anything", ""));
        assert!(!contains_ignore_case("ab", "abc"));
        assert!(contains_ignore_case("Hello", "ELL"));
    }
}
