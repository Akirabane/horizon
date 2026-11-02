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
}
