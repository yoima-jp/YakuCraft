#[derive(Clone, Debug, Default)]
pub(crate) struct ManifestAttributes {
    pub implementation_title: Option<String>,
    pub implementation_version: Option<String>,
    pub automatic_module_name: Option<String>,
}

pub(crate) fn parse_manifest(bytes: &[u8]) -> (ManifestAttributes, Vec<String>) {
    let content = String::from_utf8_lossy(bytes);
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let mut attributes = std::collections::BTreeMap::<String, String>::new();
    let mut current_key: Option<String> = None;
    let mut diagnostics = Vec::new();
    let mut in_main_section = true;

    for (line_index, line) in content.lines().enumerate() {
        if line.is_empty() {
            in_main_section = false;
            current_key = None;
            continue;
        }
        if let Some(continuation) = line.strip_prefix(' ') {
            if let Some(key) = current_key.as_ref() {
                if in_main_section {
                    attributes
                        .entry(key.clone())
                        .and_modify(|value| value.push_str(continuation));
                }
            } else {
                diagnostics.push(format!(
                    "行 {}: 継続行の前に属性がありません",
                    line_index + 1
                ));
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            diagnostics.push(format!("行 {}: 属性行に `:` がありません", line_index + 1));
            current_key = None;
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            diagnostics.push(format!("行 {}: 属性名が空です", line_index + 1));
            current_key = None;
            continue;
        }
        let value = value.trim_start().to_owned();
        if in_main_section {
            attributes.insert(key.clone(), value);
        }
        current_key = Some(key);
    }

    (
        ManifestAttributes {
            implementation_title: attributes.get("implementation-title").cloned(),
            implementation_version: attributes.get("implementation-version").cloned(),
            automatic_module_name: attributes.get("automatic-module-name").cloned(),
        },
        diagnostics,
    )
}
