//! Level layout: walk a level's EBX data and collect every placed mesh with
//! its world transform.
//!
//! How a Battlefield 1 level is linked (seen in the Verdun data):
//! - `LevelData` / `SubWorldData` / `WorldPartData` / `LayerData` /
//!   `SpatialPrefabBlueprint` have an `Objects` list.
//! - Reference objects (`ReferenceObjectData` and subtypes) have `Blueprint`
//!   (a link) and `BlueprintTransform`. `SubWorldReferenceObjectData` names its
//!   target by `BundleName` instead.
//! - `ObjectBlueprint` has one `Object` (an entity).
//! - An entity with a `Mesh` link is a visible model, placed by its `Transform`.
//! - `StaticModelGroupEntityData` has `MemberDatas`; each member has a
//!   `MeshAsset` and a list of `InstanceTransforms`.
//!
//! Transforms: `LinearTransform` has right/up/forward/trans vectors. A point
//! maps to x*right + y*up + z*forward + trans, so they are the columns of an
//! affine matrix. A child's world transform is parent * child.

pub mod havok;

use anyhow::{Context, Result};
use fb_ebx::{format_guid, Complex, Guid, Partition, Pointer, Value};
use fb_vfs::dbobject::DbValue;
use fb_vfs::payload::BlockStats;
use fb_vfs::superbundle::{read_entry, EntryKind, Superbundle};
use fb_vfs::GameFs;
use glam::{Affine3A, Vec3A};
use std::collections::{BTreeMap, HashMap, HashSet};

/// All EBX partitions of one or more superbundles, indexed by GUID and name.
pub struct World {
    pub partitions: Vec<Partition>,
    /// Lower-case asset name of each partition (from the bundle).
    pub names: Vec<String>,
    by_guid: HashMap<Guid, usize>,
    by_name: HashMap<String, usize>,
    /// Res entries from the bundles, by resource id.
    pub res: HashMap<u64, DbValue>,
    /// Res entries by lower-case name (some, like the shader database, have no usable rid).
    res_named: HashMap<String, DbValue>,
    /// Chunk entries from the bundles, by chunk id.
    pub chunks: HashMap<Guid, DbValue>,
    pub parse_failures: usize,
}

impl World {
    pub fn load(fs: &GameFs, superbundles: &[&str]) -> Result<World> {
        let mut world =
            World {
            partitions: vec![],
            names: vec![],
            by_guid: HashMap::new(),
            by_name: HashMap::new(),
            res: HashMap::new(),
            res_named: HashMap::new(),
            chunks: HashMap::new(),
            parse_failures: 0,
        };
        let mut stats = BlockStats::default();
        for sb_name in superbundles {
            let sb = Superbundle::open(fs, sb_name)?;
            for b in &sb.bundles {
                let bundle = sb.read_bundle(b)?;
                for e in bundle.get("ebx").and_then(DbValue::as_list).unwrap_or(&[]) {
                    let name = e.get("name").and_then(DbValue::as_str).context("ebx without name")?.to_lowercase();
                    if world.by_name.contains_key(&name) {
                        continue;
                    }
                    let parsed = read_entry(fs, EntryKind::Ebx, e, &mut stats).and_then(|d| fb_ebx::parse(&d));
                    match parsed {
                        Ok(p) => {
                            let i = world.partitions.len();
                            world.by_guid.insert(p.file_guid, i);
                            world.by_name.insert(name.clone(), i);
                            world.partitions.push(p);
                            world.names.push(name);
                        }
                        Err(_) => world.parse_failures += 1,
                    }
                }
                for e in bundle.get("chunks").and_then(DbValue::as_list).unwrap_or(&[]) {
                    if let Some(id) = e.get("id").and_then(DbValue::as_guid) {
                        world.chunks.entry(*id).or_insert_with(|| e.clone());
                    }
                }
                for e in bundle.get("res").and_then(DbValue::as_list).unwrap_or(&[]) {
                    if let Some(rid) = e.get("resRid").and_then(DbValue::as_i64) {
                        world.res.entry(rid as u64).or_insert_with(|| e.clone());
                    }
                    if let Some(n) = e.get("name").and_then(DbValue::as_str) {
                        world.res_named.entry(n.to_lowercase()).or_insert_with(|| e.clone());
                    }
                }
            }
        }
        Ok(world)
    }

    /// Read a res entry by its name (any case).
    pub fn read_res_named(&self, fs: &GameFs, name: &str) -> Result<Vec<u8>> {
        let e = self.res_named.get(&name.to_lowercase()).with_context(|| format!("res {name} not in loaded bundles"))?;
        read_entry(fs, EntryKind::Res, e, &mut BlockStats::default())
    }

    pub fn partition_by_name(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    pub fn partition_by_guid(&self, g: &Guid) -> Option<usize> {
        self.by_guid.get(g).copied()
    }

    /// Resolve a link found in partition `part` to (partition, instance index).
    pub fn resolve(&self, part: usize, p: &Pointer) -> Option<(usize, usize)> {
        match p {
            Pointer::Null => None,
            Pointer::Internal(i) => Some((part, *i)),
            Pointer::External(file, inst) => {
                let target = self.partition_by_guid(file)?;
                Some((target, self.partitions[target].find_instance(inst)?))
            }
        }
    }

    /// A readable name for a link target: the partition name, even if it is not loaded.
    pub fn link_name(&self, part: usize, p: &Pointer) -> String {
        match p {
            Pointer::Null => "null".into(),
            Pointer::Internal(_) => self.names[part].clone(),
            Pointer::External(file, _) => match self.partition_by_guid(file) {
                Some(i) => self.names[i].clone(),
                None => format!("<not loaded {}>", format_guid(file)),
            },
        }
    }

    fn instance(&self, at: (usize, usize)) -> &Complex {
        &self.partitions[at.0].instances[at.1].value
    }
}

/// One mesh placed in the world.
#[derive(Debug, Clone)]
pub struct PlacedMesh {
    /// Asset name of the mesh (for example ".../treelarch_02/dm_treelarch_02_backdrop_mesh").
    pub mesh: String,
    /// Where the mesh asset is, if it is loaded.
    pub mesh_at: Option<(usize, usize)>,
    pub transform: Affine3A,
    /// Entity type that placed it (for example "StaticModelEntityData" or "StaticModelGroup").
    pub source: String,
    /// Name of the sub-world or layer partition it came from.
    pub layer: String,
}

#[derive(Debug, Default)]
pub struct WalkStats {
    pub references_followed: usize,
    pub excluded: usize,
    pub unresolved_links: BTreeMap<String, usize>,
    /// Entities that place nothing visible (logic, sound, spawns...), by type.
    pub other_entities: BTreeMap<String, usize>,
    pub cycles_skipped: usize,
    pub depth_limit_hits: usize,
    pub subworlds: Vec<String>,
    pub subworlds_skipped: Vec<String>,
    /// Static model groups whose transforms came from Havok data.
    pub groups_from_havok: usize,
    /// Groups that needed Havok data but it could not be read (with the reason).
    pub group_physics_failures: BTreeMap<String, usize>,
    /// Members whose Havok instance count differs from their InstanceCount.
    pub group_count_mismatches: usize,
    /// Members that have instances but no transform from any source.
    pub group_members_without_transforms: usize,
}

const MAX_DEPTH: usize = 48;

pub struct Walker<'w> {
    world: &'w World,
    fs: &'w GameFs,
    pub meshes: Vec<PlacedMesh>,
    pub stats: WalkStats,
    stack: HashSet<(usize, usize)>,
    /// Level part (sub-world or layer partition) currently being walked.
    layer: Vec<usize>,
    /// Sub-worlds to leave out, by lower-case name part (for example "rush0").
    pub skip_subworlds: Vec<String>,
}

impl<'w> Walker<'w> {
    pub fn new(world: &'w World, fs: &'w GameFs) -> Self {
        Walker {
            world,
            fs,
            meshes: vec![],
            stats: WalkStats::default(),
            stack: HashSet::new(),
            layer: vec![],
            skip_subworlds: vec![],
        }
    }

    fn current_layer(&self) -> String {
        self.layer.last().map(|&p| self.world.names[p].clone()).unwrap_or_default()
    }

    /// Find and read the Havok instances of a static model group:
    /// Components -> (component with PhysicsBodies) -> PhysicsBodies[0] -> Asset -> Resource.
    fn group_physics(&mut self, part: usize, group: &Complex) -> Option<GroupPhysics> {
        let world = self.world;
        let mut fail = |why: &str| {
            *self.stats.group_physics_failures.entry(why.to_string()).or_default() += 1;
            None
        };
        let Some(Value::Array { items: comps, .. }) = group.get("Components") else { return fail("no Components") };
        let body = comps.iter().find_map(|p| {
            let Value::Pointer(p) = p else { return None };
            let at = world.resolve(part, p)?;
            let Some(Value::Array { items: bodies, .. }) = world.instance(at).get("PhysicsBodies") else { return None };
            let Some(Value::Pointer(b)) = bodies.first() else { return None };
            world.resolve(at.0, b)
        });
        let Some(body) = body else { return fail("no physics body") };
        let Some(Value::Pointer(asset)) = world.instance(body).get("Asset") else { return fail("body has no Asset") };
        let Some(asset) = world.resolve(body.0, &asset.clone()) else { return fail("Asset not loaded") };
        let Some(Value::ResourceRef(rid)) = world.instance(asset).get("Resource") else { return fail("asset has no Resource") };
        let Some(entry) = world.res.get(rid) else { return fail("res not in bundles") };
        let mut bs = BlockStats::default();
        let data = match read_entry(self.fs, EntryKind::Res, entry, &mut bs) {
            Ok(d) => d,
            Err(_) => return fail("res read failed"),
        };
        match parse_group_physics(&data) {
            Ok(g) => {
                self.stats.groups_from_havok += 1;
                Some(g)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                fail(&msg)
            }
        }
    }

    /// Walk a level, starting at the primary instance of the partition `level_name`.
    pub fn walk_level(&mut self, level_name: &str) -> Result<()> {
        let part = self.world.partition_by_name(level_name).with_context(|| format!("level {level_name} not loaded"))?;
        self.walk_container(part, 0, Affine3A::IDENTITY, 0);
        Ok(())
    }

    /// Something with an `Objects` list (level, sub-world, layer, prefab).
    fn walk_container(&mut self, part: usize, inst: usize, parent: Affine3A, depth: usize) {
        let c = self.world.instance((part, inst));
        // Remember the innermost level part (sub-world or layer) for the log.
        let is_level_part = matches!(
            self.world.partitions[part].primary().map(|i| i.value.type_name.as_str()),
            Some("LevelData" | "SubWorldData" | "WorldPartData" | "LayerData")
        );
        if is_level_part {
            self.layer.push(part);
        }
        if let Some(Value::Array { items, .. }) = c.get("Objects") {
            for item in items.clone() {
                if let Value::Pointer(p) = item {
                    self.walk_link(part, &p, parent, depth + 1);
                }
            }
        }
        if is_level_part {
            self.layer.pop();
        }
    }

    fn walk_link(&mut self, part: usize, p: &Pointer, parent: Affine3A, depth: usize) {
        if matches!(p, Pointer::Null) {
            return;
        }
        match self.world.resolve(part, p) {
            Some(at) => self.walk_instance(at, parent, depth),
            None => *self.stats.unresolved_links.entry(self.world.link_name(part, p)).or_default() += 1,
        }
    }

    fn walk_instance(&mut self, at: (usize, usize), parent: Affine3A, depth: usize) {
        if depth > MAX_DEPTH {
            self.stats.depth_limit_hits += 1;
            return;
        }
        if !self.stack.insert(at) {
            self.stats.cycles_skipped += 1;
            return;
        }
        self.visit(at, parent, depth);
        self.stack.remove(&at);
    }

    fn visit(&mut self, at: (usize, usize), parent: Affine3A, depth: usize) {
        let world = self.world;
        let c = world.instance(at);
        let part = at.0;

        // Reference objects: follow the blueprint with the blueprint transform.
        if c.get("BlueprintTransform").is_some() {
            if matches!(c.get("Excluded"), Some(Value::Bool(true))) {
                self.stats.excluded += 1;
                return;
            }
            self.stats.references_followed += 1;
            let local = c.get("BlueprintTransform").and_then(transform_of).unwrap_or(Affine3A::IDENTITY);
            let xf = parent * local;
            let target = match c.get("Blueprint") {
                Some(Value::Pointer(Pointer::Null)) | None => None,
                Some(Value::Pointer(p)) => match world.resolve(part, p) {
                    Some(t) => Some(t),
                    None => {
                        *self.stats.unresolved_links.entry(world.link_name(part, p)).or_default() += 1;
                        return;
                    }
                },
                _ => None,
            };
            let target = match target {
                Some(t) => t,
                None => {
                    // Sub-worlds name their target by bundle name.
                    let Some(Value::String(bundle)) = c.get("BundleName") else { return };
                    let lower = bundle.to_lowercase();
                    if self.skip_subworlds.iter().any(|s| lower.ends_with(&format!("/{s}"))) {
                        self.stats.subworlds_skipped.push(bundle.clone());
                        return;
                    }
                    match world.partition_by_name(bundle) {
                        Some(p) => {
                            self.stats.subworlds.push(bundle.clone());
                            (p, 0)
                        }
                        None => {
                            *self.stats.unresolved_links.entry(format!("bundle {bundle}")).or_default() += 1;
                            return;
                        }
                    }
                }
            };
            let bp = world.instance(target);
            if bp.get("Objects").is_some() {
                self.walk_container(target.0, target.1, xf, depth);
            } else if let Some(Value::Pointer(obj)) = bp.get("Object") {
                let obj = obj.clone();
                self.walk_link(target.0, &obj, xf, depth + 1);
            }
            return;
        }

        // Static model groups: many instances of a few meshes.
        if let Some(Value::Array { items, .. }) = c.get("MemberDatas") {
            let ent = c.get("Transform").and_then(transform_of).unwrap_or(Affine3A::IDENTITY);
            // Members without stored transforms get them from the group's Havok physics data.
            let needs_physics = items.iter().any(|m| {
                matches!(m, Value::Struct(m) if matches!(m.get("InstanceTransforms"), Some(Value::Array { items, .. }) if items.is_empty())
                    && matches!(m.get("InstanceCount"), Some(Value::UInt(n)) if *n > 0))
            });
            let physics = if needs_physics { self.group_physics(part, c) } else { None };
            let mut used_per_member = vec![0usize; items.len()];
            for (mi, m) in items.iter().enumerate() {
                let Value::Struct(m) = m else { continue };
                let Some(Value::Pointer(mesh)) = m.get("MeshAsset") else { continue };
                let mesh_name = world.link_name(part, mesh);
                let mesh_at = world.resolve(part, mesh);
                let expected = match m.get("InstanceCount") {
                    Some(Value::UInt(n)) => *n as usize,
                    _ => 0,
                };
                let mut push = |xf: Affine3A, source: &str| {
                    self.meshes.push(PlacedMesh {
                        mesh: mesh_name.clone(),
                        mesh_at,
                        transform: parent * ent * xf,
                        source: source.into(),
                        layer: self.current_layer(),
                    });
                };
                let stored: Vec<Affine3A> = match m.get("InstanceTransforms") {
                    Some(Value::Array { items: xfs, .. }) => xfs.iter().filter_map(transform_of).collect(),
                    _ => vec![],
                };
                if !stored.is_empty() {
                    for xf in stored {
                        push(xf, "StaticModelGroup");
                    }
                } else if let Some(ph) = &physics {
                    let mine: Vec<Affine3A> =
                        ph.member_of_instance.iter().zip(&ph.transforms).filter(|(mm, _)| **mm as usize == mi).map(|(_, t)| *t).collect();
                    used_per_member[mi] = mine.len();
                    for xf in mine {
                        push(xf, "StaticModelGroup(havok)");
                    }
                }
                if expected > 0 && used_per_member[mi] == 0 && !matches!(m.get("InstanceTransforms"), Some(Value::Array { items, .. }) if !items.is_empty()) {
                    self.stats.group_members_without_transforms += 1;
                } else if physics.is_some() && used_per_member[mi] != 0 && used_per_member[mi] != expected {
                    self.stats.group_count_mismatches += 1;
                }
            }
            return;
        }

        // Any entity with a Mesh link.
        if let Some(Value::Pointer(mesh)) = c.get("Mesh") {
            if !matches!(mesh, Pointer::Null) {
                let local = c.get("Transform").and_then(transform_of).unwrap_or(Affine3A::IDENTITY);
                self.meshes.push(PlacedMesh {
                    mesh: world.link_name(part, mesh),
                    mesh_at: world.resolve(part, mesh),
                    transform: parent * local,
                    source: c.type_name.clone(),
                    layer: self.current_layer(),
                });
                return;
            }
        }

        // Nested containers (prefab blueprints reached directly, layers...).
        if c.get("Objects").is_some() {
            let local = c.get("Transform").and_then(transform_of).unwrap_or(Affine3A::IDENTITY);
            self.walk_container(at.0, at.1, parent * local, depth);
            return;
        }

        *self.stats.other_entities.entry(c.type_name.clone()).or_default() += 1;
    }
}

fn vec3(v: Option<&Value>) -> Option<Vec3A> {
    let Some(Value::Struct(s)) = v else { return None };
    let f = |n: &str| match s.get(n) {
        Some(Value::Float(x)) => Some(*x as f32),
        _ => None,
    };
    Some(Vec3A::new(f("x")?, f("y")?, f("z")?))
}

/// Convert a `LinearTransform` value to an affine matrix.
pub fn transform_of(v: &Value) -> Option<Affine3A> {
    let Value::Struct(t) = v else { return None };
    Some(Affine3A::from_cols(vec3(t.get("right"))?, vec3(t.get("up"))?, vec3(t.get("forward"))?, vec3(t.get("trans"))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v3(x: f64, y: f64, z: f64) -> Value {
        Value::Struct(Complex {
            type_name: "Vec3".into(),
            fields: vec![("x".into(), Value::Float(x)), ("y".into(), Value::Float(y)), ("z".into(), Value::Float(z))],
        })
    }

    fn lt(right: Value, up: Value, forward: Value, trans: Value) -> Value {
        Value::Struct(Complex {
            type_name: "LinearTransform".into(),
            fields: vec![("right".into(), right), ("up".into(), up), ("forward".into(), forward), ("trans".into(), trans)],
        })
    }

    #[test]
    fn transform_maps_points_like_frostbite() {
        // Rotate 90 degrees around Y and move by (10, 0, 0).
        let t = transform_of(&lt(v3(0.0, 0.0, -1.0), v3(0.0, 1.0, 0.0), v3(1.0, 0.0, 0.0), v3(10.0, 0.0, 0.0))).unwrap();
        // The point (1, 0, 0) goes to 1 * right + trans = (10, 0, -1).
        let p = t.transform_point3(glam::Vec3::new(1.0, 0.0, 0.0));
        assert!((p - glam::Vec3::new(10.0, 0.0, -1.0)).length() < 1e-6);
    }

    #[test]
    fn child_transform_is_parent_times_child() {
        let parent = transform_of(&lt(v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), v3(0.0, 0.0, 1.0), v3(100.0, 0.0, 0.0))).unwrap();
        let child = transform_of(&lt(v3(2.0, 0.0, 0.0), v3(0.0, 2.0, 0.0), v3(0.0, 0.0, 2.0), v3(1.0, 0.0, 0.0))).unwrap();
        let p = (parent * child).transform_point3(glam::Vec3::new(1.0, 0.0, 0.0));
        // Child: (1,0,0) -> (3,0,0). Parent: +100 -> (103,0,0).
        assert!((p - glam::Vec3::new(103.0, 0.0, 0.0)).length() < 1e-6);
    }
}

/// Instance transforms of a static model group, from its Havok physics res.
#[derive(Debug, Clone)]
pub struct GroupPhysics {
    /// Member index (into MemberDatas) of each instance.
    pub member_of_instance: Vec<u16>,
    pub transforms: Vec<Affine3A>,
}

/// Size of one hknpShapeInstance record in BF1 data (32-bit Havok layout).
const SHAPE_INSTANCE_SIZE: usize = 128;
/// Offset of the instance count inside hknpStaticCompoundShape (32-bit layout).
const COMPOUND_COUNT_OFFSET: usize = 0x64;

/// Parse a group physics res: a Frostbite header, then a Havok packfile.
///
/// Frostbite header (seen on Verdun, 2026-10-03): u32 at 0x34 = instance
/// count, u32 at 0x38 = offset of a u16 table giving the member index of each
/// instance. The Havok part has an `hknpStaticCompoundShape` with that many
/// instances; each instance is rotation (3 x float4), translation (float4),
/// scale (float4), then shape data.
pub fn parse_group_physics(buf: &[u8]) -> Result<GroupPhysics> {
    use anyhow::ensure;
    let u32_at = |at: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(buf.get(at..at + 4).context("header past end")?.try_into().unwrap()) as usize)
    };
    let count = u32_at(0x34)?;
    let table = u32_at(0x38)?;
    let member_of_instance: Vec<u16> = (0..count)
        .map(|i| buf.get(table + i * 2..table + i * 2 + 2).map(|b| u16::from_le_bytes([b[0], b[1]])))
        .collect::<Option<_>>()
        .context("member table past end")?;

    let hk_at = havok::check_magic(buf)?;
    let pf = havok::Packfile::parse(&buf[hk_at..])?;
    ensure!(pf.pointer_size == 4, "Havok pointer size {} (expected 4)", pf.pointer_size);
    let d = pf.data_bytes();
    let compound = pf
        .find_class("hknpStaticCompoundShape")
        .into_iter()
        .find(|&i| {
            let o = pf.objects[i].offset + COMPOUND_COUNT_OFFSET;
            d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) == Some(count)
        })
        .with_context(|| format!("no hknpStaticCompoundShape with {count} instances"))?;
    let obj = &pf.objects[compound];
    let len = pf.object_len(compound);
    ensure!(len >= count * SHAPE_INSTANCE_SIZE, "compound too small for {count} instances");
    let start = obj.offset + len - count * SHAPE_INSTANCE_SIZE;

    let mut transforms = Vec::with_capacity(count);
    for i in 0..count {
        let at = start + i * SHAPE_INSTANCE_SIZE;
        let c0 = havok::vec4(d, at)?;
        let c1 = havok::vec4(d, at + 16)?;
        let c2 = havok::vec4(d, at + 32)?;
        let t = havok::vec4(d, at + 48)?;
        let s = havok::vec4(d, at + 64)?;
        transforms.push(Affine3A::from_cols(
            Vec3A::new(c0[0], c0[1], c0[2]) * s[0],
            Vec3A::new(c1[0], c1[1], c1[2]) * s[1],
            Vec3A::new(c2[0], c2[1], c2[2]) * s[2],
            Vec3A::new(t[0], t[1], t[2]),
        ));
    }
    Ok(GroupPhysics { member_of_instance, transforms })
}

impl World {
    /// Read and decompress a res file by resource id.
    pub fn read_res(&self, fs: &GameFs, rid: u64) -> Result<Vec<u8>> {
        let e = self.res.get(&rid).with_context(|| format!("res {rid:016x} not in loaded bundles"))?;
        read_entry(fs, EntryKind::Res, e, &mut BlockStats::default())
    }

    /// Read and decompress a chunk by id. A bundle entry that holds the whole
    /// chunk is used first; else the chunk lists of the .toc files (call
    /// `GameFs::index_toc_chunks` first); else a partial bundle entry.
    pub fn read_chunk(&self, fs: &GameFs, id: &Guid) -> Result<Vec<u8>> {
        let entry = self.chunks.get(id);
        if let Some(e) = entry.filter(|e| e.get("rangeStart").is_none()) {
            return read_entry(fs, EntryKind::Chunk, e, &mut BlockStats::default());
        }
        if let Some(sha1) = fs.toc_chunks.get(id) {
            return fs.read(sha1, None, &mut BlockStats::default());
        }
        let e = entry.with_context(|| format!("chunk {} not in loaded bundles or any .toc", format_guid(id)))?;
        read_entry(fs, EntryKind::Chunk, e, &mut BlockStats::default())
    }

    /// The instance at a resolved link.
    pub fn get(&self, at: (usize, usize)) -> &Complex {
        self.instance(at)
    }
}
