use std::io;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init(logs_dir: &Path, debug_logs: bool) -> io::Result<WorkerGuard> {
    let level = log_level(debug_logs);
    let appender = tracing_appender::rolling::daily(logs_dir, "yakucraft.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // API Key、Authorization header、AI request headers、credential、翻訳本文は絶対に記録しない。
    tracing_subscriber::registry()
        .with(EnvFilter::new(level))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;

    Ok(guard)
}

fn log_level(debug_logs: bool) -> &'static str {
    if debug_logs { "debug" } else { "info" }
}

#[cfg(test)]
mod tests {
    use super::log_level;

    #[test]
    fn debug_log_setting_selects_the_startup_filter() {
        assert_eq!(log_level(true), "debug");
        assert_eq!(log_level(false), "info");
    }
}
