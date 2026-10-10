//! Terrain mesh scattering records from the terrain's `VisualResource`
//! (worked out from the Verdun data, DESIGN.md "scattered grass and
//! debris"). The resource holds one record per `TerrainMeshScatteringType`,
//! in identifier order. Each record has the scatter mesh name (a path
//! containing `meshscatter/`) and, some bytes after it, a group of 4 distance
//! values (metres). The records are byte-packed, so the reader takes the last
//! group of 4 values in the record (before the next name) that are all whole
//! or half metres between 1 and 10,000. In all 17 Verdun records that is
//! the group; other plausible-looking values come earlier, and only small
//! integers follow it. What the 4 values mean exactly is not known (they are
//! not always rising); we use the largest as the draw distance.

/// One scattering record.
#[derive(Debug, Clone, PartialEq)]
pub struct ScatterRecord {
    /// Mesh asset name, as stored (for example
    /// `artassets/meshscatter/dolomites/ms_dm_meshgrass_01_Mesh`).
    pub mesh: String,
    /// The 4 distance values after the name (metres), or None if not found.
    pub distances: Option<[f32; 4]>,
}

impl ScatterRecord {
    /// Draw distance: the largest of the 4 values.
    pub fn draw_distance(&self) -> Option<f32> {
        self.distances.map(|d| d.iter().copied().fold(0.0, f32::max))
    }
}

fn plausible(v: f32) -> bool {
    v.is_finite() && (1.0..=10_000.0).contains(&v) && (v * 2.0).fract() == 0.0
}

/// Read the records, in the order they are stored (= identifier order).
pub fn parse_visual_resource(data: &[u8]) -> Vec<ScatterRecord> {
    let is_path = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'/';
    // Name spans first: a record runs from its name to the next name.
    let mut names: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if !is_path(data[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < data.len() && is_path(data[i]) {
            i += 1;
        }
        let run = &data[start..i];
        if run.len() >= 12 && run.windows(12).any(|w| w.eq_ignore_ascii_case(b"meshscatter/")) {
            names.push((start, i));
        }
    }
    let f = |o: usize| data.get(o..o + 4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    names
        .iter()
        .enumerate()
        .map(|(k, &(start, end))| {
            let next = names.get(k + 1).map(|n| n.0).unwrap_or(data.len());
            let distances = (end..next.saturating_sub(15)).rev().find_map(|o| {
                let v = [f(o)?, f(o + 4)?, f(o + 8)?, f(o + 12)?];
                v.iter().all(|&x| plausible(x)).then_some(v)
            });
            ScatterRecord { mesh: String::from_utf8_lossy(&data[start..end]).into_owned(), distances }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_names_and_unaligned_distances() {
        let mut d = vec![0u8; 3];
        d.extend_from_slice(b"artassets/meshscatter/test/ms_grass_Mesh");
        d.extend_from_slice(&[0, 7, 9]);
        // An earlier plausible-looking group (ignored), then the record's
        // last group at an odd offset, followed by small integers.
        for v in [60.0f32, 50.0, 2.0, 1.5] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[0, 4, 0, 0, 0, 0, 0, 0, 0]);
        for v in [15.0f32, 20.0, 7.0, 15.0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
        d.extend_from_slice(b"xpack1/artassets/meshscatter/generic/ms_planks_Mesh");
        d.extend_from_slice(&[0; 4]);
        let r = parse_visual_resource(&d);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].mesh, "artassets/meshscatter/test/ms_grass_Mesh");
        assert_eq!(r[0].distances, Some([15.0, 20.0, 7.0, 15.0]));
        assert_eq!(r[0].draw_distance(), Some(20.0));
        assert_eq!(r[1].mesh, "xpack1/artassets/meshscatter/generic/ms_planks_Mesh");
        assert_eq!(r[1].distances, None);
    }
}
