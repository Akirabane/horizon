use super::*;

#[test]
#[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
fn bounded_guest_upward_reset_controls_keep_legacy_queues_and_do_not_repeat_awards() {
    let (mut g, _rx) = scene();
    // No tracked start: the real accepted single upward move clears no award.
    move_once(&mut g, 110.5, false);
    assert_eq!(done(&g), [false; 4]);
    assert!(
        g.lan
            .as_ref()
            .unwrap()
            .play
            .get(&1)
            .unwrap()
            .marks
            .fall
            .is_none()
    );
    move_once(&mut g, 109.5, false);
    let mark = Some([12.5, 109.5, 12.5]);
    assert_eq!(
        g.lan.as_ref().unwrap().play.get(&1).unwrap().marks.fall,
        mark
    );
    // Empty queue remains legacy: it cannot end the current fall.
    g.tick_lan(false);
    assert_eq!(
        g.lan.as_ref().unwrap().hub.guests[0]
            .motion
            .as_ref()
            .unwrap()
            .fall_distance,
        1.0
    );
    assert_eq!(
        g.lan.as_ref().unwrap().play.get(&1).unwrap().marks.fall,
        mark
    );
    let guest = &mut g.lan.as_mut().unwrap().hub.guests[0];
    for (x, y) in [(12.5, 110.0), (12.75, 110.0)] {
        guest
            .inbox
            .push_back((S::ClientTickEnd(horizon_protocol::packets::Empty), 0));
        guest.inbox.push_back((
            S::MovePlayerPos(play::MovePlayerPos {
                x,
                y,
                z: 12.5,
                flags: 0,
            }),
            0,
        ));
        guest
            .inbox
            .push_back((S::ClientTickEnd(horizon_protocol::packets::Empty), 0));
    }
    g.tick_lan_packets();
    assert_eq!(
        g.lan.as_ref().unwrap().hub.guests[0].position,
        [12.75, 110.0, 12.5]
    );
    assert_eq!(g.lan.as_ref().unwrap().hub.guests[0].moves.len(), 2);
    // Explicitly preserve the unclosed multi-move path, even with an upward move.
    g.tick_lan(false);
    assert_eq!(
        g.lan.as_ref().unwrap().hub.guests[0]
            .motion
            .as_ref()
            .unwrap()
            .fall_distance,
        1.0
    );
    assert_eq!(
        g.lan.as_ref().unwrap().play.get(&1).unwrap().marks.fall,
        mark
    );
    assert_eq!(done(&g), [false; 4]);
    move_once(&mut g, 110.5, false);
    assert_eq!(done(&g), [true, true, false, false]);
    for name in IDS {
        let _ = g.run_command(&format!("advancement revoke {NAME} only fallproof:{name}"));
    }
    g.tick_lan(false);
    assert_eq!(
        done(&g),
        [false; 4],
        "a later legacy tick cannot repeat the award"
    );
    assert!(
        g.lan
            .as_ref()
            .unwrap()
            .play
            .get(&1)
            .unwrap()
            .marks
            .fall
            .is_none()
    );
    // The helper's nonpositive-health branch, using an actual kill command;
    // the dead guest's movement path itself stays excluded as before.
    move_once(&mut g, 109.5, false);
    assert!(
        g.lan
            .as_ref()
            .unwrap()
            .play
            .get(&1)
            .unwrap()
            .marks
            .fall
            .is_some()
    );
    assert!(g.run_command(&format!("kill {NAME}")));
    let mut host = g.lan.take().unwrap();
    g.as_guest(&mut host, 1, |game| {
        assert!(game.player.as_ref().unwrap().health.health() <= 0.0);
        assert!(game.adv.marks.fall.is_some());
        game.reset_single_upward_guest_fall();
        assert!(game.adv.marks.fall.is_none());
        assert_eq!(game.player.as_ref().unwrap().fall_distance, 0.0);
    })
    .unwrap();
    g.lan = Some(host);
    assert_eq!(done(&g), [false; 4], "no award at nonpositive health");
}
