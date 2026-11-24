//! Declarative data for biome selection and terrain generation.
//!
//! This module deliberately contains **data, not world rules**. `WorldGen`
//! evaluates the graphs defined here through a tiny interpreter, then applies
//! the selected biome table to heights, surfaces, geology and vegetation. The
//! tables are plain Rust constants so they are cheap, deterministic and easy to
//! lift into external assets or a visual editor later without touching the chunk
//! layout or save format.

use super::block::Block;
use super::chunk::OCEAN_FLOOR_WORLD;

/// Maximum number of nodes in one generation graph. Graphs are tiny and fixed,
/// so evaluation can use a stack array instead of allocating per column.
pub const MAX_GRAPH_NODES: usize = 32;

/// Number of named noise fields in [`NOISE_FIELDS`].
pub const NOISE_FIELD_COUNT: usize = 3;

/// Indices into [`NOISE_FIELDS`].
pub const RELIEF_NOISE: usize = 0;
pub const OCEAN_NOISE: usize = 1;
pub const FOREST_NOISE: usize = 2;

/// The starting area stays plains while nearby biome cells ramp in. The cradle
/// is smaller than the normal view distance so biome borders are visible from
/// spawn.
pub const PLAINS_CRADLE_RADIUS: f32 = 32.0;
pub const PLAINS_TRANSITION_RADIUS: f32 = 96.0;

/// Biomes are picked per jittered Voronoi cell. A cell is about as wide as one
/// screen of terrain, and neighbouring cells may resolve to the same biome.
pub const BIOME_CELL_SIZE: i32 = 64;
/// Maximum centre offset as a fraction of one cell. Staying below 0.5 means the
/// nearest centre is always in the cell grid around a column.
pub const BIOME_CELL_JITTER: f32 = 0.35;
/// Distance used to soften terrain values across a patch border. Only the
/// surface, geology and vegetation switch discretely at the border.
pub const BIOME_CELL_SMOOTH_RADIUS: f32 = 8.0;
/// Separate hash domains for a cell's biome and its jittered centre.
pub const BIOME_CELL_BIOME_OFFSET: u32 = 0x51ed_270b;
pub const BIOME_CELL_JITTER_OFFSET: u32 = 0x6a09_e667;
/// Relative share of each biome in [`BIOME_CELL_WEIGHTS`] order: plains,
/// forest, desert, mesa, alpine. Forest stays common; alpine stays rare.
pub const BIOME_CELL_WEIGHTS: [u32; 5] = [4, 3, 2, 1, 1];

/// Number of independent noise fields used to carve caves. Two warped 2D fields
/// form the winding tunnels, one drifts their height, one roughens walls and one
/// masks the discrete surface entrances. Rooms are placed by hash so they stay
/// compact instead of becoming broad noise patches.
pub const CAVE_NOISE_FIELD_COUNT: usize = 6;
/// Indices into [`CAVE_NOISE_SEED_OFFSETS`].
pub const CAVE_WORM_A_NOISE: usize = 0;
pub const CAVE_WORM_B_NOISE: usize = 1;
pub const CAVE_LAYER_NOISE: usize = 2;
pub const CAVE_DETAIL_NOISE: usize = 3;
pub const CAVE_MOUTH_NOISE: usize = 4;
pub const CAVE_WARP_NOISE: usize = 5;
/// Separate hash domains so cave shapes never correlate with surface terrain.
pub const CAVE_NOISE_SEED_OFFSETS: [u32; CAVE_NOISE_FIELD_COUNT] = [
    0x2c1b_3c6d,
    0x516e_7f21,
    0x9e37_79b9,
    0x71c5_0a27,
    0xb83d_5e91,
    0xd06f_2b43,
];

/// Deepest cave galleries, measured from the local surface. Worm layers are
/// stacked through this whole depth; short entrance tunnels can reach dry
/// inland terrain, including near spawn.
pub const CAVE_MAX_DEPTH: i32 = 320;
/// Depth over which cave density fades out at the bottom of the network.
pub const CAVE_DEPTH_FADE: i32 = 64;

/// Nominal depth of the shallowest worm layer and vertical spacing between
/// layers. The first layer sits well below the surface so only the separate,
/// narrow entrances can break through.
pub const CAVE_FIRST_LAYER_DEPTH: f64 = 24.0;
pub const CAVE_LAYER_SPACING: f64 = 16.0;
/// Number of stacked tunnel layers from the first layer to the deepest fade.
pub const CAVE_LAYER_COUNT: i32 = 20;
/// Low-frequency height drift for each layer, in metres. Tunnels undulate with
/// the landscape, and adjacent layers sometimes overlap or meet at rooms.
pub const CAVE_LAYER_DRIFT: f64 = 4.0;
pub const CAVE_LAYER_SCALE: f64 = 118.0;

/// Horizontal metres per noise unit for the two worm fields. Zero-contour
/// strips form winding tubes; two scales create natural junctions where they
/// cross instead of one repetitive maze.
pub const CAVE_WORM_A_SCALE: f64 = 48.0;
pub const CAVE_WORM_B_SCALE: f64 = 36.0;
/// Warp field scale and displacement, in metres. It bends both worm fields into
/// longer, less mechanical curves.
pub const CAVE_WARP_SCALE: f64 = 92.0;
pub const CAVE_WARP_METRES: f64 = 14.0;
/// Half-width of a worm path in noise units, and its vertical half-height in
/// metres. Two metre-scale radii give tunnels roughly 6 m across, like a large
/// Minecraft burrow rather than a cavern slab.
pub const CAVE_WORM_WIDTH: f64 = 0.055;
pub const CAVE_WORM_RADIUS: f64 = 3.0;
/// Small field that roughens cave walls without changing the network layout.
pub const CAVE_DETAIL_SCALE: f64 = 10.0;
pub const CAVE_DETAIL_WEIGHT: f64 = 0.035;

/// Rooms are individual spheres placed on worm paths. They are common enough to
/// open up junctions but small enough that the world stays mostly tunnelled.
pub const CAVE_ROOM_CELL_SIZE: i32 = 64;
pub const CAVE_ROOM_CELL_CHANCE: f32 = 0.42;
pub const CAVE_ROOM_MIN_RADIUS: f64 = 4.0;
pub const CAVE_ROOM_RADIUS_RANGE: f64 = 5.0;
/// A room is only accepted when a worm already passes this close in noise units.
pub const CAVE_ROOM_WORM_MARGIN: f64 = -0.08;
/// Maximum room height is needed to keep generated worms below the surface cap.
pub const CAVE_ROOM_MAX_RADIUS: f64 = CAVE_ROOM_MIN_RADIUS + CAVE_ROOM_RADIUS_RANGE;

/// Rare short links join two neighbouring worm layers. They are deliberate cave
/// junctions, not terrain shafts: only a few are placed, they are only a few
/// metres across, and they exist only where both adjacent worms are present.
pub const CAVE_CONNECTOR_CELL_SIZE: i32 = 48;
pub const CAVE_CONNECTOR_CELL_CHANCE: f32 = 0.6;
pub const CAVE_CONNECTOR_MIN_RADIUS: f64 = 2.0;
pub const CAVE_CONNECTOR_RADIUS_RANGE: f64 = 1.5;
pub const CAVE_CONNECTOR_WORM_MARGIN: f64 = 0.0;

/// The starting area widens worms slightly so a new player finds a real gallery
/// nearby without making the whole world porous.
pub const CAVE_SPAWN_WORM_BONUS: f64 = 0.015;
pub const CAVE_SPAWN_WORM_RADIUS: f64 = 224.0;

/// Surface entrances are short slanted tunnels placed one per selected cell.
/// Each one ends at a first-layer worm instead of continuing into the deep
/// crust, so a mouth is a hole in a hillside rather than a vertical pit.
pub const CAVE_MOUTH_CELL_SIZE: i32 = 12;
pub const CAVE_MOUTH_CELL_CHANCE: f32 = 0.20;
pub const CAVE_MOUTH_TUNNEL_RADIUS: f64 = 2.5;
pub const CAVE_MOUTH_TUNNEL_DEPTH: i32 = 36;
/// An entrance site is accepted when a worm lies this far sideways at the first
/// layer. The lateral offset makes the short tunnel slope into the hillside.
pub const CAVE_MOUTH_WORM_RADIUS: i32 = 16;
/// A coarse mask keeps entrances uncommon; spawn gets a stronger bias.
pub const CAVE_MOUTH_NOISE_SCALE: f64 = 96.0;
pub const CAVE_MOUTH_NOISE_MIN: f64 = 0.34;
pub const CAVE_SPAWN_MOUTH_BONUS: f64 = 0.25;
pub const CAVE_SPAWN_MOUTH_RADIUS: f64 = 112.0;

/// Biomes used by the current data tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BiomeId {
    #[default]
    Plains,
    Forest,
    Desert,
    Mesa,
    /// Treeless high-elevation biome; `Alpine` is the real-world biome name.
    Alpine,
}

impl BiomeId {
    /// Stable index into [`BIOMES`] and [`BIOME_CELL_WEIGHTS`].
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Short ASCII label for HUD readouts.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Plains => "Plains",
            Self::Forest => "Forest",
            Self::Desert => "Desert",
            Self::Mesa => "Mesa",
            Self::Alpine => "Alpine",
        }
    }
}

/// Maximum number of ordered rock layers in one biome's geology column. Short
/// tables are padded with zero-thickness strata and stop at the first one.
pub const MAX_GEOLOGY_STRATA: usize = 8;
/// The exposed voxel plus two more use the biome's surface material. Ordered
/// strata start immediately below that cap.
pub const GEOLOGY_SURFACE_CAP: i32 = 3;
/// Wide, low-amplitude noise makes stratum boundaries undulate instead of
/// forming perfectly flat planes.
pub const GEOLOGY_STRATA_SCALE: f64 = 72.0;
pub const GEOLOGY_STRATA_WARP: f32 = 2.5;
pub const GEOLOGY_STRATA_SEED_OFFSET: u32 = 0x7f4a_7c15;

/// One ordered rock layer. Thickness is in 1 m world voxels, measured downward
/// from the bottom of the biome's surface cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeologyStratum {
    pub block: Block,
    pub thickness: i32,
}

/// One biome's contribution to terrain, surface and resource distribution.
#[derive(Clone, Copy, Debug)]
pub struct BiomeDefinition {
    pub id: BiomeId,
    pub min_height: i32,
    pub max_height: i32,
    pub height_base: i32,
    pub height_amplitude: f32,
    pub surface: Block,
    /// Chance that a dry surface of this biome carries a natural turf layer.
    /// Plains is continuous turf; forest is deliberately patchy.
    pub turf_coverage: f32,
    /// Chance that a high-elevation surface is capped with snow instead.
    pub snow_coverage: f32,
    /// Whether this biome supports wild trees and bushes.
    pub flora: bool,
    /// Base tree chance per square metre before forest noise is applied.
    pub tree_base: f32,
    /// Extra tree chance at maximum forest noise.
    pub tree_gain: f32,
    pub tree_height_bonus: i32,
    pub forest_weight: f32,
    /// Inclusive 16-bit hash thresholds used by detail placement.
    pub bush_threshold: u32,
    pub stick_threshold: u32,
    pub pebble_start: u32,
    pub pebble_end: u32,
    /// Loose pebbles are made from this biome's characteristic rock.
    pub pebble: Block,
    /// Ordered underground layers, top first, immediately below the surface cap.
    pub strata: [GeologyStratum; MAX_GEOLOGY_STRATA],
}

/// Biome table. The two new surfaces are appended after the original materials,
/// so the save header and every existing material index stay unchanged.
pub const BIOMES: [BiomeDefinition; 5] = [
    BiomeDefinition {
        id: BiomeId::Plains,
        min_height: -3,
        max_height: 6,
        height_base: 1,
        height_amplitude: 6.0,
        surface: Block::Topsoil,
        turf_coverage: 1.0,
        snow_coverage: 0.0,
        flora: true,
        tree_base: 0.006,
        tree_gain: 0.022,
        tree_height_bonus: 0,
        forest_weight: 1.0,
        bush_threshold: 800,
        stick_threshold: 420,
        pebble_start: 900,
        pebble_end: 2000,
        pebble: Block::Limestone,
        strata: [
            GeologyStratum {
                block: Block::Limestone,
                thickness: 8,
            },
            GeologyStratum {
                block: Block::Shale,
                thickness: 18,
            },
            GeologyStratum {
                block: Block::Sandstone,
                thickness: 12,
            },
            GeologyStratum {
                block: Block::Slate,
                thickness: 20,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 4000,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
        ],
    },
    BiomeDefinition {
        id: BiomeId::Forest,
        min_height: -1,
        max_height: 10,
        height_base: 3,
        height_amplitude: 7.0,
        surface: Block::Humus,
        turf_coverage: 0.35,
        snow_coverage: 0.0,
        flora: true,
        tree_base: 0.045,
        tree_gain: 0.060,
        tree_height_bonus: 2,
        forest_weight: 1.15,
        bush_threshold: 1800,
        stick_threshold: 700,
        pebble_start: 600,
        pebble_end: 1600,
        pebble: Block::Slate,
        strata: [
            GeologyStratum {
                block: Block::Sandstone,
                thickness: 6,
            },
            GeologyStratum {
                block: Block::Shale,
                thickness: 16,
            },
            GeologyStratum {
                block: Block::Limestone,
                thickness: 10,
            },
            GeologyStratum {
                block: Block::Slate,
                thickness: 24,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 4000,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
        ],
    },
    BiomeDefinition {
        id: BiomeId::Desert,
        min_height: 0,
        max_height: 9,
        height_base: 4,
        height_amplitude: 5.0,
        surface: Block::Sand,
        turf_coverage: 0.0,
        snow_coverage: 0.0,
        flora: true,
        tree_base: 0.0015,
        tree_gain: 0.0020,
        tree_height_bonus: 0,
        forest_weight: 0.25,
        bush_threshold: 180,
        stick_threshold: 80,
        pebble_start: 2400,
        pebble_end: 5200,
        pebble: Block::Sandstone,
        strata: [
            GeologyStratum {
                block: Block::Sandstone,
                thickness: 20,
            },
            GeologyStratum {
                block: Block::Limestone,
                thickness: 14,
            },
            GeologyStratum {
                block: Block::Shale,
                thickness: 18,
            },
            GeologyStratum {
                block: Block::Slate,
                thickness: 16,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 4000,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
        ],
    },
    BiomeDefinition {
        id: BiomeId::Mesa,
        min_height: 3,
        max_height: 22,
        height_base: 11,
        height_amplitude: 12.0,
        surface: Block::RedSandstone,
        turf_coverage: 0.0,
        snow_coverage: 0.0,
        flora: true,
        tree_base: 0.0020,
        tree_gain: 0.0030,
        tree_height_bonus: 0,
        forest_weight: 0.2,
        bush_threshold: 220,
        stick_threshold: 100,
        pebble_start: 1800,
        pebble_end: 4200,
        pebble: Block::Sandstone,
        strata: [
            GeologyStratum {
                block: Block::RedSandstone,
                thickness: 10,
            },
            GeologyStratum {
                block: Block::Sandstone,
                thickness: 12,
            },
            GeologyStratum {
                block: Block::Shale,
                thickness: 24,
            },
            GeologyStratum {
                block: Block::Limestone,
                thickness: 16,
            },
            GeologyStratum {
                block: Block::Slate,
                thickness: 18,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 4000,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
        ],
    },
    BiomeDefinition {
        id: BiomeId::Alpine,
        min_height: 6,
        max_height: 34,
        height_base: 16,
        height_amplitude: 18.0,
        surface: Block::Granite,
        turf_coverage: 0.0,
        snow_coverage: 0.22,
        flora: true,
        tree_base: 0.006,
        tree_gain: 0.010,
        tree_height_bonus: 0,
        forest_weight: 0.15,
        bush_threshold: 220,
        stick_threshold: 120,
        pebble_start: 1300,
        pebble_end: 5000,
        pebble: Block::Granite,
        strata: [
            GeologyStratum {
                block: Block::Granite,
                thickness: 32,
            },
            GeologyStratum {
                block: Block::Slate,
                thickness: 28,
            },
            GeologyStratum {
                block: Block::Shale,
                thickness: 18,
            },
            GeologyStratum {
                block: Block::Limestone,
                thickness: 12,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 4000,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
            GeologyStratum {
                block: Block::Granite,
                thickness: 0,
            },
        ],
    },
];

/// Ordered strata for one biome, top layer first. The zero-thickness padding in
/// the table is not part of the geology column.
pub fn geology_strata(biome: BiomeId) -> &'static [GeologyStratum] {
    let strata = &BIOMES[biome.index()].strata;
    let end = strata
        .iter()
        .position(|stratum| stratum.thickness <= 0)
        .unwrap_or(strata.len());
    &strata[..end]
}

/// Noise layer definition. Seed offsets keep existing seeds stable while new
/// fields can be added without disturbing older layers.
#[derive(Clone, Copy, Debug)]
pub struct NoiseDef {
    pub seed_offset: u32,
    pub octaves: usize,
    pub frequency: f64,
    pub persistence: f64,
}

pub const NOISE_FIELDS: [NoiseDef; NOISE_FIELD_COUNT] = [
    // Relief, ocean and forest keep their original offsets, so terrain noise is
    // unchanged by the switch from climate bands to biome cells.
    NoiseDef {
        seed_offset: 0,
        octaves: 3,
        frequency: 0.012,
        persistence: 0.45,
    },
    NoiseDef {
        seed_offset: 505,
        octaves: 3,
        frequency: 0.008,
        persistence: 0.55,
    },
    NoiseDef {
        seed_offset: 404,
        octaves: 3,
        frequency: 0.018,
        persistence: 0.5,
    },
];

/// Values a graph may read from the world or the selected biome.
#[derive(Clone, Copy, Debug)]
pub enum GraphInput {
    Noise(usize),
    ShoreBlend,
    BiomeMinHeight,
    BiomeMaxHeight,
    BiomeHeightBase,
    BiomeHeightAmplitude,
    ForestWeight,
}

/// One operation in the generation graph. Node indices refer to earlier nodes
/// in the same graph, which keeps evaluation allocation-free and deterministic.
// Sub and Smoothstep are retained for the graph evaluator tests and future
// generation graphs even though the current terrain graphs do not use them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum GraphNode {
    Const(f32),
    Input(GraphInput),
    Add(usize, usize),
    Sub(usize, usize),
    Mul(usize, usize),
    Min(usize, usize),
    Max(usize, usize),
    Clamp {
        value: usize,
        min: usize,
        max: usize,
    },
    Smoothstep {
        value: usize,
        edge0: f32,
        edge1: f32,
    },
    Lerp {
        a: usize,
        b: usize,
        t: usize,
    },
}

/// A named expression tree evaluated once per world column.
#[derive(Clone, Copy, Debug)]
pub struct Graph {
    pub nodes: &'static [GraphNode],
    pub output: usize,
}

/// Blend biome relief into the shared shoreline and ocean-floor rules.
pub const HEIGHT_GRAPH: Graph = Graph {
    nodes: &[
        GraphNode::Input(GraphInput::Noise(RELIEF_NOISE)), // 0
        GraphNode::Input(GraphInput::BiomeHeightAmplitude), // 1
        GraphNode::Mul(0, 1),                              // 2
        GraphNode::Input(GraphInput::BiomeHeightBase),     // 3
        GraphNode::Add(2, 3),                              // 4: inland relief
        GraphNode::Input(GraphInput::BiomeMinHeight),      // 5
        GraphNode::Input(GraphInput::BiomeMaxHeight),      // 6
        GraphNode::Clamp {
            value: 4,
            min: 5,
            max: 6,
        }, // 7: clamped inland
        GraphNode::Input(GraphInput::Noise(OCEAN_NOISE)),  // 8
        GraphNode::Const(14.0),                            // 9
        GraphNode::Mul(8, 9),                              // 10
        GraphNode::Const(-18.0),                           // 11
        GraphNode::Add(10, 11),                            // 12: rolling seabed
        GraphNode::Const(OCEAN_FLOOR_WORLD as f32),        // 13
        GraphNode::Max(12, 13),                            // 14
        GraphNode::Const(-4.0),                            // 15
        GraphNode::Min(14, 15),                            // 16: clamped seabed
        GraphNode::Input(GraphInput::ShoreBlend),          // 17
        GraphNode::Lerp { a: 16, b: 7, t: 17 },            // 18: coast blend
        GraphNode::Const(OCEAN_FLOOR_WORLD as f32),        // 19
        GraphNode::Max(18, 19),                            // 20
        GraphNode::Input(GraphInput::BiomeMaxHeight),      // 21
        GraphNode::Min(20, 21),                            // 22
    ],
    output: 22,
};

/// Normalise the forest field and scale it by the selected biome.
pub const FOREST_GRAPH: Graph = Graph {
    nodes: &[
        GraphNode::Input(GraphInput::Noise(FOREST_NOISE)), // 0
        GraphNode::Const(0.5),                             // 1
        GraphNode::Mul(0, 1),                              // 2
        GraphNode::Const(0.5),                             // 3
        GraphNode::Add(2, 3),                              // 4: forest in 0..=1
        GraphNode::Input(GraphInput::ForestWeight),        // 5
        GraphNode::Mul(4, 5),                              // 6
    ],
    output: 6,
};

/// Biome-controlled values handed to the graph interpreter.
#[derive(Clone, Copy, Default)]
pub struct BiomeParams {
    pub min_height: f32,
    pub max_height: f32,
    pub height_base: f32,
    pub height_amplitude: f32,
    pub forest_weight: f32,
}

/// Everything the graph interpreter needs for one world column.
#[derive(Clone, Copy)]
pub struct GraphContext {
    pub noise: [f64; NOISE_FIELD_COUNT],
    pub shore_blend: f32,
    pub biome: BiomeParams,
}

/// Smooth 0..=1 interpolation between two edges.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Evaluate a generation graph against one column's inputs.
pub fn eval_graph(graph: &Graph, context: &GraphContext) -> f32 {
    let mut values = [0.0_f32; MAX_GRAPH_NODES];
    for (index, node) in graph.nodes.iter().enumerate() {
        values[index] = match *node {
            GraphNode::Const(value) => value,
            GraphNode::Input(input) => match input {
                GraphInput::Noise(field) => context.noise[field] as f32,
                GraphInput::ShoreBlend => context.shore_blend,
                GraphInput::BiomeMinHeight => context.biome.min_height,
                GraphInput::BiomeMaxHeight => context.biome.max_height,
                GraphInput::BiomeHeightBase => context.biome.height_base,
                GraphInput::BiomeHeightAmplitude => context.biome.height_amplitude,
                GraphInput::ForestWeight => context.biome.forest_weight,
            },
            GraphNode::Add(a, b) => values[a] + values[b],
            GraphNode::Sub(a, b) => values[a] - values[b],
            GraphNode::Mul(a, b) => values[a] * values[b],
            GraphNode::Min(a, b) => values[a].min(values[b]),
            GraphNode::Max(a, b) => values[a].max(values[b]),
            GraphNode::Clamp { value, min, max } => values[value].clamp(values[min], values[max]),
            GraphNode::Smoothstep {
                value,
                edge0,
                edge1,
            } => smoothstep(edge0, edge1, values[value]),
            GraphNode::Lerp { a, b, t } => values[a] + (values[b] - values[a]) * values[t],
        };
    }
    values[graph.output]
}

/// Total number of weighted slots across all biome cells.
pub const fn biome_cell_weight_total() -> u32 {
    let mut total = 0;
    let mut index = 0;
    while index < BIOME_CELL_WEIGHTS.len() {
        total += BIOME_CELL_WEIGHTS[index];
        index += 1;
    }
    total
}

/// Choose one biome for a Voronoi cell from a deterministic roll. The order is
/// the same as [`BIOMES`], so stable biome indices never change.
pub fn biome_for_cell_roll(roll: u32) -> BiomeId {
    let mut remaining = roll % biome_cell_weight_total();
    for (weight, definition) in BIOME_CELL_WEIGHTS.iter().zip(BIOMES.iter()) {
        if remaining < *weight {
            return definition.id;
        }
        remaining -= *weight;
    }
    BiomeId::Plains
}

/// Resolved biome values for one world column. Discrete choices (surface,
/// geology and turf) come from the dominant cell; numeric rules are blended so
/// height and vegetation density do not jump at a patch border.
#[derive(Clone, Copy, Debug)]
pub struct BiomeSample {
    pub dominant: BiomeId,
    pub min_height: i32,
    pub max_height: i32,
    pub height_base: i32,
    pub height_amplitude: f32,
    pub turf_coverage: f32,
    pub snow_coverage: f32,
    pub tree_base: f32,
    pub tree_gain: f32,
    pub tree_height_bonus: i32,
    pub forest_weight: f32,
    pub bush_threshold: u32,
    pub stick_threshold: u32,
    pub pebble_start: u32,
    pub pebble_end: u32,
}

impl BiomeSample {
    /// Discrete rules come from the dominant cell's table row.
    #[inline]
    pub fn definition(&self) -> &'static BiomeDefinition {
        &BIOMES[self.dominant.index()]
    }

    /// The subset of values needed by the graph interpreter.
    pub fn params(&self) -> BiomeParams {
        BiomeParams {
            min_height: self.min_height as f32,
            max_height: self.max_height as f32,
            height_base: self.height_base as f32,
            height_amplitude: self.height_amplitude,
            forest_weight: self.forest_weight,
        }
    }
}

/// Resolve one biome cell into a concrete sample. All discrete rules come from
/// this table row; only terrain smoothing blends neighbouring samples.
pub fn sample_biome(biome: BiomeId) -> BiomeSample {
    let definition = &BIOMES[biome.index()];
    BiomeSample {
        dominant: definition.id,
        min_height: definition.min_height,
        max_height: definition.max_height,
        height_base: definition.height_base,
        height_amplitude: definition.height_amplitude,
        turf_coverage: definition.turf_coverage,
        snow_coverage: definition.snow_coverage,
        tree_base: definition.tree_base,
        tree_gain: definition.tree_gain,
        tree_height_bonus: definition.tree_height_bonus,
        forest_weight: definition.forest_weight,
        bush_threshold: definition.bush_threshold,
        stick_threshold: definition.stick_threshold,
        pebble_start: definition.pebble_start,
        pebble_end: definition.pebble_end,
    }
}

/// Weighted average of neighbouring cell samples. `dominant` is passed in
/// separately because it always comes from the nearest cell, never the average.
pub fn blend_biome_samples(
    samples: &[(BiomeSample, f32)],
    total_weight: f32,
    dominant: BiomeId,
) -> BiomeSample {
    if total_weight <= 0.0 || samples.is_empty() {
        return sample_biome(dominant);
    }

    let mut min_height = 0.0;
    let mut max_height = 0.0;
    let mut height_base = 0.0;
    let mut height_amplitude = 0.0;
    let mut turf_coverage = 0.0;
    let mut snow_coverage = 0.0;
    let mut tree_base = 0.0;
    let mut tree_gain = 0.0;
    let mut tree_height_bonus = 0.0;
    let mut forest_weight = 0.0;
    let mut bush_threshold = 0.0;
    let mut stick_threshold = 0.0;
    let mut pebble_start = 0.0;
    let mut pebble_end = 0.0;

    for (sample, weight) in samples {
        min_height += sample.min_height as f32 * weight;
        max_height += sample.max_height as f32 * weight;
        height_base += sample.height_base as f32 * weight;
        height_amplitude += sample.height_amplitude * weight;
        turf_coverage += sample.turf_coverage * weight;
        snow_coverage += sample.snow_coverage * weight;
        tree_base += sample.tree_base * weight;
        tree_gain += sample.tree_gain * weight;
        tree_height_bonus += sample.tree_height_bonus as f32 * weight;
        forest_weight += sample.forest_weight * weight;
        bush_threshold += sample.bush_threshold as f32 * weight;
        stick_threshold += sample.stick_threshold as f32 * weight;
        pebble_start += sample.pebble_start as f32 * weight;
        pebble_end += sample.pebble_end as f32 * weight;
    }

    BiomeSample {
        dominant,
        min_height: (min_height / total_weight).round() as i32,
        max_height: (max_height / total_weight).round() as i32,
        height_base: (height_base / total_weight).round() as i32,
        height_amplitude: height_amplitude / total_weight,
        turf_coverage: turf_coverage / total_weight,
        snow_coverage: snow_coverage / total_weight,
        tree_base: tree_base / total_weight,
        tree_gain: tree_gain / total_weight,
        tree_height_bonus: (tree_height_bonus / total_weight).round() as i32,
        forest_weight: forest_weight / total_weight,
        bush_threshold: (bush_threshold / total_weight).round() as u32,
        stick_threshold: (stick_threshold / total_weight).round() as u32,
        pebble_start: (pebble_start / total_weight).round() as u32,
        pebble_end: (pebble_end / total_weight).round() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_evaluator_supports_the_roadmap_operations() {
        const GRAPH: Graph = Graph {
            nodes: &[
                GraphNode::Const(4.0), // 0
                GraphNode::Const(2.0), // 1
                GraphNode::Add(0, 1),  // 2 = 6
                GraphNode::Sub(0, 1),  // 3 = 2
                GraphNode::Mul(2, 3),  // 4 = 12
                GraphNode::Min(0, 1),  // 5 = 2
                GraphNode::Max(0, 1),  // 6 = 4
                GraphNode::Clamp {
                    value: 4,
                    min: 5,
                    max: 0,
                }, // 7 = 4
                GraphNode::Smoothstep {
                    value: 1,
                    edge0: 0.0,
                    edge1: 4.0,
                }, // 8 = 0.5
                GraphNode::Lerp { a: 5, b: 0, t: 8 }, // 9 = 3
            ],
            output: 9,
        };
        let context = GraphContext {
            noise: [0.0; NOISE_FIELD_COUNT],
            shore_blend: 0.0,
            biome: BiomeParams::default(),
        };
        assert_eq!(eval_graph(&GRAPH, &context), 3.0);
    }

    #[test]
    fn biome_labels_are_stable_ascii_names() {
        assert_eq!(BiomeId::Plains.label(), "Plains");
        assert_eq!(BiomeId::Forest.label(), "Forest");
        assert_eq!(BiomeId::Desert.label(), "Desert");
        assert_eq!(BiomeId::Mesa.label(), "Mesa");
        assert_eq!(BiomeId::Alpine.label(), "Alpine");
        assert_eq!(BiomeId::default(), BiomeId::Plains);
    }

    #[test]
    fn biome_cell_rolls_cover_every_biome_with_stable_indices() {
        assert_eq!(biome_cell_weight_total(), 11);
        for (roll, expected) in [
            (0, BiomeId::Plains),
            (3, BiomeId::Plains),
            (4, BiomeId::Forest),
            (6, BiomeId::Forest),
            (7, BiomeId::Desert),
            (8, BiomeId::Desert),
            (9, BiomeId::Mesa),
            (10, BiomeId::Alpine),
            (11, BiomeId::Plains),
            (u32::MAX, BiomeId::Plains),
        ] {
            assert_eq!(biome_for_cell_roll(roll), expected, "roll {roll}");
        }
    }

    #[test]
    fn sample_blending_keeps_the_nearest_cell_discrete() {
        let plains = sample_biome(BiomeId::Plains);
        let alpine = sample_biome(BiomeId::Alpine);
        let blended = blend_biome_samples(&[(plains, 1.0), (alpine, 1.0)], 2.0, BiomeId::Forest);

        assert_eq!(blended.dominant, BiomeId::Forest);
        assert_eq!(blended.definition().surface, Block::Humus);
        assert!(blended.min_height > plains.min_height);
        assert!(blended.min_height < alpine.min_height);
    }

    #[test]
    fn biome_table_uses_real_materials_and_patchy_forest_turf() {
        let plains = sample_biome(BiomeId::Plains);
        let forest = sample_biome(BiomeId::Forest);
        let desert = sample_biome(BiomeId::Desert);
        let mesa = sample_biome(BiomeId::Mesa);
        let alpine = sample_biome(BiomeId::Alpine);

        assert_eq!(plains.definition().surface, Block::Topsoil);
        assert_eq!(plains.turf_coverage, 1.0);
        assert!(plains.definition().flora);

        assert_eq!(forest.definition().surface, Block::Humus);
        assert!((0.0..1.0).contains(&forest.turf_coverage));
        assert!(forest.definition().flora);
        assert!(forest.tree_base > plains.tree_base);
        assert!(forest.bush_threshold > plains.bush_threshold);

        assert_eq!(desert.definition().surface, Block::Sand);
        assert_eq!(desert.turf_coverage, 0.0);
        assert!(desert.definition().flora);

        assert_eq!(mesa.definition().surface, Block::RedSandstone);
        assert_eq!(mesa.turf_coverage, 0.0);
        assert!(mesa.definition().flora);

        assert_eq!(alpine.definition().surface, Block::Granite);
        assert!(alpine.snow_coverage > 0.0);
        assert!(alpine.definition().flora);
    }

    #[test]
    fn geology_tables_are_ordered_stone_layers() {
        for definition in BIOMES {
            let strata = geology_strata(definition.id);
            assert!(
                !strata.is_empty(),
                "{:?} needs at least one stratum",
                definition.id
            );
            assert!(
                strata.iter().all(|stratum| stratum.thickness > 0),
                "{:?} strata must all have positive thickness",
                definition.id
            );
            assert!(
                strata.iter().all(|stratum| matches!(
                    stratum.block,
                    Block::Granite
                        | Block::Slate
                        | Block::Shale
                        | Block::Sandstone
                        | Block::Limestone
                        | Block::RedSandstone
                )),
                "{:?} strata must be stone, not soil or air",
                definition.id
            );
            assert!(
                strata
                    .iter()
                    .any(|stratum| stratum.block != strata[0].block),
                "{:?} should have stacked layers of different rock",
                definition.id
            );
            assert!(
                matches!(
                    definition.pebble,
                    Block::Granite | Block::Slate | Block::Sandstone | Block::Limestone
                ),
                "{:?} pebble must map to a real pebble item",
                definition.id
            );
        }
    }
}
