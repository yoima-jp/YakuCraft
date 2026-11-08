//! 起動失敗時の診断情報を確認する。
//!
//! `cargo run -p yc-llama --example verify_failure -- <server_path> <broken_model_path>`
//!
//! 壊れたモデルを渡すと `llama-server` は読み込み中に終了する。そのとき
//! [`yc_llama::LlamaError::ServerExited`] に終了ステータスと直近のログが入っていることを確認する。

use std::path::PathBuf;

use yc_llama::{Backend, LlamaError, LlamaServer, ServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let server_path = PathBuf::from(args.next().expect("server path"));
    let model_path = PathBuf::from(args.next().expect("broken model path"));

    let config = ServerConfig::new(server_path, model_path, Backend::Cpu);
    let result = LlamaServer::start(&config).await;

    match result {
        Ok(server) => {
            println!("unexpectedly started at {}", server.base_url());
            server.shutdown().await;
            Err("the broken model should not have loaded".into())
        }
        Err(LlamaError::ServerExited { status, log }) => {
            println!("status: {status}");
            println!("log lines: {}", log.lines().count());
            let relevant = log
                .lines()
                .find(|line| line.contains("error") || line.contains("failed"))
                .unwrap_or("<no error line>");
            println!("error line: {}", relevant.trim());
            assert!(
                log.lines().count() > 0,
                "the error must include the captured log"
            );
            assert!(
                log.contains("error") || log.contains("failed"),
                "the log should mention the failure"
            );
            println!("diagnostics look usable");
            Ok(())
        }
        Err(other) => {
            println!("unexpected error variant: {other}");
            Err("expected ServerExited".into())
        }
    }
}
