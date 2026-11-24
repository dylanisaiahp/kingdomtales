//! Block materials, architectural shapes and the packed voxel value.
//!
//! Material and shape are deliberately separate concepts:
//!
//! - [`Block`] is the material (stone, wood, turf, ...).
//! - [`Shape`] is the architectural form (cube, half block, slope, stairs).
//! - [`Voxel`] packs a material, a shape and a full 3D orientation into two
//!   bytes. Chunks therefore stay compact while still supporting non-cube
//!   building pieces, including sideways and upside-down slopes and stairs.

use std::sync::LazyLock;

use bevy::math::{Mat3, Quat, Vec3};

/// Every material in the world. `Air` is empty space.
///
/// The enum is deliberately small and `Copy` so a chunk is just a flat
/// `Vec<Voxel>`. It is also the material half of the building system: adding a
/// new building material only needs a new variant and its palette entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Block {
    #[default]
    Air = 0,
    Turf,
    Topsoil,
    Granite,
    Sand,
    Snow,
    Water,
    Wood,
    Leaves,
    Bedrock,
    Slate,
    Shale,
    Sandstone,
    Limestone,
    /// Dark, organic forest soil.
    Humus,
    /// Iron-rich red sandstone used by mesas.
    RedSandstone,
}

impl Block {
    /// Every solid, placeable material. Used by the hammer's material picker.
    /// Water and bedrock are intentionally excluded.
    pub const BUILDABLE: [Block; 13] = [
        Block::Turf,
        Block::Topsoil,
        Block::Granite,
        Block::Sand,
        Block::Snow,
        Block::Wood,
        Block::Leaves,
        Block::Slate,
        Block::Shale,
        Block::Sandstone,
        Block::Limestone,
        Block::Humus,
        Block::RedSandstone,
    ];

    #[inline]
    pub const fn index(self) -> u8 {
        self as u8
    }

    #[inline]
    pub const fn from_index(value: u8) -> Block {
        match value {
            1 => Block::Turf,
            2 => Block::Topsoil,
            3 => Block::Granite,
            4 => Block::Sand,
            5 => Block::Snow,
            6 => Block::Water,
            7 => Block::Wood,
            8 => Block::Leaves,
            9 => Block::Bedrock,
            10 => Block::Slate,
            11 => Block::Shale,
            12 => Block::Sandstone,
            13 => Block::Limestone,
            14 => Block::Humus,
            15 => Block::RedSandstone,
            _ => Block::Air,
        }
    }

    /// Blocks that stop the player and hide the faces of their neighbours.
    #[inline]
    pub const fn is_solid(self) -> bool {
        !matches!(self, Block::Air | Block::Water)
    }

    /// Opaque blocks fully hide adjacent faces (used by the mesher).
    ///
    /// Water is deliberately *not* opaque: terrain under water still gets meshed.
    #[inline]
    pub const fn is_opaque(self) -> bool {
        self.is_solid()
    }

    #[inline]
    pub const fn is_water(self) -> bool {
        matches!(self, Block::Water)
    }

    /// Soils that support loose sticks, pebbles and surface plants.
    #[inline]
    pub const fn is_soil(self) -> bool {
        matches!(self, Block::Topsoil | Block::Humus)
    }

    /// Natural stone materials that can form stone deposits.
    #[inline]
    pub const fn is_stone(self) -> bool {
        matches!(
            self,
            Block::Granite
                | Block::Slate
                | Block::Shale
                | Block::Sandstone
                | Block::Limestone
                | Block::RedSandstone
        )
    }

    /// sRGB base colour. The mesher converts this to linear space and applies
    /// face shading and ambient occlusion.
    pub fn srgb(self) -> [f32; 3] {
        match self {
            Block::Air => [0.0, 0.0, 0.0],
            Block::Turf => [0.32, 0.58, 0.23],
            Block::Topsoil => [0.34, 0.24, 0.14],
            Block::Granite => [0.56, 0.48, 0.45],
            Block::Sand => [0.84, 0.77, 0.55],
            Block::Snow => [0.92, 0.95, 0.98],
            Block::Water => [0.16, 0.42, 0.68],
            Block::Wood => [0.36, 0.24, 0.14],
            Block::Leaves => [0.20, 0.46, 0.17],
            Block::Bedrock => [0.18, 0.18, 0.20],
            Block::Slate => [0.32, 0.36, 0.40],
            Block::Shale => [0.38, 0.34, 0.31],
            Block::Sandstone => [0.78, 0.66, 0.45],
            Block::Limestone => [0.75, 0.72, 0.62],
            Block::Humus => [0.24, 0.18, 0.13],
            Block::RedSandstone => [0.68, 0.34, 0.22],
        }
    }

    /// Seconds of holding the mining action needed to break this block with the
    /// correct tool. `None` means the block cannot be mined.
    pub fn break_time(self) -> Option<f32> {
        match self {
            Block::Air | Block::Water | Block::Bedrock => None,
            Block::Turf => Some(0.35),
            Block::Topsoil => Some(0.40),
            Block::Granite => Some(0.95),
            Block::Sand => Some(0.30),
            Block::Snow => Some(0.25),
            Block::Wood => Some(0.65),
            Block::Leaves => Some(0.20),
            Block::Slate => Some(0.85),
            Block::Shale => Some(0.70),
            Block::Sandstone => Some(0.60),
            Block::Limestone => Some(0.65),
            Block::Humus => Some(0.45),
            Block::RedSandstone => Some(0.70),
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Block::Air => "Air",
            Block::Turf => "Turf",
            Block::Topsoil => "Topsoil",
            Block::Granite => "Granite",
            Block::Sand => "Sand",
            Block::Snow => "Snow",
            Block::Water => "Water",
            Block::Wood => "Wood",
            Block::Leaves => "Leaves",
            Block::Bedrock => "Bedrock",
            Block::Slate => "Slate",
            Block::Shale => "Shale",
            Block::Sandstone => "Sandstone",
            Block::Limestone => "Limestone",
            Block::Humus => "Humus",
            Block::RedSandstone => "Red Sandstone",
        }
    }
}

/// Architectural shape of a voxel. Material is stored separately.
///
/// The first five shapes are buildable architectural pieces. The natural shapes
/// are generated by the world (sticks, pebbles and foliage clusters); they are
/// deliberately not offered by the hammer, but they reuse the same voxel
/// storage, meshing, raycast, mining and save/load pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Shape {
    #[default]
    Cube = 0,
    HalfBlock,
    Slope,
    Stairs,
    Panel,
    Stick,
    Pebble,
    Foliage,
    Trunk,
    Stem,
    /// Small decorative foliage/leaves, smaller than a full cell for bushes and detail.
    SmallFoliage,
    /// Trunk/stem after bark has been scraped off. Slightly smaller and cannot be scraped again.
    DebarkedTrunk,
    DebarkedStem,
    /// Natural single-cell stone outcrop/cluster. Mineable with Small Stone Hammer.
    StoneDeposit,
}

/// Height of a panel-shaped voxel in block units. Blocks are 0.5 m, so this is
/// a 0.1 m panel: about the same visual weight as a thin mat or wall covering.
pub const PANEL_HEIGHT: f32 = 0.2;

impl Shape {
    /// Every shape, including generated natural detail shapes.
    pub const ALL: [Shape; 14] = [
        Shape::Cube,
        Shape::HalfBlock,
        Shape::Slope,
        Shape::Stairs,
        Shape::Panel,
        Shape::Stick,
        Shape::Pebble,
        Shape::Foliage,
        Shape::Trunk,
        Shape::Stem,
        Shape::SmallFoliage,
        Shape::DebarkedTrunk,
        Shape::DebarkedStem,
        Shape::StoneDeposit,
    ];

    /// Shapes the hammer can place.
    pub const BUILDABLE: [Shape; 5] = [
        Shape::Cube,
        Shape::HalfBlock,
        Shape::Slope,
        Shape::Stairs,
        Shape::Panel,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Shape::Cube => "Cube",
            Shape::HalfBlock => "Half Block",
            Shape::Slope => "Slope",
            Shape::Stairs => "Stairs",
            Shape::Panel => "Panel",
            Shape::Stick => "Stick",
            Shape::Pebble => "Pebble",
            Shape::Foliage => "Foliage",
            Shape::Trunk => "Trunk",
            Shape::Stem => "Stem",
            Shape::SmallFoliage => "Small Foliage",
            Shape::DebarkedTrunk => "Debarked Trunk",
            Shape::DebarkedStem => "Debarked Stem",
            Shape::StoneDeposit => "Stone Deposit",
        }
    }

    /// Stable index used by save files and wire formats.
    pub const fn index(self) -> u8 {
        match self {
            Shape::Cube => 0,
            Shape::HalfBlock => 1,
            Shape::Slope => 2,
            Shape::Stairs => 3,
            Shape::Panel => 4,
            Shape::Stick => 5,
            Shape::Pebble => 6,
            Shape::Foliage => 7,
            Shape::Trunk => 8,
            Shape::Stem => 9,
            Shape::SmallFoliage => 10,
            Shape::DebarkedTrunk => 11,
            Shape::DebarkedStem => 12,
            Shape::StoneDeposit => 13,
        }
    }

    pub const fn from_index(value: u8) -> Self {
        match value {
            1 => Shape::HalfBlock,
            2 => Shape::Slope,
            3 => Shape::Stairs,
            4 => Shape::Panel,
            5 => Shape::Stick,
            6 => Shape::Pebble,
            7 => Shape::Foliage,
            8 => Shape::Trunk,
            9 => Shape::Stem,
            10 => Shape::SmallFoliage,
            11 => Shape::DebarkedTrunk,
            12 => Shape::DebarkedStem,
            13 => Shape::StoneDeposit,
            _ => Shape::Cube,
        }
    }

    /// Only full cubes take part in greedy meshing and fully occlude neighbours.
    #[inline]
    pub const fn is_full_cube(self) -> bool {
        matches!(self, Shape::Cube)
    }

    /// Natural shapes are scattered by world generation rather than placed with
    /// the hammer. Sticks, pebbles and foliage are decorative; stems/trunks and
    /// debarked wood are solid. All natural shapes are raycastable and mineable.
    #[inline]
    pub const fn is_natural(self) -> bool {
        matches!(
            self,
            Shape::Stick
                | Shape::Pebble
                | Shape::Foliage
                | Shape::SmallFoliage
                | Shape::Stem
                | Shape::Trunk
                | Shape::DebarkedTrunk
                | Shape::DebarkedStem
                | Shape::StoneDeposit
        )
    }

    #[allow(dead_code)]
    #[inline]
    pub const fn is_wood_trunk(self) -> bool {
        matches!(self, Shape::Trunk | Shape::DebarkedTrunk)
    }

    #[allow(dead_code)]
    #[inline]
    pub const fn is_wood_stem(self) -> bool {
        matches!(self, Shape::Stem | Shape::DebarkedStem)
    }

    /// Buildable shapes can be selected in the hammer menu. Generated tree
    /// trunks are solid but still not offered as building pieces.
    #[inline]
    pub const fn is_buildable(self) -> bool {
        matches!(
            self,
            Shape::Cube | Shape::HalfBlock | Shape::Slope | Shape::Stairs | Shape::Panel
        )
    }
}

/// Cell-local bounds for one loose object, or a trunk, centred in its 1 m
/// world voxel. Generation stores the object in the world voxel's lower-left
/// detail cell; the geometry is offset to the middle of that 1 m block so the
/// piece does not sit in a corner. The object remains one voxel and one pickup
/// (or one mined trunk cell).
pub fn centered_detail_bounds(shape: Shape) -> (Vec3, Vec3) {
    match shape {
        // A trunk is a 0.5 m wide column, visually centred in the 1 m world
        // voxel. Narrower trunks read more naturally and leave room for roots/branches.
        Shape::Trunk => (Vec3::new(0.75, 0.0, 0.75), Vec3::new(1.25, 1.0, 1.25)),
        Shape::DebarkedTrunk => (Vec3::new(0.78, 0.0, 0.78), Vec3::new(1.22, 1.0, 1.22)),
        // Overall bounds covering every pebble variant (single/pair/trio, all
        // arrangements) so selection, raycasts and meshing stay inside the 1 m
        // world voxel. Pebbles are kept small but clearly visible.
        Shape::Pebble => (Vec3::new(0.60, 0.0, 0.60), Vec3::new(1.40, 0.16, 1.40)),
        // Overall bounds covering every stick variant and yaw.
        Shape::Stick => (Vec3::new(0.64, 0.02, 0.64), Vec3::new(1.36, 0.08, 1.36)),
        // Irregular stone outcrop/cluster: larger than loose pebbles but still
        // a single-cell natural object.
        Shape::StoneDeposit => (Vec3::new(0.62, 0.0, 0.62), Vec3::new(1.38, 0.72, 1.38)),
        _ => (Vec3::ZERO, Vec3::ONE),
    }
}

/// Rotate a centred-detail point around the 1 m world-voxel centre `(1, *, 1)`
/// by `quarter_turns` * 90 degrees (yaw).
fn rotate_centered_y(point: Vec3, quarter_turns: u8) -> Vec3 {
    let (x, z) = match quarter_turns % 4 {
        0 => (point.x, point.z),
        1 => (1.0 - (point.z - 1.0), 1.0 + (point.x - 1.0)),
        2 => (1.0 - (point.x - 1.0), 1.0 - (point.z - 1.0)),
        _ => (1.0 + (point.z - 1.0), 1.0 - (point.x - 1.0)),
    };
    Vec3::new(x, point.y, z)
}

fn rotate_box_y(min: Vec3, max: Vec3, quarter_turns: u8) -> (Vec3, Vec3) {
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, max.y, max.z),
    ];
    let mut out_min = Vec3::splat(f32::INFINITY);
    let mut out_max = Vec3::splat(f32::NEG_INFINITY);
    for corner in corners {
        let rotated = rotate_centered_y(corner, quarter_turns);
        out_min = out_min.min(rotated);
        out_max = out_max.max(rotated);
    }
    (out_min, out_max)
}

/// Visual parts for a stick variant, expressed in cell-local space and centred
/// in its 1 m world voxel. `orientation.index()` selects thin natural silhouettes
/// and yaws. Stubs are the same thickness as the main stick and overlap it so
/// there is no visible gap.
pub fn stick_parts(orientation: Orientation) -> Vec<(Vec3, Vec3)> {
    let index = orientation.index() as usize;
    let variant = index % 3;
    let yaw = (index / 3) as u8 % 4;
    // Main branch and stubs share the same 0.04 m thickness/height.
    // Stubs overlap the main branch by 0.01 m to avoid gaps.
    let y0 = 0.02;
    let y1 = 0.06;
    let canonical: &[(Vec3, Vec3)] = match variant {
        0 => &[
            (Vec3::new(0.64, y0, 0.97), Vec3::new(1.36, y1, 1.03)),
            (Vec3::new(1.02, y0, 1.02), Vec3::new(1.08, y1, 1.18)),
            (Vec3::new(0.88, y0, 0.82), Vec3::new(0.94, y1, 0.98)),
        ],
        1 => &[
            (Vec3::new(0.64, y0, 0.97), Vec3::new(1.36, y1, 1.03)),
            (Vec3::new(0.88, y0, 1.02), Vec3::new(0.94, y1, 1.18)),
            (Vec3::new(1.06, y0, 0.82), Vec3::new(1.12, y1, 0.98)),
        ],
        _ => &[
            (Vec3::new(0.64, y0, 0.97), Vec3::new(1.36, y1, 1.03)),
            (Vec3::new(0.97, y0, 1.02), Vec3::new(1.03, y1, 1.18)),
            (Vec3::new(0.97, y0, 0.82), Vec3::new(1.03, y1, 0.98)),
        ],
    };
    canonical
        .iter()
        .map(|&(min, max)| rotate_box_y(min, max, yaw))
        .collect()
}

/// Visual parts for a natural stone deposit: a single-cell irregular cluster of
/// rocks, clearly larger and more substantial than loose pebbles.
pub fn stone_deposit_parts(orientation: Orientation) -> Vec<(Vec3, Vec3)> {
    let index = orientation.index() as usize % 6;
    let yaw = (index / 3) as u8 % 4;
    let variant = index % 3;
    let canonical: &[(Vec3, Vec3)] = match variant {
        0 => &[
            (Vec3::new(0.74, 0.00, 0.78), Vec3::new(1.18, 0.34, 1.22)),
            (Vec3::new(0.92, 0.00, 0.68), Vec3::new(1.30, 0.24, 1.02)),
            (Vec3::new(0.82, 0.30, 0.86), Vec3::new(1.10, 0.56, 1.16)),
            (Vec3::new(0.98, 0.22, 0.76), Vec3::new(1.24, 0.42, 0.98)),
        ],
        1 => &[
            (Vec3::new(0.70, 0.00, 0.76), Vec3::new(1.08, 0.30, 1.14)),
            (Vec3::new(0.96, 0.00, 0.84), Vec3::new(1.34, 0.40, 1.24)),
            (Vec3::new(0.80, 0.26, 0.82), Vec3::new(1.04, 0.52, 1.08)),
            (Vec3::new(1.04, 0.28, 0.92), Vec3::new(1.26, 0.62, 1.16)),
        ],
        _ => &[
            (Vec3::new(0.76, 0.00, 0.72), Vec3::new(1.20, 0.28, 1.16)),
            (Vec3::new(0.86, 0.00, 0.94), Vec3::new(1.28, 0.36, 1.30)),
            (Vec3::new(0.82, 0.24, 0.78), Vec3::new(1.08, 0.48, 1.04)),
            (Vec3::new(0.96, 0.32, 0.96), Vec3::new(1.18, 0.66, 1.20)),
        ],
    };
    canonical
        .iter()
        .map(|&(min, max)| rotate_box_y(min, max, yaw))
        .collect()
}

/// Visual parts for a pebble or pebble pile, expressed in cell-local space and
/// centred in its 1 m world voxel. `orientation.index()` selects a single stone,
/// a pair, or a trio; piles are distinct models kept within the voxel (they are
/// not separate objects) and yield as many items as there are stones.
pub fn pebble_parts(orientation: Orientation) -> Vec<(Vec3, Vec3)> {
    let index = orientation.index() as usize % 12;
    let canonical: &[(Vec3, Vec3)] = match index {
        // Single small stones (more noticeable, but still pebbles).
        0 => &[(Vec3::new(0.84, 0.0, 0.86), Vec3::new(1.12, 0.14, 1.14))],
        1 => &[(Vec3::new(0.88, 0.0, 0.80), Vec3::new(1.14, 0.13, 1.06))],
        2 => &[(Vec3::new(0.80, 0.0, 0.90), Vec3::new(1.06, 0.15, 1.16))],
        3 => &[(Vec3::new(0.90, 0.0, 0.88), Vec3::new(1.18, 0.12, 1.14))],
        // Two-pebble piles.
        4 => &[
            (Vec3::new(0.74, 0.0, 0.84), Vec3::new(0.98, 0.13, 1.08)),
            (Vec3::new(1.02, 0.0, 0.92), Vec3::new(1.26, 0.15, 1.16)),
        ],
        5 => &[
            (Vec3::new(0.80, 0.0, 0.74), Vec3::new(1.04, 0.14, 0.98)),
            (Vec3::new(1.02, 0.0, 1.02), Vec3::new(1.26, 0.12, 1.26)),
        ],
        6 => &[
            (Vec3::new(0.76, 0.0, 0.98), Vec3::new(1.00, 0.12, 1.22)),
            (Vec3::new(1.04, 0.0, 0.78), Vec3::new(1.28, 0.14, 1.02)),
        ],
        7 => &[
            (Vec3::new(0.82, 0.0, 0.78), Vec3::new(1.06, 0.15, 1.02)),
            (Vec3::new(0.98, 0.0, 1.04), Vec3::new(1.22, 0.12, 1.28)),
        ],
        // Three-pebble piles.
        8 => &[
            (Vec3::new(0.74, 0.0, 0.84), Vec3::new(0.94, 0.13, 1.04)),
            (Vec3::new(0.98, 0.0, 0.76), Vec3::new(1.18, 0.14, 0.96)),
            (Vec3::new(1.00, 0.0, 1.00), Vec3::new(1.22, 0.12, 1.22)),
        ],
        9 => &[
            (Vec3::new(0.76, 0.0, 0.76), Vec3::new(0.96, 0.14, 0.96)),
            (Vec3::new(1.02, 0.0, 0.84), Vec3::new(1.22, 0.12, 1.04)),
            (Vec3::new(0.90, 0.0, 1.02), Vec3::new(1.10, 0.13, 1.22)),
        ],
        10 => &[
            (Vec3::new(0.78, 0.0, 0.94), Vec3::new(0.98, 0.12, 1.14)),
            (Vec3::new(1.02, 0.0, 0.98), Vec3::new(1.24, 0.14, 1.20)),
            (Vec3::new(0.92, 0.0, 0.74), Vec3::new(1.12, 0.13, 0.92)),
        ],
        _ => &[
            (Vec3::new(0.74, 0.0, 0.96), Vec3::new(0.94, 0.14, 1.16)),
            (Vec3::new(0.98, 0.0, 0.78), Vec3::new(1.18, 0.12, 0.98)),
            (Vec3::new(1.04, 0.0, 1.02), Vec3::new(1.24, 0.13, 1.22)),
        ],
    };
    canonical.to_vec()
}

/// How many pebbles are represented by a pebble variant. Single stones and
/// piles remain one voxel/pickup, but the variant changes the silhouette.
#[allow(dead_code)]
pub fn pebble_variant_count(orientation: Orientation) -> u8 {
    match orientation.index() as usize % 12 {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
}

/// Cell-local bounds of a stem in its storage detail cell.
///
/// Upright stems are centred 0.5 m columns. Horizontal limbs/roots are 0.5 m
/// thick, sit on ground (y 0..0.5), span 1 m along their facing axis for
/// continuous limbs, and are aligned to the centred 0.5 m trunk cross-section.
#[allow(dead_code)]
pub fn stem_bounds(orientation: Orientation) -> (Vec3, Vec3) {
    stem_bounds_for_shape(Shape::Stem, orientation)
}

/// Local bounds for stem-like shapes. Debarked stems are slightly thinner.
pub fn stem_bounds_for_shape(shape: Shape, orientation: Orientation) -> (Vec3, Vec3) {
    let debarked = matches!(shape, Shape::DebarkedStem);
    let up = orientation.up();
    if up.y.abs() > 0.5 {
        return if debarked {
            (Vec3::new(0.28, 0.0, 0.28), Vec3::new(0.72, 1.0, 0.72))
        } else {
            (Vec3::new(0.25, 0.0, 0.25), Vec3::new(0.75, 1.0, 0.75))
        };
    }

    let along = up;
    let positive = (along.x + along.z) > 0.0;
    let (along_min, along_max) = if positive { (0.25, 1.25) } else { (0.75, 1.75) };
    let (perp_min, perp_max) = if debarked {
        (0.78f32, 1.22f32)
    } else {
        (0.75f32, 1.25f32)
    };
    let height = if debarked { 0.44f32 } else { 0.5f32 };
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for &ay in &[0.0f32, height] {
        for &a_along in &[along_min, along_max] {
            for &a_perp in &[perp_min, perp_max] {
                let p = if along.x.abs() > 0.5 {
                    Vec3::new(a_along, ay, a_perp)
                } else {
                    Vec3::new(a_perp, ay, a_along)
                };
                min = min.min(p);
                max = max.max(p);
            }
        }
    }
    (min, max)
}

/// Whether a shape uses geometry offset to the middle of its 1 m world voxel.
/// Stems are included because their bounds are pre-oriented to align with the
/// centred 0.5 m trunk cross-section and may extend outside the 0..1 cell.
pub const fn uses_centered_detail_bounds(shape: Shape) -> bool {
    matches!(
        shape,
        Shape::Pebble
            | Shape::Stick
            | Shape::Trunk
            | Shape::Stem
            | Shape::DebarkedTrunk
            | Shape::DebarkedStem
            | Shape::StoneDeposit
    )
}

/// Up to four cell-local boxes that approximate a shaped voxel's physical
/// collision. The raycast uses exact wedges, but movement collision stays
/// conservative and cheap by composing boxes. Slopes are represented by a
/// short staircase of four slices, which keeps walking predictable.
#[derive(Clone, Copy, Debug)]
pub struct CollisionBoxes {
    min: [Vec3; 4],
    max: [Vec3; 4],
    len: usize,
}

impl CollisionBoxes {
    pub(crate) fn empty() -> Self {
        Self {
            min: [Vec3::ZERO; 4],
            max: [Vec3::ZERO; 4],
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, min: Vec3, max: Vec3) {
        if self.len < self.min.len() {
            self.min[self.len] = min;
            self.max[self.len] = max;
            self.len += 1;
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (Vec3, Vec3)> + '_ {
        (0..self.len).map(|index| (self.min[index], self.max[index]))
    }
}

/// Number of stacked slices used to approximate a slope for movement.
pub const SLOPE_COLLISION_STEPS: usize = 4;

impl Shape {
    /// Physical collision boxes for this shape in a unit cell, already rotated
    /// by `orientation` around the cell centre. The returned bounds are
    /// axis-aligned, which is conservative for rotated half blocks and stairs.
    pub fn collision_boxes(self, orientation: Orientation) -> CollisionBoxes {
        let mut boxes = CollisionBoxes::empty();
        match self {
            Shape::Cube => boxes.push(Vec3::ZERO, Vec3::ONE),
            Shape::HalfBlock => boxes.push(Vec3::ZERO, Vec3::new(1.0, 0.5, 1.0)),
            Shape::Panel => boxes.push(Vec3::ZERO, Vec3::new(1.0, PANEL_HEIGHT, 1.0)),
            Shape::Stairs => {
                boxes.push(Vec3::ZERO, Vec3::new(1.0, 0.5, 1.0));
                boxes.push(Vec3::new(0.0, 0.5, 0.5), Vec3::ONE);
            }
            Shape::Slope => {
                let step = 1.0 / SLOPE_COLLISION_STEPS as f32;
                for index in 0..SLOPE_COLLISION_STEPS {
                    let z0 = index as f32 * step;
                    let z1 = (index + 1) as f32 * step;
                    boxes.push(Vec3::new(0.0, 0.0, z0), Vec3::new(1.0, z1, z1));
                }
            }
            // A trunk is one full 0.5 m detail cube centred in its 1 m world
            // voxel. It is axis-aligned and does not follow build orientations.
            Shape::Trunk | Shape::DebarkedTrunk => {
                let (min, max) = centered_detail_bounds(self);
                boxes.push(min, max);
            }
            // Wood limbs/stems are solid so tree branches, roots and bush cores
            // collide. Loose clutter and foliage remain decorative.
            Shape::Stem | Shape::DebarkedStem => {
                let (min, max) = stem_bounds_for_shape(self, orientation);
                boxes.push(min, max);
            }
            // Stone deposits are solid, mineable natural outcrops.
            Shape::StoneDeposit => {
                for (min, max) in stone_deposit_parts(orientation) {
                    boxes.push(min, max);
                }
            }
            // Foliage has no collision (pass-through) but does have shape bounds
            // for rendering/selection handled elsewhere.
            Shape::Stick | Shape::Pebble | Shape::Foliage | Shape::SmallFoliage => {}
        }

        // Centred natural shapes are already expressed in world-voxel space and
        // must not be reoriented around the storage cell centre.
        if uses_centered_detail_bounds(self) {
            return boxes;
        }

        let rotation = orientation.quat();
        let centre = Vec3::splat(0.5);
        let mut oriented = CollisionBoxes::empty();
        for (min, max) in boxes.iter() {
            let (min, max) = oriented_box_bounds(min, max, rotation, centre);
            oriented.push(min, max);
        }
        oriented
    }
}

/// Axis-aligned bounds of a rotated box.
fn oriented_box_bounds(min: Vec3, max: Vec3, rotation: Quat, pivot: Vec3) -> (Vec3, Vec3) {
    let mut bounds_min = Vec3::splat(f32::INFINITY);
    let mut bounds_max = Vec3::splat(f32::NEG_INFINITY);
    for mask in 0..8 {
        let corner = Vec3::new(
            if mask & 1 != 0 { max.x } else { min.x },
            if mask & 2 != 0 { max.y } else { min.y },
            if mask & 4 != 0 { max.z } else { min.z },
        );
        let rotated = rotation * (corner - pivot) + pivot;
        bounds_min = bounds_min.min(rotated);
        bounds_max = bounds_max.max(rotated);
    }
    (bounds_min, bounds_max)
}

/// Horizontal facing for slopes and stairs. `North` is -Z, matching the
/// player's default forward direction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Facing {
    #[default]
    North = 0,
    East,
    South,
    West,
}

impl Facing {
    pub const ALL: [Facing; 4] = [Facing::North, Facing::East, Facing::South, Facing::West];

    /// Unit vector for this horizontal facing.
    pub const fn direction(self) -> Vec3 {
        match self {
            Facing::North => Vec3::NEG_Z,
            Facing::East => Vec3::X,
            Facing::South => Vec3::Z,
            Facing::West => Vec3::NEG_X,
        }
    }

    /// Pick the nearest cardinal direction from a horizontal look vector.
    pub fn from_direction(direction: Vec3) -> Facing {
        if direction.x.abs() >= direction.z.abs() {
            if direction.x >= 0.0 {
                Facing::East
            } else {
                Facing::West
            }
        } else if direction.z >= 0.0 {
            Facing::South
        } else {
            Facing::North
        }
    }

    /// Rotate this facing around Y by quarter turns clockwise when viewed from above.
    pub const fn rotated_y(self, quarter_turns: u8) -> Facing {
        let i = match self {
            Facing::North => 0i32,
            Facing::East => 1,
            Facing::South => 2,
            Facing::West => 3,
        };
        match (i + (quarter_turns as i32 & 3)) & 3 {
            0 => Facing::North,
            1 => Facing::East,
            2 => Facing::South,
            _ => Facing::West,
        }
    }

    /// The facing whose front points toward the given direction (opposite of from_direction).
    pub fn facing_toward(direction: Vec3) -> Facing {
        Self::from_direction(-direction)
    }
}

/// One of the 24 proper orientations of a voxel.
///
/// The index is `up_axis * 4 + turn`. `up_axis` selects which world axis the
/// canonical +Y points along, and `turn` rotates the canonical forward (-Z)
/// around that up axis in 90 degree steps. This includes sideways orientations,
/// so slopes and stairs can be mounted on walls or laid on their side.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(transparent)]
pub struct Orientation(u8);

impl Orientation {
    pub const ALL: [Orientation; 24] = {
        let mut all = [Orientation(0); 24];
        let mut index = 0;
        while index < 24 {
            all[index] = Orientation(index as u8);
            index += 1;
        }
        all
    };

    /// Up-axis order used by the index: +Y, -Y, +X, -X, +Z, -Z.
    const AXES: [Vec3; 6] = [
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Z,
        Vec3::NEG_Z,
    ];

    pub const fn from_index(index: u8) -> Self {
        Self(index % 24)
    }

    pub const fn index(self) -> u8 {
        self.0
    }

    /// World-space direction the canonical +Y axis points along.
    pub fn up(self) -> Vec3 {
        Self::AXES[(self.0 / 4) as usize]
    }

    /// World-space direction the canonical forward (-Z) axis points along.
    pub fn forward(self) -> Vec3 {
        let up = self.up();
        let reference = if up.y.abs() > 0.5 {
            Vec3::NEG_Z
        } else {
            Vec3::Y
        };
        let turn = (self.0 % 4) as f32 * std::f32::consts::FRAC_PI_2;
        Quat::from_axis_angle(up, turn) * reference
    }

    /// Rotation that maps canonical shape geometry into this orientation.
    pub fn quat(self) -> Quat {
        ORIENTATION_QUATS[self.0 as usize]
    }

    /// Orientation with an upright piece whose front points along `facing`.
    pub fn from_facing(facing: Facing) -> Self {
        Self::from_facing_inverted(facing, false)
    }

    /// Orientation with the piece upright or upside-down and facing `facing`.
    pub fn from_facing_inverted(facing: Facing, inverted: bool) -> Self {
        let up = if inverted { Vec3::NEG_Y } else { Vec3::Y };
        Self::from_up_forward(up, facing.direction())
    }

    pub fn from_up_forward(up: Vec3, forward: Vec3) -> Self {
        Self::ALL
            .into_iter()
            .find(|orientation| {
                orientation.up().dot(up) > 0.5 && orientation.forward().dot(forward) > 0.5
            })
            .unwrap_or_default()
    }

    /// Best-effort horizontal facing derived from the forward axis.
    pub fn facing(self) -> Facing {
        let forward = self.forward();
        if forward.x.abs() < 0.5 && forward.z.abs() < 0.5 {
            Facing::North
        } else {
            Facing::from_direction(forward)
        }
    }

    /// Whether the canonical up axis is pointing downward.
    pub fn inverted(self) -> bool {
        self.up().y < -0.5
    }

    pub fn next(self) -> Self {
        Self((self.0 + 1) % 24)
    }

    /// Next useful orientation for a shape. Cubes do not rotate; half blocks
    /// and panels have six distinct resting faces; slopes and stairs use all
    /// 24.
    pub fn next_for_shape(self, shape: Shape) -> Self {
        match shape {
            Shape::Cube => self,
            Shape::HalfBlock => {
                const HALF_BLOCK_CYCLE: [u8; 6] = [0, 4, 8, 12, 16, 20];
                let current = HALF_BLOCK_CYCLE
                    .iter()
                    .position(|index| *index == self.0)
                    .unwrap_or(0);
                Self(HALF_BLOCK_CYCLE[(current + 1) % HALF_BLOCK_CYCLE.len()])
            }
            Shape::Slope | Shape::Stairs => self.next(),
            Shape::Panel => {
                const PANEL_CYCLE: [u8; 6] = [0, 4, 8, 12, 16, 20];
                let current = PANEL_CYCLE
                    .iter()
                    .position(|index| *index == self.0)
                    .unwrap_or(0);
                Self(PANEL_CYCLE[(current + 1) % PANEL_CYCLE.len()])
            }
            Shape::Stick
            | Shape::Pebble
            | Shape::Foliage
            | Shape::SmallFoliage
            | Shape::Trunk
            | Shape::Stem
            | Shape::DebarkedTrunk
            | Shape::DebarkedStem
            | Shape::StoneDeposit => self,
        }
    }
}

static ORIENTATION_QUATS: LazyLock<[Quat; 24]> = LazyLock::new(|| {
    let mut quats = [Quat::IDENTITY; 24];
    for (index, slot) in quats.iter_mut().enumerate() {
        let orientation = Orientation(index as u8);
        let right = orientation.forward().cross(orientation.up());
        *slot = Quat::from_mat3(&Mat3::from_cols(
            right,
            orientation.up(),
            -orientation.forward(),
        ))
        .normalize();
    }
    quats
});

/// A packed voxel: material + architectural shape + orientation.
///
/// Layout: bits 0..4 material, 4..8 shape, 8..13 orientation. Two bytes per
/// voxel keeps the existing chunk memory profile reasonable. The shape field was
/// widened from three bits when the tree trunk and stem shapes were appended;
/// saved shape indices themselves are unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Voxel(u16);

impl Default for Voxel {
    fn default() -> Self {
        Self::AIR
    }
}

impl Voxel {
    pub const AIR: Voxel = Voxel(0);

    pub fn new(block: Block) -> Self {
        Self::from_oriented_parts(block, Shape::Cube, Orientation::default())
    }

    pub const fn from_oriented_parts(block: Block, shape: Shape, orientation: Orientation) -> Self {
        Voxel((block as u16) | ((shape as u16) << 4) | ((orientation.index() as u16) << 8))
    }

    #[inline]
    pub const fn block(self) -> Block {
        Block::from_index((self.0 & 0x0F) as u8)
    }

    #[inline]
    pub const fn shape(self) -> Shape {
        match (self.0 >> 4) & 0x0F {
            1 => Shape::HalfBlock,
            2 => Shape::Slope,
            3 => Shape::Stairs,
            4 => Shape::Panel,
            5 => Shape::Stick,
            6 => Shape::Pebble,
            7 => Shape::Foliage,
            8 => Shape::Trunk,
            9 => Shape::Stem,
            10 => Shape::SmallFoliage,
            11 => Shape::DebarkedTrunk,
            12 => Shape::DebarkedStem,
            13 => Shape::StoneDeposit,
            _ => Shape::Cube,
        }
    }

    #[inline]
    pub const fn orientation(self) -> Orientation {
        Orientation::from_index(((self.0 >> 8) & 0x1F) as u8)
    }

    #[inline]
    pub fn facing(self) -> Facing {
        self.orientation().facing()
    }

    #[inline]
    pub fn inverted(self) -> bool {
        self.orientation().inverted()
    }

    #[inline]
    pub const fn is_air(self) -> bool {
        self.block().is_air()
    }

    #[inline]
    pub const fn is_solid(self) -> bool {
        self.block().is_solid()
    }

    /// Only full cubes hide neighbouring faces and merge during greedy meshing.
    /// Shaped voxels deliberately leave neighbour faces visible above them.
    #[inline]
    pub const fn is_opaque(self) -> bool {
        self.shape().is_full_cube() && self.block().is_opaque()
    }

    #[inline]
    pub const fn is_water(self) -> bool {
        self.block().is_water()
    }

    #[inline]
    pub const fn is_full_cube(self) -> bool {
        self.shape().is_full_cube()
    }

    /// Replace only the shape, keeping material and orientation.
    pub const fn with_shape(self, shape: Shape) -> Self {
        Self::from_oriented_parts(self.block(), shape, self.orientation())
    }
}

impl From<Block> for Voxel {
    fn from(block: Block) -> Self {
        Voxel::new(block)
    }
}

impl PartialEq<Block> for Voxel {
    fn eq(&self, other: &Block) -> bool {
        self.block() == *other
    }
}

impl PartialEq<Voxel> for Block {
    fn eq(&self, other: &Voxel) -> bool {
        *self == other.block()
    }
}

impl std::fmt::Debug for Voxel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Voxel")
            .field("block", &self.block())
            .field("shape", &self.shape())
            .field("orientation", &self.orientation().index())
            .field("facing", &self.facing())
            .field("inverted", &self.inverted())
            .finish()
    }
}

impl Block {
    #[inline]
    pub const fn is_air(self) -> bool {
        matches!(self, Block::Air)
    }
}

/// Convert an sRGB channel to linear space (the space Bevy's vertex colours use).
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_indices_fit_the_packed_voxel_layout() {
        // Voxel stores the material in the low four bits.
        for block in Block::BUILDABLE {
            assert!(block.index() < 16, "{block:?} cannot be packed");
            assert_eq!(Block::from_index(block.index()), block);
        }
        assert_eq!(Block::RedSandstone.index(), 15);
    }

    #[test]
    fn water_is_not_solid_or_opaque() {
        assert!(!Block::Water.is_solid());
        assert!(!Block::Water.is_opaque());
        assert!(Block::Granite.is_solid());
    }

    #[test]
    fn voxel_round_trips_material_and_shape() {
        let voxel = Voxel::from_oriented_parts(
            Block::Granite,
            Shape::Stairs,
            Orientation::from_facing_inverted(Facing::West, true),
        );
        assert_eq!(voxel.block(), Block::Granite);
        assert_eq!(voxel.shape(), Shape::Stairs);
        assert_eq!(voxel.facing(), Facing::West);
        assert!(voxel.inverted());
        assert!(!voxel.is_opaque());
        assert!(voxel.is_solid());
    }

    #[test]
    fn appended_shapes_round_trip_through_the_packed_voxel() {
        for shape in [Shape::Trunk, Shape::Stem] {
            for orientation in Orientation::ALL {
                let voxel = Voxel::from_oriented_parts(Block::Wood, shape, orientation);
                assert_eq!(voxel.block(), Block::Wood);
                assert_eq!(voxel.shape(), shape);
                assert_eq!(voxel.orientation(), orientation);
                assert_eq!(Shape::from_index(shape.index()), shape);
            }
        }
    }

    #[test]
    fn centered_clutter_sits_in_the_middle_of_its_world_voxel() {
        let (min, max) = centered_detail_bounds(Shape::Pebble);
        let centre = (min + max) * 0.5;
        assert!((centre.x - 1.0).abs() < 1e-3, "Pebble X centre");
        assert!((centre.z - 1.0).abs() < 1e-3, "Pebble Z centre");
        assert!(max.y <= 0.2, "Pebbles should stay low to the ground");

        let (min, max) = centered_detail_bounds(Shape::Stick);
        let centre = (min + max) * 0.5;
        assert!((centre.x - 1.0).abs() < 1e-3, "Stick X centre");
        assert!((centre.z - 1.0).abs() < 1e-3, "Stick Z centre");
        assert!(max.y <= 0.12, "Sticks should stay thin/low");

        let (min, max) = centered_detail_bounds(Shape::Trunk);
        let centre = (min + max) * 0.5;
        assert!((centre.x - 1.0).abs() < 1e-5, "trunk X centre");
        assert!((centre.z - 1.0).abs() < 1e-5, "trunk Z centre");
        assert!((max.x - min.x - 0.5).abs() < 1e-5, "trunk width");
        assert!((max.z - min.z - 0.5).abs() < 1e-5, "trunk depth");
        assert!((max.y - min.y - 1.0).abs() < 1e-5, "trunk height");
    }

    #[test]
    fn stick_and_pebble_parts_stay_within_their_bounds() {
        for index in 0..24u8 {
            let orientation = Orientation::from_index(index);
            let (bounds_min, bounds_max) = centered_detail_bounds(Shape::Stick);
            let parts = stick_parts(orientation);
            assert!(!parts.is_empty(), "stick variant {index} missing parts");
            for (min, max) in parts {
                assert!(min.x >= bounds_min.x - 1e-4 && max.x <= bounds_max.x + 1e-4);
                assert!(min.y >= bounds_min.y - 1e-4 && max.y <= bounds_max.y + 1e-4);
                assert!(min.z >= bounds_min.z - 1e-4 && max.z <= bounds_max.z + 1e-4);
                assert!(max.x > min.x && max.y > min.y && max.z > min.z);
            }

            let (bounds_min, bounds_max) = centered_detail_bounds(Shape::Pebble);
            let parts = pebble_parts(orientation);
            assert!(!parts.is_empty(), "pebble variant {index} missing parts");
            for (min, max) in parts {
                assert!(min.x >= bounds_min.x - 1e-4 && max.x <= bounds_max.x + 1e-4);
                assert!(min.y >= bounds_min.y - 1e-4 && max.y <= bounds_max.y + 1e-4);
                assert!(min.z >= bounds_min.z - 1e-4 && max.z <= bounds_max.z + 1e-4);
                assert!(max.x > min.x && max.y > min.y && max.z > min.z);
            }
        }
    }

    #[test]
    fn stick_variants_have_consistent_overall_dimensions_and_connected_stubs() {
        let mut expected_extent: Option<Vec3> = None;
        for variant in 0..3u8 {
            let orientation = Orientation::from_index(variant); // canonical yaw
            let parts = stick_parts(orientation);
            assert!(parts.len() >= 2, "stick variant {variant} missing stubs");
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for &(a, b) in &parts {
                min = min.min(a);
                max = max.max(b);
            }
            let extent = max - min;
            if let Some(expected) = expected_extent {
                assert!(
                    (extent.x - expected.x).abs() < 1e-3
                        && (extent.y - expected.y).abs() < 1e-3
                        && (extent.z - expected.z).abs() < 1e-3,
                    "stick variant {variant} overall dimensions differ: {extent:?} vs {expected:?}"
                );
            } else {
                expected_extent = Some(extent);
            }

            let main_idx = parts
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    let va = (a.1.x - a.0.x) * (a.1.y - a.0.y) * (a.1.z - a.0.z);
                    let vb = (b.1.x - b.0.x) * (b.1.y - b.0.y) * (b.1.z - b.0.z);
                    va.partial_cmp(&vb).unwrap()
                })
                .unwrap()
                .0;
            let main = parts[main_idx];
            for (i, &part) in parts.iter().enumerate() {
                if i == main_idx {
                    continue;
                }
                let ox = main.1.x.min(part.1.x) - main.0.x.max(part.0.x);
                let oy = main.1.y.min(part.1.y) - main.0.y.max(part.0.y);
                let oz = main.1.z.min(part.1.z) - main.0.z.max(part.0.z);
                assert!(
                    ox > 0.005 && oy > 0.005 && oz > 0.005,
                    "stick variant {variant} stub {i} is not properly connected to main branch"
                );
            }
        }
    }

    #[test]
    fn pebble_variants_form_single_and_piles() {
        let singles = (0..12u8)
            .filter(|i| pebble_variant_count(Orientation::from_index(*i)) == 1)
            .count();
        let pairs = (0..12u8)
            .filter(|i| pebble_variant_count(Orientation::from_index(*i)) == 2)
            .count();
        let trios = (0..12u8)
            .filter(|i| pebble_variant_count(Orientation::from_index(*i)) == 3)
            .count();
        assert_eq!(singles, 4);
        assert_eq!(pairs, 4);
        assert_eq!(trios, 4);
    }

    #[test]
    fn full_cube_occludes_but_shaped_voxel_does_not() {
        assert!(Voxel::new(Block::Granite).is_opaque());
        assert!(
            !Voxel::new(Block::Granite)
                .with_shape(Shape::HalfBlock)
                .is_opaque()
        );
        assert!(!Voxel::new(Block::Water).is_opaque());
    }

    #[test]
    fn facing_follows_cardinal_look_direction() {
        assert_eq!(Facing::from_direction(Vec3::NEG_Z), Facing::North);
        assert_eq!(Facing::from_direction(Vec3::X), Facing::East);
        assert_eq!(Facing::from_direction(Vec3::Z), Facing::South);
        assert_eq!(Facing::from_direction(Vec3::NEG_X), Facing::West);
    }

    #[test]
    fn block_and_voxel_compare_by_material() {
        let voxel = Voxel::from_oriented_parts(
            Block::Granite,
            Shape::Slope,
            Orientation::from_facing_inverted(Facing::South, false),
        );
        assert_eq!(voxel, Block::Granite);
        assert_eq!(Block::Granite, voxel);
        assert_ne!(voxel, Block::Topsoil);
    }

    #[test]
    fn all_orientations_are_distinct_proper_rotations() {
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for orientation in Orientation::ALL {
            let up = orientation.quat() * Vec3::Y;
            let forward = orientation.quat() * Vec3::NEG_Z;

            assert!((up - orientation.up()).length() < 1e-4);
            assert!((forward - orientation.forward()).length() < 1e-4);
            assert!((up.length() - 1.0).abs() < 1e-4);
            assert!((forward.length() - 1.0).abs() < 1e-4);
            assert!(
                up.dot(forward).abs() < 1e-4,
                "up and forward must stay perpendicular"
            );

            let key = [
                up.x.round() as i32,
                up.y.round() as i32,
                up.z.round() as i32,
                forward.x.round() as i32,
                forward.y.round() as i32,
                forward.z.round() as i32,
            ];
            assert!(
                seen.insert(key),
                "orientation {} is a duplicate",
                orientation.index()
            );
        }
        assert_eq!(seen.len(), 24);
    }

    #[test]
    fn half_block_rotation_visits_six_resting_faces() {
        use std::collections::HashSet;

        let mut orientation = Orientation::default();
        let mut ups = HashSet::new();
        for _ in 0..6 {
            let up = orientation.up();
            ups.insert([
                up.x.round() as i32,
                up.y.round() as i32,
                up.z.round() as i32,
            ]);
            orientation = orientation.next_for_shape(Shape::HalfBlock);
        }
        assert_eq!(ups.len(), 6, "half blocks rest on six distinct faces");
        assert_eq!(orientation, Orientation::default());
    }

    #[test]
    fn collision_boxes_follow_each_shape() {
        let cube: Vec<_> = Shape::Cube
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(cube.len(), 1);
        assert_eq!(cube[0].0, Vec3::ZERO);
        assert_eq!(cube[0].1, Vec3::ONE);

        let half: Vec<_> = Shape::HalfBlock
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(half.len(), 1);
        assert!((half[0].1.y - 0.5).abs() < 1e-5);

        let panel: Vec<_> = Shape::Panel
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(panel.len(), 1);
        assert!((panel[0].1.y - PANEL_HEIGHT).abs() < 1e-5);

        let stairs: Vec<_> = Shape::Stairs
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(stairs.len(), 2);
        assert!((stairs[0].1.y - 0.5).abs() < 1e-5);
        assert!((stairs[1].0.y - 0.5).abs() < 1e-5);
        assert!((stairs[1].0.z - 0.5).abs() < 1e-5);

        let slope: Vec<_> = Shape::Slope
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(slope.len(), SLOPE_COLLISION_STEPS);
        assert!((slope[0].0.y - 0.0).abs() < 1e-5);
        assert!((slope[0].1.y - 0.25).abs() < 1e-5);
        assert!((slope.last().unwrap().1.y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn shaped_collision_boxes_stay_inside_the_cell_in_every_orientation() {
        for shape in Shape::ALL {
            for orientation in Orientation::ALL {
                for (min, max) in shape.collision_boxes(orientation).iter() {
                    if uses_centered_detail_bounds(shape) {
                        let limit = 2.0;
                        assert!(
                            min.cmpge(Vec3::splat(-limit - 1e-4)).all()
                                && max.cmple(Vec3::splat(limit + 1e-4)).all(),
                            "{shape:?} orientation {} escaped its centred volume: {min:?}..{max:?}",
                            orientation.index()
                        );
                    } else {
                        assert!(
                            min.cmpge(Vec3::splat(-1e-4)).all()
                                && max.cmple(Vec3::splat(1.0 + 1e-4)).all(),
                            "{shape:?} orientation {} escaped its cell: {min:?}..{max:?}",
                            orientation.index()
                        );
                    }
                    assert!(
                        min.cmple(max).all(),
                        "{shape:?} orientation {} has inverted bounds",
                        orientation.index()
                    );
                }
            }
        }
    }

    #[test]
    fn decorative_natural_shapes_round_trip_and_have_no_collision() {
        for shape in [
            Shape::Stick,
            Shape::Pebble,
            Shape::Foliage,
            Shape::SmallFoliage,
        ] {
            assert!(shape.is_natural());
            assert!(!shape.is_buildable());
            let voxel = Voxel::new(Block::Wood).with_shape(shape);
            assert_eq!(voxel.shape(), shape);
            assert_eq!(
                voxel
                    .shape()
                    .collision_boxes(Orientation::default())
                    .iter()
                    .count(),
                0
            );
        }
        for shape in Shape::BUILDABLE {
            assert!(shape.is_buildable());
            assert!(!shape.is_natural());
        }
    }

    #[test]
    fn stems_are_solid_structural_collision() {
        let upright: Vec<_> = Shape::Stem
            .collision_boxes(Orientation::default())
            .iter()
            .collect();
        assert_eq!(upright.len(), 1, "upright stem should collide");
        assert!((upright[0].0.x - 0.25).abs() < 1e-5);
        assert!((upright[0].0.z - 0.25).abs() < 1e-5);
        assert!((upright[0].1.x - 0.75).abs() < 1e-5);
        assert!((upright[0].1.z - 0.75).abs() < 1e-5);
        assert!((upright[0].0.y - 0.0).abs() < 1e-5);
        assert!((upright[0].1.y - 1.0).abs() < 1e-5);

        // A horizontal limb/root spans the full cell along its facing axis for
        // continuous limbs and keeps a narrow 0.5m vertical/perpendicular section.
        let flat = Orientation::from_up_forward(Vec3::X, Vec3::Y);
        let horizontal: Vec<_> = Shape::Stem.collision_boxes(flat).iter().collect();
        assert_eq!(horizontal.len(), 1, "horizontal stem should collide");
        assert!(
            (horizontal[0].1.x - horizontal[0].0.x - 1.0).abs() < 1e-5,
            "horizontal limb should span full cell along facing axis"
        );
        assert!(
            horizontal[0].1.y - horizontal[0].0.y < 0.6,
            "horizontal limb should keep a narrow vertical cross-section"
        );
        assert!(
            (horizontal[0].0.z - 0.75).abs() < 1e-5 && (horizontal[0].1.z - 1.25).abs() < 1e-5,
            "horizontal limb should align to centred trunk cross-section"
        );
    }

    #[test]
    fn slope_rotation_visits_every_orientation() {
        use std::collections::HashSet;

        let mut orientation = Orientation::default();
        let mut seen = HashSet::new();
        for _ in 0..24 {
            seen.insert(orientation.index());
            orientation = orientation.next_for_shape(Shape::Slope);
        }
        assert_eq!(seen.len(), 24, "slopes must reach sideways orientations");
        assert_eq!(orientation, Orientation::default());
    }
}
