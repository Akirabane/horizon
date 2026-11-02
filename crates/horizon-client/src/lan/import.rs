//! `player-import`: a player's record, as `player-export` made it on another
//! server (or the same one), applied to the held player that has just logged in.
//!
//! The record can be applied whole or in parts. A part left out keeps what the
//! held player has: the state this server loaded for it when it logged in (its own
//! file, or a new player's defaults). The parts are named as the operator of a proxy
//! names them, and each is a set of the guest's fields:
//!
//! | part | what it sets |
//! | --- | --- |
//! | `inventory` | the inventory, armour, off hand and the selected slot |
//! | `ender_chest` | the ender chest |
//! | `experience` | the level, the progress, the total and the seed |
//! | `health` | health and absorption, food and saturation, and the body's air, fire, frozen ticks, food exhaustion and timer |
//! | `effects` | the active effects |
//! | `game_mode` | the game mode, the flight and its speed |
//! | `position` | where it stands, how it looks, its velocity, its fall |
//! | `spawn_point` | the respawn point |
//! | `recipe_book` | the recipe book |
//! | `advancements` | the advancements (the caller applies them: they need the game's tree) |
//!
//! What the record holds that no part names (the compound's keys Horizon does not
//! model) is taken when every part is.
use super::{
    guest::{Guest, SavedBody},
    hub::Hub,
    playerdata,
};
use crate::save::Level;
use horizon_world::nbt::{Limits, Tag, read_named};
use serde_json::Value;

/// The parts of a record, in the order the replies list them.
pub const PARTS: [&str; 10] = [
    "inventory",
    "ender_chest",
    "experience",
    "health",
    "effects",
    "game_mode",
    "position",
    "spawn_point",
    "recipe_book",
    "advancements",
];

/// What an import did, for the caller to finish.
#[derive(Debug, PartialEq)]
pub struct Imported {
    /// The parts that were applied, in [`PARTS`] order.
    pub parts: Vec<&'static str>,
    /// The advancements to give the player, when that part was asked for.
    pub advancements: Option<Value>,
    /// Whether the other players are told the player has arrived.
    pub announce: bool,
}

/// What a request of `player-import` asks: the record, the parts (all by default)
/// and whether the world is told (yes by default).
struct Request<'a> {
    record: &'a Value,
    parts: Vec<&'static str>,
    announce: bool,
}

fn read_request(request: &Value) -> Result<Request<'_>, String> {
    let record = request
        .get("record")
        .filter(|r| r.is_object())
        .ok_or("the request needs a record")?;
    let parts = match request.get("parts") {
        None | Some(Value::Null) => PARTS.to_vec(),
        Some(Value::Array(names)) => {
            let mut parts = Vec::new();
            for name in names {
                let name = name.as_str().ok_or("a part is a name")?;
                let part = PARTS
                    .iter()
                    .find(|p| **p == name)
                    .ok_or_else(|| format!("{name:?} is not a part of a record"))?;
                if !parts.contains(part) {
                    parts.push(*part);
                }
            }
            // Always in the order of PARTS, whatever order they were asked in.
            PARTS.into_iter().filter(|p| parts.contains(p)).collect()
        }
        Some(_) => return Err("parts is a list of names".into()),
    };
    let announce = match request.get("announce") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("announce is true or false".into()),
    };
    Ok(Request {
        record,
        parts,
        announce,
    })
}

/// Reads the player compound out of a record, after checking the record is one this
/// server can take for `uuid`.
fn read_compound(record: &Value, uuid: u128) -> Result<Tag, String> {
    match record.get("version").and_then(Value::as_i64) {
        Some(super::arrivals::RECORD_VERSION) => {}
        Some(other) => return Err(format!("record version {other} is not understood")),
        None => return Err("the record has no version".into()),
    }
    let theirs = record.get("data_version").and_then(Value::as_i64);
    if theirs != Some(i64::from(horizon_world::DATA_VERSION)) {
        return Err(format!(
            "the record is of data version {theirs:?}, this server's is {}",
            horizon_world::DATA_VERSION
        ));
    }
    let named = record.get("uuid").and_then(Value::as_str);
    if named.and_then(super::arrivals::parse_uuid) != Some(uuid) {
        return Err("the record is another player's".into());
    }
    let text = record
        .get("player")
        .and_then(Value::as_str)
        .ok_or("the record has no player compound")?;
    let bytes = crate::skins::base64_decode(text).ok_or("the player compound is not base64")?;
    let (named, _) = read_named(&bytes, Limits::saved())
        .map_err(|e| format!("the player compound is not NBT: {e}"))?;
    match named.tag {
        tag @ Tag::Compound(_) => Ok(tag),
        _ => Err("the player compound is not a compound".into()),
    }
}

/// What the held player has now, for the parts that are not imported.
struct Kept {
    inventory: Option<(
        Vec<Option<crate::items::ItemStack>>,
        [Option<crate::items::ItemStack>; 4],
        Option<crate::items::ItemStack>,
        usize,
    )>,
    ender: Option<Vec<Option<crate::items::ItemStack>>>,
    experience: Option<(i32, f32, i32, i32)>,
    health: Option<(f32, Option<f32>, i32, f32)>,
    effects: Option<crate::effects::PlayerEffects>,
    game_mode: Option<(i32, bool, f32)>,
    position: Option<([f64; 3], [f64; 3], f32, f32, f32, bool)>,
    spawn: Option<(Option<[f64; 3]>, Option<(String, [i32; 3], f32, bool)>)>,
    recipes: Option<crate::recipe_book::Settings>,
    /// The body's values: (fall and velocity: position), (air, fire, frozen,
    /// exhaustion, timer: health).
    body: Option<SavedBody>,
    other: Option<(
        Option<Tag>,
        Option<(String, [i32; 3])>,
        bool,
        Vec<String>,
        horizon_world::attachments::Attachments,
    )>,
    dimension: String,
}

impl Kept {
    fn of(g: &Guest, parts: &[&str]) -> Self {
        let has = |p: &str| parts.contains(&p);
        let all = parts.len() == PARTS.len();
        Kept {
            inventory: (!has("inventory")).then(|| {
                (
                    g.inventory.clone(),
                    g.armor.clone(),
                    g.offhand.clone(),
                    g.selected,
                )
            }),
            ender: (!has("ender_chest")).then(|| g.ender_items.clone()),
            experience: (!has("experience")).then_some((
                g.xp_level,
                g.xp_progress,
                g.xp_total,
                g.xp_seed,
            )),
            health: (!has("health")).then_some((
                g.health,
                g.saved_absorption,
                g.food,
                g.saturation,
            )),
            effects: (!has("effects")).then(|| g.effects.clone()),
            game_mode: (!has("game_mode")).then_some((g.game_mode, g.flying, g.flying_speed)),
            position: (!has("position")).then_some((
                g.position,
                g.previous_position,
                g.yaw,
                g.pitch,
                g.head_yaw,
                g.on_ground,
            )),
            spawn: (!has("spawn_point")).then(|| (g.spawn, g.respawn_elsewhere.clone())),
            recipes: (!has("recipe_book")).then(|| g.recipe_book.clone()),
            body: g.loaded_body,
            other: (!all).then(|| {
                (
                    g.saved.clone(),
                    g.last_death.clone(),
                    g.seen_credits,
                    g.post_effects.clone(),
                    g.attachments.clone(),
                )
            }),
            dimension: g.dimension.clone(),
        }
    }

    /// Puts back what the import must not have changed.
    fn restore(self, g: &mut Guest, parts: &[&str]) {
        let has = |p: &str| parts.contains(&p);
        if let Some((inventory, armor, offhand, selected)) = self.inventory {
            (g.inventory, g.armor, g.offhand, g.selected) = (inventory, armor, offhand, selected);
        }
        if let Some(ender) = self.ender {
            g.ender_items = ender;
        }
        if let Some((level, progress, total, seed)) = self.experience {
            (g.xp_level, g.xp_progress, g.xp_total, g.xp_seed) = (level, progress, total, seed);
        }
        if let Some((health, absorption, food, saturation)) = self.health {
            (g.health, g.saved_absorption, g.food, g.saturation) =
                (health, absorption, food, saturation);
        }
        if let Some(effects) = self.effects {
            g.effects = effects;
        }
        if let Some((mode, flying, speed)) = self.game_mode {
            (g.game_mode, g.flying, g.flying_speed) = (mode, flying, speed);
        }
        if let Some((position, previous, yaw, pitch, head, ground)) = self.position {
            (g.position, g.previous_position) = (position, previous);
            (g.yaw, g.pitch, g.head_yaw, g.on_ground) = (yaw, pitch, head, ground);
        }
        if let Some((spawn, elsewhere)) = self.spawn {
            (g.spawn, g.respawn_elsewhere) = (spawn, elsewhere);
        }
        if let Some(recipes) = self.recipes {
            g.recipe_book = recipes;
        }
        // The body mixes the two: its fall and its velocity go with the position, the
        // rest with the health.
        if let (Some(mine), Some(imported)) = (self.body, g.loaded_body) {
            g.loaded_body = Some(SavedBody {
                fall_distance: if has("position") {
                    imported.fall_distance
                } else {
                    mine.fall_distance
                },
                velocity: if has("position") {
                    imported.velocity
                } else {
                    mine.velocity
                },
                fire_ticks: if has("health") {
                    imported.fire_ticks
                } else {
                    mine.fire_ticks
                },
                air: if has("health") {
                    imported.air
                } else {
                    mine.air
                },
                ticks_frozen: if has("health") {
                    imported.ticks_frozen
                } else {
                    mine.ticks_frozen
                },
                food_exhaustion: if has("health") {
                    imported.food_exhaustion
                } else {
                    mine.food_exhaustion
                },
                food_tick_timer: if has("health") {
                    imported.food_tick_timer
                } else {
                    mine.food_tick_timer
                },
            });
        } else if self.body.is_some() && g.loaded_body.is_none() {
            g.loaded_body = self.body;
        }
        if let Some((saved, death, credits, post, attachments)) = self.other {
            (
                g.saved,
                g.last_death,
                g.seen_credits,
                g.post_effects,
                g.attachments,
            ) = (saved, death, credits, post, attachments);
        }
        // The client is in one dimension, and only a login can change it.
        g.dimension = self.dimension;
    }
}

/// Applies the request to the held player `uuid` and lets it into the world. The
/// caller gives the player its advancements (`Imported::advancements`).
///
/// Nothing changes when it refuses: the record is read and checked first, and the
/// guest is touched only once everything is known to be well.
pub fn import_player(hub: &mut Hub, uuid: u128, request: &Value) -> Result<Imported, String> {
    let request = read_request(request)?;
    let tag = read_compound(request.record, uuid)?;
    let Some(i) = hub
        .guests
        .iter()
        .position(|g| g.uuid == uuid && g.held && !g.placed)
    else {
        return Err("no player with that unique id is held".into());
    };
    let mut level = Level::default();
    level.read_player(&tag);
    // A position in another dimension than the client's cannot be taken: the client
    // would have to change its world, and only a login does that.
    if request.parts.contains(&"position") {
        let theirs = playerdata_dimension(&level);
        if theirs != hub.guests[i].dimension {
            return Err(format!(
                "the record is in {theirs}, the held player in {}: the position cannot be taken",
                hub.guests[i].dimension
            ));
        }
    }
    let kept = Kept::of(&hub.guests[i], &request.parts);
    playerdata::apply(&mut hub.guests[i], &level);
    kept.restore(&mut hub.guests[i], &request.parts);
    hub.finish_arrival(i, request.announce);
    let advancements = request
        .parts
        .contains(&"advancements")
        .then(|| request.record.get("advancements").cloned())
        .flatten()
        .filter(|a| !a.is_null());
    Ok(Imported {
        parts: request.parts,
        advancements,
        announce: request.announce,
    })
}

/// The dimension a record names, the way `playerdata::apply` places it.
fn playerdata_dimension(level: &Level) -> String {
    match level.dimension.as_str() {
        d @ ("minecraft:the_nether" | "minecraft:the_end") => d.to_owned(),
        _ => "minecraft:overworld".to_owned(),
    }
}
