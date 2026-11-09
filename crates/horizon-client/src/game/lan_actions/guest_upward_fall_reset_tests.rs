use super::*;
use horizon_client::advancements::Tree;
use horizon_core::position::ChunkPos;

const NAME: &str = "FallGuest";
const IDS: [&str; 4] = ["fall_any", "fall_small", "fall_mid", "fall_big"];

fn scene() -> (Game<'static>, mpsc::Receiver<Out>) {
    let mut g = crate::game::native_machines::tests::server_game();
    g.build_registries();
    g.set_allow_commands(true);
    let scratch = std::env::temp_dir().join(format!("guest-fall-unit-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    g.world_root = scratch.clone();
    let data = scratch.join("fixture-data");
    let folder = data.join("fallproof/advancement");
    std::fs::create_dir_all(&folder).unwrap();
    for (name, distance) in IDS.into_iter().zip([
        serde_json::Value::Null,
        serde_json::json!({"max":1.0}),
        serde_json::json!({"min":4.9,"max":5.1}),
        serde_json::json!({"min":8.0}),
    ]) {
        let mut criterion = serde_json::json!({"trigger":"minecraft:fall_from_height"});
        if !distance.is_null() {
            criterion["conditions"] = serde_json::json!({"distance":{"y":distance}});
        }
        std::fs::write(
            folder.join(format!("{name}.json")),
            serde_json::to_vec(&serde_json::json!({"criteria":{name:criterion}})).unwrap(),
        )
        .unwrap();
    }
    g.adv.tree = Arc::new(Tree::load(&data));
    assert_eq!(g.adv.tree.list.len(), 4);
    let stone = g
        .pack
        .assets
        .registry
        .default_state("minecraft:stone")
        .unwrap()
        .0;
    let mut chunk = Chunk::empty(0, 0);
    for x in 0..16 {
        for z in 0..16 {
            chunk.set(x, 99, z, stone);
        }
    }
    g.world.chunks.insert((0, 0), Arc::new(chunk));
    g.block_ticks.add_empty_chunk(ChunkPos::new(0, 0)).unwrap();
    assert!(g.run_command("forceload add 0 0"));
    assert!(g.run_command("tick freeze"));
    g.lan_host(
        super::super::super::lan::LanSettings {
            port: 0,
            guest_commands: true,
            force_game_mode: false,
        },
        NetConfig {
            listen: false,
            announce: false,
            ..NetConfig::loopback(0, "guest-fall-unit".into(), 2)
        },
        HubConfig {
            default_game_mode: 0,
            guest_commands: true,
            force_game_mode: false,
            hardcore: false,
            view_distance: 2,
            simulation_distance: 2,
            dimension: "minecraft:overworld".into(),
            allow_flight: false,
            seed: 0,
            max_players: 2,
            integrated: false,
            levels: Default::default(),
        },
    )
    .unwrap();
    let (mut guest, rx) = observer(1, [12.5, 110.0, 12.5], "minecraft:overworld");
    guest.name = NAME.into();
    guest.game_mode = 0;
    guest.on_ground = false;
    guest.load_timeout = 0;
    g.lan.as_mut().unwrap().hub.guests.push(guest);
    g.sync_tick_chunks();
    g.ticked_entities_through = g.next_entity;
    (g, rx)
}

fn done(g: &Game<'_>) -> [bool; 4] {
    let guest = &g.lan.as_ref().unwrap().hub.guests[0];
    IDS.map(|name| {
        let i = g.adv.tree.find_id(&format!("fallproof:{name}")).unwrap();
        guest.advancements.done(&g.adv.tree.list[i])
    })
}

fn move_once(g: &mut Game<'_>, y: f64, ground: bool) {
    let guest = &mut g.lan.as_mut().unwrap().hub.guests[0];
    assert!(guest.moves.is_empty());
    assert!(guest.inbox.is_empty());
    guest
        .inbox
        .push_back((S::ClientTickEnd(horizon_protocol::packets::Empty), 0));
    guest.inbox.push_back((
        S::MovePlayerPos(play::MovePlayerPos {
            x: 12.5,
            y,
            z: 12.5,
            flags: u8::from(ground),
        }),
        0,
    ));
    guest
        .inbox
        .push_back((S::ClientTickEnd(horizon_protocol::packets::Empty), 0));
    g.tick_lan_packets();
    let guest = &g.lan.as_ref().unwrap().hub.guests[0];
    assert!(guest.inbox.is_empty());
    assert_eq!(
        guest.position,
        [12.5, y, 12.5],
        "actual Hub accepted position"
    );
    assert_eq!(guest.on_ground, ground);
    assert_eq!(guest.moves.len(), 1, "one accepted move at this boundary");
    g.tick_lan(false);
}

#[test]
#[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
fn accepted_airborne_upward_move_resets_guest_fall_and_awards_actual_recipient() {
    let (mut g, _rx) = scene();
    for name in IDS {
        assert!(g.run_command(&format!("advancement grant {NAME} only fallproof:{name}")));
    }
    assert_eq!(
        done(&g),
        [true; 4],
        "actual positive criterion control before revoke"
    );
    for name in IDS {
        let _ = g.run_command(&format!("advancement revoke {NAME} only fallproof:{name}"));
    }
    assert_eq!(done(&g), [false; 4]);
    assert!(
        g.adv
            .progress
            .to_json(&g.adv.tree, horizon_world::DATA_VERSION)
            .as_object()
            .unwrap()
            .keys()
            .all(|k| !k.starts_with("fallproof:"))
    );
    move_once(&mut g, 109.0, false);
    let host = g.lan.as_ref().unwrap();
    let guest = &host.hub.guests[0];
    assert_eq!((guest.uuid, guest.game_mode, guest.health), (1, 0, 20.0));
    let physical = guest.motion.as_ref().unwrap();
    assert_eq!(
        physical.fall_distance, 1.0,
        "prior real physical fall premise"
    );
    assert_eq!(
        host.play.get(&1).unwrap().marks.fall,
        Some([12.5, 109.0, 12.5]),
        "actual tracked start after prior player tick"
    );
    assert_eq!(done(&g), [false; 4], "descent alone never awards");
    let clock = (g.world.game_time, host.hub.ticks);
    move_once(&mut g, 109.5, false);
    let host = g.lan.as_ref().unwrap();
    let guest = &host.hub.guests[0];
    assert_eq!(guest.position, [12.5, 109.5, 12.5]);
    assert!(!guest.on_ground);
    assert_eq!((guest.uuid, guest.health), (1, 20.0));
    println!(
        "actual clock boundaries {clock:?} -> {:?}",
        (g.world.game_time, host.hub.ticks)
    );
    let physical = guest.motion.as_ref().unwrap().fall_distance;
    let mark = host.play.get(&1).unwrap().marks.fall;
    let awarded = done(&g);
    println!("physical={physical:?}, tracked={mark:?}, actual recipient criteria={awarded:?}");
    assert_eq!(
        (physical, mark, awarded),
        (0.0, None, [true, true, false, false]),
        "upward reset: physical / tracked-start / actual recipient progress are distinct"
    );
}
