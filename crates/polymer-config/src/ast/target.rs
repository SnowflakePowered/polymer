use std::collections::HashMap;

use petgraph::{
    Direction::Outgoing,
    algo::toposort,
    graphmap::DiGraphMap,
};

use crate::{
    arena::Handle,
    error::{Error, Result},
};

use super::{ConfigurationDefinition, OwnedTargetPath};

#[derive(Clone, Debug)]
pub struct TargetDefinition {
    pub path: OwnedTargetPath,
    pub output: Option<TargetOutputDefinition>,
    pub repeat: bool,
    pub parent: Option<Handle<TargetDefinition>>,
    pub additional_parents: Box<[OwnedTargetPath]>,
}

#[derive(Clone, Debug)]
pub struct TargetOutputDefinition {
    pub format: Box<str>,
    pub path: Box<str>,
}

/// Resolved target relationships that are independent of any configuration instance.
#[derive(Debug)]
pub struct TargetGraph {
    target_by_path: HashMap<OwnedTargetPath, Handle<TargetDefinition>>,
    graph: DiGraphMap<Handle<TargetDefinition>, ()>,
    output_roots: Box<[Handle<TargetDefinition>]>,
}

impl ConfigurationDefinition {
    /// Returns the compiled serialization-target graph shared by every instance.
    pub fn target_graph(&self) -> Result<&TargetGraph> {
        if let Some(graph) = self.target_graph.get() {
            return Ok(graph);
        }

        let graph = TargetGraph::compile(self)?;
        let _ = self.target_graph.set(graph);
        Ok(self
            .target_graph
            .get()
            .expect("the target graph was initialized"))
    }
}

impl TargetGraph {
    fn compile(definition: &ConfigurationDefinition) -> Result<Self> {
        let target_by_path = definition
            .targets
            .iter()
            .map(|(handle, target)| (target.path.clone(), handle))
            .collect::<HashMap<_, _>>();
        let mut graph: DiGraphMap<Handle<TargetDefinition>, ()> =
            DiGraphMap::with_capacity(definition.targets.len(), 0);
        for (handle, target) in definition.targets.iter() {
            graph.add_node(handle);
            if let Some(parent) = target.parent {
                graph.add_edge(parent, handle, ());
            }
        }

        for (handle, target) in definition.targets.iter() {
            for parent in &target.additional_parents {
                let parent_handle = target_by_path
                    .get(parent.as_str())
                    .copied()
                    .ok_or_else(|| Error::UnknownTarget(parent.as_str().into()))?;
                graph.add_edge(parent_handle, handle, ());
            }
        }

        toposort(&graph, None).map_err(|cycle| {
            let handle = cycle.node_id();
            Error::TargetCycle(definition.targets[handle].path.as_str().into())
        })?;
        let output_roots = definition
            .targets
            .iter()
            .filter_map(|(handle, target)| {
                (target.parent.is_none() && target.output.is_some()).then_some(handle)
            })
            .collect::<Box<[_]>>();
        Ok(Self {
            target_by_path,
            graph,
            output_roots,
        })
    }

    pub fn resolve(&self, path: &str) -> Option<Handle<TargetDefinition>> {
        self.target_by_path.get(path).copied()
    }

    pub fn children(
        &self,
        target: Handle<TargetDefinition>,
    ) -> impl Iterator<Item = Handle<TargetDefinition>> + '_ {
        self.graph.neighbors_directed(target, Outgoing)
    }

    pub fn output_roots(&self) -> &[Handle<TargetDefinition>] {
        &self.output_roots
    }
}
