//! Which embedding models the component supports, and how the embedding model
//! setting names them.

/// The prefix of a setting that hands embedding to Ollama.
const OLLAMA_PREFIX: &str = "ollama:";

/// An embedding model the component supports, as the setting names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SupportedModel {
    /// A model from the fixed list, run on this PC.
    OnDevice(OnDeviceModel),
    /// A model that Ollama runs.
    Ollama(OllamaModelName),
}

impl SupportedModel {
    /// The model that `setting` names, or `None` when it names no supported
    /// model.
    pub(super) fn from_setting(setting: &str) -> Option<Self> {
        if let Some(name) = setting.strip_prefix(OLLAMA_PREFIX) {
            return OllamaModelName::new(name).map(Self::Ollama);
        }
        OnDeviceModel::from_repository(setting).map(Self::OnDevice)
    }
}

/// A model from the fixed list of models run on this PC, named by its Hugging
/// Face repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OnDeviceModel {
    RuriV3_30m,
    EmbeddingGemma300m,
    AllMiniLmL6V2,
}

impl OnDeviceModel {
    const ALL: [Self; 3] = [
        Self::RuriV3_30m,
        Self::EmbeddingGemma300m,
        Self::AllMiniLmL6V2,
    ];

    /// The Hugging Face repository, which is also the setting's value.
    pub(super) fn repository(self) -> &'static str {
        match self {
            Self::RuriV3_30m => "cl-nagoya/ruri-v3-30m",
            Self::EmbeddingGemma300m => "google/embeddinggemma-300m",
            Self::AllMiniLmL6V2 => "sentence-transformers/all-MiniLM-L6-v2",
        }
    }

    fn from_repository(repository: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|model| model.repository() == repository)
    }
}

/// The name of a model in Ollama: what follows `ollama:` in the setting. Never
/// empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OllamaModelName(String);

impl OllamaModelName {
    fn new(name: &str) -> Option<Self> {
        (!name.is_empty()).then(|| Self(name.to_owned()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(super) fn for_test(name: &str) -> Self {
        Self(name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_each_model_in_the_fixed_list_by_its_repository() {
        for (setting, model) in [
            ("cl-nagoya/ruri-v3-30m", OnDeviceModel::RuriV3_30m),
            (
                "google/embeddinggemma-300m",
                OnDeviceModel::EmbeddingGemma300m,
            ),
            (
                "sentence-transformers/all-MiniLM-L6-v2",
                OnDeviceModel::AllMiniLmL6V2,
            ),
        ] {
            assert_eq!(
                SupportedModel::from_setting(setting),
                Some(SupportedModel::OnDevice(model)),
                "{setting}"
            );
        }
    }

    #[test]
    fn takes_what_follows_the_ollama_prefix_as_the_ollama_model() {
        assert_eq!(
            SupportedModel::from_setting("ollama:nomic-embed-text:v1.5"),
            Some(SupportedModel::Ollama(OllamaModelName(
                "nomic-embed-text:v1.5".to_owned()
            )))
        );
        assert_eq!(
            SupportedModel::from_setting("ollama:x"),
            Some(SupportedModel::Ollama(OllamaModelName("x".to_owned())))
        );
    }

    #[test]
    fn names_no_model_for_other_settings() {
        for setting in [
            "",
            "ollama:",
            "Ollama:nomic-embed-text",
            "ollama",
            "nomic-embed-text",
            "cl-nagoya/ruri-v3-30m ",
            "CL-NAGOYA/RURI-V3-30M",
            "ruri-v3-30m",
            "google/embeddinggemma-300m-qat",
        ] {
            assert_eq!(SupportedModel::from_setting(setting), None, "{setting:?}");
        }
    }
}
