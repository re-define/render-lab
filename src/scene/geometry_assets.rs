//! Rigid, mesh-local derived assets, shareable by any number of instances of the source mesh.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}
impl LocalBounds {
    pub fn from_mesh(mesh: &Mesh) -> Result<Self> {
        ensure!(
            !mesh.positions.is_empty(),
            "a bound needs at least one vertex"
        );
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for &p in &mesh.positions {
            let p = Vec3::from(p);
            ensure!(p.is_finite(), "non-finite source position");
            lo = lo.min(p);
            hi = hi.max(p);
        }
        Ok(Self {
            min: lo.into(),
            max: hi.into(),
        })
    }
}

/// The optional shadow mesh is in the same local space as the source mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeometryAssets {
    pub source_mesh: Id,
    pub local_bounds: Option<LocalBounds>,
    pub shadow_mesh: Option<Id>,
}

impl GeometryAssets {
    pub(crate) fn occlusion_override(&self) -> Option<Id> {
        self.shadow_mesh.filter(|&mesh| mesh != self.source_mesh)
    }

    pub(crate) fn validate(&self, world: &World) -> Result<()> {
        ensure!(
            self.local_bounds.is_some() || self.shadow_mesh.is_some(),
            "empty geometry assets"
        );
        let source = world
            .entities
            .get(&self.source_mesh)
            .and_then(|e| e.mesh.as_deref())
            .context("missing geometry asset source mesh")?;
        if let Some(b) = self.local_bounds {
            let actual = LocalBounds::from_mesh(source)?;
            let (lo, hi) = (Vec3::from(b.min), Vec3::from(b.max));
            ensure!(
                lo.is_finite()
                    && hi.is_finite()
                    && lo.cmple(actual.min.into()).all()
                    && hi.cmpge(actual.max.into()).all(),
                "authored bounds must enclose the source vertices"
            );
        }
        if let Some(proxy) = self.shadow_mesh {
            let mesh = world
                .entities
                .get(&proxy)
                .and_then(|e| e.mesh.as_ref())
                .context("missing shadow proxy mesh")?;
            ensure!(
                !mesh.indices.is_empty()
                    && mesh.indices.len().is_multiple_of(3)
                    && mesh
                        .indices
                        .iter()
                        .all(|&i| (i as usize) < mesh.positions.len())
                    && mesh.positions.iter().all(|p| Vec3::from(*p).is_finite())
                    && mesh.joints.is_empty()
                    && mesh.weights.is_empty(),
                "expected a finite rigid triangle shadow proxy"
            );
        }
        Ok(())
    }
}

impl World {
    pub(crate) fn validate_geometry_binding(
        &self,
        entity: &Entity,
        binding: Option<Id>,
    ) -> Result<()> {
        let Some(id) = binding else {
            return Ok(());
        };
        let products = self
            .entities
            .get(&id)
            .and_then(|e| e.geometry_assets.as_ref())
            .context("missing bound geometry assets")?;
        let instance = entity
            .instance
            .as_ref()
            .context("geometry binding needs an instance")?;
        ensure!(
            instance.mesh == products.source_mesh,
            "geometry assets refer to a different source mesh"
        );
        let source = self
            .entities
            .get(&instance.mesh)
            .context("missing source mesh")?;
        ensure!(
            entity.skin_binding.is_none() && source.plant.is_none() && source.hierarchy.is_none(),
            "rigid geometry assets require an ordinary rigid instance"
        );
        Ok(())
    }

    /// Update a shared asset, keeping its source identity and all instance bindings stable.
    pub fn set_geometry_assets(&mut self, id: Id, products: GeometryAssets) -> Result<()> {
        let previous = self
            .entities
            .get(&id)
            .and_then(|e| e.geometry_assets.as_ref())
            .context("missing geometry assets")?;
        ensure!(
            previous.source_mesh == products.source_mesh,
            "changing the source mesh requires a new asset"
        );
        products.validate(self)?;
        if previous.as_ref() != &products {
            self.entities
                .edit(id, EntityWrite::GeometryAssets)
                .unwrap()
                .geometry_assets = Some(Arc::new(products));
        }
        Ok(())
    }
    /// Bind or detach precomputed products. Validation precedes publication to extraction readers.
    pub fn bind_geometry_assets(&mut self, instance: Id, assets: Option<Id>) -> Result<()> {
        let entity = self
            .entities
            .get(&instance)
            .context("unknown geometry instance")?;
        self.validate_geometry_binding(entity, assets)?;
        if entity
            .instance
            .as_ref()
            .context("geometry binding needs an instance")?
            .geometry_assets
            != assets
        {
            self.entities
                .edit(instance, EntityWrite::GeometryBinding)
                .unwrap()
                .instance
                .as_mut()
                .unwrap()
                .geometry_assets = assets;
        }
        Ok(())
    }
}
