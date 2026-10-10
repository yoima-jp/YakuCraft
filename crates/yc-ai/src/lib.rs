mod batch;
mod credentials;
mod error;
mod http;
mod models;
mod prompt;
mod provider;
mod url_policy;

pub mod chatgpt;
pub mod deepl;
pub mod openai;
pub mod openai_compatible;

pub use chatgpt::{CHATGPT_MODELS, ChatGptLogin, ChatGptModel, ChatGptProvider};
pub use deepl::DeepLProvider;
pub use openai::OpenAiProvider;
pub use openai_compatible::OpenAiCompatibleProvider;

pub use batch::{DEFAULT_CONCURRENCY, MAX_BATCH_ENTRIES, MAX_BATCH_SOURCE_CHARACTERS};
pub use credentials::{
    CredentialBackend, CredentialManager, CredentialStatus, CredentialStoreError, ProviderId,
    Secret,
};
pub use error::{AiError, Result};
pub use models::{
    GlossaryTerm, TranslationContext, TranslationEntry, TranslationOutcome, TranslationProgress,
};
pub use provider::{
    JobManager, ProviderCapabilities, TranslationJob, TranslationOptions, TranslationProvider,
    translate_entries,
};
pub use url_policy::{UrlPolicyError, validate_provider_url};

#[cfg(test)]
mod tests;
