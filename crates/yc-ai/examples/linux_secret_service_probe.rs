#[cfg(target_os = "linux")]
use std::{env, error::Error};

#[cfg(target_os = "linux")]
use yc_ai::{AiError, CredentialManager, ProviderId};

#[cfg(target_os = "linux")]
const API_KEY: &str = "yakucraft-linux-verify-api-key";
#[cfg(target_os = "linux")]
const ACCOUNT_LABEL: &str = "linux-verify@example.invalid";

#[cfg(target_os = "linux")]
fn chatgpt_credential() -> String {
    serde_json::json!({
        "type": "oauth",
        "access": format!("{}{}", "a".repeat(5_000), "access-token"),
        "refresh": format!("{}{}", "r".repeat(1_000), "refresh-token"),
        "expires": 4_102_444_800_000_u64,
        "accountId": "yakucraft-linux-verify-account",
        "email": ACCOUNT_LABEL,
    })
    .to_string()
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    let action = env::args()
        .nth(1)
        .ok_or("expected save, verify, remove, or unavailable")?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(async move {
        let credentials = CredentialManager::linux()?;
        match action.as_str() {
            "save" => {
                credentials
                    .save_api_key(ProviderId::OpenAiCompatible, API_KEY)
                    .await?;
                credentials
                    .save_chatgpt_credential(&chatgpt_credential())
                    .await?;
                println!("Secret Service saved the API key and long ChatGPT credential.");
            }
            "verify" => {
                let api_key = credentials
                    .load_api_key(ProviderId::OpenAiCompatible)
                    .await?
                    .ok_or("the API key was not retained")?;
                if api_key.expose_secret() != API_KEY {
                    return Err("the API key did not match after process restart".into());
                }
                let chatgpt = credentials.status(ProviderId::ChatGpt).await?;
                if !chatgpt.configured || chatgpt.account_label.as_deref() != Some(ACCOUNT_LABEL) {
                    return Err("the long ChatGPT credential was not retained".into());
                }
                println!("A new process read both Secret Service credentials.");
            }
            "remove" => {
                credentials
                    .remove_api_key(ProviderId::OpenAiCompatible)
                    .await?;
                credentials.remove_api_key(ProviderId::ChatGpt).await?;
                println!("Secret Service verification credentials removed.");
            }
            "unavailable" => {
                match credentials
                    .save_api_key(ProviderId::OpenAiCompatible, API_KEY)
                    .await
                {
                    Err(AiError::CredentialStore) => {}
                    Err(error) => return Err(format!("unexpected credential error: {error}").into()),
                    Ok(_) => return Err("credential storage unexpectedly succeeded without Secret Service".into()),
                }
                println!("Secret Service absence returned a credential-store error without terminating the process.");
            }
            _ => return Err("expected save, verify, remove, or unavailable".into()),
        }
        Ok(())
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {}
