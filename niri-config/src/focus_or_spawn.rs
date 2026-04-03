use std::collections::HashSet;

use knuffel::errors::DecodeError;
use smithay::input::keyboard::Keysym;

use crate::binds::{Action, Key, Trigger};
use crate::utils::expect_only_children;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct FocusOrSpawn(pub Vec<FocusOrSpawnEntry>);

#[derive(Debug, Clone, PartialEq)]
pub struct FocusOrSpawnEntry {
    pub trigger: Keysym,
    pub app_id: String,
    pub spawn: Option<Action>,
    pub cycle_all_workspaces: bool,
    pub cycle_all_outputs: bool,
}

impl FocusOrSpawn {
    pub fn find_by_trigger(&self, trigger: Keysym) -> Option<&FocusOrSpawnEntry> {
        self.0.iter().find(|entry| entry.trigger == trigger)
    }
}

impl<S> knuffel::Decode<S> for FocusOrSpawn
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        for child in node.children() {
            match FocusOrSpawnEntry::decode_node(child, ctx) {
                Err(err) => ctx.emit_error(err),
                Ok(entry) => {
                    if seen.insert(entry.trigger) {
                        entries.push(entry);
                    } else {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "focus-or-spawn entry",
                            "duplicate focus-or-spawn key",
                        ));
                    }
                }
            }
        }

        Ok(Self(entries))
    }
}

impl<S> knuffel::Decode<S> for FocusOrSpawnEntry
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        for val in node.arguments.iter() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "no arguments expected for this node",
            ));
        }

        let key = node
            .node_name
            .parse::<Key>()
            .map_err(|err| DecodeError::conversion(&node.node_name, err))?;
        let trigger = match key {
            Key {
                trigger: Trigger::Keysym(keysym),
                modifiers,
            } if modifiers.is_empty() => keysym,
            _ => {
                return Err(DecodeError::conversion(
                    &node.node_name,
                    "focus-or-spawn key must be a single keyboard key without modifiers",
                ));
            }
        };

        let mut app_id = None;
        let mut cycle_all_workspaces = false;
        let mut cycle_all_outputs = false;
        for (name, val) in &node.properties {
            match &***name {
                "app-id" => app_id = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?),
                "all-workspaces" => {
                    cycle_all_workspaces = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                }
                "all-outputs" => {
                    cycle_all_outputs = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                }
                name_str => {
                    ctx.emit_error(DecodeError::unexpected(
                        name,
                        "property",
                        format!("unexpected property `{}`", name_str.escape_default()),
                    ));
                }
            }
        }
        let app_id =
            app_id.ok_or_else(|| DecodeError::missing(node, "missing required property `app-id`"))?;

        let mut children = node.children();
        let child = children.next();
        for unwanted_child in children {
            ctx.emit_error(DecodeError::unexpected(
                unwanted_child,
                "node",
                "only one action is allowed per focus-or-spawn entry",
            ));
        }

        let spawn = match child {
            Some(child) => {
                let action = Action::decode_node(child, ctx)?;
                if !matches!(action, Action::Spawn(_) | Action::SpawnSh(_)) {
                    return Err(DecodeError::unexpected(
                        child,
                        "action",
                        "focus-or-spawn entries only support spawn or spawn-sh",
                    ));
                }
                Some(action)
            }
            None => None,
        };

        Ok(Self {
            trigger,
            app_id,
            spawn,
            cycle_all_workspaces,
            cycle_all_outputs,
        })
    }
}
