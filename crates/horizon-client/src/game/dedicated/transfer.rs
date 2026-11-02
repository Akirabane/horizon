//! A proxy's requests that read a connected player out of the server:
//! `player-export <uuid> [freeze]` (the rest of the proxy's requests, `player-expect` and
//! `player-drop`, are `lan/arrivals.rs`'s). They run between two ticks, on the server thread, so
//! the player is never caught half-way through a tick.
use crate::game::{Game, lan::LanHost};
use horizon_client::lan::arrivals;
use horizon_protocol::packets::play::{self, Clientbound as C};
use serde_json::{Value, json};
use std::time::Instant;

impl Game<'_> {
    /// A proxy's `player-*` request: runs it and writes one line to the log with what became of it
    /// (`arrivals::request_log`): nothing else of a hand-over is visible in the server's log.
    pub(super) fn player_request(&mut self, verb: &str, args: &str) -> Value {
        let name = args
            .split_whitespace()
            .next()
            .and_then(arrivals::parse_uuid)
            .and_then(|uuid| {
                self.lan
                    .as_ref()?
                    .hub
                    .guests
                    .iter()
                    .find(|g| g.uuid == uuid)
                    .map(|g| g.name.clone())
            });
        let closed = || json!({ "error": "the server is not open to players" });
        let reply = match verb {
            "player-expect" => match self.lan.as_mut() {
                Some(host) => arrivals::expect_request(&mut host.hub, args, Instant::now()),
                None => closed(),
            },
            "player-drop" => match self.lan.as_mut() {
                Some(host) => arrivals::drop_request(&mut host.hub, args),
                None => closed(),
            },
            "player-release" => match self.lan.as_mut() {
                Some(host) => arrivals::release_request(&mut host.hub, args),
                None => closed(),
            },
            "player-export" => self.player_export(args),
            "player-import" => self.player_import(args),
            "player-confirm" => self.player_confirm(args),
            other => json!({ "error": format!("unknown request {other:?}") }),
        };
        let (level, line) = arrivals::request_log(verb, name.as_deref(), args, &reply);
        super::log(level, &line);
        reply
    }

    /// `player-export <uuid> [freeze]`: closes the player's menus as vanilla does when a menu closes
    /// (the cursor stack and the crafting grids go back into the inventory), then answers with the
    /// player's record (`arrivals::export_record`). With `freeze`, the player's own inputs stop being
    /// acted on until `player-release`.
    pub(super) fn player_export(&mut self, args: &str) -> Value {
        let mut parts = args.split_whitespace();
        let Some(uuid) = parts.next().and_then(arrivals::parse_uuid) else {
            return json!({ "error": "player-export needs the player's unique id" });
        };
        let freeze = match parts.next() {
            None => false,
            Some("freeze") => true,
            Some(_) => return json!({ "error": "the only option of player-export is freeze" }),
        };
        if parts.next().is_some() {
            return json!({ "error": "player-export takes a unique id and at most freeze" });
        }
        let Some(mut host) = self.lan.take() else {
            return json!({ "error": "the server is not open to players" });
        };
        let reply = self.export_from(&mut host, uuid, freeze);
        self.lan = Some(host);
        reply
    }

    /// `player-import <uuid> <json>`: applies a record to the held player (`lan/import.rs`), lets it
    /// into the world, and gives it its advancements, which need the game's tree.
    pub(super) fn player_import(&mut self, args: &str) -> Value {
        let Some((id, request)) = args.trim().split_once(char::is_whitespace) else {
            return json!({ "error": "player-import needs a unique id and a request" });
        };
        let Some(uuid) = arrivals::parse_uuid(id) else {
            return json!({ "error": "player-import needs the player's unique id" });
        };
        let request: Value = match serde_json::from_str(request.trim()) {
            Ok(v) => v,
            Err(e) => return json!({ "error": format!("the request is not JSON: {e}") }),
        };
        let Some(mut host) = self.lan.take() else {
            return json!({ "error": "the server is not open to players" });
        };
        let name = host
            .hub
            .guests
            .iter()
            .find(|g| g.uuid == uuid)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let reply = match horizon_client::lan::import::import_player(&mut host.hub, uuid, &request)
        {
            Ok(done) => {
                if let Some(saved) = done.advancements
                    && let Some(g) = host.hub.guests.iter_mut().find(|g| g.uuid == uuid)
                {
                    let tree = self.advancement_tree();
                    let mut progress = horizon_client::advancements::Progress::from_json(&saved);
                    // PlayerAdvancements.applyFrom: advancements this server lacks are dropped.
                    progress
                        .criteria
                        .retain(|id, _| tree.by_id.contains_key(id));
                    g.advancements = progress;
                    g.advancements_loaded = true;
                }
                if done.placed {
                    super::log("INFO", &arrivals::placed_line(&name));
                    if done.announce {
                        super::log("INFO", &format!("System chat: {name} joined the game"));
                    }
                }
                json!({
                    "ok": true,
                    "parts": done.parts,
                    "placed": done.placed,
                    "announced": done.announce,
                    "recentered": done.recentered,
                })
            }
            Err(e) => json!({ "error": e }),
        };
        self.lan = Some(host);
        reply
    }

    /// `player-confirm <uuid>`: places a held player whose record was imported without placing it.
    pub(super) fn player_confirm(&mut self, args: &str) -> Value {
        let Some(host) = self.lan.as_mut() else {
            return json!({ "error": "the server is not open to players" });
        };
        let name = args
            .split_whitespace()
            .next()
            .and_then(arrivals::parse_uuid)
            .and_then(|uuid| host.hub.guests.iter().find(|g| g.uuid == uuid))
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let reply = arrivals::confirm_request(&mut host.hub, args);
        if reply["placed"] == true {
            super::log("INFO", &arrivals::placed_line(&name));
        }
        if reply["announced"] == true {
            super::log("INFO", &format!("System chat: {name} joined the game"));
        }
        reply
    }

    fn export_from(&mut self, host: &mut LanHost, uuid: u128, freeze: bool) -> Value {
        let i = match arrivals::check_exportable(&host.hub, uuid) {
            Ok(i) => i,
            Err(why) => return json!({ "error": why }),
        };
        let conn = host.hub.guests[i].conn;
        let window = host.hub.guests[i].menu.as_ref().map(|m| m.window);
        // Nothing is sent to the client by this: it is for a player who is leaving.
        self.guest_close_all(host, conn);
        // The player may stay (an aborted hand-over): its client closes the menu too and shows
        // the inventory as it now is.
        if let Some(container) = window {
            host.hub
                .send_to(conn, &C::ContainerClose(play::ContainerClose { container }));
        }
        host.hub.send_inventory(i);
        let advancements = if self.adv.tree.list.is_empty() {
            Value::Null
        } else {
            host.hub.guests[i]
                .advancements
                .to_json(&self.adv.tree, horizon_world::DATA_VERSION)
        };
        match arrivals::export_record(&mut host.hub, i, freeze, advancements) {
            Ok(record) => json!({ "ok": true, "record": record }),
            Err(e) => json!({ "error": e }),
        }
    }
}
