//! `LivingEntity.onEquippedItemBroken` and `ServerPlayer.onEquippedItemBroken` for the acting
//! player: the break told to the clients (or played here) and the broken statistic.
use super::*;

impl Game<'_> {
    /// `LivingEntity.onEquippedItemBroken` for the acting player (the guest whose turn it
    /// is, else the host): `level.broadcastEntityEvent` with `entityEventForEquipmentBreak`
    /// of the slot, which each client answers with `breakItem` (the sound and the crumbs,
    /// `remote_equipment_break`). Without a hub to tell, or in a window that runs the world
    /// itself, the sound and the crumbs are played here.
    pub(in crate::game) fn player_equipment_broke(
        &mut self,
        slot: horizon_world::player_inventory::EquipmentSlot,
        stack: &horizon_client::items::ItemStack,
    ) {
        use horizon_world::player_inventory::EquipmentSlot;
        let event = match slot {
            EquipmentSlot::MainHand => 47,
            EquipmentSlot::OffHand => 48,
            EquipmentSlot::Head => 49,
            EquipmentSlot::Chest => 50,
            EquipmentSlot::Legs => 51,
            EquipmentSlot::Feet => 52,
            EquipmentSlot::Body => 65,
            EquipmentSlot::Saddle => 68,
        };
        // The mods hear a server player's break (a creative player's gear takes no wear).
        if !self.creative
            && let (Some(uuid), Some(name)) = (self.lan_acting_uuid, self.lan_acting_name.clone())
        {
            self.native_player_destroy_item(uuid, &name, &stack.item, equipment_slot_name(slot));
        }
        // ServerPlayer.onEquippedItemBroken: the broken statistic, whatever the slot.
        self.award_stat("broken", &stack.item, 1);
        let Some(at) = self
            .player
            .as_ref()
            .map(|p| [p.position.x, p.position.y, p.position.z])
        else {
            return;
        };
        let told = self.lan_acting.is_some();
        if told {
            self.guest_equipment_breaks.push((event, at));
        }
        if told && !self.options.execution_role.is_client() {
            return;
        }
        // LivingEntity.breakItem: the stack's break sound where it stands, then the crumbs.
        self.break_sound_at(stack, at);
        self.eat_particles(stack, 5);
    }
}

#[cfg(test)]
mod tests {
    use crate::game::{Audio, Game, Options, Progress, Settings, load_resources};
    use horizon_client::motion::PlayerMotion;
    use horizon_core::geometry::Vec3;

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn breaking_worn_armour_counts_the_broken_statistic() {
        let root = std::path::PathBuf::from(std::env::var_os("HORIZON_TEST_RESOURCES").unwrap());
        let settings = Settings::default();
        let loaded = load_resources(
            root.clone(),
            None,
            &root,
            &settings,
            std::time::Instant::now(),
            false,
            true,
            &Progress::new(),
            &mut |_, _| unreachable!(),
        )
        .unwrap();
        let mut options = Options::parse(std::iter::empty()).unwrap().unwrap();
        options.resources = root.clone();
        options.execution_role = crate::game::execution_role::ExecutionRole::IntegratedServer;
        let mut game = Game::new(
            &loaded.resources,
            loaded.pack,
            loaded.gui.unwrap(),
            options,
            settings,
            Audio::silent(&root),
            None,
        )
        .unwrap();
        game.player = Some(PlayerMotion::new(game.motion, Vec3::new(4.5, 62.0, 4.5)).unwrap());
        game.creative = false;
        game.build_registries();
        game.set_allow_commands(true);
        assert!(game.run_command("gamemode survival"));
        // A helmet one point from breaking, hurt once: it breaks
        // (ServerPlayer.onEquippedItemBroken awards ITEM_BROKEN for every slot).
        assert!(game.run_command(
            "item replace entity @s armor.head with \
             minecraft:leather_helmet[minecraft:max_damage=10,minecraft:damage=9]"
        ));
        let generic = game
            .health_resources
            .damage
            .id("minecraft:generic")
            .unwrap();
        game.hurt_armor(generic, 4.0);
        assert!(game.armor[0].is_none(), "the helmet broke");
        assert_eq!(game.stats.get("broken", "minecraft:leather_helmet"), 1);
        // A held tool that breaks counts once, as before.
        let max = game
            .items()
            .get("minecraft:wooden_sword")
            .unwrap()
            .max_damage;
        assert!(game.run_command(&format!(
            "item replace entity @s weapon.mainhand with minecraft:wooden_sword[minecraft:damage={}]",
            max - 1
        )));
        game.damage_held(1);
        assert!(game.held().is_none(), "the sword broke");
        assert_eq!(game.stats.get("broken", "minecraft:wooden_sword"), 1);
    }
}
