//! Console authoring and inspection of one actor’s physical limb configuration.
use super::*;
use crate::physics::articulation::BendRange;

const NAMES: [&str; 3] = ["arm", "right-leg", "left-leg"];
impl PhysicalPose {
    pub fn describe(layer: Option<&Self>) -> String {
        let default = Self::default();
        let value = layer.unwrap_or(&default);
        let mut lines = vec![format!("Physical limb weight {:.2}", value.weight)];
        for (i, name) in NAMES.into_iter().enumerate() {
            let drive = value.drives[i];
            let range = drive.bend.map_or_else(
                || "off".into(),
                |r| format!("{}..{} deg", r.min_degrees, r.max_degrees),
            );
            let bend = value
                .limbs
                .as_ref()
                .and_then(|limbs| limbs[i].flexion_degrees())
                .map_or_else(|| "inactive".into(), |v| format!("{v:.2} deg"));
            lines.push(format!(
                "{name} · solved {bend} · limit {range} · compliance {} m/N · damping {} /s",
                drive.compliance, drive.damping
            ));
        }
        lines.join("\n")
    }
    pub fn edit(layer: &mut Option<Arc<Self>>, limb: &str, args: &[&str]) -> Result<()> {
        let i = NAMES
            .iter()
            .position(|name| *name == limb)
            .context("choose arm, right-leg or left-leg")?;
        let mut drive = layer
            .as_ref()
            .map_or_else(LimbDrive::default, |v| v.drives[i]);
        match args {
            ["limit", "off"] => drive.bend = None,
            ["limit", min, max] => {
                drive.bend = Some(BendRange {
                    min_degrees: min.parse()?,
                    max_degrees: max.parse()?,
                })
            }
            ["drive", compliance, damping] => {
                drive.compliance = compliance.parse()?;
                drive.damping = damping.parse()?;
            }
            _ => anyhow::bail!(
                "contact limbs LIMB limit MIN MAX | limit off | drive COMPLIANCE DAMPING"
            ),
        }
        drive.validate()?;
        let current = Arc::make_mut(layer.get_or_insert_with(Default::default));
        current.drives[i] = drive;
        if !current.active() && default_drives(&current.drives) {
            *layer = None;
        }
        Ok(())
    }

    pub fn configure(layer: &mut Option<Arc<Self>>, arg: &str) -> Result<()> {
        let weight = match arg {
            "off" => 0.,
            "on" => 1.,
            other => other.parse()?,
        };
        ensure!(
            (0.0..=1.0).contains(&weight),
            "limb blend weight must be in 0..=1"
        );
        if let Some(current) = layer {
            let state = Arc::make_mut(current);
            state.weight = weight;
            if weight == 0. {
                state.reset();
                if default_drives(&state.drives) {
                    *layer = None;
                }
            }
        } else if weight > 0. {
            *layer = Some(Arc::new(Self {
                weight,
                ..Default::default()
            }));
        }
        Ok(())
    }
}
