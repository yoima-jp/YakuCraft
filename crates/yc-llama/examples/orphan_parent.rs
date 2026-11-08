//! `verify_runtime` の孤児検査用の親プロセス。
//!
//! `llama-server` を起動したまま待機し、自身の PID を出力する。呼び出し側がこれを
//! 強制終了したとき、Job Object / kill_on_drop によって子が道連れになるかを確認する。
//!
//! `cargo run -p yc-llama --example orphan_parent -- <server_path> <model_path>`

use std::time::Duration;

use yc_llama::{Backend, LlamaServer, ServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let server_path = args.next().expect("server path");
    let model_path = args.next().expect("model path");

    let config = ServerConfig::new(server_path, model_path, Backend::Cpu);
    let server = LlamaServer::start(&config).await?;
    // 呼び出し側はこの行で親 PID を学び、後で強制終了する。
    println!("{}", std::process::id());
    println!("server={}", server.base_url());

    tokio::time::sleep(Duration::from_secs(120)).await;
    Ok(())
}
