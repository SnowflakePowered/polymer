use crate::DependencyEdge;
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

/// Iterative Kosaraju traversal avoids recursion on application-controlled graphs.
pub(super) struct Components<I> {
    pub index: HashMap<I, usize>,
    pub members: Vec<Vec<I>>,
}
impl<I> Default for Components<I> {
    fn default() -> Self {
        Self {
            index: HashMap::new(),
            members: Vec::new(),
        }
    }
}
impl<I: Copy + Eq + Hash> Components<I> {
    pub fn new(edges: &HashMap<I, Vec<DependencyEdge<I>>>) -> Self {
        let mut visited = HashSet::new();
        let mut finished = Vec::new();
        let mut reversed = HashMap::<I, Vec<I>>::new();
        for (&id, inputs) in edges {
            for edge in inputs {
                reversed.entry(edge.source).or_default().push(id);
            }
            Self::finish(id, edges, &mut visited, &mut finished);
        }
        let mut result = Self::default();
        for id in finished.into_iter().rev() {
            if result.index.contains_key(&id) {
                continue;
            }
            let component = result.members.len();
            let mut members = Vec::new();
            let mut pending = vec![id];
            while let Some(member) = pending.pop() {
                if result.index.contains_key(&member) {
                    continue;
                }
                result.index.insert(member, component);
                members.push(member);
                pending.extend(reversed.get(&member).into_iter().flatten().copied());
            }
            result.members.push(members);
        }
        result
    }
    fn finish(
        id: I,
        edges: &HashMap<I, Vec<DependencyEdge<I>>>,
        visited: &mut HashSet<I>,
        finished: &mut Vec<I>,
    ) {
        let mut stack = vec![(id, false)];
        while let Some((id, exiting)) = stack.pop() {
            if exiting {
                finished.push(id);
                continue;
            }
            if !visited.insert(id) {
                continue;
            }
            stack.push((id, true));
            stack.extend(
                edges
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .map(|edge| (edge.source, false)),
            );
        }
    }
}
