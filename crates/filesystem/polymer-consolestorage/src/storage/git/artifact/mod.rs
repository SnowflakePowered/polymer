use super::*;

mod history;
mod record;
mod tree;

pub(super) use record::*;
use tree::*;

pub(super) struct ArtifactRepository<'r> {
    repo: &'r gix::Repository,
    reference: &'r str,
}

impl<'r> ArtifactRepository<'r> {
    pub(super) fn new(repo: &'r gix::Repository, reference: &'r str) -> Self {
        Self { repo, reference }
    }

    pub(super) fn tip(&self) -> Result<Option<gix::ObjectId>, Error> {
        GitVolumeStorage::tip_in(self.repo, self.reference)
    }

    fn root(&self) -> Result<gix::Tree<'r>, Error> {
        GitVolumeStorage::root_tree(self.repo, self.tip()?)
    }

    pub(super) fn get(&self, id: &str) -> Result<Option<ArtifactTree<'r>>, Error> {
        let Some(entry) = self
            .root()?
            .lookup_entry_by_path(format!("meta/artifacts/{}", TreeKey::new(id)))
            .map_err(|error| Error::git("lookup artifact", error))?
        else {
            return Ok(None);
        };
        self.repo
            .find_tree(entry.oid())
            .map(|tree| Some(ArtifactTree(tree)))
            .map_err(|error| Error::git("read artifact tree", error))
    }

    fn trees(&self, root: &gix::Tree<'r>) -> Result<Vec<(String, ArtifactTree<'r>)>, Error> {
        let Some(artifacts) = root
            .lookup_entry_by_path("meta/artifacts")
            .map_err(|error| Error::git("lookup artifacts", error))?
        else {
            return Ok(Vec::new());
        };
        let artifacts = self
            .repo
            .find_tree(artifacts.oid())
            .map_err(|error| Error::git("read artifacts", error))?;
        let mut trees = Vec::new();
        for entry in artifacts.iter() {
            let entry = entry.map_err(|error| Error::git("decode artifact entry", error))?;
            if entry.kind() != gix::objs::tree::EntryKind::Tree {
                continue;
            }
            let tree = self
                .repo
                .find_tree(entry.oid())
                .map_err(|error| Error::git("read artifact tree", error))?;
            trees.push((entry.filename().to_string(), ArtifactTree(tree)));
        }
        Ok(trees)
    }

    pub(super) fn records(&self) -> Result<Vec<ArtifactRecord>, Error> {
        self.trees(&self.root()?)?
            .iter()
            .map(|(_, tree)| tree.record())
            .collect()
    }

    pub(super) fn summaries(&self) -> Result<Vec<ArtifactSummary>, Error> {
        self.trees(&self.root()?)?
            .iter()
            .map(|(_, tree)| tree.summary())
            .collect()
    }

    /// Metadata roots that record one of `ids` under a key other than its current
    /// [`PlatformRepository::tree_key`], left behind by earlier storage layouts.
    pub(super) fn stale_keys(
        &self,
        root: &gix::Tree<'r>,
        ids: &BTreeSet<&str>,
    ) -> Result<Vec<String>, Error> {
        let mut stale = Vec::new();
        for (key, tree) in self.trees(root)? {
            let Some(id) = tree.value("id")? else {
                continue;
            };
            let id = String::from_utf8_lossy(&id).trim().to_owned();
            if ids.contains(id.as_str()) && key != TreeKey::new(&id).to_string() {
                stale.push(format!("meta/artifacts/{key}"));
            }
        }
        Ok(stale)
    }
}
