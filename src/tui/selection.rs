/// Returns the selected item from a derived table view.
///
/// TUI tables often render filtered vectors while the backing state keeps the
/// full collection. Selection indices must therefore be resolved against the
/// rendered view, not the backing collection.
pub(super) fn selected<'a, T>(view: &[&'a T], selected: Option<usize>) -> Option<&'a T> {
    selected.and_then(|index| view.get(index).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_resolves_against_filtered_view() {
        let all = ["first", "second", "third"];
        let filtered = vec![&all[1], &all[2]];

        assert_eq!(selected(&filtered, Some(0)), Some(&"second"));
        assert_eq!(selected(&filtered, Some(1)), Some(&"third"));
    }

    #[test]
    fn selected_returns_none_for_out_of_range() {
        let all = ["first"];
        let filtered = vec![&all[0]];

        assert_eq!(selected(&filtered, Some(9)), None);
        assert_eq!(selected(&filtered, None), None);
    }
}
