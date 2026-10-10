use toml::Value;

use crate::{LoaderKind, MetadataParseError, make_mod_info};

pub(crate) fn parse_mods(
    bytes: &[u8],
    source: &str,
    loader: LoaderKind,
) -> Result<(Vec<crate::ModInfo>, Vec<String>), MetadataParseError> {
    let root = parse_toml(bytes)?;
    let mod_records = root
        .get("mods")
        .and_then(Value::as_array)
        .ok_or(MetadataParseError::MissingArray("mods"))?;
    if mod_records.is_empty() {
        return Ok((
            vec![make_mod_info(
                None,
                None,
                None,
                loader,
                None,
                Some(source.to_owned()),
            )],
            Vec::new(),
        ));
    }

    let mods = mod_records
        .iter()
        .map(|record| {
            let mod_id = value_text(record.get("modId").or_else(|| record.get("modid")));
            let minecraft_version = mod_id
                .as_deref()
                .and_then(|mod_id| minecraft_dependency(&root, mod_id));
            make_mod_info(
                mod_id,
                value_text(record.get("displayName").or_else(|| record.get("name"))),
                value_text(record.get("version")),
                loader,
                minecraft_version,
                Some(source.to_owned()),
            )
        })
        .collect();
    Ok((mods, Vec::new()))
}

pub(crate) fn find_icon_path(bytes: &[u8], mod_id: Option<&str>) -> Option<String> {
    let root = parse_toml(bytes).ok()?;
    let records = root.get("mods").and_then(Value::as_array);
    let selected = records.and_then(|records| {
        mod_id
            .and_then(|mod_id| {
                records
                    .iter()
                    .find(|record| mod_record_id(record) == Some(mod_id))
                    .and_then(|record| path_text(record.get("logoFile")))
            })
            .or_else(|| {
                records
                    .iter()
                    .find_map(|record| path_text(record.get("logoFile")))
            })
    });

    selected.or_else(|| path_text(root.get("logoFile")))
}

fn parse_toml(bytes: &[u8]) -> Result<Value, toml::de::Error> {
    let content = String::from_utf8_lossy(bytes);
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    toml::from_str(content)
}

fn mod_record_id(record: &Value) -> Option<&str> {
    record
        .get("modId")
        .or_else(|| record.get("modid"))
        .and_then(Value::as_str)
}

fn minecraft_dependency(root: &Value, mod_id: &str) -> Option<String> {
    root.get("dependencies")?
        .get(mod_id)?
        .as_array()?
        .iter()
        .find(|dependency| {
            dependency
                .get("modId")
                .or_else(|| dependency.get("modid"))
                .and_then(Value::as_str)
                == Some("minecraft")
        })
        .and_then(|dependency| {
            value_text(
                dependency
                    .get("versionRange")
                    .or_else(|| dependency.get("versionrange")),
            )
        })
}

fn value_text(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(text) => Some(text.clone()),
        Value::Integer(number) => Some(number.to_string()),
        Value::Float(number) => Some(number.to_string()),
        _ => None,
    })
}

fn path_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(str::to_owned)
}
