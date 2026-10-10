//! Walking: a capsule player with gravity, on the terrain and the placed meshes.
//! F switches between flying and walking. In walk mode: W/A/S/D move,
//! Left Shift sprints, Space jumps; hold the right mouse button to look.

use crate::camera::FlyCam;
use crate::collision::CollisionWorld;
use crate::log::GameLog;
use bevy::prelude::*;

pub const HEIGHT: f32 = 1.8;
pub const RADIUS: f32 = 0.35;
pub const EYE: f32 = 1.65;
const WALK_SPEED: f32 = 4.0;
const SPRINT_SPEED: f32 = 7.0;
const JUMP_SPEED: f32 = 4.5;
const GRAVITY: f32 = 9.81;
/// Below the terrain surface by more than this, the player is underground
/// (in a bunker, entered through a hole) and the terrain is ignored.
const UNDERGROUND: f32 = 1.5;
/// Largest drop the player follows without leaving the ground (walking downhill).
const STICK: f32 = 0.35;

#[derive(Resource)]
pub struct Walk {
    pub enabled: bool,
    /// Feet position (Bevy coordinates).
    pub feet: Vec3,
    pub velocity: Vec3,
    pub grounded: bool,
}

/// The terrain, for ground height (Frostbite coordinates inside).
#[derive(Resource)]
pub struct Ground(pub fb_terrain::Terrain);

impl Ground {
    /// Terrain height at a Bevy position; None in holes or outside.
    pub fn height(&self, p: Vec3) -> Option<f32> {
        let f = crate::convert::to_frostbite(p.to_array());
        self.0.height_at(f[0], f[2])
    }
}

pub fn toggle_walk(keys: Res<ButtonInput<KeyCode>>, mut walk: ResMut<Walk>, ground: Res<Ground>, cams: Query<&Transform, With<FlyCam>>, log: Res<GameLog>, mut started: Local<bool>) {
    // Test mode starts in walk mode.
    let auto_start = !*started && std::env::var("BF1_AUTOWALK").is_ok() && cams.single().is_ok();
    *started = true;
    if !keys.just_pressed(KeyCode::KeyF) && !auto_start {
        return;
    }
    walk.enabled = !walk.enabled;
    if walk.enabled {
        if let Ok(cam) = cams.single() {
            // Start under the camera, on the terrain if the camera is above it.
            let mut feet = cam.translation - Vec3::Y * EYE;
            if let Some(h) = ground.height(feet) {
                if feet.y > h - UNDERGROUND {
                    feet.y = feet.y.max(h);
                }
            }
            walk.feet = feet;
            walk.velocity = Vec3::ZERO;
        }
    }
    log.line(format!("walk mode {}", if walk.enabled { "on" } else { "off (flying)" }));
}

pub fn walk_player(
    time: Res<Time>,
    mut auto_log: Local<f32>,
    mut auto_last: Local<Vec3>,
    keys: Res<ButtonInput<KeyCode>>,
    mut walk: ResMut<Walk>,
    ground: Res<Ground>,
    mut world: ResMut<CollisionWorld>,
    mut cams: Query<(&mut Transform, &FlyCam)>,
    log: Res<GameLog>,
) {
    if !walk.enabled {
        return;
    }
    let Ok((mut tf, cam)) = cams.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 30.0);

    if let Some(n) = world.update_area(walk.feet) {
        log.line(format!("collision area rebuilt at ({:.0}, {:.0}, {:.0}): {n} triangles", walk.feet.x, walk.feet.y, walk.feet.z));
    }

    // Input: move on the ground plane in the look direction.
    let yaw = Quat::from_rotation_y(cam.yaw);
    let (fwd, right) = (yaw * -Vec3::Z, yaw * Vec3::X);
    let mut dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        dir += fwd;
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= fwd;
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= right;
    }
    // Test mode: BF1_AUTOWALK=1 walks forward on its own and turns a little every 4 seconds.
    let auto = std::env::var("BF1_AUTOWALK").is_ok();
    if auto {
        let turn = Quat::from_rotation_y((time.elapsed_secs() / 4.0).floor() * 0.7);
        dir = turn * fwd;
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { SPRINT_SPEED } else { WALK_SPEED };
    let wish = dir.normalize_or_zero() * speed;
    walk.velocity.x = wish.x;
    walk.velocity.z = wish.z;
    if walk.grounded && keys.just_pressed(KeyCode::Space) {
        walk.velocity.y = JUMP_SPEED;
    }
    walk.velocity.y -= GRAVITY * dt;

    let mut feet = walk.feet + walk.velocity * dt;
    let mut grounded = false;
    let mut on_mesh = false;

    // Meshes: push three spheres along the capsule out of nearby triangles.
    for _ in 0..4 {
        let mut moved = false;
        for k in 0..3 {
            let y = RADIUS + (HEIGHT - 2.0 * RADIUS) * k as f32 / 2.0;
            let (push, up) = world.push_sphere(feet + Vec3::Y * y, RADIUS);
            if push.length_squared() > 1e-8 {
                feet += push;
                moved = true;
                if let Some(n) = up {
                    // The bottom sphere climbs small ledges: medium slopes count as floor there.
                    let floor_limit = if k == 0 { 0.2 } else { 0.6 };
                    if n.y > floor_limit {
                        // Floor: stop falling (never turn walking speed into upward speed).
                        if k == 0 {
                            grounded = true;
                            on_mesh = true;
                        }
                        walk.velocity.y = walk.velocity.y.max(0.0);
                    } else {
                        // Wall or ceiling: remove only the horizontal speed into it.
                        let nh = Vec3::new(n.x, 0.0, n.z).normalize_or_zero();
                        let into = walk.velocity.dot(nh);
                        if into < 0.0 {
                            walk.velocity -= nh * into;
                        }
                        if n.y < -0.6 {
                            walk.velocity.y = walk.velocity.y.min(0.0);
                        }
                    }
                }
            }
        }
        if !moved {
            break;
        }
    }

    // Terrain (unless underground or in a hole). Stick to it when walking downhill:
    // if we were on the ground and it is less than STICK below, stay on it.
    if let Some(h) = ground.height(feet) {
        if !on_mesh && walk.grounded && walk.velocity.y <= 0.0 && feet.y > h && feet.y - h < STICK {
            feet.y = h;
        }
        if feet.y <= h && feet.y > h - UNDERGROUND {
            feet.y = h;
            grounded = true;
            walk.velocity.y = walk.velocity.y.max(0.0);
        }
    }
    if grounded {
        walk.velocity.y = walk.velocity.y.max(0.0);
    }
    // Safety: never fall forever.
    if feet.y < -200.0 {
        log.line("fell below -200 m: back to the terrain surface");
        feet.y = ground.height(feet).unwrap_or(100.0) + 1.0;
        walk.velocity = Vec3::ZERO;
    }
    walk.feet = feet;
    walk.grounded = grounded;
    tf.translation = feet + Vec3::Y * EYE;
    if auto && time.elapsed_secs() > *auto_log {
        *auto_log = time.elapsed_secs() + 1.0;
        let h = ground.height(feet).map(|h| format!("{h:.2}")).unwrap_or("hole/outside".into());
        if walk.velocity.length() > 1.0 && (feet - *auto_last).length() < 0.2 {
            log.line(format!("autowalk: not moving; touching {:?}", world.touching(feet + Vec3::Y * 0.9, 1.0)));
        }
        *auto_last = feet;
        log.line(format!(
            "autowalk: feet ({:.2}, {:.2}, {:.2}) terrain {h} grounded {} velocity ({:.2}, {:.2}, {:.2})",
            feet.x, feet.y, feet.z, grounded, walk.velocity.x, walk.velocity.y, walk.velocity.z
        ));
    }
}
