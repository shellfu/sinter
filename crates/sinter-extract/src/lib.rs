mod body_terms;
mod extract;
mod language;
mod scope;

pub use extract::{ExtractError, Extractor};
pub use language::{
    LANGUAGES, LanguageSpec, ManifestSpec, ModuleRoot, manifest_root, spec_for_path,
};
pub use sinter_core::FileFacts;
