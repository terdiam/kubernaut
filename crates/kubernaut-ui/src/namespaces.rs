//! The namespaces a cluster is narrowed to, and the picker entries for them.
//!
//! The selection is a set: empty means every namespace. One namespace is
//! watched on the server; two or more are watched cluster-wide and filtered as
//! rows arrive, because a watch covers one namespace or all of them.

use gpui_kit::{SharedString, assets::IconName};

use crate::palette::{Command, Item};

/// What the toolbar and status bar say for a selection.
pub fn label(selected: &[SharedString]) -> SharedString {
    match selected {
        [] => "All namespaces".into(),
        [one] => one.clone(),
        many => format!("{} namespaces", many.len()).into(),
    }
}

/// Add a namespace to the selection, or take it out if it is already there.
/// The list stays sorted so the pills do not reshuffle as the user picks.
pub fn toggle(selected: &mut Vec<SharedString>, namespace: &SharedString) {
    match selected.binary_search(namespace) {
        Ok(at) => {
            selected.remove(at);
        }
        Err(at) => selected.insert(at, namespace.clone()),
    }
}

/// The saved selection that still applies: only namespaces the cluster still
/// has, sorted and without repeats. A namespace deleted since last time drops
/// out rather than leaving a filter that can only ever show nothing.
pub fn restore(saved: &[String], known: &[SharedString]) -> Vec<SharedString> {
    let mut scope: Vec<SharedString> = known
        .iter()
        .filter(|namespace| saved.iter().any(|saved| saved == &***namespace))
        .cloned()
        .collect();
    scope.sort();
    scope.dedup();
    scope
}

/// How to watch a selection: the namespace to ask the server for, and the set
/// to filter rows by when the server cannot do it.
pub fn watch_scope(selected: &[SharedString]) -> (Option<String>, Vec<SharedString>) {
    match selected {
        [] => (None, Vec::new()),
        [one] => (Some(one.to_string()), Vec::new()),
        many => (None, many.to_vec()),
    }
}

/// Whether a row belongs in the view. An empty scope admits everything, and a
/// row with no namespace (a cluster-scoped kind) is never filtered out.
pub fn admits(scope: &[SharedString], namespace: Option<&str>) -> bool {
    scope.is_empty()
        || namespace.is_none_or(|namespace| {
            namespace.is_empty() || scope.iter().any(|selected| &**selected == namespace)
        })
}

/// The picker's entries: clear first, then every namespace, ticked when
/// selected.
pub fn items(known: &[SharedString], selected: &[SharedString]) -> Vec<Item> {
    let mut items = Vec::with_capacity(known.len() + 1);
    items.push(Item {
        label: "All namespaces".into(),
        hint: if selected.is_empty() {
            "Current"
        } else {
            "Clear the selection"
        }
        .into(),
        icon: IconName::Globe,
        command: Command::ClearNamespaces,
    });
    items.extend(known.iter().map(|namespace| {
        let ticked = selected.contains(namespace);
        Item {
            label: namespace.clone(),
            hint: if ticked { "Selected" } else { "Namespace" }.into(),
            icon: if ticked {
                IconName::Check
            } else {
                IconName::Folder
            },
            command: Command::ToggleNamespace(namespace.clone()),
        }
    }));
    items
}

#[cfg(test)]
mod tests {
    use super::{admits, items, label, restore, toggle, watch_scope};
    use crate::palette::Command;
    use gpui_kit::SharedString;

    fn names(list: &[&str]) -> Vec<SharedString> {
        list.iter()
            .map(|n| SharedString::from(n.to_string()))
            .collect()
    }

    #[test]
    fn the_label_counts_once_there_are_several() {
        assert_eq!(&*label(&[]), "All namespaces");
        assert_eq!(&*label(&names(&["prod"])), "prod");
        assert_eq!(&*label(&names(&["a", "b", "c"])), "3 namespaces");
    }

    #[test]
    fn toggling_adds_in_order_and_takes_out_what_is_already_there() {
        let mut selected = Vec::new();
        for ns in ["team-b", "default", "team-a"] {
            toggle(&mut selected, &ns.into());
        }
        assert_eq!(selected, names(&["default", "team-a", "team-b"]));

        toggle(&mut selected, &"team-a".into());
        assert_eq!(selected, names(&["default", "team-b"]));
        toggle(&mut selected, &"default".into());
        toggle(&mut selected, &"team-b".into());
        assert!(
            selected.is_empty(),
            "removing the last one shows everything"
        );
    }

    #[test]
    fn a_saved_selection_keeps_only_namespaces_that_still_exist() {
        let known = names(&["default", "team-a", "team-b"]);
        let saved = vec![
            "team-b".to_string(),
            "gone".to_string(),
            "team-a".to_string(),
        ];
        assert_eq!(restore(&saved, &known), names(&["team-a", "team-b"]));
        assert!(restore(&["gone".to_string()], &known).is_empty());
        assert!(restore(&[], &known).is_empty());
    }

    #[test]
    fn one_namespace_is_a_server_filter_and_several_are_a_client_filter() {
        assert_eq!(watch_scope(&[]), (None, Vec::new()));
        assert_eq!(
            watch_scope(&names(&["a"])),
            (Some("a".to_string()), Vec::new())
        );
        let (server, client) = watch_scope(&names(&["a", "b"]));
        assert_eq!(server, None);
        assert_eq!(client, names(&["a", "b"]));
    }

    #[test]
    fn rows_outside_the_selection_are_dropped_but_cluster_scoped_ones_stay() {
        let scope = names(&["a", "b"]);
        assert!(admits(&scope, Some("a")));
        assert!(!admits(&scope, Some("c")));
        assert!(admits(&scope, None), "a Node has no namespace");
        assert!(admits(&scope, Some("")));
        assert!(admits(&[], Some("anything")), "no selection admits all");
    }

    #[test]
    fn the_picker_ticks_what_is_selected_and_offers_to_clear() {
        let known = names(&["a", "b"]);
        let entries = items(&known, &names(&["b"]));
        assert_eq!(&*entries[0].hint, "Clear the selection");
        assert!(matches!(entries[0].command, Command::ClearNamespaces));
        assert_eq!(&*entries[1].hint, "Namespace");
        assert_eq!(&*entries[2].hint, "Selected");
        assert!(matches!(&entries[2].command, Command::ToggleNamespace(n) if &**n == "b"));

        assert_eq!(&*items(&known, &[])[0].hint, "Current");
    }
}
