//! `block.properties` (spec 4.1): `block.<id>=<block> <block> ...`, each block
//! `[namespace:]name` with optional state filters `:<property>=<v1>,<v2>`. Values in one filter
//! are alternatives; separate filters must all match. Names the game does not know are simply
//! never asked for. Tags (`%tag`) are resolved against the current world before lookup.

use crate::Properties;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    id: i32,
    /// `minecraft:stone`.
    name: String,
    filters: Vec<(String, Vec<String>)>,
    tag: bool,
}

/// The ids `block.properties` gives block states.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockIds {
    rules: Vec<Rule>,
    configured: bool,
    tagged: bool,
    /// Lines skipped and why.
    pub notes: Vec<String>,
}

impl BlockIds {
    pub fn parse(properties: &Properties) -> Self {
        Self::parse_kind(properties, "block")
    }

    /// The same for `<kind>.<id>=` lines: `entity` in `entity.properties`, `item` in
    /// `item.properties` (4.1); names there carry no state filters.
    pub fn parse_kind(properties: &Properties, kind: &str) -> Self {
        let mut out = Self::default();
        let prefix = format!("{kind}.");
        for e in &properties.entries {
            let Some(id) = e.key.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let Ok(id) = id.parse::<i32>() else {
                // `layer.*` (4.2) and other keys are not ids.
                continue;
            };
            for word in e.value.split_whitespace() {
                let (tag, word) = match word.strip_prefix('%') {
                    Some(tag) => (true, tag),
                    None => (false, word),
                };
                let (names, filters): (Vec<&str>, Vec<&str>) =
                    word.split(':').partition(|p| !p.contains('='));
                let name = match names[..] {
                    [name] => format!("minecraft:{name}"),
                    [namespace, name] => format!("{namespace}:{name}"),
                    _ => {
                        out.notes
                            .push(format!("line {}: not a block name: {word}", e.line));
                        continue;
                    }
                };
                let filters = filters
                    .iter()
                    .filter_map(|f| f.split_once('='))
                    .map(|(k, v)| (k.to_owned(), v.split(',').map(str::to_owned).collect()))
                    .collect();
                let name = if tag { format!("%{name}") } else { name };
                out.rules.push(Rule {
                    id,
                    name,
                    filters,
                    tag,
                });
                out.configured = true;
                out.tagged |= tag;
            }
        }
        out
    }

    /// Expand tag rules using this world's membership. Explicit names take precedence over
    /// tags (IRIS_TAG_SUPPORT=2), even when the tag precedes the explicit name in the file.
    /// Resolve again from the parsed rules after a tag reload; missing tags add no matches.
    pub fn resolve_tags(&self, tags: &HashMap<String, HashSet<String>>) -> Self {
        let mut out = self.clone();
        out.rules = self
            .rules
            .iter()
            .flat_map(|rule| {
                if !rule.tag {
                    return vec![rule.clone()];
                }
                let mut names: Vec<_> = tags
                    .get(rule.name.strip_prefix('%').unwrap_or(&rule.name))
                    .into_iter()
                    .flatten()
                    .collect();
                names.sort();
                names
                    .into_iter()
                    .map(|name| Rule {
                        name: name.clone(),
                        ..rule.clone()
                    })
                    .collect()
            })
            .collect();
        out
    }

    /// The id of block `name` (`minecraft:stone`) in the state with `properties`. The rule with
    /// explicit name wins over tags, then the most filters that match, then file order.
    pub fn id<'a>(&self, name: &str, properties: impl Fn(&str) -> Option<&'a str>) -> Option<i32> {
        self.rules
            .iter()
            .filter(|r| r.name == name)
            .filter(|r| {
                r.filters
                    .iter()
                    .all(|(k, vs)| properties(k).is_some_and(|v| vs.iter().any(|x| x == v)))
            })
            .min_by_key(|r| (r.tag, std::cmp::Reverse(r.filters.len())))
            .map(|r| r.id)
    }

    /// Every name without state filters and its id (the first line naming it wins): what
    /// `entity.properties` and `item.properties` hold.
    pub fn plain(&self) -> std::collections::HashMap<String, i32> {
        let mut out = std::collections::HashMap::new();
        for r in self
            .rules
            .iter()
            .filter(|r| r.filters.is_empty() && !r.tag)
            .chain(
                self.rules
                    .iter()
                    .filter(|r| r.filters.is_empty() && r.tag && !r.name.starts_with('%')),
            )
        {
            out.entry(r.name.clone()).or_insert(r.id);
        }
        out
    }

    /// Whether tag synchronization can change these assignments, even if tags are absent.
    pub fn uses_tags(&self) -> bool {
        self.tagged
    }

    pub fn is_empty(&self) -> bool {
        !self.configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Macros;

    #[test]
    fn names_states_and_precedence() {
        let text = "block.10=stone minecraft:dirt mod:thing \\\n grass_block:snowy=false\n\
                    block.11=grass_block\nblock.12=oak_leaves:persistent=true:distance=1,2 %leaves\n\
                    layer.translucent=glass\n";
        let ids = BlockIds::parse(&Properties::parse(text, &Macros::standard()));
        let none = |_: &str| None;
        assert_eq!(ids.id("minecraft:stone", none), Some(10));
        assert_eq!(ids.id("mod:thing", none), Some(10));
        assert_eq!(ids.id("minecraft:glass", none), None);
        let snowy = |s: &'static str| move |k: &str| (k == "snowy").then_some(s);
        // The filtered entry is the more specific one.
        assert_eq!(ids.id("minecraft:grass_block", snowy("false")), Some(10));
        assert_eq!(ids.id("minecraft:grass_block", snowy("true")), Some(11));
        let leaves = |p: &'static str, d: &'static str| {
            move |k: &str| match k {
                "persistent" => Some(p),
                "distance" => Some(d),
                _ => None,
            }
        };
        assert_eq!(
            ids.id("minecraft:oak_leaves", leaves("true", "2")),
            Some(12)
        );
        assert_eq!(ids.id("minecraft:oak_leaves", leaves("true", "3")), None);
        assert_eq!(ids.id("minecraft:oak_leaves", leaves("false", "1")), None);
        assert!(ids.notes.is_empty(), "{:?}", ids.notes);
    }

    #[test]
    fn tag_only_assignments_survive_parsing() {
        let ids = BlockIds::parse(&Properties::parse(
            "block.42=%minecraft:logs\n",
            &Macros::standard(),
        ));
        assert!(
            !ids.is_empty(),
            "a valid tag assignment must not be discarded"
        );
    }

    #[test]
    fn tags_match_members_states_and_preserve_explicit_fallbacks_after_reload() {
        let parsed = BlockIds::parse(&Properties::parse(
            "block.42=%logs:axis=x %test:unknown\nblock.7=oak_log\nblock.90=%test:wood\n",
            &Macros::standard(),
        ));
        let tags = HashMap::from([
            (
                "minecraft:logs".into(),
                HashSet::from(["minecraft:oak_log".into(), "test:log".into()]),
            ),
            ("test:wood".into(), HashSet::from(["test:log".into()])),
        ]);
        assert_eq!(parsed.id("minecraft:logs", |_| Some("x")), None);
        assert!(!parsed.plain().contains_key("%minecraft:logs"));
        let resolved = parsed.resolve_tags(&tags);
        assert_eq!(resolved.id("minecraft:oak_log", |_| Some("x")), Some(7));
        assert_eq!(resolved.id("test:log", |_| Some("x")), Some(42));
        assert_eq!(resolved.id("test:log", |_| Some("y")), Some(90));
        assert_eq!(resolved.id("test:unlisted", |_| None), None);
        let reloaded = parsed.resolve_tags(&HashMap::new());
        assert_eq!(reloaded.id("test:log", |_| Some("x")), None);
        assert_eq!(reloaded.id("minecraft:oak_log", |_| None), Some(7));
        assert!(!reloaded.is_empty());
        assert!(reloaded.uses_tags());
        let absent = BlockIds::parse(&Properties::parse("block.42=%absent", &Macros::standard()))
            .resolve_tags(&HashMap::new());
        assert!(
            !absent.is_empty(),
            "a present ID file keeps its unmatched fallback"
        );
        assert_eq!(absent.id("test:log", |_| None), None);
        for kind in ["item", "entity"] {
            let properties = Properties::parse(
                &format!("{kind}.42=%test:group\n{kind}.7=test:explicit"),
                &Macros::standard(),
            );
            let ids = BlockIds::parse_kind(&properties, kind).resolve_tags(&HashMap::from([(
                "test:group".into(),
                HashSet::from(["test:member".into(), "test:explicit".into()]),
            )]));
            assert_eq!(ids.plain()["test:member"], 42);
            assert_eq!(ids.plain()["test:explicit"], 7);
        }
    }

    #[test]
    fn advertised_tag_branch_and_legacy_explicit_fallback_both_match() {
        let text =
            "#if IRIS_TAG_SUPPORT >= 2\nblock.42=%test:logs\n#else\nblock.7=test:log\n#endif";
        let tags = HashMap::from([("test:logs".into(), HashSet::from(["test:log".into()]))]);
        let macros = Macros::standard();
        let ids = BlockIds::parse(&Properties::parse(text, &macros)).resolve_tags(&tags);
        assert_eq!(ids.id("test:log", |_| None), Some(42));
        let mut legacy = macros;
        legacy.undef("IRIS_TAG_SUPPORT");
        let fallback = BlockIds::parse(&Properties::parse(text, &legacy)).resolve_tags(&tags);
        assert!(!fallback.uses_tags());
        assert_eq!(fallback.id("test:log", |_| None), Some(7));
        assert!(BlockIds::default().is_empty());
        assert!(!BlockIds::default().uses_tags());
    }

    #[test]
    fn entity_lines_by_name() {
        let text = "entity.10=zombie minecraft:husk\nentity.11=zombie mod:thing\nblock.5=stone\n";
        let ids = BlockIds::parse_kind(&Properties::parse(text, &Macros::standard()), "entity");
        let plain = ids.plain();
        assert_eq!(
            plain["minecraft:zombie"], 10,
            "the first line naming it wins"
        );
        assert_eq!(plain["minecraft:husk"], 10);
        assert_eq!(plain["mod:thing"], 11);
        assert!(!plain.contains_key("minecraft:stone"));
    }
}
