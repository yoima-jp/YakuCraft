// Parser messages can contain translation text, which must never be logged.
pub fn parse_error_detail(kind: &str, path: &str, line: usize, column: usize) -> String {
    format!("{kind}: {path}:{line}:{column}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_detail_excludes_input() {
        let error =
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(r#""秘密の訳文""#)
                .unwrap_err();
        let detail = super::parse_error_detail(
            &format!("JSON {:?}", error.classify()),
            "ja_jp.json",
            error.line(),
            error.column(),
        );
        assert!(!detail.contains("秘密の訳文"));
        assert!(detail.contains("ja_jp.json:1:"));
    }
}
