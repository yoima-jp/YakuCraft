use serde_json::Value;

use crate::{LoaderKind, MetadataParseError, make_mod_info};

pub(crate) fn parse_fabric(
    bytes: &[u8],
    source: &str,
) -> Result<(Vec<crate::ModInfo>, Vec<String>), MetadataParseError> {
    let root = parse_json_relaxed(bytes)?;
    let mod_id = required_string(&root, "id")?;
    let name = text(root.get("name"));
    let version = text(root.get("version"));
    let minecraft_version = root
        .get("depends")
        .and_then(|depends| depends.get("minecraft"))
        .and_then(json_constraint);
    let nested_jars = root
        .get("jars")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|jar| match jar {
            Value::String(path) => Some(path.clone()),
            _ => jar.get("file").and_then(|value| text(Some(value))),
        })
        .collect();

    Ok((
        vec![make_mod_info(
            Some(mod_id),
            name,
            version,
            LoaderKind::Fabric,
            minecraft_version,
            Some(source.to_owned()),
        )],
        nested_jars,
    ))
}

pub(crate) fn parse_quilt(
    bytes: &[u8],
    source: &str,
) -> Result<(Vec<crate::ModInfo>, Vec<String>), MetadataParseError> {
    let root = parse_json_relaxed(bytes)?;
    let loader = root
        .get("quilt_loader")
        .filter(|value| value.is_object())
        .ok_or(MetadataParseError::MissingField("quilt_loader"))?;
    let metadata = loader.get("metadata").unwrap_or(&Value::Null);
    let mod_id = required_string(loader, "id")?;
    let name = text(metadata.get("name")).or_else(|| text(root.get("name")));
    let version = text(loader.get("version")).or_else(|| text(root.get("version")));
    let minecraft_version = loader
        .get("depends")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|dependency| text(dependency.get("id")).as_deref() == Some("minecraft"))
        .and_then(|dependency| dependency.get("versions"))
        .and_then(json_constraint);
    let nested_jars = loader
        .get("jars")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|jar| match jar {
            Value::String(path) => Some(path.clone()),
            _ => jar.get("file").and_then(|value| text(Some(value))),
        })
        .collect();

    Ok((
        vec![make_mod_info(
            Some(mod_id),
            name,
            version,
            LoaderKind::Quilt,
            minecraft_version,
            Some(source.to_owned()),
        )],
        nested_jars,
    ))
}

pub(crate) fn parse_legacy(
    bytes: &[u8],
    source: &str,
) -> Result<(Vec<crate::ModInfo>, Vec<String>), MetadataParseError> {
    let root = parse_json_relaxed(bytes)?;
    let records = root
        .as_array()
        .or_else(|| root.get("modList").and_then(Value::as_array))
        .ok_or(MetadataParseError::MissingArray("modList"))?;
    let mods = records
        .iter()
        .map(|record| {
            make_mod_info(
                text(record.get("modid")).or_else(|| text(record.get("modId"))),
                text(record.get("name")),
                text(record.get("version")),
                LoaderKind::LegacyForge,
                text(record.get("mcversion")).or_else(|| text(record.get("mcVersion"))),
                Some(source.to_owned()),
            )
        })
        .collect();
    Ok((mods, Vec::new()))
}

pub(crate) fn parse_jarjar(bytes: &[u8]) -> Result<Vec<String>, MetadataParseError> {
    let root = parse_json_relaxed(bytes)?;
    let jars = root
        .get("jars")
        .and_then(Value::as_array)
        .ok_or(MetadataParseError::MissingArray("jars"))?;
    Ok(jars
        .iter()
        .filter_map(|jar| text(jar.get("path")))
        .collect())
}

pub fn parse_json_relaxed(bytes: &[u8]) -> Result<Value, MetadataParseError> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(_) => Ok(serde_json::from_str(&sanitize_json(text))?),
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

fn required_string(value: &Value, field: &'static str) -> Result<String, MetadataParseError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or(MetadataParseError::MissingField(field))
}

fn json_constraint(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(values) => {
            let values: Vec<String> = values
                .iter()
                .filter_map(|value| text(Some(value)))
                .collect();
            (!values.is_empty()).then(|| values.join(" "))
        }
        _ => text(Some(value)),
    }
}

fn sanitize_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut uncommented = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;

    while index < chars.len() {
        let character = chars[index];
        if in_string {
            match character {
                '"' => {
                    uncommented.push(character);
                    in_string = false;
                    index += 1;
                }
                '\\' if index + 1 < chars.len() => {
                    let next = chars[index + 1];
                    if next == '\n' || next == '\r' {
                        uncommented.push_str("\\n");
                        index += 2;
                        if next == '\r' && chars.get(index) == Some(&'\n') {
                            index += 1;
                        }
                    } else {
                        uncommented.push(character);
                        uncommented.push(next);
                        index += 2;
                    }
                }
                '\n' => {
                    uncommented.push_str("\\n");
                    index += 1;
                }
                '\r' => {
                    uncommented.push_str("\\r");
                    index += 1;
                }
                control if control.is_control() => {
                    uncommented.push_str(&format!("\\u{:04x}", control as u32));
                    index += 1;
                }
                _ => {
                    uncommented.push(character);
                    index += 1;
                }
            }
            continue;
        }

        if character == '"' {
            in_string = true;
            uncommented.push(character);
            index += 1;
        } else if character == '/' && chars.get(index + 1) == Some(&'/') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' && chars[index] != '\r' {
                index += 1;
            }
        } else if character == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                if chars[index] == '\n' || chars[index] == '\r' {
                    uncommented.push(chars[index]);
                }
                index += 1;
            }
            index = (index + 2).min(chars.len());
        } else {
            uncommented.push(character);
            index += 1;
        }
    }

    remove_trailing_commas(&uncommented)
}

fn remove_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;

    while index < chars.len() {
        let character = chars[index];
        if in_string {
            output.push(character);
            if character == '\\' && index + 1 < chars.len() {
                index += 1;
                output.push(chars[index]);
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            output.push(character);
        } else if character == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next >= chars.len() || (chars[next] != '}' && chars[next] != ']') {
                output.push(character);
            }
        } else {
            output.push(character);
        }
        index += 1;
    }
    output
}
