//! Shared holder/context resolution before SlotCollection read or live-write consumption.
use super::{Game, Owner, R, err};
use horizon_client::datapack::DataStore;
use serde_json::Value;
use std::collections::BTreeSet;
fn id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("minecraft:{raw}")
    }
}
/// Resolve holders into the existing structured-source representation, preserving
/// ordered terms, duplicate collection visits and each transform's nesting.
pub(in crate::game) fn expand(store: &DataStore, source: &Value) -> Result<Value, String> {
    fn resolve(
        store: &DataStore,
        source: &Value,
        active: &mut BTreeSet<String>,
    ) -> Result<Value, String> {
        if let Some(name) = source.as_str() {
            if let Some(tag) = name.strip_prefix('#') {
                let names = store.slot_source_tag(&id(tag))?;
                return resolve(
                    store,
                    &Value::Array(names.into_iter().map(Value::String).collect()),
                    active,
                );
            }
            let name = id(name);
            if !active.insert(name.clone()) {
                return Err(format!("Recursive slot source {name}"));
            }
            let result = store
                .slot_sources
                .get(&name)
                .ok_or_else(|| format!("Unknown slot source {name}"))
                .and_then(|s| resolve(store, s, active));
            active.remove(&name);
            return result;
        }
        if let Some(terms) = source.as_array() {
            return terms
                .iter()
                .map(|s| resolve(store, s, active))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array);
        }
        let mut value = source.clone();
        let kind = source["type"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("minecraft:");
        match kind {
            "group" => {
                let terms = &source["terms"];
                let terms = if terms.is_array() || terms.is_string() {
                    resolve(store, terms, active)?
                } else {
                    Value::Array(vec![resolve(store, terms, active)?])
                };
                value["terms"] = terms;
            }
            "contents" | "filtered" | "limit_slots" => {
                value["slot_source"] = resolve(store, &source["slot_source"], active)?
            }
            _ => {}
        }
        Ok(value)
    }
    resolve(store, source, &mut BTreeSet::new())
}
impl Game<'_> {
    /// Literal ranges retain priority; all remaining consumers resolve the same holder snapshot.
    pub(super) fn resolved_slot_source(&self, raw: &str) -> R<Option<Value>> {
        if horizon_client::command::args::slot_ranges()
            .iter()
            .any(|(name, _)| name == raw)
        {
            return Ok(None);
        }
        let value = if self.fns.store.slot_sources.contains_key(&id(raw)) {
            Value::String(id(raw))
        } else {
            let (tag, _) = horizon_world::nbt::parse_snbt(raw)
                .map_err(|error| err("argument.resource_or_id.failed_to_parse", &[&error]))?;
            horizon_client::scoreboard::tag_to_json(&tag)
        };
        expand(&self.fns.store, &value)
            .map(Some)
            .map_err(|error| err("argument.resource_or_id.failed_to_parse", &[&error]))
    }
    /// ItemCommands.getSlotsFromProvider binds CONTAINER separately from THIS_ENTITY.
    /// Other valid context targets are absent in COMMAND_SLOT_SOURCE, never Player fallbacks.
    pub(super) fn slot_source_owner(&self, container: &Owner, source: &Value) -> Option<Owner> {
        match source["source"].as_str().unwrap_or("container") {
            "container" => Some(container.clone()),
            "this" => self
                .command_source
                .and_then(|s| s.entity)
                .map(Owner::Entity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn referenced_transforms_keep_live_filter_and_limit_nesting() {
        let mut store = DataStore::default();
        store.slot_sources.insert(
            "t:range".into(),
            json!({"type":"slot_range","source":"this","slots":"weapon.mainhand"}),
        );
        store.slot_sources.insert("t:filtered".into(),json!({"type":"filtered","slot_source":"t:range","item_filter":{"items":"minecraft:stone"}}));
        store
            .slot_source_tags
            .insert("t:twice".into(), vec![("t:filtered".into(), true)]);
        let source = json!({"type":"limit_slots","limit":1,"slot_source":{"type":"group","terms":["t:filtered","t:filtered"]}});
        let value = expand(&store, &source).unwrap();
        let child = json!({"type":"filtered","slot_source":{"type":"slot_range","source":"this","slots":"weapon.mainhand"},"item_filter":{"items":"minecraft:stone"}});
        assert_eq!(
            value,
            json!({"type":"limit_slots","limit":1,"slot_source":{"type":"group","terms":[child,child]}})
        );
        assert_eq!(expand(&store, &json!("#t:twice")).unwrap(), json!([child]));
        assert!(expand(&store, &json!("t:absent")).is_err());
        store.slot_sources.insert(
            "t:cycle".into(),
            json!({"type":"limit_slots","limit":1,"slot_source":"t:cycle"}),
        );
        assert!(expand(&store, &json!("t:cycle")).is_err());
    }
}
