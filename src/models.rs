//! Local model catalogue for `GET /v1/models`.
//!
//! Listings come from configuration, or from a built-in catalogue when none
//! are configured. Providers are never contacted.

use crate::config::ModelConfig;
use serde::Serialize;

pub(crate) fn resolve_catalogue(configured: &[ModelConfig]) -> Vec<ModelConfig> {
    if configured.is_empty() {
        static_catalogue()
    } else {
        configured.to_vec()
    }
}

pub(crate) fn static_catalogue() -> Vec<ModelConfig> {
    vec![ModelConfig {
        id: "steve-test-model".into(),
        owned_by: "steve".into(),
        created: 0,
    }]
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ModelList {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ModelObject {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: String,
}

impl ModelList {
    pub(crate) fn from_catalogue(models: &[ModelConfig]) -> Self {
        Self {
            object: "list",
            data: models
                .iter()
                .map(|model| ModelObject {
                    id: model.id.clone(),
                    object: "model",
                    created: model.created,
                    owned_by: model.owned_by.clone(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_catalogue_lists_at_least_one_model() {
        let models = static_catalogue();
        assert!(!models.is_empty());
        let list = ModelList::from_catalogue(&models);
        assert_eq!(list.object, "list");
        assert_eq!(list.data[0].id, "steve-test-model");
        assert_eq!(list.data[0].object, "model");
        assert_eq!(list.data[0].owned_by, "steve");
    }

    #[test]
    fn configured_models_replace_the_static_catalogue() {
        let configured = vec![
            ModelConfig {
                id: "alpha".into(),
                owned_by: "lab".into(),
                created: 7,
            },
            ModelConfig {
                id: "beta".into(),
                owned_by: "lab".into(),
                created: 8,
            },
        ];
        let resolved = resolve_catalogue(&configured);
        assert_eq!(resolved, configured);

        let list = ModelList::from_catalogue(&resolved);
        let json = serde_json::to_value(&list).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "object": "list",
                "data": [
                    {"id": "alpha", "object": "model", "created": 7, "owned_by": "lab"},
                    {"id": "beta", "object": "model", "created": 8, "owned_by": "lab"}
                ]
            })
        );
    }

    #[test]
    fn empty_configuration_uses_the_static_catalogue() {
        let resolved = resolve_catalogue(&[]);
        assert_eq!(resolved, static_catalogue());
    }
}
