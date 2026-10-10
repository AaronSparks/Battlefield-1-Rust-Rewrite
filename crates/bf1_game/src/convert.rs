//! Frostbite world coordinates to Bevy. Bevy is right-handed with y up.
//! Frostbite positions go in unchanged: the map then matches the game's
//! deploy screen (see docs/DESIGN.md, "Coordinates"). Until 2026-10-06 we
//! negated z here, and the map was drawn mirrored. All conversion stays in
//! this file, so a change of convention is made in one place.

use bevy::math::{Affine3A, Mat4};

/// A Frostbite position or direction in Bevy coordinates.
pub fn point(p: [f32; 3]) -> [f32; 3] {
    p
}

/// A Bevy position or direction in Frostbite coordinates (the reverse of `point`),
/// for lookups in Frostbite data such as the terrain.
pub fn to_frostbite(p: [f32; 3]) -> [f32; 3] {
    p
}

/// A Frostbite affine transform (12 numbers: x axis, y axis, z axis,
/// translation, as from `Affine3A::to_cols_array`) as a Bevy matrix.
/// Plain arrays keep us independent of the glam version each crate uses.
pub fn transform(cols: [f32; 12]) -> Mat4 {
    Mat4::from(Affine3A::from_cols_array(&cols))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    #[test]
    fn converted_transform_moves_converted_points() {
        let m = Affine3A::from_rotation_y(0.7) * Affine3A::from_translation(Vec3::new(3.0, 1.0, 5.0));
        let p = Vec3::new(1.0, 2.0, 3.0);
        let world_fb = m.transform_point3(p);
        let world_bevy = transform(m.to_cols_array()).transform_point3(Vec3::from(point(p.to_array())));
        assert!((world_bevy - Vec3::from(point(world_fb.to_array()))).length() < 1e-5);
    }
}
