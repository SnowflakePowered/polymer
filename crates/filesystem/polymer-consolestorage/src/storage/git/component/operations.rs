use super::*;

impl ComponentHistory<'_> {
    pub(in crate::storage::git) fn history(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactRevision>, Error> {
        self.recover()?;
        let states = self.states()?;
        let contributions = self.contributions(&states)?;
        let Some(component) = contributions
            .iter()
            .find(|revision| revision.artifact.id() == id)
            .map(|revision| &revision.component.id)
        else {
            return Ok(Vec::new());
        };
        let state = states
            .iter()
            .find(|state| &state.revision.component.id == component)
            .ok_or_else(|| Error::not_found(id))?;
        Ok(self
            .revisions(state)?
            .into_iter()
            .rev()
            .map(|revision| ArtifactRevision {
                revision: revision.revision,
                artifact: ArtifactSummary::from(&revision.artifact),
            })
            .collect())
    }

    pub(in crate::storage::git) fn restore(&self, id: &str, revision: &str) -> Result<(), Error> {
        let old_tip = self.clean_tip()?;
        let before = self.states()?;
        let contributions = self.contributions(&before)?;
        let component = contributions
            .iter()
            .find(|entry| entry.artifact.id() == id)
            .map(|entry| &entry.component.id)
            .ok_or_else(|| Error::not_found(id))?;
        let index = before
            .iter()
            .position(|state| &state.revision.component.id == component)
            .ok_or_else(|| Error::not_found(id))?;
        let selected = &before[index];
        if !self
            .revisions(selected)?
            .iter()
            .any(|entry| entry.revision == revision)
        {
            return Err(Error::HistoryConflict(
                "revision is not in this component's history".into(),
            ));
        }
        let tip = gix::ObjectId::from_hex(revision.as_bytes())
            .map_err(|error| Error::git("parse component revision", error))?;
        if tip == selected.tip {
            return Ok(());
        }
        let mut after = before.clone();
        after[index] = self.state(selected.reference.clone(), tip)?;
        self.transition(old_tip, &before, &after)
    }

    fn retained_states(
        &self,
        before: &[ComponentState],
        ids: &[&str],
    ) -> Result<Vec<ComponentState>, Error> {
        let mut after = Vec::new();
        for state in before {
            let history = self.revisions(state)?;
            if let Some(revision) = self.composer()?.retained_revision(&history, ids) {
                let tip = gix::ObjectId::from_hex(revision.revision.as_bytes())
                    .map_err(|error| Error::git("parse retained component", error))?;
                after.push(self.state(state.reference.clone(), tip)?);
            }
        }
        Ok(after)
    }

    pub(in crate::storage::git) fn affected(&self, ids: &[&str]) -> Result<Vec<String>, Error> {
        self.clean_tip()?;
        let states = self.states()?;
        let revisions = self.contributions(&states)?;
        let mut removing = ids
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<BTreeSet<_>>();
        loop {
            let count = removing.len();
            let selected = removing.iter().map(String::as_str).collect::<Vec<_>>();
            let mut affected = self.composer()?.affected(&revisions, &selected);
            let after = self.retained_states(&states, &selected)?;
            let retained = self.contributions(&after)?;
            let retained_ids = retained
                .iter()
                .map(|revision| revision.artifact.id())
                .collect::<BTreeSet<_>>();
            affected.extend(
                revisions
                    .iter()
                    .filter(|revision| !retained_ids.contains(revision.artifact.id()))
                    .map(|revision| revision.artifact.id().to_owned()),
            );
            match self.composer()?.compose(&retained, &self.composer_state()?) {
                Err(Error::AffectedDepots {
                    affected: dependencies,
                }) => affected.extend(dependencies),
                Err(error) => return Err(error),
                Ok(_) => {}
            }
            removing.extend(affected);
            if removing.len() == count {
                break;
            }
        }
        Ok(removing
            .into_iter()
            .filter(|id| !ids.contains(&id.as_str()))
            .collect())
    }

    pub(in crate::storage::git) fn uninstall(&self, ids: &[&str]) -> Result<(), Error> {
        let old_tip = self.clean_tip()?;
        let affected = self.affected(ids)?;
        if !affected.is_empty() {
            return Err(Error::AffectedDepots { affected });
        }
        let before = self.states()?;
        let after = self.retained_states(&before, ids)?;
        if before
            .iter()
            .map(|state| state.tip)
            .collect::<BTreeSet<_>>()
            == after.iter().map(|state| state.tip).collect::<BTreeSet<_>>()
        {
            return Ok(());
        }
        self.transition(old_tip, &before, &after)
    }
}
