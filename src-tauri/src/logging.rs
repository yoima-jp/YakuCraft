use std::io;
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn sanitize_sensitive_lines(value: &str) -> String {
    // OSやProviderのエラー文に資格情報が混ざっても、ログへ値を残さない。
    value
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            let has_sensitive_label = [
                "authorization",
                "api key",
                "api_key",
                "apikey",
                "credential",
                "password",
                "secret",
                "bearer ",
                "token",
                "access_token",
                "refresh_token",
                "sk-",
                "sk_",
                "AIza",
                "gsk_",
                "xai-",
                "pplx-",
                "hf_",
            ]
            .iter()
            .any(|marker| lower.contains(&marker.to_ascii_lowercase()));

            if has_sensitive_label {
                "[redacted sensitive error content]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

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
