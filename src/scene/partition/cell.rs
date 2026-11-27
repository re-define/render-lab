use super::*;
#[derive(Clone, Serialize, Deserialize)]
pub struct Cell {
    pub first: Id,
    pub count: usize,
    pub center: Vec3,
    /// Asset key includes its content version. Zero denotes the pinned, immediately usable root.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, u8>,
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
    /// Mobile actors keep their semantic ID after leaving the authored ID range.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) roaming: BTreeMap<Id, Mobile>,
    #[serde(skip)]
    pub(super) exports: Arc<std::sync::Mutex<archive::Cache>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mobile {
    pub cell: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip)]
    pub checkpoint: Option<Checkpoint>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
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
        let mut layout = Self {
            base,
            cells: cells.into(),
            activate,
            preload,
            unload,
            overrides: Default::default(),
            roaming: Default::default(),
            exports: Default::default(),
        };
        layout.validate()?;
        layout.prepare_batches()?;
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
                    && c.count > 0
                    && c.assets.keys().all(|key| !key.is_empty())
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
        for (&id, mobile) in &self.roaming {
            let preceding = ranges.partition_point(|&(first, _)| first <= id);
            ensure!(
                ranges
                    .binary_search_by_key(&mobile.cell, |&(first, _)| first)
                    .is_ok()
                    && preceding > 0
                    && id < ranges[preceding - 1].1
                    && mobile
                        .file
                        .as_ref()
                        .is_none_or(|f| !f.as_os_str().is_empty()
                            && f.components()
                                .all(|part| matches!(part, Component::Normal(_)))),
                "invalid mobile partition actor {id}"
            );
        }
        Ok(())
    }
    /// Cook independent root actors into atomic scheduling batches at authoring/load time.
    /// A source cell may contain many batches at the same location with identical dependencies.
    pub(crate) fn prepare_batches(&mut self) -> Result<()> {
        if self.cells.iter().all(|c| c.count <= BATCH_ROWS) {
            return Ok(());
        }
        let mut batches = Vec::new();
        for cell in self.cells.iter() {
            if cell.count <= BATCH_ROWS {
                batches.push(cell.clone());
                continue;
            }
            let rows: Vec<Actor> =
                serde_json::from_slice(&read_bounded(&self.base.join(&cell.file))?)?;
            ensure!(
                rows.len() == cell.count && !self.overrides.contains_key(&cell.first),
                "invalid uncooked partition cell"
            );
            for (index, rows) in rows.chunks(BATCH_ROWS).enumerate() {
                let mut batch = cell.clone();
                batch.count = rows.len();
                batch.first += (index * BATCH_ROWS) as u64;
                batch.file = publish(&self.base, &serde_json::to_vec(rows)?)?;
                batches.push(batch);
            }
        }
        self.cells = batches.into();
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
        for c in Arc::make_mut(&mut result.cells) {
            c.file = archive::export(
                &self.exports,
                archive::Owner::Cell(c.first),
                dir,
                &self.base,
                Some(&c.file),
                result.overrides.remove(&c.first).as_ref(),
            )?;
        }
        for (&id, mobile) in &mut result.roaming {
            if mobile.checkpoint.is_some() || mobile.file.is_some() {
                mobile.file = Some(archive::export(
                    &self.exports,
                    archive::Owner::Mobile(id),
                    dir,
                    &self.base,
                    mobile.file.as_deref(),
                    mobile.checkpoint.as_ref(),
                )?);
                mobile.checkpoint = None;
            }
        }
        result.base = dir.to_owned();
        Ok(result)
    }
}
pub(super) fn publish(dir: &Path, data: &[u8]) -> Result<PathBuf> {
    use std::io::Write;
    let folder = dir.join("cells");
    std::fs::create_dir_all(&folder)?;
    let key = crate::assets::key_name(&crate::assets::content_key("world-cell", 1, data));
    let file = PathBuf::from("cells").join(format!("{key}.json"));
    let path = dir.join(&file);
    if !path.is_file() {
        let mut out = tempfile::NamedTempFile::new_in(folder)?;
        out.write_all(data)?;
        if let Err(e) = out.persist_noclobber(&path)
            && !path.is_file()
        {
            return Err(e.into());
        }
    }
    Ok(file)
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
    validate_actors(rows, world)
}
pub(super) fn validate_actors(rows: &[Actor], world: &World) -> Result<()> {
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
pub(super) fn validate_leaf(id: Id, e: &Entity) -> Result<()> {
    ensure!(
        e.parent.is_none()
            && e.mesh.is_none()
            && e.material.is_none()
            && e.light.is_none()
            && e.skin.is_none()
            && e.skin_binding.is_none()
            && e.texture.is_none()
            && e.animation.is_none()
            && e.plant.is_none()
            && e.geometry_assets.is_none()
            && e.gaussian_payload.is_none()
            && e.meshlets.is_none()
            && e.hierarchy.is_none()
            && e.topology.is_none(),
        "cannot checkpoint actor {id} after a change to its cell schema"
    );
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
    pub(super) fn bytes(&self) -> Result<Vec<u8>> {
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
