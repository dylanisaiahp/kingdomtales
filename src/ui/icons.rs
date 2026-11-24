//! Procedural UI icons generated from small pixel patterns.
//!
//! Items, notification tones, and HUD symbols are drawn from code as small
//! nearest-filtered RGBA textures at startup, so the game needs no external art
//! assets. The palette is tuned to the clean slate/gold UI direction while
//! keeping each symbol readable and distinct.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::items::ItemId;
use crate::notifications::NotificationTone;

const ICON_SIZE: usize = 12;
type Pattern = [&'static str; ICON_SIZE];

/// HUD-only icons that are not inventory items or notification tones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HudIcon {
    Portrait,
    Hunger,
    Thirst,
    Stamina,
    Insight,
    Sort,
}

impl HudIcon {
    pub const ALL: [HudIcon; 6] = [
        HudIcon::Portrait,
        HudIcon::Hunger,
        HudIcon::Thirst,
        HudIcon::Stamina,
        HudIcon::Insight,
        HudIcon::Sort,
    ];

    #[inline]
    #[allow(dead_code)]
    pub fn index(self) -> usize {
        match self {
            HudIcon::Portrait => 0,
            HudIcon::Hunger => 1,
            HudIcon::Thirst => 2,
            HudIcon::Stamina => 3,
            HudIcon::Insight => 4,
            HudIcon::Sort => 5,
        }
    }
}

/// One texture handle per item, notification tone, and HUD symbol.
#[derive(Resource)]
pub struct UiIcons {
    items: [Handle<Image>; 40],
    tones: [Handle<Image>; 5],
    hud: [Handle<Image>; 6],
}

impl FromWorld for UiIcons {
    fn from_world(world: &mut World) -> Self {
        let mut images = world.resource_mut::<Assets<Image>>();
        let items = std::array::from_fn(|index| {
            let item = ItemId::ALL[index];
            images.add(build_icon(item_pattern(item), item_tint(item)))
        });
        let tones = std::array::from_fn(|index| {
            let tone = NotificationTone::ALL[index];
            images.add(build_icon(tone_pattern(tone), None))
        });
        let hud = std::array::from_fn(|index| {
            let icon = HudIcon::ALL[index];
            images.add(build_icon(hud_pattern(icon), None))
        });
        Self { items, tones, hud }
    }
}

impl UiIcons {
    #[inline]
    pub fn item(&self, item: ItemId) -> Handle<Image> {
        self.items[item.index()].clone()
    }

    #[inline]
    pub fn tone(&self, tone: NotificationTone) -> Handle<Image> {
        self.tones[tone.index()].clone()
    }

    #[inline]
    pub fn hud(&self, icon: HudIcon) -> Handle<Image> {
        self.hud[icon.index()].clone()
    }
}

fn build_icon(pattern: Pattern, tint: Option<Color>) -> Image {
    let mut data = vec![0_u8; ICON_SIZE * ICON_SIZE * 4];
    for (y, row) in pattern.iter().enumerate() {
        for (x, &symbol) in row.as_bytes().iter().enumerate() {
            let [r, g, b, a] = palette(symbol, tint);
            let offset = (y * ICON_SIZE + x) * 4;
            data[offset..offset + 4].copy_from_slice(&[r, g, b, a]);
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: ICON_SIZE as u32,
            height: ICON_SIZE as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Pebbles use their real ground-drop colors instead of a shared gray palette.
fn item_tint(item: ItemId) -> Option<Color> {
    match item {
        ItemId::GranitePebble
        | ItemId::SlatePebble
        | ItemId::SandstonePebble
        | ItemId::LimestonePebble => Some(item.color()),
        _ => None,
    }
}

/// Apply an optional item color to a neutral icon shade. Untinted symbols keep
/// their fixed palette colour.
fn tinted(tint: Option<Color>, brightness: f32, fallback: [u8; 3]) -> [u8; 4] {
    let [r, g, b] = tint.map_or(fallback, |color| {
        let srgba = color.to_srgba();
        [
            (srgba.red * 255.0).round().clamp(0.0, 255.0) as u8,
            (srgba.green * 255.0).round().clamp(0.0, 255.0) as u8,
            (srgba.blue * 255.0).round().clamp(0.0, 255.0) as u8,
        ]
    });
    let shade = |channel: u8| (f32::from(channel) * brightness).round().clamp(0.0, 255.0) as u8;
    [shade(r), shade(g), shade(b), 255]
}

/// Shared icon palette. `m`, `l` and `d` use the optional per-item tint when
/// present. `A/a` provide warm gold accents matching the UI theme.
fn palette(symbol: u8, tint: Option<Color>) -> [u8; 4] {
    match symbol {
        b'm' => tinted(tint, 1.0, [150, 164, 182]),
        b'l' => tinted(tint, 1.18, [210, 220, 232]),
        b'd' => tinted(tint, 0.72, [88, 102, 120]),
        b'K' => [23, 29, 37, 255],    // near-black detail
        b'W' => [242, 240, 233, 255], // soft white
        b'w' => [216, 214, 206, 255],
        b'g' => [150, 164, 182, 255],
        b'G' => [104, 127, 150, 255], // secondary slate
        b'S' => [196, 208, 220, 255],
        b's' => [120, 136, 154, 255],
        b'A' => [228, 204, 140, 255], // warm gold highlight
        b'a' => [217, 182, 92, 255],  // warm gold accent
        b'R' => [214, 118, 108, 255],
        b'r' => [150, 72, 66, 255],
        b'O' => [214, 156, 92, 255], // hunger
        b'o' => [150, 102, 58, 255],
        b'Y' => [232, 214, 150, 255],
        b'y' => [176, 150, 86, 255],
        b'L' => [245, 236, 190, 255],
        b'N' => [138, 94, 58, 255],
        b'n' => [86, 58, 36, 255],
        b'T' => [198, 166, 118, 255],
        b't' => [142, 114, 78, 255],
        b'E' => [150, 196, 150, 255],
        b'e' => [82, 126, 88, 255],
        b'B' => [126, 168, 196, 255], // thirst / water
        b'b' => [74, 110, 140, 255],
        b'P' => [176, 150, 196, 255],
        b'p' => [110, 88, 136, 255],
        b'C' => [170, 206, 214, 255],
        b'c' => [96, 138, 150, 255],
        b'V' => [176, 198, 150, 255], // stamina
        b'v' => [108, 132, 96, 255],
        _ => [0, 0, 0, 0],
    }
}

fn item_pattern(item: ItemId) -> Pattern {
    match item {
        ItemId::Turf => TURF,
        ItemId::Topsoil => TOPSOIL,
        ItemId::Sand => SAND,
        ItemId::Snow => SNOW,
        ItemId::Wood => WOOD,
        ItemId::Leaves => LEAVES,
        ItemId::Stick => STICK,
        ItemId::Fiber => FIBER,
        ItemId::Clay => CLAY,
        ItemId::Berry => BERRY,
        ItemId::IronOre => IRON_ORE,
        ItemId::Granite => GRANITE,
        ItemId::Slate => SLATE,
        ItemId::Shale => SHALE,
        ItemId::Sandstone => SANDSTONE,
        ItemId::Limestone => LIMESTONE,
        ItemId::GranitePebble => GRANITE_PEBBLE,
        ItemId::SlatePebble => SLATE_PEBBLE,
        ItemId::SandstonePebble => SANDSTONE_PEBBLE,
        ItemId::LimestonePebble => LIMESTONE_PEBBLE,
        ItemId::Hammer => HAMMER,
        ItemId::Wrench => WRENCH,
        ItemId::Pickaxe => PICKAXE,
        ItemId::Axe => AXE,
        ItemId::Shovel => SHOVEL,
        ItemId::Hoe => HOE,
        ItemId::Scythe => SCYTHE,
        ItemId::Lantern => LANTERN,
        ItemId::Magnet => MAGNET,
        ItemId::Humus => HUMUS,
        ItemId::RedSandstone => RED_SANDSTONE,
        ItemId::SharpenedStone => SHARPENED_STONE,
        ItemId::StoneTippedStick => STONE_TIPPED_STICK,
        ItemId::Cordage => CORDAGE,
        ItemId::StoneHandAxe => STONE_HAND_AXE,
        ItemId::BarkScraper => BARK_SCRAPER,
        ItemId::SmallStoneHammer => SMALL_STONE_HAMMER,
        ItemId::Bark => BARK,
        ItemId::Workbench => WORKBENCH,
        ItemId::Furnace => FURNACE,
    }
}

fn tone_pattern(tone: NotificationTone) -> Pattern {
    match tone {
        NotificationTone::Normal => TONE_NORMAL,
        NotificationTone::Rare => TONE_RARE,
        NotificationTone::Info => TONE_INFO,
        NotificationTone::Success => TONE_SUCCESS,
        NotificationTone::Error => TONE_ERROR,
    }
}

fn hud_pattern(icon: HudIcon) -> Pattern {
    match icon {
        HudIcon::Portrait => HUD_PORTRAIT,
        HudIcon::Hunger => HUD_HUNGER,
        HudIcon::Thirst => HUD_THIRST,
        HudIcon::Stamina => HUD_STAMINA,
        HudIcon::Insight => HUD_INSIGHT,
        HudIcon::Sort => HUD_SORT,
    }
}

// --- HUD icons ---

const HUD_PORTRAIT: Pattern = [
    "....GGGG....",
    "...GwwwwG...",
    "..GwWWWWwG..",
    "..GwWKKWwG..",
    "..GwWWWWwG..",
    "...GwwwwG...",
    "..GwwwwwwG..",
    ".GwwwwwwwwG.",
    ".GwWWWWWWwG.",
    "GwwWWWWWWwwG",
    "GwwwwwwwwwwG",
    ".GGGGGGGGGG.",
];

const HUD_HUNGER: Pattern = [
    "............",
    ".....Nn.....",
    "....Nnnn....",
    "...Nnnnn....",
    "..OooNnn....",
    ".OoooonN....",
    ".Oooooo.....",
    "..Ooooo.....",
    "...Ooo......",
    "....o.......",
    "............",
    "............",
];

const HUD_THIRST: Pattern = [
    ".....BB.....",
    ".....BB.....",
    "....BBBB....",
    "....BBBB....",
    "...BBBBBB...",
    "..BbBBBBbB..",
    "..BbBBBBbB..",
    ".BbBBBBBBbB.",
    ".BbBBBBBBbB.",
    "..BbBBBBbB..",
    "...BBBBBB...",
    "....bbbb....",
];

const HUD_STAMINA: Pattern = [
    "......AA....",
    ".....AaA....",
    "....AaA.....",
    "...AaAAAA...",
    "..AaAaaa....",
    "...AAAAa....",
    ".....AaA....",
    "....AaA.....",
    "...AaA......",
    "..AaA.......",
    "...a........",
    "............",
];

const HUD_INSIGHT: Pattern = [
    "............",
    "...GGGGGG...",
    "..GwWWWWwG..",
    ".GwWWWWWWwG.",
    "GwWWaaaaWWwG",
    "GwWWaaaaWWwG",
    ".GwWWWWWWwG.",
    "..GwWWWWwG..",
    "...GGGGGGa..",
    "........aa..",
    ".........a..",
    "............",
];

const HUD_SORT: Pattern = [
    "............",
    "..WWWWWWWW..",
    "..WWWWWWWW..",
    "............",
    "...WWWWWW...",
    "...WWWWWW...",
    ".....AA.....",
    "....WWWW....",
    "....WWWW....",
    "....AAAA....",
    ".....AA.....",
    "............",
];

// --- Items ---

const TURF: Pattern = [
    "....EEEE....",
    "..EEEEEEEE..",
    ".EEEEEEEEEE.",
    "EEeEEEEeEEEE",
    "NNNNNNNNNNNN",
    "NnNNTNNnNNTN",
    "NNNNNNNNNNNN",
    "nNNTNnNNTNnN",
    "NNNNNNNNNNNN",
    "NnNNNNTNNNnN",
    "NNNNNNNNNNNN",
    "nNNNNNNNNNNn",
];

const TOPSOIL: Pattern = [
    "....NNNN....",
    "..NNNNNNNN..",
    ".NNNNNNNNNN.",
    "NNNNNNNNNNNN",
    "NnTNNNNTnNNN",
    "NNNNNNNNNNNN",
    "nNNNnNNNNTNN",
    "NNTNNNNnNNNN",
    "NNNNNNNNNNNN",
    "NnNNTNNNNnNN",
    "NNNNNNNNNNNN",
    ".NNNNNNNNNN.",
];

const SAND: Pattern = [
    "............",
    "..YYYYYYYY..",
    ".YYyYYYYyYY.",
    "YYYYYYYYYYYY",
    "YyYYYYYYyYYY",
    "YYYYYYyYYYYY",
    "YYyYYYYYYYYy",
    "YYYYYYYYYYyY",
    "YyYYYYyYYYYY",
    "YYYYYYYYYYYY",
    ".YYyYYYYyYY.",
    "..YYYYYYYY..",
];

const SNOW: Pattern = [
    "............",
    "..WWWWWWWW..",
    ".WwWWWWWWwW.",
    "WWWWWWWWWWWW",
    "WwWWWWWWWWwW",
    "WWWWwWWwWWWW",
    "WWWWWWWWWWWW",
    "WwWWWWWWWWwW",
    "WWWWWWwWWWWW",
    "WWwWWWWWWwWW",
    ".WwWWWWWWwW.",
    "..WWWWWWWW..",
];

const WOOD: Pattern = [
    "....NnNn....",
    "...NnTTnN...",
    "..NnTTTTnN..",
    "..NnTttTnN..",
    "...NnTTnN...",
    "....NnNn....",
    "....NnNn....",
    "...NnTTnN...",
    "..NnTTTTnN..",
    "..NnTttTnN..",
    "...NnTTnN...",
    "....NnNn....",
];

const LEAVES: Pattern = [
    "....EEEE....",
    "..EEEEEEEE..",
    ".EeEEEEEEeE.",
    "EEEEeEEeEEEE",
    "EEeEEEEEEeEE",
    "EEEEEEEEEEEE",
    ".EeEEeEEeEe.",
    "EEEEEEEEEEEE",
    "EEeEEEEEEeEE",
    "EEEEeEEeEEEE",
    ".EeEEEEEEeE.",
    "..EEEEEEEE..",
];

const STICK: Pattern = [
    "..........N.",
    ".........Nn.",
    "........Nn..",
    ".......Nn...",
    "......Nn....",
    ".....Nn.....",
    "....Nn......",
    "...Nn.......",
    "..Nn........",
    ".Nn.........",
    "Nn..........",
    "n...........",
];

const FIBER: Pattern = [
    "...T....T...",
    "..Tt....tT..",
    "..T.T..T.T..",
    "...Tt..tT...",
    "....T..T....",
    "...Tt..tT...",
    "..T.T..T.T..",
    "..Tt....tT..",
    "...T....T...",
    "..Tt....tT..",
    "..T......T..",
    "...t....t...",
];

const CLAY: Pattern = [
    "............",
    "....oooo....",
    "..ooOOOOoo..",
    ".oOOOOOOOOo.",
    ".oOOOOoOOOo.",
    "oOOOOOOOOOOo",
    "oOOoOOOOoOOo",
    "oOOOOOOOOOOo",
    ".oOOOoOOOOo.",
    ".oOOOOOOOOo.",
    "..ooOOOOoo..",
    "....oooo....",
];

const BERRY: Pattern = [
    ".....EE.....",
    "....EeeE....",
    "...E....E...",
    "....RrrR....",
    "...RrrrRR...",
    "..RrRRrrRR..",
    "..RrRRRrRR..",
    "...RRrrRR...",
    "....RRRR....",
    ".....RR.....",
    "............",
    "............",
];

const IRON_ORE: Pattern = [
    "gggggggggggg",
    "gGGmmmmmmGGg",
    "gGmSSSSSSmGg",
    "gmSSmSSSSSmg",
    "gmmSSSSmSSmg",
    "gmSSSSSSSSmg",
    "gmSSmSSmSSmg",
    "gmSSSSSSSSmg",
    "gGmSSSSSSmGg",
    "gGGmmmmmmGGg",
    "gggGGGGGGggg",
    "gggggggggggg",
];

const GRANITE: Pattern = [
    "mmmmmmmmmmmm",
    "mllmmmmllmmm",
    "mmmmmddmmmmm",
    "mmdmmmmmmmll",
    "mmmmllmmmmmm",
    "mmmmdmmmddmm",
    "mllmmmmmmmmm",
    "mmmmmmmllmmm",
    "mmddmmmmmmmm",
    "mmmmmllmmddm",
    "mllmmmmmmmmm",
    "mmmmmmmmmmmm",
];

const SLATE: Pattern = [
    "gggggggggggg",
    "gSSSSSSSSSSg",
    "gSssssssssSg",
    "gSSSSSSSSSSg",
    "gSSSSSSSSSSg",
    "gSssssssssSg",
    "gSSSSSSSSSSg",
    "gSSSSSSSSSSg",
    "gSssssssssSg",
    "gSSSSSSSSSSg",
    "gSssssssssSg",
    "gggggggggggg",
];

const SHALE: Pattern = [
    "gggggggggggg",
    "gmmmmmmmmmmg",
    "gmddddddddmg",
    "gmmmmmmmmmmg",
    "gmmmmmmmmmmg",
    "gmddddddddmg",
    "gmmmmmmmmmmg",
    "gmmmmmmmmmmg",
    "gmddddddddmg",
    "gmmmmmmmmmmg",
    "gmddddddddmg",
    "gggggggggggg",
];

const SANDSTONE: Pattern = [
    "yyyyyyyyyyyy",
    "yYYYYYYYYYYy",
    "yYyyyyyyyyYy",
    "yYYYYYYYYYYy",
    "yYYyYYYYyYYy",
    "yYYYYYYYYYYy",
    "yYyyyyyyyyYy",
    "yYYYYYYYYYYy",
    "yYYyYYYYyYYy",
    "yYYYYYYYYYYy",
    "yYyyyyyyyyYy",
    "yyyyyyyyyyyy",
];

const LIMESTONE: Pattern = [
    "wwwwwwwwwwww",
    "wWWWWWWWWWWw",
    "wWwwWWWWwwWw",
    "wWWWWWWWWWWw",
    "wWWWwwwwWWWw",
    "wWWWWWWWWWWw",
    "wWwwWWWWwwWw",
    "wWWWWWWWWWWw",
    "wWWWwwwwWWWw",
    "wWWWWWWWWWWw",
    "wWwwWWWWwwWw",
    "wwwwwwwwwwww",
];

const GRANITE_PEBBLE: Pattern = [
    "............",
    "............",
    "....mmmm....",
    "...mllllm...",
    "..mllllllm..",
    "..mllmlllm..",
    "...mllldm...",
    "....mddm....",
    ".....mm.....",
    "............",
    "............",
    "............",
];

const SLATE_PEBBLE: Pattern = [
    "............",
    "............",
    "............",
    "...mmmmmm...",
    "..mllllllm..",
    "..mllmmllm..",
    "...mllldm...",
    "....mddm....",
    ".....mm.....",
    "............",
    "............",
    "............",
];

const SANDSTONE_PEBBLE: Pattern = [
    "............",
    "............",
    "...mmmm.....",
    "..mllllm....",
    ".mllllllm...",
    ".mllmmllm...",
    "..mllldm....",
    "...mddm.....",
    "....mm......",
    "............",
    "............",
    "............",
];

const LIMESTONE_PEBBLE: Pattern = [
    "............",
    "............",
    ".....mm.....",
    "...mlllm....",
    "..mlllllm...",
    "..mllmllm...",
    "...mlddm....",
    "....mmm.....",
    "............",
    "............",
    "............",
    "............",
];

const HAMMER: Pattern = [
    "............",
    "..llllllll..",
    ".lmmmmmmmmg.",
    ".lmmmmmmmmg.",
    "..gggnnGGg..",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....nn.....",
    "............",
];

const WRENCH: Pattern = [
    "....gggg....",
    "...gmmmmg...",
    "...gG..Gg...",
    "...gG..Gg...",
    "....gGGg....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    "....gGGg....",
    "...gG..Gg...",
    "...gmmmmg...",
    "....gggg....",
];

const PICKAXE: Pattern = [
    ".gggg..gggg.",
    "gmmmmggmmmmg",
    "gGGgg..ggGGg",
    "..gg....gg..",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....nn.....",
    "............",
];

const AXE: Pattern = [
    ".....gggg...",
    "...ggmmmmg..",
    "..gmmmmGGg..",
    "..gmmmG.....",
    "..gGg.Nn....",
    "...gg.Nn....",
    "......Nn....",
    "......Nn....",
    "......Nn....",
    "......Nn....",
    "......nn....",
    "............",
];

const SHOVEL: Pattern = [
    ".....gggg...",
    "....gmmmmg..",
    "...gmmmmmmg.",
    "...gmmmmmmg.",
    "....gmmmmg..",
    ".....gGGg...",
    "......Nn....",
    "......Nn....",
    "......Nn....",
    "......Nn....",
    "......nn....",
    "............",
];

const HOE: Pattern = [
    "..gggggggg..",
    ".gmmmmmmmmg.",
    ".gGGGGGGGGg.",
    "......gggg..",
    ".......Nn...",
    ".......Nn...",
    ".......Nn...",
    ".......Nn...",
    ".......Nn...",
    ".......Nn...",
    ".......nn...",
    "............",
];

const SCYTHE: Pattern = [
    "............",
    "..gggggggg..",
    ".gmmmmmmmmg.",
    "gGGGgggggg..",
    "....Nn......",
    "....Nn......",
    "....Nn......",
    "....Nn......",
    "....Nn......",
    "....Nn......",
    "....nn......",
    "............",
];

const LANTERN: Pattern = [
    ".....gg.....",
    "....gGGg....",
    "...gGyyGg...",
    "...gYLLYg...",
    "..gYYLLYYg..",
    "..gYYLLYYg..",
    "..gYyLLyYg..",
    "...gYyyYg...",
    "...gGyyGg...",
    "....gGGg....",
    ".....gg.....",
    ".....gg.....",
];

const MAGNET: Pattern = [
    "...RRRRRR...",
    "..RrrrrrrR..",
    "..Rr....rR..",
    "..Rr....rR..",
    "..Rr....rR..",
    "..Rr....rR..",
    "..BB....BB..",
    "..Bb....bB..",
    "..Bb....bB..",
    "..BBBBBBBB..",
    "...bbbbbb...",
    "............",
];

const HUMUS: Pattern = [
    "............",
    "..nNNNNNNn..",
    ".nNNNttNNNn.",
    "nNNtNNNNtNNn",
    "nNNNNnnNNNNn",
    "nNtNNNNNNtNn",
    "nNNNNttNNNNn",
    "nNNnNNNNnNNn",
    "nNNNNNNNNNNn",
    ".nNNtNNtNNn.",
    "..nNNNNNNn..",
    "............",
];

const RED_SANDSTONE: Pattern = [
    "oooooooooooo",
    "oOOOOOOOOOOo",
    "oOooooooooOo",
    "oOOOOOOOOOOo",
    "oOOoOOOOoOOo",
    "oOOOOOOOOOOo",
    "oOooooooooOo",
    "oOOOOOOOOOOo",
    "oOOoOOOOoOOo",
    "oOOOOOOOOOOo",
    "oOooooooooOo",
    "oooooooooooo",
];

const SHARPENED_STONE: Pattern = [
    "............",
    ".....ll.....",
    "....llll....",
    "...llllll...",
    "..llllmlll..",
    "..lllmmmll..",
    "...llmmll...",
    "...llmdll...",
    "....lddl....",
    ".....dd.....",
    "............",
    "............",
];

const STONE_TIPPED_STICK: Pattern = [
    "........ll..",
    ".......llll.",
    "......llmm..",
    ".....lldd...",
    "....Nn......",
    "...Nn.......",
    "..Nn........",
    ".Nn.........",
    "Nn..........",
    "n...........",
    "............",
    "............",
];

const CORDAGE: Pattern = [
    "............",
    "...TT..TT...",
    "..TttTTttT..",
    "..Tt....tT..",
    "...T....T...",
    "....TTTT....",
    "...T....T...",
    "..Tt....tT..",
    "..TttTTttT..",
    "...TT..TT...",
    "............",
    "............",
];

const STONE_HAND_AXE: Pattern = [
    ".....ggg....",
    "....gmmmg...",
    "...gmmmllg..",
    "...gmmll....",
    "....gg.Nn...",
    ".......Nn...",
    ".......Nn...",
    "......TNn...",
    ".....TTNn...",
    "......Nn....",
    "......nn....",
    "............",
];

const BARK_SCRAPER: Pattern = [
    "............",
    ".....llll...",
    "....llmmll..",
    "...llmmdd...",
    "....lldd....",
    ".....Nn.....",
    "....Nn......",
    "...Nn.......",
    "..Nn........",
    ".Nn.........",
    "nn..........",
    "............",
];

const SMALL_STONE_HAMMER: Pattern = [
    "............",
    "...mmmmmm...",
    "..mllllllm..",
    "..mllmmllm..",
    "...mddddm...",
    "....gNNg....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....Nn.....",
    ".....nn.....",
    "............",
];

const BARK: Pattern = [
    "............",
    "..NNNNNNNN..",
    ".NnTTTTTTnN.",
    ".NTtNNNNtTN.",
    ".NTNNttNNTN.",
    ".NTtNNNNtTN.",
    ".NTNNNNNNTN.",
    ".NTtNNNNtTN.",
    ".NTNNttNNTN.",
    ".NnTTTTTTnN.",
    "..NNNNNNNN..",
    "............",
];

const WORKBENCH: Pattern = [
    "nnnnnnnnnnnn",
    "nTTTTTTTTTTn",
    "nTtTTTTTTtTn",
    "nnnnnnnnnnnn",
    ".nT......Tn.",
    ".nT......Tn.",
    ".nnnnnnnnnn.",
    ".nT......Tn.",
    ".nT......Tn.",
    ".nT......Tn.",
    ".nn......nn.",
    "............",
];

const FURNACE: Pattern = [
    "....nnnn....",
    "...nNNNNn...",
    "..nNNNNNNn..",
    ".nNNNNNNNNn.",
    ".nNNNNNNNNn.",
    ".nNN....NNn.",
    ".nN..OO..Nn.",
    ".nN..OO..Nn.",
    ".nNN....NNn.",
    ".nNNNNNNNNn.",
    ".nnnnnnnnnn.",
    "............",
];

// --- Notification tones ---

const TONE_NORMAL: Pattern = [
    "............",
    ".....aa.....",
    "....aAAa....",
    "...aAAAAa...",
    "..aAAaaAAa..",
    "..aAa..aAa..",
    "...a....a...",
    "............",
    "............",
    "............",
    "............",
    "............",
];

const TONE_RARE: Pattern = [
    "............",
    ".....AA.....",
    "....AAAA....",
    "...AAaaAA...",
    "..AAaaaaAA..",
    "..AAaaaaAA..",
    "...AAaaAA...",
    "....AAAA....",
    ".....AA.....",
    "............",
    "............",
    "............",
];

const TONE_INFO: Pattern = [
    "............",
    "....BBBB....",
    "...BbbbbbB..",
    "..BbbbbbbB..",
    "..BbBBBBbB..",
    "..BbB..BbB..",
    "..BbBBBBbB..",
    "..BbbbbbbB..",
    "...BbbbbbB..",
    "....BBBB....",
    "............",
    "............",
];

const TONE_SUCCESS: Pattern = [
    "............",
    "..........EE",
    ".........EE.",
    "........EE..",
    ".......EE...",
    "..EE..EE....",
    "...EEEE.....",
    "....EE......",
    "............",
    "............",
    "............",
    "............",
];

const TONE_ERROR: Pattern = [
    "............",
    ".....RR.....",
    ".....RR.....",
    ".....RR.....",
    ".....RR.....",
    ".....RR.....",
    ".....RR.....",
    "............",
    ".....RR.....",
    ".....RR.....",
    "............",
    "............",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn assert_pattern(pattern: Pattern, label: &str) {
        const ALLOWED: &[u8] = b".KWwgGSsRrOoYyLNnTtEeBbPpCcmldAaVv";
        assert_eq!(pattern.len(), ICON_SIZE, "{label} row count");
        for row in pattern {
            assert_eq!(row.len(), ICON_SIZE, "{label} column count");
            assert!(
                row.bytes().all(|symbol| ALLOWED.contains(&symbol)),
                "{label} has an unknown symbol"
            );
        }
    }

    #[test]
    fn every_item_has_a_square_pattern() {
        for item in ItemId::ALL {
            assert_pattern(item_pattern(item), item.display_name());
        }
    }

    #[test]
    fn every_tone_has_a_square_pattern() {
        for tone in NotificationTone::ALL {
            assert_pattern(tone_pattern(tone), &format!("{tone:?}"));
        }
    }

    #[test]
    fn every_hud_icon_has_a_square_pattern() {
        for icon in HudIcon::ALL {
            assert_pattern(hud_pattern(icon), &format!("{icon:?}"));
        }
    }

    #[test]
    fn item_patterns_are_distinct() {
        let patterns: HashSet<Pattern> =
            ItemId::ALL.iter().map(|item| item_pattern(*item)).collect();
        assert_eq!(patterns.len(), ItemId::ALL.len());
    }

    #[test]
    fn icons_are_transparent_outside_their_shapes() {
        let image = build_icon(LANTERN, None);
        assert_eq!(
            image.data.as_ref().unwrap().len(),
            ICON_SIZE * ICON_SIZE * 4
        );
        assert_eq!(image.data.as_ref().unwrap()[0..4], [0, 0, 0, 0]);
    }

    #[test]
    fn pebbles_use_their_item_colors() {
        for item in [
            ItemId::GranitePebble,
            ItemId::SlatePebble,
            ItemId::SandstonePebble,
            ItemId::LimestonePebble,
        ] {
            let tint = item_tint(item).expect("pebbles have an item tint");
            let srgba = tint.to_srgba();
            let expected = [
                (srgba.red * 255.0).round().clamp(0.0, 255.0) as u8,
                (srgba.green * 255.0).round().clamp(0.0, 255.0) as u8,
                (srgba.blue * 255.0).round().clamp(0.0, 255.0) as u8,
            ];
            let base = palette(b'm', Some(tint));
            assert_eq!(base[0..3], expected, "{item:?} base color");
            assert_eq!(base[3], 255, "{item:?} base alpha");

            let highlight = palette(b'l', Some(tint));
            let shade = palette(b'd', Some(tint));
            assert!(highlight[0] >= base[0], "{item:?} highlight");
            assert!(shade[0] <= base[0], "{item:?} shade");
        }
    }
}
