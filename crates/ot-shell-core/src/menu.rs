//! The view's menu entries as a tree a shell can show without knowing the view.
//!
//! [`MenuEntry::Affinity`] is filled in here, from the process's affinity as it
//! stands when the menu opens, so every shell shows the same submenu. Each item
//! gets an id; the shell answers with the id of the item chosen
//! ([`crate::Shell::menu_closed`]) and never sees a [`MenuAction`].

use std::fmt::Write as _;

use ot_probe::{ControlError, PlatformControl, ProcessControl};
use ot_ui::{MenuAction, MenuEntry};

/// One line of a menu, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuNode {
    Item {
        /// What the shell answers with when this is chosen.
        id: usize,
        label: String,
        enabled: bool,
        checked: bool,
    },
    Submenu {
        label: String,
        /// A submenu that cannot be opened, shown greyed.
        enabled: bool,
        children: Vec<MenuNode>,
    },
    Separator,
}

/// Turn the view's entries into nodes, collecting each item's action in
/// `actions`; an item's id is its index there.
pub(crate) fn build(
    entries: &[MenuEntry],
    actions: &mut Vec<MenuAction>,
    control: PlatformControl,
) -> Vec<MenuNode> {
    let mut nodes = Vec::with_capacity(entries.len());
    for e in entries {
        nodes.push(match e {
            MenuEntry::Item {
                action,
                label,
                enabled,
                checked,
            } => {
                actions.push(*action);
                MenuNode::Item {
                    id: actions.len() - 1,
                    label: label.to_string(),
                    enabled: *enabled,
                    checked: *checked,
                }
            }
            MenuEntry::Submenu { label, entries } => MenuNode::Submenu {
                label: (*label).to_owned(),
                enabled: true,
                children: build(entries, actions, control),
            },
            MenuEntry::Affinity { target } => {
                let affinity = control.affinity(*target);
                MenuNode::Submenu {
                    label: "Set affinity".to_owned(),
                    enabled: affinity.is_ok(),
                    children: affinity_items(&affinity, actions),
                }
            }
            MenuEntry::Separator => MenuNode::Separator,
        });
    }
    nodes
}

/// The affinity submenu: "All processors", then one check item per logical
/// processor the system has. Choosing one toggles that processor in the mask;
/// the resulting mask is what the action carries.
fn affinity_items(
    affinity: &Result<ot_probe::Affinity, ControlError>,
    actions: &mut Vec<MenuAction>,
) -> Vec<MenuNode> {
    let Ok(a) = affinity else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    actions.push(MenuAction::SetAffinity(a.system));
    nodes.push(MenuNode::Item {
        id: actions.len() - 1,
        label: "All processors".to_owned(),
        enabled: true,
        checked: a.mask == a.system,
    });
    nodes.push(MenuNode::Separator);
    for cpu in 0..64u32 {
        let bit = 1u64 << cpu;
        if a.system & bit == 0 {
            continue;
        }
        // Toggling the last processor would leave nothing to run on; the menu
        // offers it anyway and the action refuses an empty mask.
        actions.push(MenuAction::SetAffinity(a.mask ^ bit));
        let mut label = String::new();
        let _ = write!(label, "CPU {cpu}");
        nodes.push(MenuNode::Item {
            id: actions.len() - 1,
            label,
            enabled: true,
            checked: a.mask & bit != 0,
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_index_the_actions_in_order() {
        let entries = vec![
            MenuEntry::item(MenuAction::EndTask, "End task", true),
            MenuEntry::Separator,
            MenuEntry::Submenu {
                label: "More",
                entries: vec![MenuEntry::item(MenuAction::EndTree, "End tree", false)],
            },
        ];
        let mut actions = Vec::new();
        let nodes = build(&entries, &mut actions, PlatformControl);
        assert_eq!(actions, vec![MenuAction::EndTask, MenuAction::EndTree]);
        let MenuNode::Submenu { children, .. } = &nodes[2] else {
            panic!("expected a submenu");
        };
        assert!(matches!(
            children[0],
            MenuNode::Item {
                id: 1,
                enabled: false,
                ..
            }
        ));
    }
}
