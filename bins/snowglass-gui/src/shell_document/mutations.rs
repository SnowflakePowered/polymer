use super::tracked_surface::TrackedSurfaceWidget;
use crate::shell_runtime::Mutation;
use blitz_dom::{LocalName, QualName, ns};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct MutationEffect {
    pub(super) needs_resolve: bool,
    pub(super) needs_geometry_sync: bool,
}

pub(super) fn apply_dom_mutations(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) -> MutationEffect {
    if mutations.is_empty() {
        return MutationEffect::default();
    }

    // Stylo assumes every descendant reached by attribute invalidation already
    // has a primary computed style. Solid can append a newly-created subtree
    // and then set an attribute on one of its ancestors in the same mutation
    // batch. If we feed that order directly to Blitz, the later invalidation
    // sees the attached-but-unstyled descendants and panics in
    // `ElementStyles::primary`.
    //
    // Attach the new tree without attribute invalidations, give it its base
    // styles, then apply attributes. The next normal paint resolve computes
    // the final styles; the intermediate resolve is never presented.
    let (focus, mutations): (Vec<_>, Vec<_>) = mutations
        .into_iter()
        .partition(|mutation| matches!(mutation, Mutation::Focus { .. }));
    let has_new_elements = mutations
        .iter()
        .any(|mutation| matches!(mutation, Mutation::CreateElement { .. }));
    let mut effect = if has_new_elements {
        let (attributes, structure): (Vec<_>, Vec<_>) =
            mutations.into_iter().partition(|mutation| {
                matches!(
                    mutation,
                    Mutation::SetAttribute { .. } | Mutation::RemoveAttribute { .. }
                )
            });
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, structure);
        document.as_mut().resolve(1.0);
        let mut effect =
            apply_dom_mutation_batch(document, script_to_dom, dom_to_script, attributes);
        effect.needs_resolve = true;
        effect.needs_geometry_sync = true;
        effect
    } else {
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, mutations)
    };
    if !focus.is_empty() {
        effect.needs_resolve = true;
        effect.needs_geometry_sync = true;
    }
    for mutation in focus {
        if let Mutation::Focus { node } = mutation
            && let Some(node) = script_to_dom.get(&node).copied()
        {
            document.as_mut().set_focus_to(node);
        }
    }
    effect
}

fn apply_dom_mutation_batch(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) -> MutationEffect {
    let mut needs_resolve = false;
    let mut needs_geometry_sync = false;
    let mut mutator = document.as_mut().mutate();
    for mutation in mutations {
        match mutation {
            Mutation::CreateElement {
                node,
                tag,
                namespace,
            } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let is_tracked_surface = tag == "tracked-surface";
                let namespace = match namespace.as_deref() {
                    Some("http://www.w3.org/2000/svg") => ns!(svg),
                    _ => ns!(html),
                };
                let actual = mutator.create_element(
                    QualName::new(None, namespace, LocalName::from(tag)),
                    Vec::new(),
                );
                if is_tracked_surface {
                    mutator
                        .doc
                        .set_custom_widget(actual, Box::new(TrackedSurfaceWidget::default()));
                }
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateText { node, text } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let actual = mutator.create_text_node(&text);
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateComment { node } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let actual = mutator.create_comment_node();
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::AppendChild { parent, child } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                mutator.append_children(parent, &[child]);
            }
            Mutation::InsertBefore {
                parent,
                child,
                before,
            } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let Some((parent, child, before)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                    .zip(script_to_dom.get(&before).copied())
                    .map(|((parent, child), before)| (parent, child, before))
                else {
                    continue;
                };
                if mutator.parent_id(before) == Some(parent) {
                    mutator.insert_nodes_before(before, &[child]);
                }
            }
            Mutation::RemoveChild { parent, child } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                if mutator.parent_id(child) == Some(parent) {
                    mutator.remove_node(child);
                }
            }
            Mutation::SetText { node, text } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                if let Some(node) = script_to_dom.get(&node).copied() {
                    mutator.set_node_text(node, &text);
                }
            }
            Mutation::SetAttribute { node, name, value } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let attribute_name = LocalName::from(name);
                let previous = mutator
                    .doc
                    .get_node(node)
                    .and_then(blitz_dom::Node::element_data)
                    .and_then(|element| element.attr(attribute_name.clone()))
                    .map(str::to_owned);
                if previous.as_deref() == Some(value.as_str()) {
                    continue;
                }
                needs_resolve = true;
                needs_geometry_sync |= attribute_name.as_ref() != "style"
                    || style_change_requires_geometry(previous.as_deref(), Some(&value));
                // HTML attributes are in the null namespace. Using the HTML
                // element namespace here creates a second attribute instead
                // of replacing one parsed from markup, leaving Stylo and hit
                // testing on the stale value while the JavaScript mirror sees
                // the update.
                mutator.set_attribute(node, QualName::new(None, ns!(), attribute_name), &value);
            }
            Mutation::RemoveAttribute { node, name } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let attribute_name = LocalName::from(name);
                let qualified_name = QualName::new(None, ns!(), attribute_name.clone());
                if mutator
                    .doc
                    .get_node(node)
                    .and_then(blitz_dom::Node::element_data)
                    .and_then(|element| element.attr(attribute_name.clone()))
                    .is_none()
                {
                    continue;
                }
                needs_resolve = true;
                needs_geometry_sync |= attribute_name.as_ref() != "style"
                    || style_change_requires_geometry(
                        mutator
                            .doc
                            .get_node(node)
                            .and_then(blitz_dom::Node::element_data)
                            .and_then(|element| element.attr(attribute_name.clone())),
                        None,
                    );
                mutator.clear_attribute(node, qualified_name);
            }
            Mutation::Focus { .. } => unreachable!("focus mutations are applied after DOM edits"),
        }
    }
    MutationEffect {
        needs_resolve,
        needs_geometry_sync,
    }
}

/// Returns whether an inline-style edit can alter layout geometry. CSS
/// transforms and opacity affect paint/compositing only; all other changed
/// declarations remain conservative because custom properties and uncommon
/// properties may feed layout.
pub(super) fn style_change_requires_geometry(
    previous: Option<&str>,
    current: Option<&str>,
) -> bool {
    fn declarations(style: Option<&str>) -> HashMap<String, String> {
        style
            .unwrap_or_default()
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect()
    }

    let previous = declarations(previous);
    let current = declarations(current);
    previous
        .keys()
        .chain(current.keys())
        .filter(|name| !matches!(name.as_str(), "transform" | "transform-origin" | "opacity"))
        .any(|name| previous.get(name) != current.get(name))
}
