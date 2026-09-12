use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::Deserialize;

const CYBERIMPLANT: u32 = 300;
const BOOSTER: u32 = 303;
const CYBER_LEARNING: u32 = 745;

#[derive(Debug, Clone, Deserialize)]
pub struct ImplantType {
    pub type_id: u32,
    pub name: String,
    pub group_id: u32,
}

#[derive(Debug, Deserialize)]
struct ImplantAsset {
    source: String,
    source_sha256: String,
    generated: String,
    attribution: String,
    types: Vec<ImplantType>,
}

#[derive(Debug)]
pub struct ImplantClassifier {
    types: HashMap<u32, ImplantType>,
}

impl ImplantClassifier {
    pub fn bundled() -> Result<Self, String> {
        let asset: ImplantAsset = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/esi/implant-types.json"
        )))
        .map_err(|error| format!("invalid bundled implant SDE: {error}"))?;
        if asset.source.trim().is_empty()
            || asset.generated.trim().is_empty()
            || asset.attribution.trim().is_empty()
            || asset.source_sha256.len() != 64
            || !asset
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("implant SDE attribution metadata is absent".into());
        }
        let mut types = HashMap::new();
        for entry in asset.types {
            if entry.type_id == 0 || entry.name.trim().is_empty() || entry.group_id == 0 {
                return Err("implant SDE contains a malformed record".into());
            }
            if types.insert(entry.type_id, entry).is_some() {
                return Err("implant SDE contains duplicate type IDs".into());
            }
        }
        Ok(Self { types })
    }

    pub fn classify(&self, ids: &[u32]) -> Vec<String> {
        let mut seen = HashSet::new();
        ids.iter()
            .filter(|id| seen.insert(**id))
            .filter_map(|id| self.types.get(id))
            .filter(|entry| valuable(&entry.name, entry.group_id))
            .map(|entry| entry.name.clone())
            .collect()
    }
}

fn grade_of(name: &str) -> Option<u32> {
    let dashed = Regex::new(r"\b[A-Z]{2,4}-(\d{3,4})$").expect("constant regex");
    let legacy = Regex::new(r"\b[A-Z]{2,4}(\d{2,4})$").expect("constant regex");
    let captures = dashed.captures(name).or_else(|| legacy.captures(name))?;
    captures.get(1)?.as_str().chars().last()?.to_digit(10)
}

fn valuable(name: &str, group_id: u32) -> bool {
    match group_id {
        BOOSTER => false,
        CYBER_LEARNING => name.trim().to_ascii_lowercase().ends_with("- improved"),
        CYBERIMPLANT => true,
        _ => grade_of(name).is_none_or(|grade| grade >= 5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valuable_classifier_matches_fctool_rules() {
        assert!(valuable("Mid-grade Amulet Alpha", CYBERIMPLANT));
        assert!(valuable("Ocular Filter - Improved", CYBER_LEARNING));
        assert!(!valuable("Ocular Filter - Standard", CYBER_LEARNING));
        assert!(valuable("Zainou Shield Management SM-705", 739));
        assert!(!valuable("Zainou Shield Management SM-703", 739));
        assert!(!valuable("Agency 'Hardshell' TB9 Dose IV", BOOSTER));
        assert!(valuable("Michi's Excavation Augmentor", 1025));
        assert!(!valuable("Zainou 'Sharpshooter' ZMX10", 746));
    }

    #[test]
    fn bundled_asset_loads_and_duplicates_and_unknowns_are_skipped() {
        let classifier = ImplantClassifier::bundled().unwrap();
        let names = classifier.classify(&[10217, 10217, u32::MAX]);
        assert_eq!(names, ["Ocular Filter - Improved"]);
    }
}
