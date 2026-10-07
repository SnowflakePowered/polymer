use super::ShellDocument;
use super::image::ImageWidget;
use super::scrolling::Scroller;
use super::tracked_surface::{TrackedSurfaceWidget, is_tracked_surface_element};
use crate::shell_runtime::deno::{ScrollBehavior, ScrollCommand};
use blitz_dom::node::NodeData;
use blitz_dom::node::Widget;
use blitz_dom::{LocalName, NodeId, QualName, local_name, ns};
use std::collections::HashMap;
use style::selector_parser::RestyleDamage;

impl ShellDocument {
    fn mark_structure_changed(&mut self) {
        self.dom_structure_revision = self.dom_structure_revision.wrapping_add(1);
        self.mark_changed();
    }

    fn exists(&self, nodes: &[NodeId]) -> bool {
        nodes
            .iter()
            .all(|node| self.document.as_ref().get_node(*node).is_some())
    }

    fn attach_custom_widgets(&mut self, root: NodeId) {
        let mut widgets: Vec<(NodeId, Box<dyn Widget>)> = Vec::new();
        let mut images = Vec::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let Some(node) = self.document.as_ref().get_node(id) else {
                continue;
            };
            pending.extend(node.children.iter().copied());
            let Some(element) = node.element_data() else {
                continue;
            };
            if is_tracked_surface_element(element) {
                let source = element
                    .attr(local_name!("src"))
                    .and_then(|source| source.parse().ok());
                widgets.push((id, Box::new(TrackedSurfaceWidget::new(source))));
            } else if element.name.local == local_name!("img") {
                images.push(id);
            }
        }
        for (node, widget) in widgets {
            self.document.as_mut().set_custom_widget(node, widget);
        }
        for node in images {
            ImageWidget::attach(self.document.as_mut(), &self.image_loader, node);
        }
    }

    pub(crate) fn create_element(&mut self, tag: &str, namespace: Option<&str>) -> NodeId {
        let namespace = match namespace {
            Some("http://www.w3.org/2000/svg") => ns!(svg),
            _ => ns!(html),
        };
        let node = self.document.as_mut().mutate().create_element(
            QualName::new(None, namespace, LocalName::from(tag)),
            Vec::new(),
        );
        self.attach_custom_widgets(node);
        node
    }

    pub(crate) fn create_text(&mut self, text: &str) -> NodeId {
        self.document.as_mut().mutate().create_text_node(text)
    }

    pub(crate) fn create_comment(&mut self, text: &str) -> NodeId {
        self.document.as_mut().mutate().create_comment_node(text)
    }

    pub(crate) fn create_fragment(&mut self) -> NodeId {
        self.create_element("#document-fragment", None)
    }

    pub(crate) fn template_content(&mut self, template: NodeId) -> Option<NodeId> {
        self.exists(&[template])
            .then(|| self.document.as_mut().mutate().template_contents(template))
    }

    pub(crate) fn clone_node(&mut self, node: NodeId, deep: bool) -> Option<NodeId> {
        let document = self.document.as_mut();
        let clone = match (&document.get_node(node)?.data, deep) {
            (_, true) => document.deep_clone_node(node),
            (NodeData::Element(element), false) => {
                let (name, attributes) = (element.name.clone(), element.attrs().to_vec());
                document.mutate().create_element(name, attributes)
            }
            (NodeData::Text(text), false) => {
                let text = text.content.clone();
                document.mutate().create_text_node(&text)
            }
            (NodeData::Comment { contents }, false) => {
                let contents = contents.clone();
                document.mutate().create_comment_node(&contents)
            }
            _ => return None,
        };
        self.attach_custom_widgets(clone);
        Some(clone)
    }

    pub(crate) fn set_inner_html(&mut self, node: NodeId, html: &str) {
        if !self.exists(&[node]) {
            return;
        }
        self.document.as_mut().mutate().set_inner_html(node, html);
        self.attach_custom_widgets(node);
        self.mark_structure_changed();
    }

    pub(crate) fn append_child(&mut self, parent: NodeId, child: NodeId) {
        if !self.exists(&[parent, child]) {
            return;
        }
        self.document
            .as_mut()
            .mutate()
            .append_children(parent, &[child]);
        self.mark_structure_changed();
    }

    pub(crate) fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        if child == before
            || !self.exists(&[parent, child, before])
            || self
                .document
                .as_ref()
                .get_node(before)
                .and_then(|node| node.parent)
                != Some(parent)
        {
            return;
        }
        self.document
            .as_mut()
            .mutate()
            .insert_nodes_before(before, &[child]);
        self.mark_structure_changed();
    }

    pub(crate) fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        let document = self.document.as_mut();
        if document.get_node(child).and_then(|node| node.parent) != Some(parent) {
            return;
        }
        // A conditional UI subtree can be removed while one of its controls owns
        // focus. Pause dialogs exercise this whenever their focussed Resume
        // button disappears. Clear focus before detaching any subtree which
        // contains it.
        if let Some(focused) = document.get_focussed_node_id()
            && std::iter::successors(Some(focused), |node| {
                document.get_node(*node).and_then(|node| node.parent)
            })
            .any(|node| node == child)
        {
            document.clear_focus();
        }
        let containers = hoisted_layout_containers(document, child);
        let mut mutator = document.mutate();
        mutator.remove_node(child);
        for container in containers {
            if let Some(node) = mutator.doc.get_node_mut(container) {
                node.insert_damage(RestyleDamage::from_bits_retain(u16::MAX));
                node.mark_ancestors_dirty();
            }
        }
        drop(mutator);
        self.mark_structure_changed();
    }

    pub(crate) fn set_text(&mut self, node: NodeId, text: &str) {
        if !self.exists(&[node]) {
            return;
        }
        self.document.as_mut().mutate().set_node_text(node, text);
        self.mark_changed();
    }

    fn attribute(&self, node: NodeId, name: &LocalName) -> Option<String> {
        self.document
            .as_ref()
            .get_node(node)
            .and_then(blitz_dom::Node::element_data)
            .and_then(|element| element.attr(name.clone()))
            .map(str::to_owned)
    }

    fn attribute_changed(
        &mut self,
        name: &LocalName,
        previous: Option<&str>,
        current: Option<&str>,
    ) {
        self.dirty = true;
        self.needs_resolve = true;
        self.needs_geometry_sync |=
            &*name != "style" || style_change_requires_geometry(previous, current);
    }

    pub(crate) fn set_attribute(&mut self, node: NodeId, name: &str, value: &str) {
        let name = LocalName::from(name);
        let previous = self.attribute(node, &name);
        let is_element = self
            .document
            .as_ref()
            .get_node(node)
            .is_some_and(|node| node.element_data().is_some());
        if !is_element || previous.as_deref() == Some(value) {
            return;
        }
        // HTML attributes are in the null namespace. Using the HTML element
        // namespace here creates a second attribute instead of replacing one
        // parsed from markup.
        self.document.as_mut().mutate().set_attribute(
            node,
            QualName::new(None, ns!(), name.clone()),
            value,
        );
        if ImageWidget::is_trigger(&name) {
            ImageWidget::attach(self.document.as_mut(), &self.image_loader, node);
        }
        self.attribute_changed(&name, previous.as_deref(), Some(value));
    }

    pub(crate) fn remove_attribute(&mut self, node: NodeId, name: &str) {
        let name = LocalName::from(name);
        let Some(previous) = self.attribute(node, &name) else {
            return;
        };
        self.document
            .as_mut()
            .mutate()
            .clear_attribute(node, QualName::new(None, ns!(), name.clone()));
        self.attribute_changed(&name, Some(&previous), None);
    }

    pub(crate) fn focus(&mut self, node: NodeId) -> bool {
        let document = self.document.as_mut();
        let connected = document
            .get_node(node)
            .is_some_and(|node| node.flags.is_in_document());
        if !connected || !document.set_focus_to(node) {
            return false;
        }
        self.mark_changed();
        true
    }

    pub(crate) fn scroll(&mut self, node: NodeId, command: ScrollCommand) {
        let immediate = matches!(
            command,
            ScrollCommand::To { behavior, .. } | ScrollCommand::By { behavior, .. }
                if !matches!(behavior, ScrollBehavior::Smooth)
        ) && self
            .document
            .as_ref()
            .get_node(node)
            .is_some_and(|node| node.has_boxes());
        if immediate {
            self.scroll_animating |= command.animates();
            Scroller::apply(self.document.as_mut(), node, command);
        } else {
            self.pending_scrolls.push((node, command));
        }
        self.mark_changed();
    }

    pub(super) fn apply_pending_scrolls(&mut self) {
        for (node, command) in std::mem::take(&mut self.pending_scrolls) {
            self.scroll_animating |= command.animates();
            self.needs_geometry_sync = true;
            Scroller::apply(self.document.as_mut(), node, command);
        }
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

fn hoisted_layout_containers(document: &blitz_dom::BaseDocument, root: NodeId) -> Vec<NodeId> {
    let mut subtree = std::collections::HashSet::new();
    let mut containers = Vec::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        subtree.insert(id);
        if let Some(node) = document.get_node(id) {
            pending.extend(node.children.iter().copied());
        }
    }
    for id in &subtree {
        let mut container = document
            .get_node(*id)
            .and_then(|node| node.layout_parent.get());
        while let Some(node) = container.and_then(|id| document.get_node(id))
            && matches!(node.data, blitz_dom::node::NodeData::AnonymousBlock(_))
        {
            container = node.layout_parent.get();
        }
        if let Some(container) = container
            .filter(|container| !subtree.contains(container) && !containers.contains(container))
        {
            containers.push(container);
        }
    }
    containers
}
