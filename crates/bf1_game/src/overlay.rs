//! Position overlay (DESIGN.md "position overlay and inspect key"): text at
//! the top left with the camera position, view angles, frame rate, weather,
//! a ready-to-copy `BF1_CAMERA` line and the last inspect result (15 s).
//! F1 shows or hides it. It starts hidden when `BF1_SCREENSHOT_AT` is set
//! (test screenshots stay clean; `BF1_OVERLAY=on` shows it anyway) or with
//! `BF1_OVERLAY=off`.

use crate::camera::FlyCam;
use crate::inspect::{camera_line, InspectResult};
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

/// How long an inspect result stays on screen (seconds).
const RESULT_SECONDS: f32 = 15.0;

#[derive(Component)]
pub struct OverlayText;

pub fn spawn_overlay(mut commands: Commands) {
    let setting = std::env::var("BF1_OVERLAY").unwrap_or_default();
    let hidden = setting == "off" || (std::env::var("BF1_SCREENSHOT_AT").is_ok() && setting != "on");
    commands
        .spawn((
            Node { position_type: PositionType::Absolute, top: Val::Px(8.0), left: Val::Px(8.0), padding: UiRect::all(Val::Px(6.0)), max_width: Val::Px(900.0), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            if hidden { Visibility::Hidden } else { Visibility::Visible },
        ))
        .with_children(|p| {
            p.spawn((Text::new(""), TextFont { font_size: bevy::text::FontSize::Px(15.0), ..default() }, TextColor(Color::srgb(0.92, 0.92, 0.92)), OverlayText));
        });
}

/// F1 shows or hides the overlay.
pub fn toggle_overlay(keys: Res<ButtonInput<KeyCode>>, texts: Query<&ChildOf, With<OverlayText>>, mut panels: Query<&mut Visibility>) {
    if !keys.just_pressed(KeyCode::F1) {
        return;
    }
    for child_of in &texts {
        if let Ok(mut v) = panels.get_mut(child_of.parent()) {
            *v = if *v == Visibility::Hidden { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

/// Refresh the text (a few times per second).
pub fn update_overlay(
    time: Res<Time>,
    mut next: Local<f32>,
    diagnostics: Res<DiagnosticsStore>,
    env: Res<crate::scene::Env>,
    result: Res<InspectResult>,
    cams: Query<(&GlobalTransform, &FlyCam)>,
    mut texts: Query<&mut Text, With<OverlayText>>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 0.2;
    let Ok((gt, cam)) = cams.single() else { return };
    let p = gt.translation();
    let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    let weather = env.list.get(env.current).map(|e| e.name.as_str()).unwrap_or("?");
    let mut s = format!(
        "x {:.1}   y {:.1}   z {:.1}   yaw {:.0}   pitch {:.0}   {fps:.0} fps   weather {weather}\n{}\nI: inspect the centre of the screen   F1: hide",
        p.x,
        p.y,
        p.z,
        cam.yaw.to_degrees(),
        cam.pitch.to_degrees(),
        camera_line(p, cam)
    );
    if !result.text.is_empty() && time.elapsed_secs() - result.at < RESULT_SECONDS {
        s.push_str(&format!("\nInspect: {}", result.text));
    }
    for mut t in &mut texts {
        t.0 = s.clone();
    }
}
