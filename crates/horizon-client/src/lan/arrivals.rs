//! Players that are about to arrive from another server, and the rule for how
//! long a server waits for them.
//!
//! A proxy that moves a player between two servers first tells the target
//! server who is coming (`player-expect <uuid>`); the player's next login is then
//! taken in as a *held* player: connected, receiving the world, but not
//! simulated, shown or announced until the hand-over finishes. Nothing here may
//! grow without bound, because a proxy that dies in the middle of a move leaves
//! its expectations and its held players behind: every entry expires, and the
//! table has a size cap.
use super::hub::Hub;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long an expectation lives when the request names no time.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10);
/// The longest an expectation may be asked to live.
pub const MAX_TTL: Duration = Duration::from_secs(60);
/// The most expectations the table keeps at once.
pub const MAX_EXPECTED: usize = 64;
/// Server ticks a held player may wait for its import before the server drops it
/// (one minute at 20 ticks per second).
pub const HELD_MAX_TICKS: u64 = 20 * 60;

/// Why an expectation could not be recorded.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpectError {
    /// The table is full of live expectations.
    Full,
}

/// The players a server was told to expect, by unique id.
#[derive(Debug, Default)]
pub struct Expectations {
    until: HashMap<u128, Instant>,
}

impl Expectations {
    /// Records that `uuid` will log in within `ttl` (cut to [`MAX_TTL`]). A second
    /// request for the same player replaces the first one's deadline. Returns
    /// whether the player was already expected.
    pub fn expect(&mut self, uuid: u128, ttl: Duration, now: Instant) -> Result<bool, ExpectError> {
        self.expire(now);
        if !self.until.contains_key(&uuid) && self.until.len() >= MAX_EXPECTED {
            return Err(ExpectError::Full);
        }
        let deadline = now + ttl.min(MAX_TTL);
        Ok(self.until.insert(uuid, deadline).is_some())
    }

    /// Consumes the expectation of `uuid` if it is still alive: one login takes
    /// it, and no second login of the same player is then held.
    pub fn take(&mut self, uuid: u128, now: Instant) -> bool {
        match self.until.remove(&uuid) {
            Some(deadline) => now < deadline,
            None => false,
        }
    }

    /// Forgets the expectation of `uuid`. Returns whether there was one.
    pub fn forget(&mut self, uuid: u128) -> bool {
        self.until.remove(&uuid).is_some()
    }

    /// Drops the expectations whose time has passed and returns them, so the
    /// caller can log that the player never came.
    pub fn expire(&mut self, now: Instant) -> Vec<u128> {
        let gone: Vec<u128> = self
            .until
            .iter()
            .filter(|(_, deadline)| now >= **deadline)
            .map(|(uuid, _)| *uuid)
            .collect();
        for uuid in &gone {
            self.until.remove(uuid);
        }
        gone
    }

    /// How many expectations are recorded (some may be past their time but not
    /// yet swept).
    pub fn len(&self) -> usize {
        self.until.len()
    }

    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
}

/// Server ticks a player may stay frozen for a hand-over (ten seconds): a proxy
/// that dies in the middle of one must not leave the player stuck.
pub const FROZEN_MAX_TICKS: u64 = 20 * 10;
/// The version of the record `player-export` answers with.
pub const RECORD_VERSION: i64 = 1;

/// Whether a player frozen at tick `since` has been frozen too long at tick `now`.
pub fn frozen_too_long(since: u64, now: u64) -> bool {
    now.saturating_sub(since) >= FROZEN_MAX_TICKS
}

/// Why the connected player `uuid` cannot be exported now, or the index of its
/// guest. A held player is not exportable: it has no state of its own yet.
pub fn check_exportable(hub: &Hub, uuid: u128) -> Result<usize, &'static str> {
    let Some(i) = hub.guests.iter().position(|g| g.uuid == uuid) else {
        return Err("that player is not connected to this server");
    };
    let g = &hub.guests[i];
    if g.held {
        Err("that player is held: it has not been handed over yet")
    } else if !g.placed {
        Err("that player has not finished joining")
    } else if g.vehicle.is_some() {
        Err("that player is riding")
    } else if g.sleeping {
        Err("that player is asleep")
    } else if g.dead {
        Err("that player is dead")
    } else {
        Ok(i)
    }
}

/// The record of the player at guest index `i` (which [`check_exportable`] gave,
/// with its menus closed): the player compound as the server saves it, as the
/// uncompressed NBT of the compound in base64, and the few values a proxy reads
/// without opening it. `advancements` is the JSON the server keeps for the
/// player, or null. With `freeze`, the player's own inputs stop being acted on
/// until it is released, aborted or too old.
pub fn export_record(
    hub: &mut Hub,
    i: usize,
    freeze: bool,
    advancements: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use horizon_world::nbt::{Limits, NamedTag, write_named};
    let g = &hub.guests[i];
    let tag = super::playerdata::compound(g);
    let bytes = write_named(
        &NamedTag {
            name: "".into(),
            tag,
        },
        Limits::saved(),
    )
    .map_err(|e| e.to_string())?;
    let velocity = g
        .motion
        .as_ref()
        .map(|m| [m.velocity.x, m.velocity.y, m.velocity.z])
        .or_else(|| g.loaded_body.map(|b| b.velocity))
        .unwrap_or([0.0; 3]);
    let record = serde_json::json!({
        "version": RECORD_VERSION,
        "data_version": horizon_world::DATA_VERSION,
        "uuid": horizon_protocol::crypto::uuid_string(g.uuid),
        "name": g.name,
        "dimension": g.dimension,
        "position": g.position,
        "rotation": [g.yaw, g.pitch],
        "velocity": velocity,
        "game_mode": g.game_mode,
        "player": super::base64(&bytes),
        "advancements": advancements,
    });
    if freeze {
        let since = hub.ticks;
        hub.guests[i].frozen_since = Some(since);
    }
    Ok(record)
}

/// The log line for a proxy's `player-*` request and what became of it, with its level.
/// `name` is the player's name when the server knows it. Only the player's id and a few
/// words of the outcome are written: never the record, which is large and is the player's.
pub fn request_log(
    verb: &str,
    name: Option<&str>,
    args: &str,
    reply: &serde_json::Value,
) -> (&'static str, String) {
    let mut words = args.split_whitespace();
    let id = words.next().unwrap_or("");
    let who = match (parse_uuid(id), name) {
        (Some(u), Some(name)) => format!("{name} ({})", horizon_protocol::crypto::uuid_string(u)),
        (Some(u), None) => horizon_protocol::crypto::uuid_string(u),
        (None, _) => format!("{id:?}"),
    };
    if let Some(why) = reply.get("error").and_then(serde_json::Value::as_str) {
        return ("WARN", format!("{verb} {who}: refused: {why}"));
    }
    let flag = |key: &str| reply.get(key).and_then(serde_json::Value::as_bool);
    let details = match verb {
        "player-expect" => format!(
            "expected for {} s{}",
            reply["ttl_seconds"],
            if flag("renewed") == Some(true) {
                " (renewed)"
            } else {
                ""
            }
        ),
        "player-export" => format!(
            "record of {} bytes{}",
            reply["record"]["player"]
                .as_str()
                .map_or(0, |p| p.len() / 4 * 3),
            if words.next() == Some("freeze") {
                ", frozen"
            } else {
                ""
            }
        ),
        "player-import" => format!(
            "parts {}, placed {}, announced {}, recentered {}",
            reply["parts"]
                .as_array()
                .map(|p| p
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default(),
            flag("placed").unwrap_or(false),
            flag("announced").unwrap_or(false),
            flag("recentered").unwrap_or(false),
        ),
        "player-confirm" => format!("placed, announced {}", flag("announced").unwrap_or(false)),
        "player-release" => match reply.get("released").and_then(serde_json::Value::as_str) {
            Some("moved") => format!(
                "moved, announced {}, was frozen {}",
                flag("announced").unwrap_or(false),
                flag("was_frozen").unwrap_or(true)
            ),
            other => other.unwrap_or("released").to_owned(),
        },
        "player-drop" => format!(
            "held {}, expected {}",
            flag("held").unwrap_or(false),
            flag("expected").unwrap_or(false)
        ),
        _ => "done".to_owned(),
    };
    ("INFO", format!("{verb} {who}: ok, {details}"))
}

/// The line written when a player is held: it logged in as the player a proxy expected.
pub fn held_line(name: &str) -> String {
    format!("{name} is held: it arrives from another server")
}

/// The line written when a held player is let into the world.
pub fn placed_line(name: &str) -> String {
    format!("{name} placed: it has arrived from another server")
}

/// The line written when a held player's connection ends before it was placed: it was
/// never announced, so it has no "left the game".
pub fn dropped_line(name: &str, reason: &str) -> String {
    format!("{name} was dropped while held: {reason}")
}

/// A unique id as text, with or without dashes.
pub fn parse_uuid(text: &str) -> Option<u128> {
    let plain: String = text.chars().filter(|c| *c != '-').collect();
    if plain.len() != 32 || !plain.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&plain, 16).ok()
}

fn error(message: &str) -> serde_json::Value {
    serde_json::json!({ "error": message })
}

/// `player-expect <uuid> [seconds]`: the player will log in within that time
/// (10 seconds by default, 60 at most) and is then held. The reply says whether
/// an earlier expectation was renewed.
pub fn expect_request(hub: &mut Hub, args: &str, now: Instant) -> serde_json::Value {
    let mut parts = args.split_whitespace();
    let Some(uuid) = parts.next().and_then(parse_uuid) else {
        return error("player-expect needs the player's unique id");
    };
    let ttl = match parts.next() {
        None => DEFAULT_TTL,
        Some(seconds) => match seconds.parse::<u64>() {
            Ok(s) if s > 0 => Duration::from_secs(s),
            _ => return error("the time must be a number of seconds above zero"),
        },
    };
    if parts.next().is_some() {
        return error("player-expect takes a unique id and at most a time");
    }
    if hub.guests.iter().any(|g| g.uuid == uuid) {
        return error("that player is already connected to this server");
    }
    match hub.arrivals.expect(uuid, ttl, now) {
        Ok(renewed) => serde_json::json!({
            "ok": true,
            "renewed": renewed,
            "ttl_seconds": ttl.min(MAX_TTL).as_secs(),
        }),
        Err(ExpectError::Full) => error("too many players are expected at once"),
    }
}

/// `player-drop <uuid>`: forgets the expectation of that player, and
/// disconnects it if it is already held, without a save or a word to anyone.
pub fn drop_request(hub: &mut Hub, args: &str) -> serde_json::Value {
    let Some(uuid) = args.split_whitespace().next().and_then(parse_uuid) else {
        return error("player-drop needs the player's unique id");
    };
    let held = hub.drop_held(uuid);
    let expected = hub.arrivals.forget(uuid);
    if !held && !expected {
        return error("no player with that unique id is held or expected");
    }
    serde_json::json!({ "ok": true, "held": held, "expected": expected })
}

/// `player-confirm <uuid>`: places the held player whose record was imported with
/// `"place": false`, announcing it if the import asked. Until then `player-drop` removes
/// it without a save or a word to anyone.
pub fn confirm_request(hub: &mut Hub, args: &str) -> serde_json::Value {
    let mut parts = args.split_whitespace();
    let Some(uuid) = parts.next().and_then(parse_uuid) else {
        return error("player-confirm needs the player's unique id");
    };
    if parts.next().is_some() {
        return error("player-confirm takes only a unique id");
    }
    match hub.confirm_arrival(uuid) {
        Ok(announced) => serde_json::json!({ "ok": true, "placed": true, "announced": announced }),
        Err(why) => error(why),
    }
}

/// `player-release <uuid> moved [announce]` or `player-release <uuid> abort`: ends the
/// hand-over of a frozen player. `moved` takes it out of the world, saving nothing and, unless
/// `announce`, without the "left the game" line; `abort` lets it go on as it was.
pub fn release_request(hub: &mut Hub, args: &str) -> serde_json::Value {
    let mut parts = args.split_whitespace();
    let Some(uuid) = parts.next().and_then(parse_uuid) else {
        return error("player-release needs the player's unique id");
    };
    let how = parts.next();
    let announce = match (how, parts.next()) {
        (Some("moved"), None) => false,
        (Some("moved"), Some("announce")) => true,
        (Some("abort"), None) => false,
        _ => return error("player-release <uuid> is followed by moved [announce] or abort"),
    };
    if parts.next().is_some() {
        return error("player-release takes a unique id, moved or abort, and at most announce");
    }
    if how == Some("moved") {
        return match hub.release_moved(uuid, announce) {
            Ok(()) => serde_json::json!({ "ok": true, "released": "moved", "announced": announce }),
            Err(why) => error(why),
        };
    }
    match hub
        .guests
        .iter_mut()
        .find(|g| g.uuid == uuid && g.frozen_since.is_some())
    {
        Some(g) => {
            g.frozen_since = None;
            serde_json::json!({ "ok": true, "released": "abort" })
        }
        None => error("that player is not frozen for a hand-over"),
    }
}

/// Whether a held player that joined at tick `since` has waited too long at tick
/// `now` for its import.
pub fn held_too_long(since: u64, now: u64) -> bool {
    now.saturating_sub(since) >= HELD_MAX_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u128 = 0xA;
    const B: u128 = 0xB;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn an_expected_player_is_taken_once_before_the_deadline() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        assert_eq!(e.expect(A, secs(10), t0), Ok(false));
        assert!(e.take(A, t0 + secs(9)));
        // The login consumed it: a second login of the same player is ordinary.
        assert!(!e.take(A, t0 + secs(9)));
        assert!(e.is_empty());
    }

    #[test]
    fn a_login_after_the_deadline_is_not_held() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(10), t0).unwrap();
        // Exactly at the deadline is already too late.
        assert!(!e.take(A, t0 + secs(10)));
        assert!(e.is_empty());
    }

    #[test]
    fn a_player_nobody_expected_is_not_held() {
        let mut e = Expectations::default();
        assert!(!e.take(B, Instant::now()));
    }

    #[test]
    fn asking_again_moves_the_deadline_and_says_so() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(5), t0).unwrap();
        assert_eq!(e.expect(A, secs(30), t0 + secs(4)), Ok(true));
        assert!(e.take(A, t0 + secs(20)));
    }

    #[test]
    fn a_long_wait_is_cut_to_the_maximum() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(3600), t0).unwrap();
        assert!(!e.take(A, t0 + MAX_TTL));
    }

    #[test]
    fn expiry_returns_the_players_that_never_came_and_keeps_the_others() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(5), t0).unwrap();
        e.expect(B, secs(30), t0).unwrap();
        assert_eq!(e.expire(t0 + secs(6)), vec![A]);
        assert_eq!(e.len(), 1);
        assert!(e.take(B, t0 + secs(7)));
    }

    #[test]
    fn the_table_refuses_new_players_when_full_but_expired_ones_make_room() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        for n in 0..MAX_EXPECTED as u128 {
            e.expect(100 + n, secs(10), t0).unwrap();
        }
        assert_eq!(e.expect(A, secs(10), t0), Err(ExpectError::Full));
        // A player already in the table can still be renewed when it is full.
        assert_eq!(e.expect(100, secs(10), t0), Ok(true));
        // Once the old ones are past their time, there is room again.
        assert_eq!(e.expect(A, secs(10), t0 + secs(11)), Ok(false));
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn forgetting_removes_only_that_player() {
        let t0 = Instant::now();
        let mut e = Expectations::default();
        e.expect(A, secs(10), t0).unwrap();
        e.expect(B, secs(10), t0).unwrap();
        assert!(e.forget(A));
        assert!(!e.forget(A));
        assert!(e.take(B, t0));
    }

    #[test]
    fn a_held_player_waits_one_minute_of_ticks() {
        assert!(!held_too_long(100, 100 + 1199));
        assert!(held_too_long(100, 100 + 1200));
        // A clock that went backwards never drops anyone.
        assert!(!held_too_long(500, 100));
    }
}

/// The hub's side: what the other players are told when a player arrives.
#[cfg(test)]
mod hub_tests {
    use crate::lan::{
        Joined, Lan, NetConfig, Out, PlayerSender,
        chunks::ChunkEncoder,
        data,
        guest::Guest,
        hub::{Hub, HubConfig, TickCtx},
    };
    use horizon_protocol::ids::play::clientbound as id;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    fn hub() -> Hub {
        let config = NetConfig {
            announce: false,
            ..NetConfig::lan(0, "ArrivalsTest".into(), 4)
        };
        let mut sync = data::SyncData::empty();
        // The entity types the tracker names: the player, as id 0.
        sync.builtin.registries.insert(
            "minecraft:entity_type".to_string(),
            horizon_protocol::registry::Ids::from_names(vec!["minecraft:player".to_string()]),
        );
        let sync = std::sync::Arc::new(sync);
        let lan = Lan::start_with(sync.clone(), config).unwrap();
        let encoder = ChunkEncoder::new(
            [] as [(std::sync::Arc<str>, bool); 0],
            &horizon_protocol::registry::Builtin::default(),
            &[],
            &[],
        );
        let config = HubConfig {
            default_game_mode: 0,
            guest_commands: false,
            force_game_mode: false,
            hardcore: false,
            view_distance: 4,
            simulation_distance: 4,
            dimension: "minecraft:overworld".into(),
            allow_flight: false,
            seed: 0,
            max_players: 4,
            integrated: false,
            levels: Default::default(),
        };
        Hub::new(lan, sync, encoder, config)
    }

    /// A guest in the overworld and the end of the channel its packets go to.
    fn guest(conn: u32, uuid: u128, name: &str) -> (Guest, Receiver<Out>) {
        let (tx, rx) = channel();
        let mut g = Guest::new(
            Joined {
                conn,
                uuid,
                name: name.into(),
                properties: Vec::new(),
                info: horizon_protocol::packets::configuration::ClientInformation::new(4),
                brand: "vanilla".into(),
                sender: PlayerSender::new(tx, std::sync::Arc::default()),
                latency: std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0)),
                address: "127.0.0.1:25565".parse().unwrap(),
            },
            conn as i32,
            [0.5, 64.0, 0.5],
            4,
        );
        g.dimension = "minecraft:overworld".into();
        (g, rx)
    }

    /// The packets waiting in a channel, in order, each as its id and payload.
    fn packets(rx: &Receiver<Out>) -> Vec<(i32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut one = |body: &[u8]| {
            let mut r = horizon_protocol::buf::Reader::new(body);
            let id = r.varint().unwrap();
            out.push((id, r.rest().to_vec()));
        };
        while let Ok(o) = rx.try_recv() {
            match o {
                Out::Packet(b) => one(&b),
                Out::Packets(bs) => bs.iter().for_each(|b| one(b)),
                _ => {}
            }
        }
        out
    }

    /// The ids of the packets waiting in a channel, in order.
    fn ids(rx: &Receiver<Out>) -> Vec<i32> {
        packets(rx).into_iter().map(|(id, _)| id).collect()
    }

    fn place(hub: &mut Hub, conn: u32) {
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = TickCtx {
            world: &mut world,
            entities: &[],
            next_entity: &mut next,
            spawn: [0.5, 64.0, 0.5],
            difficulty: 2,
            difficulty_locked: false,
            clock_rate: 1.0,
            tick_rate: 20.0,
            tick_frozen: false,
            tick_steps: Default::default(),
            collision: None,
            border: &border,
            streaming: None,
        };
        hub.place_new_player(conn, &mut ctx);
    }

    /// The control: a player nobody expected is announced as always, to the
    /// others by a chat line and a player-list entry.
    #[test]
    fn an_ordinary_login_is_announced_to_the_others() {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (newcomer, mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        place(&mut hub, 2);
        let told = ids(&seen);
        assert!(told.contains(&id::SYSTEM_CHAT), "{told:?}");
        assert!(told.contains(&id::PLAYER_INFO_UPDATE), "{told:?}");
        assert!(hub.guests[1].placed && !hub.guests[1].held);
        assert_eq!(ids(&mine)[0], id::LOGIN);
    }

    /// A player the proxy announced is held: it gets the whole join sequence,
    /// and the player already there is told nothing at all.
    #[test]
    fn an_expected_player_is_held_and_no_one_else_hears_of_it() {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (newcomer, mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        hub.arrivals
            .expect(2, Duration::from_secs(10), Instant::now())
            .unwrap();
        place(&mut hub, 2);
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
        let g = &hub.guests[1];
        assert!(g.held && !g.placed);
        assert!(hub.arrivals.is_empty(), "the login used up the expectation");
        let sent = ids(&mine);
        assert_eq!(sent[0], id::LOGIN);
        for wanted in [id::PLAYER_INFO_UPDATE, id::SET_CHUNK_CACHE_CENTER] {
            assert!(
                sent.contains(&wanted),
                "packet {wanted} missing from {sent:?}"
            );
        }
        // The held player is in its own list but did not announce itself.
        assert!(!sent.contains(&id::SYSTEM_CHAT), "{sent:?}");
    }

    /// An expectation is for one player: another uuid logs in as usual.
    #[test]
    fn the_expectation_of_one_player_does_not_hold_another() {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (newcomer, _mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        hub.arrivals
            .expect(3, Duration::from_secs(10), Instant::now())
            .unwrap();
        place(&mut hub, 2);
        assert!(hub.guests[1].placed && !hub.guests[1].held);
        assert!(ids(&seen).contains(&id::SYSTEM_CHAT));
        assert_eq!(hub.arrivals.len(), 1, "the other player is still expected");
    }

    fn tick_context<'a>(
        world: &'a mut crate::world::World,
        border: &'a crate::worldborder::WorldBorder,
        next: &'a mut u32,
    ) -> TickCtx<'a> {
        TickCtx {
            world,
            entities: &[],
            next_entity: next,
            spawn: [0.5, 64.0, 0.5],
            difficulty: 2,
            difficulty_locked: false,
            clock_rate: 1.0,
            tick_rate: 20.0,
            tick_frozen: false,
            tick_steps: Default::default(),
            collision: None,
            border,
            streaming: None,
        }
    }

    fn is_warn(event: &crate::lan::hub::HostEvent, needle: &str) -> bool {
        matches!(event, crate::lan::hub::HostEvent::Warn(w) if w.contains(needle))
    }

    /// An expectation nobody came for is reported once and forgotten.
    #[test]
    fn an_expectation_nobody_came_for_is_reported_and_swept() {
        let mut hub = hub();
        // Already past its time when the hub next ticks.
        hub.arrivals
            .expect(7, Duration::ZERO, Instant::now())
            .unwrap();
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        let events = hub.poll(&mut ctx);
        assert_eq!(
            events.iter().filter(|e| is_warn(e, "never came")).count(),
            1
        );
        assert!(hub.arrivals.is_empty());
    }

    /// A held player whose hand-over never finishes is dropped after a minute of
    /// ticks, and the players in the world are told nothing about it.
    #[test]
    fn a_held_player_that_is_never_imported_is_dropped_without_a_word() {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (newcomer, mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        hub.arrivals
            .expect(2, Duration::from_secs(10), Instant::now())
            .unwrap();
        place(&mut hub, 2);
        let _ = (ids(&seen), ids(&mine));
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        // Two ticks short of the limit (poll counts one): still waiting.
        hub.ticks = hub.guests[1].held_since + super::HELD_MAX_TICKS - 2;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        let events = hub.poll(&mut ctx);
        assert!(!events.iter().any(|e| is_warn(e, "dropped")));
        // At the limit it is kicked, with a warning in the log.
        let events = hub.poll(&mut ctx);
        assert!(
            events
                .iter()
                .any(|e| is_warn(e, "Newcomer") && is_warn(e, "dropped"))
        );
        assert!(matches!(mine.try_recv(), Ok(Out::Disconnect(_))));
        // The connection closes: the hub removes the guest.
        let gone = hub.remove_guest(1, "Timed out");
        assert!(gone.held && !gone.placed);
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
    }

    /// The control for the one above: when a placed player leaves, the others
    /// are told.
    #[test]
    fn a_placed_player_leaving_is_announced() {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (mut leaver, _mine) = guest(2, 2, "Leaver");
        leaver.placed = true;
        hub.guests.push(leaver);
        hub.remove_guest(1, "bye");
        let told = ids(&seen);
        assert!(told.contains(&id::PLAYER_INFO_REMOVE), "{told:?}");
        assert!(told.contains(&id::SYSTEM_CHAT), "{told:?}");
    }

    /// A held player is sent the chunks and the entities around it, and takes
    /// nothing from the players in the world: they still get their chunks, and
    /// never learn the held player's entity.
    #[test]
    fn a_held_player_sees_the_world_without_the_world_seeing_it() {
        let mut hub = hub();
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        for x in -5..=5 {
            for z in -5..=5 {
                // Only a lit column is sent.
                let mut chunk = crate::chunk::Chunk::empty(x, z);
                chunk.lit = true;
                world.chunks.insert((x, z), std::sync::Arc::new(chunk));
            }
        }
        let border = crate::worldborder::WorldBorder::default();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (newcomer, mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        // A second player in the world, to show that players are tracked at all.
        let (mut other, _other_rx) = guest(3, 3, "Other");
        other.placed = true;
        hub.guests.push(other);
        hub.arrivals
            .expect(2, Duration::from_secs(10), Instant::now())
            .unwrap();
        place(&mut hub, 2);
        // The server has run a few ticks since they joined.
        hub.ticks = 10;
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        let (mut held_chunks, mut watcher_chunks) = (0, 0);
        let (mut held_adds, mut watcher_adds) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            hub.tick_level(&mut ctx);
            hub.end_tick();
            let mine_now = packets(&mine);
            let seen_now = packets(&seen);
            held_chunks += mine_now
                .iter()
                .filter(|p| p.0 == id::LEVEL_CHUNK_WITH_LIGHT)
                .count();
            watcher_chunks += seen_now
                .iter()
                .filter(|p| p.0 == id::LEVEL_CHUNK_WITH_LIGHT)
                .count();
            held_adds.extend(added_of(&mine_now));
            watcher_adds.extend(added_of(&seen_now));
        }
        assert!(
            held_chunks > 0,
            "the held player is sent chunks (held {held_chunks}, watcher {watcher_chunks})"
        );
        assert!(watcher_chunks > 0, "the placed player is still sent chunks");
        assert!(
            watcher_adds.contains(&3),
            "players are tracked at all: {watcher_adds:?}"
        );
        assert!(
            held_adds.contains(&1),
            "the held player sees the watcher: {held_adds:?}"
        );
        assert!(
            !watcher_adds.contains(&2),
            "the watcher must not see the held player: {watcher_adds:?}"
        );
    }

    /// The entity ids in the Add Entity packets among some packets.
    fn added_of(packets: &[(i32, Vec<u8>)]) -> Vec<i32> {
        packets
            .iter()
            .filter(|(i, _)| *i == id::ADD_ENTITY)
            .map(|(_, payload)| {
                horizon_protocol::buf::Reader::new(payload)
                    .varint()
                    .unwrap()
            })
            .collect()
    }

    /// A held player's acknowledgements are handled (they keep the chunks
    /// coming), and nothing else it sends reaches the world.
    #[test]
    fn a_held_player_is_heard_only_when_it_acknowledges() {
        use horizon_protocol::packets::play::{ChunkBatchReceived, MovePlayerPos, Serverbound};
        let mut hub = hub();
        let (newcomer, _mine) = guest(2, 2, "Newcomer");
        hub.guests.push(newcomer);
        hub.arrivals
            .expect(2, Duration::from_secs(10), Instant::now())
            .unwrap();
        place(&mut hub, 2);
        let g = &mut hub.guests[0];
        g.unacknowledged = 3;
        let before = g.position;
        g.inbox.push_back((
            Serverbound::MovePlayerPos(MovePlayerPos {
                x: before[0] + 40.0,
                y: before[1],
                z: before[2],
                flags: 1,
            }),
            10,
        ));
        g.inbox.push_back((
            Serverbound::ChunkBatchReceived(ChunkBatchReceived {
                chunks_per_tick: 12.0,
            }),
            5,
        ));
        g.inbox_bytes = 15;
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        hub.drain_packets(&mut ctx);
        let g = &hub.guests[0];
        assert_eq!(g.position, before, "a held player does not move");
        assert_eq!(g.unacknowledged, 2, "its acknowledgement was handled");
        assert_eq!(g.chunks_per_tick, 12.0);
        assert_eq!(g.inbox_bytes, 0);
    }

    const UUID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const UUID_N: u128 = 0x123e4567_e89b_12d3_a456_426614174000;

    #[test]
    fn a_unique_id_is_read_with_or_without_dashes_and_nothing_else() {
        assert_eq!(super::parse_uuid(UUID), Some(UUID_N));
        assert_eq!(
            super::parse_uuid("123e4567e89b12d3a456426614174000"),
            Some(UUID_N)
        );
        assert_eq!(
            super::parse_uuid("123E4567-E89B-12D3-A456-426614174000"),
            Some(UUID_N)
        );
        assert_eq!(
            super::parse_uuid("123e4567-e89b-12d3-a456-42661417400"),
            None
        );
        assert_eq!(
            super::parse_uuid("123e4567-e89b-12d3-a456-42661417400g"),
            None
        );
        assert_eq!(super::parse_uuid(""), None);
    }

    fn fresh_hub() -> Hub {
        hub()
    }

    #[test]
    fn player_expect_records_the_player_and_says_for_how_long() {
        let mut hub = fresh_hub();
        let now = Instant::now();
        let reply = super::expect_request(&mut hub, UUID, now);
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["renewed"], false);
        assert_eq!(reply["ttl_seconds"], 10);
        // Asking again renews, with the time asked.
        let reply = super::expect_request(&mut hub, &format!("{UUID} 30"), now);
        assert_eq!(
            (reply["renewed"].as_bool(), reply["ttl_seconds"].as_u64()),
            (Some(true), Some(30))
        );
        // A time past the maximum is cut, and the reply says what it became.
        let reply = super::expect_request(&mut hub, &format!("{UUID} 3600"), now);
        assert_eq!(reply["ttl_seconds"], 60);
        assert_eq!(hub.arrivals.len(), 1);
        assert!(hub.arrivals.take(UUID_N, now));
    }

    #[test]
    fn player_expect_refuses_what_it_cannot_do_and_changes_nothing() {
        let mut hub = fresh_hub();
        let now = Instant::now();
        for args in [
            "",
            "not-a-uuid",
            &format!("{UUID} 0"),
            &format!("{UUID} soon"),
            &format!("{UUID} 5 6"),
        ] {
            let reply = super::expect_request(&mut hub, args, now);
            assert!(reply["error"].is_string(), "{args:?} gave {reply}");
        }
        // A player who is on the server already cannot be expected.
        let (online, _rx) = guest(1, UUID_N, "Online");
        hub.guests.push(online);
        let reply = super::expect_request(&mut hub, UUID, now);
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("already connected"),
            "{reply}"
        );
        assert!(hub.arrivals.is_empty());
    }

    #[test]
    fn player_expect_refuses_when_the_table_is_full() {
        let mut hub = fresh_hub();
        let now = Instant::now();
        for n in 0..super::MAX_EXPECTED as u128 {
            let reply = super::expect_request(&mut hub, &format!("{:032x}", 1000 + n), now);
            assert_eq!(reply["ok"], true);
        }
        let reply = super::expect_request(&mut hub, UUID, now);
        assert!(
            reply["error"].as_str().unwrap().contains("too many"),
            "{reply}"
        );
    }

    /// Dropping a held player disconnects it and says nothing to the world.
    #[test]
    fn player_drop_removes_a_held_player_without_a_word_to_anyone() {
        let mut hub = fresh_hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        // The proxy announces the player first; then the player logs in.
        assert_eq!(
            super::expect_request(&mut hub, UUID, Instant::now())["ok"],
            true
        );
        let (newcomer, mine) = guest(2, UUID_N, "Newcomer");
        hub.guests.push(newcomer);
        place(&mut hub, 2);
        let _ = (ids(&seen), ids(&mine));
        let reply = super::drop_request(&mut hub, UUID);
        assert_eq!(
            (reply["held"].as_bool(), reply["expected"].as_bool()),
            (Some(true), Some(false))
        );
        assert!(matches!(mine.try_recv(), Ok(Out::Disconnect(_))));
        // The connection closes and the hub forgets the guest.
        let gone = hub.remove_guest(1, "dropped");
        assert!(gone.held && !gone.placed);
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
    }

    #[test]
    fn player_drop_forgets_an_expectation_and_refuses_a_stranger() {
        let mut hub = fresh_hub();
        super::expect_request(&mut hub, UUID, Instant::now());
        let reply = super::drop_request(&mut hub, UUID);
        assert_eq!(
            (reply["held"].as_bool(), reply["expected"].as_bool()),
            (Some(false), Some(true))
        );
        assert!(hub.arrivals.is_empty());
        let reply = super::drop_request(&mut hub, UUID);
        assert!(reply["error"].is_string(), "{reply}");
        assert!(super::drop_request(&mut hub, "nonsense")["error"].is_string());
    }

    /// A placed player cannot be dropped this way: only a held one.
    #[test]
    fn player_drop_leaves_a_placed_player_alone() {
        let mut hub = fresh_hub();
        let (mut here, rx) = guest(1, UUID_N, "Here");
        here.placed = true;
        hub.guests.push(here);
        let reply = super::drop_request(&mut hub, UUID);
        assert!(reply["error"].is_string(), "{reply}");
        assert!(
            rx.try_recv().is_err(),
            "a placed player must not be disconnected"
        );
    }

    /// The keys a vanilla server writes in every player's file, read with `javap` from the
    /// 26.3 server jar: `Player.addAdditionalSaveData` (Inventory, SelectedItemSlot,
    /// EnderItems, XpLevel, XpP, XpTotal, XpSeed, abilities), `FoodData` (foodLevel,
    /// foodSaturationLevel, foodExhaustionLevel, foodTickTimer), `ServerPlayer` (Dimension),
    /// `Entity.saveWithoutId` (Pos, Motion, Rotation, OnGround, Air, Fire) and
    /// `LivingEntity` (Health, AbsorptionAmount).
    const VANILLA_PLAYER_KEYS: [&str; 21] = [
        "Inventory",
        "SelectedItemSlot",
        "EnderItems",
        "XpLevel",
        "XpP",
        "XpTotal",
        "XpSeed",
        "abilities",
        "foodLevel",
        "foodSaturationLevel",
        "foodExhaustionLevel",
        "foodTickTimer",
        "Dimension",
        "Pos",
        "Motion",
        "Rotation",
        "OnGround",
        "Air",
        "Fire",
        "Health",
        "AbsorptionAmount",
    ];

    /// A guest standing at a known place with known values.
    fn exported_guest() -> (Hub, usize) {
        use crate::items::ItemStack;
        let mut hub = hub();
        let (mut g, _rx) = guest(5, UUID_N, "Alex");
        g.placed = true;
        g.position = [10.5, 70.0, -3.25];
        g.yaw = 90.0;
        g.pitch = 15.0;
        g.health = 14.0;
        g.food = 12;
        g.xp_level = 7;
        g.game_mode = 1;
        g.selected = 3;
        g.inventory[0] = Some(ItemStack {
            item: std::sync::Arc::from("minecraft:stone"),
            count: 5,
            damage: 0,
            extra: None,
        });
        hub.guests.push(g);
        (hub, 0)
    }

    /// Decodes the compound of a record back into its tag.
    fn compound_of(record: &serde_json::Value) -> horizon_world::nbt::Tag {
        use horizon_world::nbt::{Limits, read_named};
        let bytes = crate::skins::base64_decode(record["player"].as_str().unwrap()).unwrap();
        read_named(&bytes, Limits::saved()).unwrap().0.tag
    }

    #[test]
    fn a_player_who_cannot_be_exported_is_refused_with_the_reason() {
        let mut hub = hub();
        let expect = |hub: &Hub| super::check_exportable(hub, UUID_N);
        assert_eq!(
            expect(&hub),
            Err("that player is not connected to this server")
        );
        let (g, _rx) = guest(5, UUID_N, "Alex");
        hub.guests.push(g);
        assert_eq!(expect(&hub), Err("that player has not finished joining"));
        hub.guests[0].placed = true;
        assert_eq!(expect(&hub), Ok(0));
        hub.guests[0].vehicle = Some(77);
        assert_eq!(expect(&hub), Err("that player is riding"));
        hub.guests[0].vehicle = None;
        hub.guests[0].sleeping = true;
        assert_eq!(expect(&hub), Err("that player is asleep"));
        hub.guests[0].sleeping = false;
        hub.guests[0].dead = true;
        assert_eq!(expect(&hub), Err("that player is dead"));
        hub.guests[0].dead = false;
        hub.guests[0].placed = false;
        hub.guests[0].held = true;
        assert_eq!(
            expect(&hub),
            Err("that player is held: it has not been handed over yet")
        );
    }

    #[test]
    fn the_record_carries_what_a_proxy_reads_and_the_vanilla_player_compound() {
        use crate::save::{float, number};
        let (mut hub, i) = exported_guest();
        let record =
            super::export_record(&mut hub, i, false, serde_json::json!({"done": []})).unwrap();
        assert_eq!(record["version"], 1);
        assert_eq!(record["data_version"], horizon_world::DATA_VERSION);
        assert_eq!(record["uuid"], UUID);
        assert_eq!(record["name"], "Alex");
        assert_eq!(record["dimension"], "minecraft:overworld");
        assert_eq!(record["position"], serde_json::json!([10.5, 70.0, -3.25]));
        assert_eq!(record["rotation"], serde_json::json!([90.0, 15.0]));
        assert_eq!(record["game_mode"], 1);
        assert_eq!(record["advancements"], serde_json::json!({"done": []}));
        assert_eq!(hub.guests[i].frozen_since, None, "no freeze was asked for");

        let tag = compound_of(&record);
        for key in VANILLA_PLAYER_KEYS {
            assert!(tag.get(key).is_some(), "the compound has no {key}");
        }
        assert_eq!(number(tag.get("XpLevel")), Some(7));
        assert_eq!(number(tag.get("foodLevel")), Some(12));
        assert_eq!(number(tag.get("SelectedItemSlot")), Some(3));
        assert_eq!(float(tag.get("Health")), Some(14.0));
        let horizon_world::nbt::Tag::String(dimension) = tag.get("Dimension").unwrap() else {
            panic!("Dimension is not a string");
        };
        assert_eq!(dimension.to_string_lossy(), "minecraft:overworld");
        // The stone in the first slot, as vanilla writes a stack: Slot, id, count.
        let horizon_world::nbt::Tag::List(items) = tag.get("Inventory").unwrap() else {
            panic!("Inventory is not a list");
        };
        let first = &items[0];
        assert_eq!(number(first.get("Slot")), Some(0));
        assert_eq!(number(first.get("count")), Some(5));
    }

    /// The values a connected player has now and an old file does not: the ones the
    /// save-gaps work added reach the record.
    #[test]
    fn the_record_has_the_live_values_of_the_body() {
        use crate::save::{float, number};
        let (mut hub, i) = exported_guest();
        hub.guests[i].loaded_body = Some(crate::lan::guest::SavedBody {
            fire_ticks: -20,
            air: 300,
            fall_distance: 0.0,
            velocity: [0.125, -0.5, 0.25],
            food_exhaustion: 2.5,
            food_tick_timer: 33,
            ticks_frozen: 40,
        });
        let record = super::export_record(&mut hub, i, false, serde_json::Value::Null).unwrap();
        assert_eq!(record["velocity"], serde_json::json!([0.125, -0.5, 0.25]));
        assert_eq!(record["advancements"], serde_json::Value::Null);
        let tag = compound_of(&record);
        assert_eq!(float(tag.get("foodExhaustionLevel")), Some(2.5));
        assert_eq!(number(tag.get("foodTickTimer")), Some(33));
        assert_eq!(number(tag.get("TicksFrozen")), Some(40));
    }

    /// A frozen player's inputs are not acted on, its acknowledgements are, and a freeze
    /// nobody finishes is let go.
    #[test]
    fn a_frozen_player_is_inert_until_it_is_let_go() {
        use horizon_protocol::packets::play::{ChunkBatchReceived, MovePlayerPos, Serverbound};
        let (mut hub, i) = exported_guest();
        super::export_record(&mut hub, i, true, serde_json::Value::Null).unwrap();
        assert_eq!(hub.guests[i].frozen_since, Some(hub.ticks));
        let before = hub.guests[i].position;
        let g = &mut hub.guests[i];
        g.unacknowledged = 2;
        g.inbox.push_back((
            Serverbound::MovePlayerPos(MovePlayerPos {
                x: before[0] + 30.0,
                y: before[1],
                z: before[2],
                flags: 1,
            }),
            10,
        ));
        g.inbox.push_back((
            Serverbound::ChunkBatchReceived(ChunkBatchReceived {
                chunks_per_tick: 9.0,
            }),
            5,
        ));
        g.inbox_bytes = 15;
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        hub.drain_packets(&mut ctx);
        assert_eq!(
            hub.guests[i].position, before,
            "a frozen player does not move"
        );
        assert_eq!(
            hub.guests[i].unacknowledged, 1,
            "its acknowledgement was handled"
        );
        // Nobody finishes the hand-over: ten seconds of ticks later it is let go.
        hub.ticks = hub.guests[i].frozen_since.unwrap() + super::FROZEN_MAX_TICKS - 2;
        assert!(!hub.poll(&mut ctx).iter().any(|e| is_warn(e, "frozen")));
        assert!(hub.guests[i].frozen_since.is_some());
        assert!(hub.poll(&mut ctx).iter().any(|e| is_warn(e, "frozen")));
        assert_eq!(hub.guests[i].frozen_since, None);
    }

    #[test]
    fn the_freeze_lasts_ten_seconds_of_ticks() {
        assert!(!super::frozen_too_long(10, 10 + 199));
        assert!(super::frozen_too_long(10, 10 + 200));
        assert!(!super::frozen_too_long(500, 100));
    }

    // ---- player-import ------------------------------------------------------

    /// The record of the exported guest, and a second hub in which the same player is held
    /// (with a placed watcher), ready to take it.
    fn held_in_a_second_hub() -> (serde_json::Value, Hub, Receiver<Out>, Receiver<Out>) {
        let (mut source, i) = exported_guest();
        let record = super::export_record(&mut source, i, false, serde_json::Value::Null).unwrap();
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        assert_eq!(
            super::expect_request(&mut hub, UUID, Instant::now())["ok"],
            true
        );
        let (newcomer, mine) = guest(2, UUID_N, "Alex");
        hub.guests.push(newcomer);
        place(&mut hub, 2);
        let _ = (ids(&seen), ids(&mine));
        (record, hub, seen, mine)
    }

    use crate::lan::import::import_player;

    #[test]
    fn a_whole_record_makes_the_held_player_the_player_it_was_exported_from() {
        let (record, mut hub, seen, mine) = held_in_a_second_hub();
        let (source, i) = exported_guest();
        let done =
            import_player(&mut hub, UUID_N, &serde_json::json!({ "record": record })).unwrap();
        assert_eq!(done.parts.len(), crate::lan::import::PARTS.len());
        assert!(done.announce);
        let g = &hub.guests[1];
        assert!(g.placed && !g.held && g.frozen_since.is_none());
        // The whole player: the compound a save would write is the same one.
        assert_eq!(
            crate::lan::playerdata::compound(g),
            crate::lan::playerdata::compound(&source.guests[i])
        );
        assert_eq!(g.position, [10.5, 70.0, -3.25]);
        assert_eq!((g.xp_level, g.food, g.game_mode, g.selected), (7, 12, 1, 3));
        // The player already there is told it has come, as for a login.
        let told = ids(&seen);
        assert!(told.contains(&id::SYSTEM_CHAT), "{told:?}");
        assert!(told.contains(&id::PLAYER_INFO_UPDATE), "{told:?}");
        // And the player is told what the import changed.
        let sent = ids(&mine);
        for wanted in [
            id::GAME_EVENT,
            id::PLAYER_ABILITIES,
            id::SET_HELD_SLOT,
            id::PLAYER_POSITION,
        ] {
            assert!(
                sent.contains(&wanted),
                "packet {wanted} missing from {sent:?}"
            );
        }
    }

    #[test]
    fn a_part_that_is_not_asked_for_keeps_what_the_held_player_has() {
        let (record, mut hub, _seen, _mine) = held_in_a_second_hub();
        let before = (
            hub.guests[1].position,
            hub.guests[1].health,
            hub.guests[1].xp_level,
            hub.guests[1].game_mode,
        );
        let request =
            serde_json::json!({ "record": record, "parts": ["inventory", "ender_chest"] });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert_eq!(done.parts, vec!["inventory", "ender_chest"]);
        let g = &hub.guests[1];
        // Only the inventory came; the position, health, experience and mode are the held player's own.
        assert_eq!((g.position, g.health, g.xp_level, g.game_mode), before);
        assert!(
            g.inventory[0].is_some(),
            "the stone came with the inventory"
        );
        assert_eq!(g.selected, 3);
        assert!(
            g.placed && !g.held,
            "a partial import still lets the player in"
        );
    }

    #[test]
    fn the_parts_come_in_a_fixed_order_whatever_order_they_were_asked_in() {
        let (record, mut hub, _seen, _mine) = held_in_a_second_hub();
        let request = serde_json::json!({ "record": record, "parts": ["position", "inventory", "inventory"] });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert_eq!(done.parts, vec!["inventory", "position"]);
    }

    #[test]
    fn a_silent_import_tells_no_one() {
        let (record, mut hub, seen, _mine) = held_in_a_second_hub();
        let request = serde_json::json!({ "record": record, "announce": false });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert!(!done.announce);
        assert!(hub.guests[1].placed);
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
    }

    /// Every refusal leaves the held player exactly as it was, and tells the world nothing.
    #[test]
    fn a_record_that_cannot_be_taken_changes_nothing() {
        let (record, mut hub, seen, mine) = held_in_a_second_hub();
        let mut cases: Vec<(&str, serde_json::Value)> = Vec::new();
        let with = |key: &str, value: serde_json::Value| {
            let mut r = record.clone();
            r[key] = value;
            serde_json::json!({ "record": r })
        };
        cases.push(("version", with("version", serde_json::json!(2))));
        cases.push(("no version", with("version", serde_json::Value::Null)));
        cases.push(("data version", with("data_version", serde_json::json!(1))));
        cases.push((
            "another player",
            with(
                "uuid",
                serde_json::json!("00000000-0000-0000-0000-000000000001"),
            ),
        ));
        cases.push(("not base64", with("player", serde_json::json!("%%%"))));
        cases.push(("not NBT", with("player", serde_json::json!("AAAA"))));
        cases.push(("no record", serde_json::json!({})));
        cases.push((
            "unknown part",
            serde_json::json!({ "record": record, "parts": ["luck"] }),
        ));
        cases.push((
            "parts not a list",
            serde_json::json!({ "record": record, "parts": "all" }),
        ));
        cases.push((
            "announce not a bool",
            serde_json::json!({ "record": record, "announce": "yes" }),
        ));
        let before = crate::lan::playerdata::compound(&hub.guests[1]);
        for (what, request) in &cases {
            let error = import_player(&mut hub, UUID_N, request).expect_err(what);
            assert!(!error.is_empty(), "{what}");
            let g = &hub.guests[1];
            assert!(g.held && !g.placed, "{what}: the player must still be held");
            assert_eq!(
                crate::lan::playerdata::compound(g),
                before,
                "{what}: nothing may change"
            );
        }
        assert_eq!(ids(&seen), Vec::<i32>::new());
        assert_eq!(ids(&mine), Vec::<i32>::new());
    }

    #[test]
    fn a_position_in_another_dimension_is_refused_but_the_other_parts_can_come() {
        let (record, mut hub, _seen, _mine) = held_in_a_second_hub();
        // Make the held player's world the Nether: the record is in the Overworld.
        hub.guests[1].dimension = "minecraft:the_nether".into();
        let all = serde_json::json!({ "record": record });
        let error = import_player(&mut hub, UUID_N, &all).unwrap_err();
        assert!(error.contains("position cannot be taken"), "{error}");
        assert!(hub.guests[1].held);
        let without = serde_json::json!({ "record": record, "parts": ["inventory", "experience"] });
        import_player(&mut hub, UUID_N, &without).unwrap();
        assert_eq!(
            hub.guests[1].dimension, "minecraft:the_nether",
            "the client's world did not change"
        );
        assert_eq!(hub.guests[1].xp_level, 7);
    }

    #[test]
    fn only_a_held_player_can_be_imported_into() {
        let (record, mut hub, _seen, _mine) = held_in_a_second_hub();
        let request = serde_json::json!({ "record": record });
        // A placed player is not a target.
        hub.guests[1].held = false;
        hub.guests[1].placed = true;
        let error = import_player(&mut hub, UUID_N, &request).unwrap_err();
        assert!(
            error.contains("no player with that unique id is held"),
            "{error}"
        );
        // Nor a player nobody holds.
        let mut empty = fresh_hub();
        let error = import_player(&mut empty, UUID_N, &request).unwrap_err();
        assert!(
            error.contains("no player with that unique id is held"),
            "{error}"
        );
    }

    // ---- player-release -----------------------------------------------------

    /// A watcher and a placed player frozen for a hand-over.
    fn frozen_with_a_watcher() -> (Hub, Receiver<Out>, Receiver<Out>) {
        let mut hub = hub();
        let (mut watcher, seen) = guest(1, 1, "Watcher");
        watcher.placed = true;
        hub.guests.push(watcher);
        let (mut mover, mine) = guest(2, UUID_N, "Mover");
        mover.placed = true;
        hub.guests.push(mover);
        hub.ticks = 50;
        hub.guests[1].frozen_since = Some(hub.ticks);
        let _ = (ids(&seen), ids(&mine));
        (hub, seen, mine)
    }

    fn left_events(hub: &mut Hub) -> Vec<Guest> {
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        hub.poll(&mut ctx)
            .into_iter()
            .filter_map(|e| match e {
                crate::lan::hub::HostEvent::Left(g) => Some(*g),
                _ => None,
            })
            .collect()
    }

    /// Moved to another server: the others see it go, but nothing is said and nothing saved.
    #[test]
    fn a_moved_player_leaves_the_world_without_the_leave_line_and_is_not_saved() {
        let (mut hub, seen, mine) = frozen_with_a_watcher();
        let reply = super::release_request(&mut hub, &format!("{UUID} moved"));
        assert_eq!(
            reply,
            serde_json::json!({"ok": true, "released": "moved", "announced": false})
        );
        assert_eq!(hub.guests.len(), 1, "the mover is out of the hub");
        let told = ids(&seen);
        assert!(told.contains(&id::PLAYER_INFO_REMOVE), "{told:?}");
        assert!(told.contains(&id::REMOVE_ENTITIES), "{told:?}");
        assert!(!told.contains(&id::SYSTEM_CHAT), "no leave line: {told:?}");
        assert!(matches!(mine.try_recv(), Ok(Out::Disconnect(_))));
        // The game is told it left, released: it saves no file and no record.
        let left = left_events(&mut hub);
        assert_eq!(left.len(), 1);
        assert!(left[0].released && left[0].placed && left[0].frozen_since.is_none());
        assert_eq!(left[0].left_reason, "moved to another server");
    }

    #[test]
    fn a_moved_player_can_be_announced_as_leaving() {
        let (mut hub, seen, _mine) = frozen_with_a_watcher();
        let reply = super::release_request(&mut hub, &format!("{UUID} moved announce"));
        assert_eq!(reply["announced"], true);
        assert!(ids(&seen).contains(&id::SYSTEM_CHAT));
    }

    /// For a player who is not there to be moved, the control: a placed player's leaving is
    /// announced as always (see `a_placed_player_leaving_is_announced`).
    #[test]
    fn an_aborted_hand_over_lets_the_player_go_on_as_it_was() {
        let (mut hub, seen, mine) = frozen_with_a_watcher();
        let reply = super::release_request(&mut hub, &format!("{UUID} abort"));
        assert_eq!(reply, serde_json::json!({"ok": true, "released": "abort"}));
        assert_eq!(hub.guests.len(), 2);
        assert!(hub.guests[1].frozen_since.is_none() && hub.guests[1].placed);
        assert_eq!(ids(&seen), Vec::<i32>::new());
        assert_eq!(ids(&mine), Vec::<i32>::new());
        assert!(left_events(&mut hub).is_empty());
    }

    #[test]
    fn a_release_is_refused_for_a_player_who_is_not_frozen_and_changes_nothing() {
        let (mut hub, seen, _mine) = frozen_with_a_watcher();
        hub.guests[1].frozen_since = None;
        for how in ["moved", "abort"] {
            let reply = super::release_request(&mut hub, &format!("{UUID} {how}"));
            assert!(
                reply["error"].as_str().unwrap().contains("not frozen"),
                "{reply}"
            );
        }
        // Not connected at all.
        let reply = super::release_request(&mut hub, "00000000000000000000000000000001 moved");
        assert!(reply["error"].is_string());
        assert_eq!(hub.guests.len(), 2);
        assert_eq!(ids(&seen), Vec::<i32>::new());
    }

    #[test]
    fn a_release_must_say_how_and_nothing_more() {
        let (mut hub, _seen, _mine) = frozen_with_a_watcher();
        for args in [
            "",
            "nonsense moved",
            UUID,
            &format!("{UUID} later"),
            &format!("{UUID} moved quietly"),
            &format!("{UUID} abort announce"),
            &format!("{UUID} moved announce now"),
        ] {
            let reply = super::release_request(&mut hub, args);
            assert!(reply["error"].is_string(), "{args:?} gave {reply}");
        }
        assert!(
            hub.guests[1].frozen_since.is_some(),
            "no refusal touched the player"
        );
    }

    // ---- player-import without placing, player-confirm, and the cancel ------

    fn import_without_placing(
        request_extra: serde_json::Value,
    ) -> (Hub, Receiver<Out>, Receiver<Out>) {
        let (record, mut hub, seen, mine) = held_in_a_second_hub();
        let mut request = serde_json::json!({ "record": record, "place": false });
        for (k, v) in request_extra.as_object().unwrap() {
            request[k] = v.clone();
        }
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert!(!done.placed);
        (hub, seen, mine)
    }

    /// The import changes the player and tells its client, but the world still does not know it.
    #[test]
    fn an_import_that_does_not_place_leaves_the_player_held_and_tells_only_its_client() {
        let (hub, seen, mine) = import_without_placing(serde_json::json!({}));
        let g = &hub.guests[1];
        assert!(g.held && !g.placed && g.imported && g.arrival_announce);
        assert_eq!(
            (g.xp_level, g.food, g.game_mode, g.position),
            (7, 12, 1, [10.5, 70.0, -3.25])
        );
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
        let sent = ids(&mine);
        for wanted in [
            id::GAME_EVENT,
            id::PLAYER_ABILITIES,
            id::SET_HELD_SLOT,
            id::PLAYER_POSITION,
        ] {
            assert!(
                sent.contains(&wanted),
                "packet {wanted} missing from {sent:?}"
            );
        }
    }

    #[test]
    fn confirming_places_the_player_and_announces_it_as_the_import_asked() {
        let (mut hub, seen, mine) = import_without_placing(serde_json::json!({}));
        let _ = ids(&mine);
        let reply = super::confirm_request(&mut hub, UUID);
        assert_eq!(
            reply,
            serde_json::json!({"ok": true, "placed": true, "announced": true})
        );
        let g = &hub.guests[1];
        assert!(g.placed && !g.held && !g.imported);
        let told = ids(&seen);
        assert!(
            told.contains(&id::SYSTEM_CHAT) && told.contains(&id::PLAYER_INFO_UPDATE),
            "{told:?}"
        );
        // The confirmation is not a second import: nothing about the player is sent again.
        assert!(!ids(&mine).contains(&id::PLAYER_POSITION));
    }

    #[test]
    fn a_silent_import_stays_silent_when_it_is_confirmed() {
        let (mut hub, seen, _mine) =
            import_without_placing(serde_json::json!({ "announce": false }));
        let reply = super::confirm_request(&mut hub, UUID);
        assert_eq!(
            reply,
            serde_json::json!({"ok": true, "placed": true, "announced": false})
        );
        assert!(hub.guests[1].placed);
        assert_eq!(ids(&seen), Vec::<i32>::new());
    }

    /// The cancel: a failure after the import, before anything reached the client, removes the
    /// player from this server with no file and no word to anyone.
    #[test]
    fn dropping_an_imported_but_unconfirmed_player_cancels_the_arrival_without_a_word() {
        let (mut hub, seen, mine) = import_without_placing(serde_json::json!({}));
        let _ = ids(&mine);
        let reply = super::drop_request(&mut hub, UUID);
        assert_eq!(
            (reply["held"].as_bool(), reply["expected"].as_bool()),
            (Some(true), Some(false))
        );
        assert!(matches!(mine.try_recv(), Ok(Out::Disconnect(_))));
        let gone = hub.remove_guest(1, "dropped");
        assert!(
            gone.held && !gone.placed && !gone.released,
            "never placed: the game saves nothing"
        );
        assert_eq!(
            ids(&seen),
            Vec::<i32>::new(),
            "the watcher must hear nothing"
        );
        // And it cannot be confirmed any more.
        let reply = super::confirm_request(&mut hub, UUID);
        assert!(reply["error"].is_string(), "{reply}");
    }

    #[test]
    fn a_confirmation_is_refused_for_a_player_that_is_not_imported_and_changes_nothing() {
        let (record, mut hub, seen, _mine) = held_in_a_second_hub();
        let _ = record;
        for args in ["", "nonsense", &format!("{UUID} now")] {
            assert!(
                super::confirm_request(&mut hub, args)["error"].is_string(),
                "{args:?}"
            );
        }
        // Held, but nothing was imported.
        let reply = super::confirm_request(&mut hub, UUID);
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("not been imported"),
            "{reply}"
        );
        assert!(hub.guests[1].held && !hub.guests[1].placed);
        // Not held at all.
        let reply = super::confirm_request(&mut hub, "00000000000000000000000000000009");
        assert!(
            reply["error"].as_str().unwrap().contains("no player"),
            "{reply}"
        );
        assert_eq!(ids(&seen), Vec::<i32>::new());
    }

    #[test]
    fn place_is_true_or_false() {
        let (record, mut hub, _seen, _mine) = held_in_a_second_hub();
        let request = serde_json::json!({ "record": record, "place": "later" });
        assert!(import_player(&mut hub, UUID_N, &request).is_err());
        assert!(hub.guests[1].held && !hub.guests[1].imported);
    }

    /// A proxy that dies between the import and the confirmation leaves nothing behind.
    #[test]
    fn an_imported_player_nobody_confirms_is_dropped_like_any_held_player() {
        let (mut hub, seen, mine) = import_without_placing(serde_json::json!({}));
        let _ = ids(&mine);
        let mut world = crate::world::World::new(horizon_world::registry::BlockStateId(0), 0);
        let border = crate::worldborder::WorldBorder::default();
        let mut next = 100;
        let mut ctx = tick_context(&mut world, &border, &mut next);
        hub.ticks = hub.guests[1].held_since + super::HELD_MAX_TICKS - 1;
        let events = hub.poll(&mut ctx);
        assert!(events.iter().any(|e| is_warn(e, "dropped")));
        assert!(matches!(mine.try_recv(), Ok(Out::Disconnect(_))));
        assert_eq!(ids(&seen), Vec::<i32>::new());
    }

    // ---- the lines of the log ------------------------------------------------

    use super::{dropped_line, held_line, placed_line, request_log};
    use serde_json::json;

    #[test]
    fn a_refused_request_is_a_warning_with_the_reason() {
        let reply = json!({ "error": "that player is riding" });
        let (level, line) = request_log(
            "player-export",
            Some("Alex"),
            &format!("{UUID} freeze"),
            &reply,
        );
        assert_eq!(level, "WARN");
        assert_eq!(
            line,
            "player-export Alex (123e4567-e89b-12d3-a456-426614174000): refused: that player is riding"
        );
    }

    #[test]
    fn an_accepted_request_says_what_it_did_and_never_writes_the_record() {
        let id = UUID.replace('-', "");
        let (level, line) = request_log(
            "player-expect",
            None,
            &format!("{id} 30"),
            &json!({"ok": true, "renewed": true, "ttl_seconds": 30}),
        );
        assert_eq!(level, "INFO");
        assert_eq!(
            line,
            "player-expect 123e4567-e89b-12d3-a456-426614174000: ok, expected for 30 s (renewed)"
        );
        // 8 base64 characters are 6 bytes.
        let reply = json!({"ok": true, "record": {"player": "AAAAAAAA", "name": "Alex"}});
        let (_, line) = request_log(
            "player-export",
            Some("Alex"),
            &format!("{UUID} freeze"),
            &reply,
        );
        assert!(line.ends_with("ok, record of 6 bytes, frozen"), "{line}");
        assert!(
            !line.contains("AAAA"),
            "the record itself must not be written: {line}"
        );
        let reply = json!({"ok": true, "parts": ["inventory", "experience"], "placed": false, "announced": true, "recentered": true});
        let (_, line) = request_log(
            "player-import",
            Some("Alex"),
            &format!("{UUID} {{\"record\":{{}}}}"),
            &reply,
        );
        assert!(
            line.ends_with(
                "ok, parts inventory,experience, placed false, announced true, recentered true"
            ),
            "{line}"
        );
        assert!(
            !line.contains("record"),
            "the request body must not be written: {line}"
        );
        let (_, line) = request_log(
            "player-release",
            Some("Alex"),
            &format!("{UUID} moved"),
            &json!({"ok": true, "released": "moved", "announced": false, "was_frozen": false}),
        );
        assert!(
            line.ends_with("ok, moved, announced false, was frozen false"),
            "{line}"
        );
        let (_, line) = request_log(
            "player-drop",
            None,
            UUID,
            &json!({"ok": true, "held": true, "expected": false}),
        );
        assert!(line.ends_with("ok, held true, expected false"), "{line}");
        let (_, line) = request_log(
            "player-confirm",
            None,
            UUID,
            &json!({"ok": true, "placed": true, "announced": true}),
        );
        assert!(line.ends_with("ok, placed, announced true"), "{line}");
    }

    #[test]
    fn a_request_without_a_readable_id_is_logged_as_typed() {
        let (level, line) = request_log(
            "player-drop",
            None,
            "nonsense",
            &json!({"error": "player-drop needs the player's unique id"}),
        );
        assert_eq!(level, "WARN");
        assert!(
            line.starts_with("player-drop \"nonsense\": refused"),
            "{line}"
        );
    }

    #[test]
    fn the_lines_of_a_held_player_never_say_it_joined_or_left() {
        assert_eq!(
            held_line("Alex"),
            "Alex is held: it arrives from another server"
        );
        assert_eq!(
            placed_line("Alex"),
            "Alex placed: it has arrived from another server"
        );
        let dropped = dropped_line("Alex", "dropped before its hand-over");
        assert_eq!(
            dropped,
            "Alex was dropped while held: dropped before its hand-over"
        );
        for line in [held_line("Alex"), dropped] {
            assert!(
                !line.contains("joined the game") && !line.contains("left the game"),
                "{line}"
            );
        }
    }

    // ---- the view follows the imported position ------------------------------

    /// A held player whose view is centred on chunk (0, 0) (`center`), and a record of a player
    /// standing at `at`.
    fn held_with_a_record_at(
        at: [f64; 3],
        center: Option<(i32, i32)>,
    ) -> (serde_json::Value, Hub, Receiver<Out>) {
        let (mut source, i) = exported_guest();
        source.guests[i].position = at;
        let record = super::export_record(&mut source, i, false, serde_json::Value::Null).unwrap();
        let mut hub = hub();
        assert_eq!(
            super::expect_request(&mut hub, UUID, Instant::now())["ok"],
            true
        );
        let (newcomer, mine) = guest(2, UUID_N, "Alex");
        hub.guests.push(newcomer);
        place(&mut hub, 2);
        hub.guests[0].center = center;
        let _ = ids(&mine);
        (record, hub, mine)
    }

    #[test]
    fn an_import_to_another_chunk_moves_the_view_at_once_and_says_so() {
        // Chunk (0, 0) before, chunk (6, -1) after: x 100.5 is block 100, chunk 6; z -3.25 is chunk -1.
        let (record, mut hub, mine) = held_with_a_record_at([100.5, 70.0, -3.25], Some((0, 0)));
        let request = serde_json::json!({ "record": record, "place": false });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert!(done.recentered);
        assert_eq!(
            hub.guests[0].center,
            Some((6, -1)),
            "the view is centred on the new chunk"
        );
        let sent = packets(&mine);
        let centre = sent
            .iter()
            .position(|(i, _)| *i == id::SET_CHUNK_CACHE_CENTER)
            .expect("the new chunk centre is sent with the import");
        let position = sent
            .iter()
            .position(|(i, _)| *i == id::PLAYER_POSITION)
            .unwrap();
        assert!(
            centre < position,
            "the centre goes before the position the client confirms"
        );
        // The centre is the vanilla pair of VarInts: chunk x then chunk z (6, then -1 as an unsigned VarInt).
        let mut r = horizon_protocol::buf::Reader::new(&sent[centre].1);
        assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (6, -1));
    }

    #[test]
    fn an_import_inside_the_same_chunk_does_not_recentre() {
        let (record, mut hub, mine) = held_with_a_record_at([3.5, 64.0, 3.5], Some((0, 0)));
        let request = serde_json::json!({ "record": record, "place": false });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert!(!done.recentered);
        assert_eq!(hub.guests[0].center, Some((0, 0)));
        assert!(!ids(&mine).contains(&id::SET_CHUNK_CACHE_CENTER));
    }

    #[test]
    fn without_the_position_part_the_view_stays_where_it_is() {
        let (record, mut hub, mine) = held_with_a_record_at([100.5, 70.0, -3.25], Some((0, 0)));
        let request =
            serde_json::json!({ "record": record, "place": false, "parts": ["inventory"] });
        let done = import_player(&mut hub, UUID_N, &request).unwrap();
        assert!(!done.recentered);
        assert_eq!(hub.guests[0].center, Some((0, 0)));
        assert!(!ids(&mine).contains(&id::SET_CHUNK_CACHE_CENTER));
    }

    /// A held player that has not been sent any chunk has no view to move: the first tick builds it
    /// around the new place.
    #[test]
    fn a_view_not_yet_built_is_not_recentred() {
        let (record, mut hub, _mine) = held_with_a_record_at([100.5, 70.0, -3.25], None);
        let request = serde_json::json!({ "record": record, "place": false });
        assert!(
            !import_player(&mut hub, UUID_N, &request)
                .unwrap()
                .recentered
        );
    }

    #[test]
    fn placing_at_once_reports_the_recentring_too() {
        let (record, mut hub, mine) = held_with_a_record_at([100.5, 70.0, -3.25], Some((0, 0)));
        let done =
            import_player(&mut hub, UUID_N, &serde_json::json!({ "record": record })).unwrap();
        assert!(done.placed && done.recentered);
        assert!(ids(&mine).contains(&id::SET_CHUNK_CACHE_CENTER));
    }
}
