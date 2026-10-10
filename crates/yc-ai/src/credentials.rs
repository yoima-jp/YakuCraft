use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::{AiError, Result};

pub const CREDENTIAL_SERVICE: &str = "dev.yakucraft.app";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub enum ProviderId {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    #[serde(rename = "deepl")]
    DeepL,
    #[serde(rename = "chatgpt")]
    ChatGpt,
}

impl ProviderId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai-compatible",
            Self::DeepL => "deepl",
            Self::ChatGpt => "chatgpt",
        }
    }

    fn account(self) -> String {
        format!("provider:{}", self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub configured: bool,
    pub provider: ProviderId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("credential store operation failed")]
pub struct CredentialStoreError;

pub trait CredentialBackend: Send + 'static {
    fn get(
        &mut self,
        provider: ProviderId,
    ) -> std::result::Result<Option<String>, CredentialStoreError>;
    fn set(
        &mut self,
        provider: ProviderId,
        secret: &str,
    ) -> std::result::Result<(), CredentialStoreError>;
    fn delete(&mut self, provider: ProviderId) -> std::result::Result<(), CredentialStoreError>;
}

pub struct Secret(String);

impl Secret {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

enum Command {
    Get {
        provider: ProviderId,
        reply: oneshot::Sender<std::result::Result<Option<String>, CredentialStoreError>>,
    },
    Set {
        provider: ProviderId,
        secret: String,
        reply: oneshot::Sender<std::result::Result<(), CredentialStoreError>>,
    },
    Delete {
        provider: ProviderId,
        reply: oneshot::Sender<std::result::Result<(), CredentialStoreError>>,
    },
}

#[derive(Clone)]
pub struct CredentialManager {
    sender: Sender<Command>,
    chatgpt_generation: Arc<AtomicU64>,
    chatgpt_mutation: Arc<AsyncMutex<()>>,
}

impl CredentialManager {
    pub fn new(backend: impl CredentialBackend) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        // Windows Credential Manager は並行操作の順序を保証しないため、同期 API をこの専用 thread に集約する。
        thread::Builder::new()
            .name("yc-ai-credential-store".to_owned())
            .spawn(move || credential_worker(backend, receiver))
            .map_err(|_| AiError::CredentialStore)?;
        Ok(Self {
            sender,
            chatgpt_generation: Arc::new(AtomicU64::new(0)),
            chatgpt_mutation: Arc::new(AsyncMutex::new(())),
        })
    }

    #[cfg(windows)]
    pub fn windows() -> Result<Self> {
        Self::new(WindowsCredentialBackend::new()?)
    }

    pub async fn status(&self, provider: ProviderId) -> Result<CredentialStatus> {
        let secret = self.get(provider).await?;
        let account_label = if provider == ProviderId::ChatGpt {
            secret
                .as_deref()
                .and_then(crate::chatgpt::account_label_from_credential)
        } else {
            None
        };
        Ok(CredentialStatus {
            configured: secret.is_some(),
            provider,
            account_label,
        })
    }

    pub async fn save_api_key(
        &self,
        provider: ProviderId,
        secret: &str,
    ) -> Result<CredentialStatus> {
        if provider == ProviderId::ChatGpt {
            return Err(AiError::InvalidConfiguration);
        }
        if secret.trim().is_empty() {
            return Err(AiError::InvalidConfiguration);
        }
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Command::Set {
                provider,
                secret: secret.to_owned(),
                reply,
            })
            .map_err(|_| AiError::CredentialStore)?;
        receiver
            .await
            .map_err(|_| AiError::CredentialStore)?
            .map_err(|_| AiError::CredentialStore)?;
        Ok(CredentialStatus {
            configured: true,
            provider,
            account_label: None,
        })
    }

    pub async fn load_api_key(&self, provider: ProviderId) -> Result<Option<Secret>> {
        self.get(provider).await.map(|secret| secret.map(Secret))
    }

    /// OAuth credentialもAPI Keyと同じWindows Credential Managerへ保存する。
    /// SQLiteやRendererへtokenを渡さず、refresh時もこの経路だけを使う。
    pub async fn save_chatgpt_credential(&self, credential: &str) -> Result<CredentialStatus> {
        crate::chatgpt::validate_serialized_credential(credential)?;
        let _mutation = self.chatgpt_mutation.lock().await;
        self.set(ProviderId::ChatGpt, credential).await?;
        self.chatgpt_generation.fetch_add(1, Ordering::AcqRel);
        self.status(ProviderId::ChatGpt).await
    }

    /// refresh開始時の資格情報世代を読み取る。
    pub(crate) fn chatgpt_credential_generation(&self) -> u64 {
        self.chatgpt_generation.load(Ordering::Acquire)
    }

    /// 保存値と世代は同じ mutation lock の下で読み、更新途中の組み合わせを返さない。
    pub(crate) async fn load_chatgpt_credential(&self) -> Result<Option<(String, u64)>> {
        let _mutation = self.chatgpt_mutation.lock().await;
        Ok(self
            .get(ProviderId::ChatGpt)
            .await?
            .map(|credential| (credential, self.chatgpt_generation.load(Ordering::Acquire))))
    }

    /// 更新値を世代と保存済み資格情報に照合して保存する。不一致なら現行値を返し、未設定ならNone。
    pub(crate) async fn save_chatgpt_credential_if_generation(
        &self,
        credential: &str,
        expected_credential: &str,
        expected_generation: u64,
    ) -> Result<Option<(String, u64)>> {
        crate::chatgpt::validate_serialized_credential(credential)?;
        let _mutation = self.chatgpt_mutation.lock().await;
        let generation = self.chatgpt_generation.load(Ordering::Acquire);
        let Some(current_credential) = self.get(ProviderId::ChatGpt).await? else {
            return Ok(None);
        };
        if generation != expected_generation || current_credential != expected_credential {
            return Ok(Some((current_credential, generation)));
        }

        self.set(ProviderId::ChatGpt, credential).await?;
        let generation = self.chatgpt_generation.fetch_add(1, Ordering::AcqRel) + 1;
        Ok(Some((credential.to_owned(), generation)))
    }

    pub async fn remove_api_key(&self, provider: ProviderId) -> Result<CredentialStatus> {
        if provider == ProviderId::ChatGpt {
            let _mutation = self.chatgpt_mutation.lock().await;
            self.chatgpt_generation.fetch_add(1, Ordering::AcqRel);
            self.delete(provider).await?;
        } else {
            self.delete(provider).await?;
        }
        Ok(CredentialStatus {
            configured: false,
            provider,
            account_label: None,
        })
    }

    async fn get(&self, provider: ProviderId) -> Result<Option<String>> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Command::Get { provider, reply })
            .map_err(|_| AiError::CredentialStore)?;
        receiver
            .await
            .map_err(|_| AiError::CredentialStore)?
            .map_err(|_| AiError::CredentialStore)
    }

    async fn set(&self, provider: ProviderId, secret: &str) -> Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Command::Set {
                provider,
                secret: secret.to_owned(),
                reply,
            })
            .map_err(|_| AiError::CredentialStore)?;
        receiver
            .await
            .map_err(|_| AiError::CredentialStore)?
            .map_err(|_| AiError::CredentialStore)
    }

    async fn delete(&self, provider: ProviderId) -> Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(Command::Delete { provider, reply })
            .map_err(|_| AiError::CredentialStore)?;
        receiver
            .await
            .map_err(|_| AiError::CredentialStore)?
            .map_err(|_| AiError::CredentialStore)
    }
}

fn credential_worker(mut backend: impl CredentialBackend, receiver: mpsc::Receiver<Command>) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Get { provider, reply } => {
                let _ = reply.send(backend.get(provider));
            }
            Command::Set {
                provider,
                secret,
                reply,
            } => {
                let _ = reply.send(backend.set(provider, &secret));
            }
            Command::Delete { provider, reply } => {
                let _ = reply.send(backend.delete(provider));
            }
        }
    }
}

#[cfg(windows)]
struct WindowsCredentialBackend {
    store: std::sync::Arc<windows_native_keyring_store::Store>,
}

// Windows Credential Manager の credential blob は 2,560 バイトまでで、keyring は password を
// UTF-16 で保存するため 1 件あたり約 1,280 文字しか入らない（実測: 1,300 文字で TooLong）。
// ChatGPT の OAuth credential は JWT を含みこれを超えるので、長い値は複数の credential に分割する。
// 保存先は Credential Manager のままにして、平文ファイルへ逃がさない。
//
// v2 は本体に世代 UUID と分割数を置き、世代ごとのキーに断片を保存する。
// v1 の世代なし断片も引き続き読み込む。短い値は本体に直接保存する。
const MAX_SINGLE_CREDENTIAL_UTF16: usize = 1_200;
const CHUNK_UTF16: usize = 1_000;
const CHUNK_MARKER_PREFIX: &str = "yc-chunked:v1:";
const CHUNK_MARKER_V2_PREFIX: &str = "yc-chunked:v2:";
// 壊れた marker で大量の credential を読みに行かないための上限（64 KiB 相当）。
const MAX_CHUNKS: usize = 64;

/// 値を Credential Manager に収まる断片へ分ける。UTF-16 の長さで数え、文字の途中では切らない。
fn split_credential(secret: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;
    for character in secret.chars() {
        let units = character.len_utf16();
        if current_units + units > CHUNK_UTF16 {
            chunks.push(std::mem::take(&mut current));
            current_units = 0;
        }
        current.push(character);
        current_units += units;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

struct ChunkMarker {
    generation: Option<uuid::Uuid>,
    count: usize,
}

impl ChunkMarker {
    fn parse(stored: &str) -> std::result::Result<Option<Self>, CredentialStoreError> {
        let (generation, count) = if let Some(count) = stored.strip_prefix(CHUNK_MARKER_PREFIX) {
            (None, count)
        } else if let Some(marker) = stored.strip_prefix(CHUNK_MARKER_V2_PREFIX) {
            let (generation, count) = marker.split_once(':').ok_or(CredentialStoreError)?;
            (
                Some(uuid::Uuid::parse_str(generation).map_err(|_| CredentialStoreError)?),
                count,
            )
        } else {
            return Ok(None);
        };
        let count = count.parse::<usize>().map_err(|_| CredentialStoreError)?;
        if !(1..=MAX_CHUNKS).contains(&count) {
            return Err(CredentialStoreError);
        }
        Ok(Some(Self { generation, count }))
    }

    fn chunk_account(&self, account: &str, index: usize) -> String {
        match self.generation {
            Some(generation) => format!("{account}#chunk:{generation}:{index}"),
            None => format!("{account}#chunk{index}"),
        }
    }

    fn remove_chunks(
        &self,
        store: &mut impl AccountStore,
        account: &str,
    ) -> std::result::Result<(), CredentialStoreError> {
        for index in 1..=self.count {
            store.remove(&self.chunk_account(account, index))?;
        }
        Ok(())
    }
}

/// account 単位の読み書き。分割の規則をこの層に閉じ込め、in-memory 実装でテストできるようにする。
trait AccountStore {
    fn read(&mut self, account: &str) -> std::result::Result<Option<String>, CredentialStoreError>;
    fn write(
        &mut self,
        account: &str,
        value: &str,
    ) -> std::result::Result<(), CredentialStoreError>;
    fn remove(&mut self, account: &str) -> std::result::Result<(), CredentialStoreError>;
}

fn read_chunked(
    store: &mut impl AccountStore,
    account: &str,
) -> std::result::Result<Option<String>, CredentialStoreError> {
    let Some(stored) = store.read(account)? else {
        return Ok(None);
    };
    let Some(marker) = ChunkMarker::parse(&stored)? else {
        return Ok(Some(stored));
    };
    let mut secret = String::new();
    for index in 1..=marker.count {
        // 断片が欠けた値を「正しい credential」として返すと、壊れたトークンで認証し続けてしまう。
        secret.push_str(
            &store
                .read(&marker.chunk_account(account, index))?
                .ok_or(CredentialStoreError)?,
        );
    }
    Ok(Some(secret))
}

fn write_chunked(
    store: &mut impl AccountStore,
    account: &str,
    secret: &str,
) -> std::result::Result<(), CredentialStoreError> {
    let previous = store
        .read(account)?
        .as_deref()
        .and_then(|stored| ChunkMarker::parse(stored).ok().flatten());
    if secret.encode_utf16().count() <= MAX_SINGLE_CREDENTIAL_UTF16 {
        store.write(account, secret)?;
    } else {
        let chunks = split_credential(secret);
        if chunks.len() > MAX_CHUNKS {
            return Err(CredentialStoreError);
        }
        let generation = uuid::Uuid::new_v4();
        let marker = ChunkMarker {
            generation: Some(generation),
            count: chunks.len(),
        };
        // 旧 marker が参照する断片は触らない。全断片の保存後に本体だけを原子的に切り替える。
        let staged = (|| {
            for (offset, chunk) in chunks.iter().enumerate() {
                store.write(&marker.chunk_account(account, offset + 1), chunk)?;
            }
            store.write(
                account,
                &format!("{CHUNK_MARKER_V2_PREFIX}{generation}:{}", marker.count),
            )
        })();
        if let Err(error) = staged {
            // 失敗した世代だけを回収する。回収失敗でも旧 credential の読み取りには影響しない。
            if marker.remove_chunks(store, account).is_err() {
                // 資格情報や保存キーをログへ出さず、回収失敗だけを記録する。
                tracing::warn!(
                    "staged credential chunks could not be removed after a write failure"
                );
            }
            return Err(error);
        }
    }
    if let Some(previous) = previous {
        previous.remove_chunks(store, account)?;
    }
    Ok(())
}

fn remove_chunked(
    store: &mut impl AccountStore,
    account: &str,
) -> std::result::Result<(), CredentialStoreError> {
    let marker = store
        .read(account)?
        .as_deref()
        .and_then(|stored| ChunkMarker::parse(stored).ok().flatten());
    if let Some(marker) = marker {
        marker.remove_chunks(store, account)?;
    }
    store.remove(account)
}

#[cfg(windows)]
impl WindowsCredentialBackend {
    fn new() -> Result<Self> {
        let store =
            windows_native_keyring_store::Store::new().map_err(|_| AiError::CredentialStore)?;
        Ok(Self { store })
    }

    fn entry(
        &self,
        account: &str,
    ) -> std::result::Result<keyring_core::Entry, CredentialStoreError> {
        use keyring_core::api::CredentialStoreApi;
        CredentialStoreApi::build(self.store.as_ref(), CREDENTIAL_SERVICE, account, None)
            .map_err(|_| CredentialStoreError)
    }
}

#[cfg(windows)]
impl AccountStore for WindowsCredentialBackend {
    fn read(&mut self, account: &str) -> std::result::Result<Option<String>, CredentialStoreError> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialStoreError),
        }
    }

    fn write(
        &mut self,
        account: &str,
        value: &str,
    ) -> std::result::Result<(), CredentialStoreError> {
        self.entry(account)?
            .set_password(value)
            .map_err(|_| CredentialStoreError)
    }

    fn remove(&mut self, account: &str) -> std::result::Result<(), CredentialStoreError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredentialStoreError),
        }
    }
}

#[cfg(windows)]
impl CredentialBackend for WindowsCredentialBackend {
    fn get(
        &mut self,
        provider: ProviderId,
    ) -> std::result::Result<Option<String>, CredentialStoreError> {
        read_chunked(self, &provider.account())
    }

    fn set(
        &mut self,
        provider: ProviderId,
        secret: &str,
    ) -> std::result::Result<(), CredentialStoreError> {
        write_chunked(self, &provider.account(), secret)
    }

    fn delete(&mut self, provider: ProviderId) -> std::result::Result<(), CredentialStoreError> {
        remove_chunked(self, &provider.account())
    }
}

#[cfg(test)]
mod chunk_tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct MemoryAccounts {
        values: HashMap<String, String>,
        fail_write_after: Option<usize>,
    }

    impl AccountStore for MemoryAccounts {
        fn read(
            &mut self,
            account: &str,
        ) -> std::result::Result<Option<String>, CredentialStoreError> {
            Ok(self.values.get(account).cloned())
        }

        fn write(
            &mut self,
            account: &str,
            value: &str,
        ) -> std::result::Result<(), CredentialStoreError> {
            // 実機の上限を再現し、どの書き込みも 1 件の上限を超えないことを確かめる。
            assert!(
                value.encode_utf16().count() <= 1_280,
                "credential blob over Windows limit"
            );
            if let Some(remaining) = self.fail_write_after.as_mut() {
                if *remaining == 0 {
                    self.fail_write_after = None;
                    return Err(CredentialStoreError);
                }
                *remaining -= 1;
            }
            self.values.insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn remove(&mut self, account: &str) -> std::result::Result<(), CredentialStoreError> {
            self.values.remove(account);
            Ok(())
        }
    }

    #[test]
    fn short_values_stay_in_a_single_credential() {
        let mut store = MemoryAccounts::default();
        write_chunked(&mut store, "provider:openai", "sk-short").unwrap();
        assert_eq!(store.values.len(), 1);
        assert_eq!(
            read_chunked(&mut store, "provider:openai")
                .unwrap()
                .as_deref(),
            Some("sk-short")
        );
    }

    #[test]
    fn long_values_round_trip_through_chunks_and_shrink_cleanly() {
        let mut store = MemoryAccounts::default();
        let long = "x".repeat(4_321);
        write_chunked(&mut store, "provider:chatgpt", &long).unwrap();
        assert_eq!(
            read_chunked(&mut store, "provider:chatgpt").unwrap(),
            Some(long)
        );
        assert_eq!(store.values.len(), 1 + 5);

        // 短い値で上書きしたら、古い断片を残さない。
        write_chunked(&mut store, "provider:chatgpt", "short").unwrap();
        assert_eq!(store.values.len(), 1);
        assert_eq!(
            read_chunked(&mut store, "provider:chatgpt")
                .unwrap()
                .as_deref(),
            Some("short")
        );

        write_chunked(&mut store, "provider:chatgpt", &"y".repeat(2_500)).unwrap();
        remove_chunked(&mut store, "provider:chatgpt").unwrap();
        assert!(store.values.is_empty());
    }

    #[test]
    fn missing_chunks_are_an_error_not_a_truncated_secret() {
        let mut store = MemoryAccounts::default();
        write_chunked(&mut store, "provider:chatgpt", &"z".repeat(3_000)).unwrap();
        let marker = ChunkMarker::parse(&store.values["provider:chatgpt"])
            .unwrap()
            .unwrap();
        store
            .values
            .remove(&marker.chunk_account("provider:chatgpt", 2));
        assert!(read_chunked(&mut store, "provider:chatgpt").is_err());
    }

    fn legacy_store(secret: &str) -> MemoryAccounts {
        let mut store = MemoryAccounts::default();
        let chunks = split_credential(secret);
        for (offset, chunk) in chunks.iter().enumerate() {
            store.values.insert(
                format!("provider:chatgpt#chunk{}", offset + 1),
                chunk.clone(),
            );
        }
        store.values.insert(
            "provider:chatgpt".into(),
            format!("{CHUNK_MARKER_PREFIX}{}", chunks.len()),
        );
        store
    }

    #[test]
    fn failed_chunk_or_marker_write_preserves_previous_credential() {
        let old = "old".repeat(1000);
        let new = "new".repeat(1000);
        for legacy in [false, true] {
            // 三断片の各書き込みと最後の marker commit をそれぞれ失敗させる。
            for failure in 0..=3 {
                let mut store = if legacy {
                    legacy_store(&old)
                } else {
                    MemoryAccounts::default()
                };
                if !legacy {
                    write_chunked(&mut store, "provider:chatgpt", &old).unwrap();
                }
                let original = store.values.clone();
                store.fail_write_after = Some(failure);
                assert!(write_chunked(&mut store, "provider:chatgpt", &new).is_err());
                assert_eq!(
                    read_chunked(&mut store, "provider:chatgpt").unwrap(),
                    Some(old.clone())
                );
                assert_eq!(store.values, original, "staged chunks must be removed");
                write_chunked(&mut store, "provider:chatgpt", &new).unwrap();
                assert_eq!(
                    read_chunked(&mut store, "provider:chatgpt").unwrap(),
                    Some(new.clone())
                );
                assert_eq!(store.values.len(), 4);
                remove_chunked(&mut store, "provider:chatgpt").unwrap();
                assert!(store.values.is_empty());
            }
        }
    }

    #[test]
    fn legacy_chunks_read_update_shrink_and_remove_cleanly() {
        let old = "x".repeat(4321);
        for replacement in ["short".to_owned(), "y".repeat(2500)] {
            let mut store = legacy_store(&old);
            assert_eq!(
                read_chunked(&mut store, "provider:chatgpt").unwrap(),
                Some(old.clone())
            );
            write_chunked(&mut store, "provider:chatgpt", &replacement).unwrap();
            assert_eq!(
                read_chunked(&mut store, "provider:chatgpt").unwrap(),
                Some(replacement)
            );
            assert!(!store.values.contains_key("provider:chatgpt#chunk1"));
            remove_chunked(&mut store, "provider:chatgpt").unwrap();
            assert!(store.values.is_empty());
        }
        let mut store = legacy_store(&old);
        remove_chunked(&mut store, "provider:chatgpt").unwrap();
        assert!(store.values.is_empty());
    }

    #[test]
    fn failed_update_of_short_credential_preserves_it() {
        let mut store = MemoryAccounts::default();
        write_chunked(&mut store, "provider:chatgpt", "old").unwrap();
        let original = store.values.clone();
        store.fail_write_after = Some(1);
        assert!(write_chunked(&mut store, "provider:chatgpt", &"new".repeat(1000)).is_err());
        assert_eq!(store.values, original);
    }

    #[test]
    fn invalid_markers_fail_reads_but_allow_replacement_or_removal() {
        for invalid in ["yc-chunked:v1:65", "yc-chunked:v2:invalid:3"] {
            let mut store = MemoryAccounts::default();
            store
                .values
                .insert("provider:chatgpt".into(), invalid.into());
            assert!(read_chunked(&mut store, "provider:chatgpt").is_err());
            write_chunked(&mut store, "provider:chatgpt", "replacement").unwrap();
            assert_eq!(
                read_chunked(&mut store, "provider:chatgpt")
                    .unwrap()
                    .as_deref(),
                Some("replacement")
            );
            store
                .values
                .insert("provider:chatgpt".into(), invalid.into());
            remove_chunked(&mut store, "provider:chatgpt").unwrap();
            assert!(store.values.is_empty());
        }
    }

    #[test]
    fn splits_on_utf16_units_without_breaking_characters() {
        let secret = "🔑".repeat(900);
        let chunks = split_credential(&secret);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.encode_utf16().count() <= CHUNK_UTF16)
        );
        assert_eq!(chunks.concat(), secret);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{CredentialManager, ProviderId};

    #[tokio::test]
    #[ignore = "reads Windows Credential Manager; run only in a deliberate local environment"]
    async fn reads_configured_provider_status_from_windows_credential_manager() {
        let manager = CredentialManager::windows().expect("credential manager should initialize");
        let status = manager
            .status(ProviderId::OpenAiCompatible)
            .await
            .expect("credential status should be readable");
        assert_eq!(status.provider, ProviderId::OpenAiCompatible);
    }
}

#[cfg(all(test, windows))]
mod windows_chunk_tests {
    use super::*;

    #[test]
    #[ignore = "writes to Windows Credential Manager; run only in a deliberate local environment"]
    fn long_credentials_round_trip_through_windows_credential_manager() {
        let mut backend = WindowsCredentialBackend::new().unwrap();
        let account = "probe:chunked-credential";
        let long = "j".repeat(4_000);
        write_chunked(&mut backend, account, &long).unwrap();
        assert_eq!(read_chunked(&mut backend, account).unwrap(), Some(long));
        remove_chunked(&mut backend, account).unwrap();
        assert_eq!(read_chunked(&mut backend, account).unwrap(), None);
    }
}
