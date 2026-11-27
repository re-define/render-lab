//! Bind the two hierarchy paths once. Simulation reads only the transforms between
//! the actor's parent and the fixture's parent, plus the fixture itself.
use super::*;

pub(super) struct Fixture {
    pub id: Id,
    actor_parents: Vec<Id>,
    fixture_parents: Vec<Id>,
}

impl Fixture {
    pub fn bind(world: &World, actor: Id, id: Id) -> Result<Self> {
        let ancestors = |id: Id| -> Result<Vec<Id>> {
            let mut path = Vec::new();
            let mut at = world
                .entities
                .get(&id)
                .context("missing interaction fixture or actor")?
                .parent;
            while let Some(parent) = at {
                ensure!(
                    path.len() < world.entities.len(),
                    "cyclic interaction fixture hierarchy"
                );
                path.push(parent);
                at = world
                    .entities
                    .get(&parent)
                    .context("missing fixture ancestor")?
                    .parent;
            }
            path.reverse();
            Ok(path)
        };
        let mut actor_parents = ancestors(actor)?;
        let mut fixture_parents = ancestors(id)?;
        let shared = actor_parents
            .iter()
            .zip(&fixture_parents)
            .take_while(|(a, b)| a == b)
            .count();
        actor_parents.drain(..shared);
        fixture_parents.drain(..shared);
        Ok(Self {
            id,
            actor_parents,
            fixture_parents,
        })
    }
    pub fn reads(&self) -> impl Iterator<Item = Id> + '_ {
        self.actor_parents
            .iter()
            .chain(&self.fixture_parents)
            .copied()
            .chain(std::iter::once(self.id))
    }
    pub fn parent_to_actor(&self, mut local: impl FnMut(Id) -> Result<Mat4>) -> Result<Mat4> {
        let mut actor = Mat4::IDENTITY;
        for &id in &self.actor_parents {
            actor *= local(id)?;
        }
        ensure!(
            actor.determinant().abs() > 1e-12,
            "singular character parent frame"
        );
        let mut transform = actor.inverse();
        for &id in &self.fixture_parents {
            transform *= local(id)?;
        }
        Ok(transform)
    }
    pub fn relative(&self, mut local: impl FnMut(Id) -> Result<Mat4>) -> Result<Mat4> {
        if self.actor_parents.is_empty() && self.fixture_parents.is_empty() {
            return local(self.id);
        }
        Ok(self.parent_to_actor(&mut local)? * local(self.id)?)
    }
}
