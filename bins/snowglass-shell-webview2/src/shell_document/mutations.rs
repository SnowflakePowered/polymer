use super::tracked_surface::TrackedSurfaceWidget;
use crate::shell_runtime::Mutation;
use blitz_dom::{LocalName, QualName, ns};
use std::collections::HashMap;
use style::values::specified::box_::Contain;

#[derive(Default)]
pub(super) struct MutationEffect {
    pub(super) needs_resolve: bool,
    pub(super) needs_geometry_sync: bool,
    pub(super) unbounded: bool,
    pub(super) contained: Vec<ContainedDamage>,
}

#[derive(Clone, Copy)]
pub(super) struct ContainedDamage {
    pub(super) node: usize,
    pub(super) before: kurbo::Rect,
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
        effect.unbounded = true;
        effect.contained.clear();
        effect
    } else {
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, mutations)
    };
    if !focus.is_empty() {
        effect.needs_resolve = true;
        effect.needs_geometry_sync = true;
        effect.unbounded = true;
        effect.contained.clear();
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
    let mut unbounded_damage = false;
    let mut contained_damage = HashMap::<usize, kurbo::Rect>::new();
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
                unbounded_damage = true;
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
                unbounded_damage = true;
                let actual = mutator.create_text_node(&text);
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateComment { node } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
                let actual = mutator.create_comment_node();
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::AppendChild { parent, child } => {
                needs_resolve = true;
                needs_geometry_sync = true;
                unbounded_damage = true;
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
                unbounded_damage = true;
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
                unbounded_damage = true;
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
                unbounded_damage = true;
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
                if let Some(contained) = containment_damage(mutator.doc, node) {
                    contained_damage
                        .entry(contained.node)
                        .or_insert(contained.before);
                } else {
                    unbounded_damage = true;
                }
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
                if let Some(contained) = containment_damage(mutator.doc, node) {
                    contained_damage
                        .entry(contained.node)
                        .or_insert(contained.before);
                } else {
                    unbounded_damage = true;
                }
                mutator.clear_attribute(node, qualified_name);
            }
            Mutation::Focus { .. } => unreachable!("focus mutations are applied after DOM edits"),
        }
    }
    MutationEffect {
        needs_resolve,
        needs_geometry_sync,
        unbounded: unbounded_damage,
        contained: contained_damage
            .into_iter()
            .map(|(node, before)| ContainedDamage { node, before })
            .collect(),
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

/// Finds the nearest standards-defined paint boundary for a mutation. Layout
/// and paint containment together guarantee that descendant layout and paint
/// cannot escape this box, so the compositor may preserve pixels outside it.
fn containment_damage(
    document: &blitz_dom::BaseDocument,
    node_id: usize,
) -> Option<ContainedDamage> {
    std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    })
    .find_map(|node| {
        let contain = document
            .get_node(node)?
            .primary_styles()?
            .get_box()
            .clone_contain();
        (contain.contains(Contain::LAYOUT | Contain::PAINT)).then(|| ContainedDamage {
            node,
            before: visual_damage_rect(document, node),
        })
    })
}

/// Returns conservative viewport-space paint bounds. Blitz exposes layout
/// bounds separately from transforms and does not yet publish visual-overflow
/// rectangles, so include translations explicitly and reserve enough fringe
/// for ordinary shadows and antialiasing.
pub(super) fn visual_damage_rect(
    document: &blitz_dom::BaseDocument,
    node_id: usize,
) -> kurbo::Rect {
    const VISUAL_OVERFLOW_MARGIN: f64 = 64.0;
    let Some(rect) = document.get_client_bounding_rect(node_id) else {
        return kurbo::Rect::ZERO;
    };
    let mut translation = kurbo::Vec2::ZERO;
    for node in std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    }) {
        if let Some(transform) = document.get_node(node).and_then(|node| node.transform) {
            let coefficients = transform.as_coeffs();
            translation += kurbo::Vec2::new(coefficients[4], coefficients[5]);
        }
    }
    kurbo::Rect::new(
        rect.x + translation.x - VISUAL_OVERFLOW_MARGIN,
        rect.y + translation.y - VISUAL_OVERFLOW_MARGIN,
        rect.x + translation.x + rect.width + VISUAL_OVERFLOW_MARGIN,
        rect.y + translation.y + rect.height + VISUAL_OVERFLOW_MARGIN,
    )
}
