use super::*;

impl Game<'_> {
    /// `DistanceTrigger`: the player's `player` condition, `start_position`
    /// at `start`, and the `distance` from `start` to the player.
    pub(super) fn trigger_distance(&mut self, name: &str, start: [f64; 3]) {
        let me = self.player_entity_for(name);
        self.trigger(name, move |c, w| {
            let pctx = adv::Entities {
                this: Some(&me),
                origin: me.position,
                tool: None,
                block_pos: None,
            };
            adv::conditions(&c["player"], &pctx, w)
                && adv::location(&c["start_position"], start, w)
                && (c["distance"].is_null() || adv::distance(&c["distance"], start, me.position))
        });
    }
    /// `ServerPlayer.resetFallDistance`, for one accepted upward guest move.
    /// Batched moves keep the existing path until their per-move context is admitted.
    pub(in crate::game) fn reset_single_upward_guest_fall(&mut self) {
        if let Some(start) = self.adv.marks.fall
            && self
                .player
                .as_ref()
                .is_some_and(|p| p.health.health() > 0.0)
        {
            self.trigger_distance("minecraft:fall_from_height", start);
        }
        self.adv.marks.fall = None;
        if let Some(player) = self.player.as_mut() {
            player.fall_distance = 0.0;
        }
    }
}
