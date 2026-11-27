use super::*;
#[derive(Clone, Serialize, Deserialize)]
pub struct Cell {
    pub first: Id,
    pub count: usize,
    pub center: Vec3,
    pub file: PathBuf,
    /// Simulation changes survive deactivation and scene saves.
    pub save_state: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Layout {
    pub cells: Arc<[Cell]>,
    pub activate: f32,
    pub preload: f32,
    pub unload: f32,
    #[serde(skip)]
    pub(crate) base: PathBuf,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) overrides: BTreeMap<Id, Checkpoint>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Actor {
    name: String,
    local: Mat4,
    instance: Option<Instance>,
    #[serde(default)]
    components: ComponentSet,
}
impl From<Actor> for Entity {
    fn from(v: Actor) -> Self {
        Self {
            name: v.name,
            local: v.local,
            instance: v.instance,
            components: v.components,
            ..Default::default()
        }
    }
}
impl From<Entity> for Actor {
    fn from(v: Entity) -> Self {
        Self {
            name: v.name,
            local: v.local,
            instance: v.instance,
            components: v.components,
        }
    }
}
impl Layout {
    pub fn new(
        base: PathBuf,
        cells: Vec<Cell>,
        activate: f32,
        preload: f32,
        unload: f32,
    ) -> Result<Self> {
        let layout = Self {
            base,
            cells: cells.into(),
            activate,
            preload,
            unload,
            overrides: Default::default(),
        };
        layout.validate()?;
        Ok(layout)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.unload.is_finite()
                && self.activate > 0.
                && self.activate < self.preload
                && self.preload < self.unload,
            "invalid cell radii"
        );
        let mut ranges = Vec::new();
        for c in self.cells.iter() {
            ensure!(
                c.center.is_finite()
                    && (1..=FRAME_ROWS).contains(&c.count)
                    && c.first > 0
                    && !c.file.as_os_str().is_empty()
                    && c.file
                        .components()
                        .all(|v| matches!(v, Component::Normal(_))),
                "invalid world cell"
            );
            ranges.push((
                c.first,
                c.first
                    .checked_add(c.count as u64)
                    .context("cell ID overflow")?,
            ));
            if let Some(rows) = self.overrides.get(&c.first) {
                ensure!(rows.count() == c.count, "invalid saved cell")
            }
        }
        ranges.sort_unstable();
        ensure!(
            ranges.windows(2).all(|r| r[0].1 <= r[1].0),
            "overlapping cell IDs"
        );
        Ok(())
    }
    pub(crate) fn reserve_after(&self, id: Id) -> Id {
        let mut next = id;
        while let Some(c) = self
            .cells
            .iter()
            .find(|c| (c.first..c.first + c.count as u64).contains(&next))
        {
            next = c.first + c.count as u64;
        }
        next
    }
    /// Portable saves publish immutable cell files before publishing the root archive.
    pub(crate) fn export(&self, dir: &Path) -> Result<Self> {
        let mut result = self.clone();
        let folder = dir.join("cells");
        std::fs::create_dir_all(&folder)?;
        for c in Arc::make_mut(&mut result.cells) {
            let data = if let Some(v) = result.overrides.remove(&c.first) {
                v.bytes()?
            } else {
                read_bounded(&self.base.join(&c.file))?
            };
            let key = crate::assets::key_name(&crate::assets::content_key("world-cell", 1, &data));
            c.file = PathBuf::from("cells").join(format!("{key}.json"));
            let path = dir.join(&c.file);
            if !path.exists() {
                use std::io::Write;
                let mut f = tempfile::NamedTempFile::new_in(&folder)?;
                f.write_all(&data)?;
                if let Err(e) = f.persist_noclobber(&path)
                    && !path.is_file()
                {
                    return Err(e.into());
                }
            }
        }
        result.base = dir.to_owned();
        Ok(result)
    }
}
pub(super) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= FILE_BYTES, "cell file too large");
    Ok(bytes)
}
pub(super) fn validate_rows(rows: &[Actor], cell: &Cell, world: &World) -> Result<()> {
    ensure!(rows.len() == cell.count, "cell row count mismatch");
    for row in rows {
        ensure!(row.local.is_finite(), "non-finite cell transform");
        if let Some(i) = &row.instance {
            ensure!(
                world
                    .entities
                    .get(&i.mesh)
                    .is_some_and(|r| r.mesh.is_some())
                    && i.material.is_none_or(|id| world
                        .entities
                        .get(&id)
                        .is_some_and(|r| r.material.is_some()))
                    && i.geometry_assets.is_none_or(|id| world
                        .entities
                        .get(&id)
                        .is_some_and(|r| r.geometry_assets.is_some())),
                "missing persistent cell asset"
            )
        }
    }
    Ok(())
}
pub(super) type Rows = Arc<Vec<Actor>>;
#[derive(Clone)]
pub(super) enum Checkpoint {
    Memory(Rows),
    File(Arc<tempfile::TempPath>, usize),
}
impl Checkpoint {
    fn count(&self) -> usize {
        match self {
            Self::Memory(v) => v.len(),
            Self::File(_, n) => *n,
        }
    }
    fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(v) => Ok(serde_json::to_vec(v)?),
            Self::File(p, _) => read_bounded(p),
        }
    }
}
impl Serialize for Checkpoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Memory(v) => v.serialize(s),
            Self::File(p, _) => {
                let bytes = read_bounded(p).map_err(serde::ser::Error::custom)?;
                let rows: Vec<Actor> =
                    serde_json::from_slice(&bytes).map_err(serde::ser::Error::custom)?;
                rows.serialize(s)
            }
        }
    }
}
impl<'de> Deserialize<'de> for Checkpoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::Memory(Rows::deserialize(d)?))
    }
}
pub(super) fn checkpoint(rows: &Rows) -> Result<tempfile::TempPath> {
    let mut f = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut f, rows)?;
    ensure!(
        f.as_file().metadata()?.len() <= FILE_BYTES,
        "cell checkpoint exceeds size limit"
    );
    Ok(f.into_temp_path())
}
