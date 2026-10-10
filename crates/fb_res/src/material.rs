//! Picking textures from a material's `TextureParameters`.
//!
//! Battlefield 1 shaders name their textures in many ways. These lists come
//! from counting the names used by the Verdun mesh materials (2026-10-03).

/// Rank of a texture parameter as the base (colour) texture. Lower is better.
pub fn base_color_rank(parameter: &str) -> Option<u8> {
    const NAMES: &[&str] = &[
        "basecolor",
        "_basecolor",
        "basecolor_smoothness",
        "basecolormap",
        "billboardcolor",
        "texbasecolor",
        "base_texture_cs",
        "cs_texture",
        "diffuse",
        "tilingbasecolor",
        "_basecolortilebreaker",
        "_tilingbasetexture",
        "trunkbasecolor",
        "texture",
    ];
    let p = parameter.to_ascii_lowercase();
    NAMES.iter().position(|n| *n == p).map(|i| i as u8)
}

/// True if a texture name looks like a colour texture (by its suffix).
pub fn is_colour_texture_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["_cs", "_c", "_d", "_ca", "_cm", "_color", "_diffuse"].iter().any(|s| n.ends_with(s))
}

/// Rank of a texture parameter as the normal map. Lower is better.
pub fn normal_rank(parameter: &str) -> Option<u8> {
    const NAMES: &[&str] = &[
        "normal",
        "_normal",
        "normal_metalmask_ao",
        "texnormalmap",
        "base_texture_n",
        "nam_texture",
        "tilingnormal",
        "_normaltilebreaker",
    ];
    let p = parameter.to_ascii_lowercase();
    NAMES.iter().position(|n| *n == p).map(|i| i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks() {
        assert_eq!(base_color_rank("BaseColor"), Some(0));
        assert!(base_color_rank("_BaseColor").unwrap() < base_color_rank("Diffuse").unwrap());
        assert_eq!(base_color_rank("Normal"), None);
        assert_eq!(normal_rank("Normal"), Some(0));
    }
}
