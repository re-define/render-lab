//! Procedural groom study. GPU guide dynamics shared by all follow fibres.
mod gpu;
mod model;
mod timeline;
pub(super) use gpu::SNAPSHOT;
pub(crate) use gpu::{STRETCH, prepare};
use timeline::Dynamics;
mod solver;
#[cfg(test)]
mod tests;
use super::assembly::{Primary, Registration};
use crate::{
    engine::{
        BoundSystems, FixedDomain, Schedule, SimulationData, SimulationSystem, SystemAccess,
        SystemContext, TransformTargets,
    },
    render::Camera,
    scene::{Id, World},
};
use anyhow::{Context, Result, bail, ensure};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use solver::{Controls, GUIDES, POINTS};
use std::{sync::Arc, time::Duration};

const SETTING: &str = "lab.hair.groom";
const CONTROL: &str = "lab.hair.controls";
const STATE: &str = "lab.hair.dynamics";
const HZ: f64 = 120.;

pub(super) const MODULE: Registration = Registration {
    key: SETTING,
    root_writers: &[],
    primary: Primary::Simulation,
    restore: Some(|w| {
        w.hydrate_components::<Mount>(SETTING)?;
        w.hydrate_components::<Controls>(CONTROL)?;
        w.hydrate_components::<Dynamics>(STATE)
    }),
    install: |w, a| {
        install(w, &mut a.schedule)?;
        a.camera = Some(Camera {
            target: Vec3::new(0.04, 1.11, 0.),
            distance: 1.82,
            yaw: 1.05,
            pitch: 0.07,
        });
        Ok(())
    },
};
pub fn scene() -> Result<World> {
    model::build()
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Mount {
    head: Id,
    skin: Id,
    display: Id,
    followers: [Id; 4],
}
impl Controls {
    fn validate(self) -> Result<()> {
        ensure!(
            (-20. ..=20.).contains(&self.wind) && (0. ..=1.).contains(&self.stiffness),
            "wind must be in -20..20 and stiffness in 0..1"
        );
        Ok(())
    }
}
struct Binding {
    owner: Id,
    mount: Mount,
}
impl Binding {
    fn new(w: &World, owner: Id) -> Result<Self> {
        let mount = *w
            .typed_component::<Mount>(owner, SETTING)?
            .context("missing groom mount")?;
        let skin = w
            .entities
            .get(&mount.skin)
            .and_then(|e| e.skin.as_ref())
            .context("missing groom skin")?;
        ensure!(
            skin.joints.len() == GUIDES * POINTS
                && !skin.joints.contains(&mount.head)
                && skin
                    .joints
                    .iter()
                    .chain([&mount.head])
                    .all(|id| w.entities.get(id).is_some_and(|e| e.parent == Some(owner))),
            "invalid groom joint layout"
        );
        w.typed_component::<Controls>(owner, CONTROL)?
            .context("missing groom controls")?
            .validate()?;
        w.typed_component::<Dynamics>(owner, STATE)?
            .context("missing groom checkpoint")?
            .validate()?;
        Ok(Self { owner, mount })
    }
}
struct Simulate {
    bindings: Arc<Vec<Binding>>,
}
fn install(w: &World, schedule: &mut Schedule) -> Result<()> {
    let mut systems = BoundSystems::default();
    systems.register(move |bindings: &Arc<Vec<Binding>>| Simulate {
        bindings: bindings.clone(),
    });
    systems.install(
        schedule,
        FixedDomain {
            name: "hair",
            step: Duration::from_secs_f64(1. / HZ),
            max_steps: 16,
        },
        w,
        SETTING,
        |w| {
            let bindings = w
                .entities
                .component_owners(SETTING)
                .iter()
                .map(|&id| Binding::new(w, id))
                .collect::<Result<Vec<_>>>()?;
            let version = w.binding_version(SETTING);
            Ok((
                Arc::new(bindings),
                Box::new(move |w| Ok(w.binding_version(SETTING) == version)),
            ))
        },
    )
}
impl SimulationSystem for Simulate {
    fn access(&self) -> SystemAccess {
        use SimulationData::{Component, LocalTransformsById};
        SystemAccess {
            name: "Groom fixed clock and head mount",
            reads: &[Component(CONTROL), Component(STATE)],
            writes: &[Component(STATE), LocalTransformsById],
        }
    }
    fn bound_transforms(&self) -> Option<TransformTargets> {
        Some(TransformTargets {
            reads: vec![],
            writes: self.bindings.iter().map(|b| b.mount.head).collect(),
        })
    }
    fn run(&self, cx: &mut SystemContext<'_>, dt: f32) -> Result<()> {
        for b in self.bindings.iter() {
            let c = *cx
                .component::<Controls>(b.owner, CONTROL)?
                .context("missing groom controls")?;
            c.validate()?;
            let mut state = cx
                .component::<Dynamics>(b.owner, STATE)?
                .context("missing groom checkpoint")?
                .into_owned();
            if state.advance(c, dt)? {
                cx.set_pose([(
                    b.mount.head,
                    solver::head_frame(state.phase_at(state.tick) as f32),
                )])?;
            }
            cx.set_bound_state(b.owner, STATE, state)?;
        }
        Ok(())
    }
}

pub(crate) fn command(w: &mut World, arg: &str) -> Result<String> {
    let id = w
        .first_raw_component(SETTING)
        .context("open the hair experiment first")?
        .0;
    let mut c = *w
        .typed_component::<Controls>(id, CONTROL)?
        .context("missing groom controls")?;
    match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
        [] | ["status"] => {
            let s = w
                .typed_component::<Dynamics>(id, STATE)?
                .context("missing groom dynamics")?;
            return Ok(format!(
                "GPU XPBD · {GUIDES} guides × {POINTS} points · {} fibres · {:.2} s · wind {:.1} m/s² · stiffness {:.2} · head {}",
                GUIDES * model::FOLLOWERS,
                s.time_at(s.tick),
                c.wind,
                c.stiffness,
                c.motion
            ));
        }
        ["view", view @ ("guides" | "strands")] => {
            let mount = *w
                .typed_component::<Mount>(id, SETTING)?
                .context("missing groom mount")?;
            let guides = *view == "guides";
            let instances = std::iter::once((mount.display, !guides))
                .chain(mount.followers.map(|id| (id, guides)));
            ensure!(
                instances
                    .clone()
                    .all(|(id, _)| w.entities.get(&id).is_some_and(
                        |e| e.instance.is_some() && e.skin_binding == Some(mount.skin)
                    )),
                "missing groom display mesh"
            );
            for (id, hide) in instances {
                let key = super::composition::HIDDEN;
                if hide {
                    w.set_setting(id, key, true.into());
                } else {
                    w.remove_typed_component(id, key);
                }
            }
            return Ok(format!("Groom view {view}"));
        }
        ["wind", v] => c.wind = v.parse()?,
        ["stiffness", v] => c.stiffness = v.parse()?,
        ["head", v @ ("on" | "off")] => c.motion = *v == "on",
        ["reset"] => {
            let b = Binding::new(w, id)?;
            let state = Dynamics::default();
            w.set_local(b.mount.head, Mat4::IDENTITY)?;
            w.set_typed_component(id, STATE, state);
            return Ok("Groom restored to its rest pose".into());
        }
        _ => bail!(
            "hair status | wind -20..20 | stiffness 0..1 | head on/off | view guides/strands | reset"
        ),
    }
    c.validate()?;
    w.set_typed_component(id, CONTROL, c);
    Ok(format!("Hair {arg}"))
}
