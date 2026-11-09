//! Item frames (`ItemFrame`, `GlowItemFrame`) in a save.
use super::*;

/// `ItemFrame.setDirection`: a frame on a wall turns to face away from it
/// (`yRot` = the 2D data value times 90, `xRot` 0); one on a floor or a
/// ceiling looks straight down or up (`xRot` = -90 times the axis step).
/// `facing` is the 3D data value (down, up, north, south, west, east).
fn frame_rotation(facing: u8) -> [f32; 2] {
    match facing {
        0 => [0.0, 90.0],
        1 => [0.0, -90.0],
        2 => [180.0, 0.0],
        3 => [0.0, 0.0],
        4 => [90.0, 0.0],
        _ => [270.0, 0.0],
    }
}

/// ItemFrame.addAdditionalSaveData.
pub fn item_frame_to_nbt(f: &ItemFrame, uuid: [i32; 4], raw: Option<&Tag>) -> Tag {
    let (lo, hi) = f.bounds();
    let base = Base {
        id: f.kind(),
        uuid,
        position: Vec3::new(
            (lo[0] + hi[0]) / 2.0,
            (lo[1] + hi[1]) / 2.0,
            (lo[2] + hi[2]) / 2.0,
        ),
        motion: Vec3::ZERO,
        rotation: frame_rotation(f.facing),
        on_ground: false,
        fall_distance: 0.0,
    };
    let mut tag = base.tag(raw);
    // `Entity.saveWithoutId`: `CustomName` before the frame's own fields.
    if let Some(name) = &f.custom_name {
        put(&mut tag, "CustomName", name.clone());
    } else if let Tag::Compound(map) = &mut tag {
        map.remove(&"CustomName".into());
    }
    put(&mut tag, "Facing", Tag::Byte(f.facing as i8));
    put(&mut tag, "block_pos", Tag::IntArray(f.pos.to_vec()));
    put(&mut tag, "ItemRotation", Tag::Byte(f.rotation as i8));
    put(
        &mut tag,
        "ItemDropChance",
        Tag::Float(f.drop_chance.to_bits()),
    );
    put(&mut tag, "Invisible", Tag::Byte(i8::from(f.invisible)));
    put(&mut tag, "Fixed", Tag::Byte(i8::from(f.fixed)));
    if let Tag::Compound(map) = &mut tag {
        map.remove(&horizon_world::nbt::JavaString::from("Item"));
    }
    if let Some(item) = &f.item {
        put(&mut tag, "Item", item_to_nbt(item));
    }
    tag
}
/// `ItemFrame.readAdditionalSaveData`: an absent Facing defaults to Direction.DOWN.
pub fn item_frame_from_nbt(tag: &Tag, id: u32) -> Option<ItemFrame> {
    let pos = attached_block_pos(tag)?;
    Some(ItemFrame {
        id,
        pos,
        facing: number(tag.get("Facing")).unwrap_or(0).clamp(0, 5) as u8,
        glow: kind(tag).as_deref() == Some("minecraft:glow_item_frame"),
        item: tag
            .get("Item")
            .and_then(item_from_nbt)
            .map(|s| crate::items::ItemStack { count: 1, ..s }),
        rotation: (number(tag.get("ItemRotation")).unwrap_or(0) & 7) as u8,
        drop_chance: float(tag.get("ItemDropChance")).map_or(1.0, |v| v as f32),
        fixed: flag(tag, "Fixed"),
        invisible: flag(tag, "Invisible"),
        check_interval: 0,
        custom_name: tag.get("CustomName").cloned(),
        random: crate::random_source::entity(),
    })
}
