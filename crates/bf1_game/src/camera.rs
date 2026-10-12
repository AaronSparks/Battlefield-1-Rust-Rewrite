//! Free fly camera. Hold the right mouse button to look around.
//! W/A/S/D move, Space/Ctrl move up/down, Shift moves fast.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;

#[derive(Component)]
pub struct FlyCam {
    pub yaw: f32,
    pub pitch: f32,
    pub speed: f32,
}

pub fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut cams: Query<(&mut Transform, &mut FlyCam)>,
    walk: Option<Res<crate::player::Walk>>,
) {
    let dt = time.delta_secs();
    for (mut tf, mut cam) in &mut cams {
        if buttons.pressed(MouseButton::Right) {
            cam.yaw -= motion.delta.x * 0.003;
            cam.pitch = (cam.pitch - motion.delta.y * 0.003).clamp(-1.5, 1.5);
        }
        tf.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
        // In walk mode the player system moves the camera.
        if walk.as_ref().is_some_and(|w| w.enabled) {
            continue;
        }
        let mut dir = Vec3::ZERO;
        let (fwd, right) = (tf.forward().as_vec3(), tf.right().as_vec3());
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
        if keys.pressed(KeyCode::Space) {
            dir += Vec3::Y;
        }
        if keys.pressed(KeyCode::ControlLeft) {
            dir -= Vec3::Y;
        }
        let fast = if keys.pressed(KeyCode::ShiftLeft) { 6.0 } else { 1.0 };
        tf.translation += dir.normalize_or_zero() * cam.speed * fast * dt;
        // Test switch: BF1_AUTOFLY=<metres per second> flies straight ahead
        // (level, along the view's heading) to measure the frame rate while moving.
        if let Some(speed) = std::env::var("BF1_AUTOFLY").ok().and_then(|v| v.parse::<f32>().ok()) {
            let ahead = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
            tf.translation += ahead * speed * dt.min(0.1);
        }
    }
}
