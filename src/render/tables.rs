//! Resolve ordinary and clustered draws from the same stable transform and shared material slots.
use super::*;
use crate::rdg::{PassPhase, rows::Rows};
use anyhow::Context;
use std::sync::Weak;

pub(super) struct MaterialTable {
    layout: Weak<scene::GeometryRoutes>,
    programs: Weak<MaterialPrograms>,
    sources: Weak<BTreeMap<MaterialId, Arc<Material>>>,
    rows: Rows,
}

#[derive(Default)]
pub(super) struct OrdinarySlots {
    // [model slot, material slot, flags, override index], followed by geometry addresses.
    draws: Vec<[[u32; 4]; 2]>,
    overrides: Vec<[u32; 16]>,
}

impl Targets {
    fn material_rows(&mut self, s: &Snapshot) -> Result<&Rows> {
        if self.shared_materials.is_none() {
            let mut cache = self.atlas.materials.lock().unwrap();
            let layout = Arc::downgrade(&s.scene.routes);
            let sources = Arc::downgrade(&s.scene.materials);
            let programs = Arc::downgrade(&s.materials);
            if !cache.as_ref().is_some_and(|c| {
                c.layout.ptr_eq(&layout)
                    && c.sources.ptr_eq(&sources)
                    && c.programs.ptr_eq(&programs)
            }) {
                let ids = &s.scene.routes.materials;
                let mut rows = cache
                    .as_ref()
                    .filter(|c| c.rows.bytes() == ids.len() * 64)
                    .map_or_else(
                        || Rows::new(&vec![[0u32; 16]; ids.len()]),
                        |c| c.rows.clone(),
                    );
                let fallback = Material::default();
                let prior = cache
                    .as_ref()
                    .filter(|c| c.layout.ptr_eq(&layout) && c.programs.ptr_eq(&programs))
                    .and_then(|c| c.sources.upgrade());
                for (slot, id) in ids.iter().enumerate() {
                    let m = id.and_then(|id| s.scene.materials.get(&id));
                    if let Some(old) = &prior
                        && id.is_none_or(|id| {
                            old.get(&id).zip(m).is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                        })
                    {
                        continue;
                    }
                    let value = self.material_value(s, m.map_or(&fallback, Arc::as_ref))?;
                    let row: [u32; 16] =
                        bytemuck::pod_read_unaligned(&bytemuck::bytes_of(&value)[64..]);
                    rows.set(slot, &row);
                }
                *cache = Some(MaterialTable {
                    layout,
                    programs,
                    sources,
                    rows,
                });
            }
            self.shared_materials = Some(cache.as_ref().unwrap().rows.clone());
        }
        Ok(self.shared_materials.as_ref().unwrap())
    }
    pub(crate) fn scene_tables(
        &mut self,
        g: &mut FrameGraph,
        s: &Snapshot,
    ) -> Result<(Resource, Resource)> {
        if let Some(tables) = self.tables {
            return Ok(tables);
        }
        let materials = g.table("Shared material slots", self.material_rows(s)?);
        let models = g.table("Current instance slots", &s.scene.models);
        self.tables = Some((models, materials));
        Ok((models, materials))
    }
    pub(crate) fn register_scene(
        &mut self,
        s: &Snapshot,
        i: &RenderInstance,
        mut instance: SurfaceInstance,
        geometry: Geometry,
        motion: Option<(motion::SurfaceHistory<'_>, MotionSource)>,
    ) -> Result<u32> {
        if self.full_upload {
            return self.register(instance, geometry, motion);
        }
        let geom = self.geometry_addresses(geometry)?;
        let slot = i.material.map_or(0, |m| s.scene.routes.material_slots[&m]);
        let model_slot = if instance.model == Mat4::IDENTITY.to_cols_array() && i.palette.is_some()
        {
            u32::MAX // The skinning pass already produced world-space vertices.
        } else {
            ensure!(
                instance.model == i.model.to_cols_array(),
                "ordinary surface model disagrees with ECS"
            );
            s.scene.routes.model_slots[&i.id].try_into()?
        };
        let flags = std::mem::take(&mut instance.tex[3]);
        let payload = &bytemuck::bytes_of(&instance)[64..];
        let extra = if payload == self.material_rows(s)?.record(slot) {
            u32::MAX
        } else {
            let extra = self.ordinary.overrides.len().try_into()?;
            self.ordinary
                .overrides
                .push(bytemuck::pod_read_unaligned(payload));
            extra
        };
        let id = u32::try_from(self.ordinary.draws.len())?
            .checked_add(self.prefix.as_ref().map_or(0, |p| p.count))
            .context("surface ID overflow")?;
        if let Some(table) = &mut self.motion {
            let (history, source) = motion.context("missing temporal surface registration")?;
            table.register(history, source)?;
        }
        self.ordinary
            .draws
            .push([[model_slot, slot.try_into()?, flags, extra], geom]);
        Ok(id)
    }
    pub(super) fn shading_table(&mut self, g: &mut FrameGraph, s: &Snapshot) -> Result<Resource> {
        if self.ordinary.draws.is_empty() {
            return Ok(visibility::surface_table(
                g,
                "Surface instances",
                &self.items,
                self.prefix.as_ref().map(|p| (p.shade, p.count)),
            ));
        }
        ensure!(self.items.is_empty(), "surface registration modes overlap");
        let (models, materials) = self.scene_tables(g, s)?;
        let slots = g.table("Ordinary draw slots", &Rows::new(&self.ordinary.draws));
        let overrides = g.table(
            "Ordinary material overrides",
            &Rows::new(&self.ordinary.overrides),
        );
        let count = u32::try_from(self.ordinary.draws.len())?;
        let (output, base) = self
            .prefix
            .as_ref()
            .map(|p| (p.shade, p.count))
            .unwrap_or_else(|| {
                let r = g.resource(
                    "Surface instances",
                    Storage::Buffer {
                        bytes: count as usize * size_of::<ShadeItem>(),
                        initial: None,
                    },
                );
                g.prefer_local_buffer(r);
                (r, 0)
            });
        let access = if base == 0 {
            Access::ComputeWrite
        } else {
            Access::ComputeReadWrite
        };
        let groups = count.div_ceil(64);
        g.pass_in(
            PassPhase::Bins,
            "Resolve ordinary scene slots",
            Op::Compute {
                program: Dispatch {
                    shader: Shader::Compute(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/ordinary_slots.comp.spv"
                    ))),
                    bindings: vec![
                        (slots, Access::ComputeRead),
                        (models, Access::ComputeRead),
                        (materials, Access::ComputeRead),
                        (overrides, Access::ComputeRead),
                        (output, access),
                    ],
                    push: bytemuck::bytes_of(&[base, count]).to_vec(),
                },
                groups: [groups.min(32768), groups.div_ceil(32768), 1],
            },
        );
        Ok(output)
    }
}
