use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::ir::{
    DisplayTransform, ElementRotation, FaceAxis, FaceName, ResolvedElement, ResolvedFace,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RawModel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub textures: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<RawElement>>,
    #[serde(
        default,
        rename = "ambientocclusion",
        skip_serializing_if = "Option::is_none"
    )]
    pub ambient_occlusion: Option<bool>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub display: IndexMap<String, RawDisplayTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gui_light: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawElement {
    pub from: [f64; 3],
    pub to: [f64; 3],
    #[serde(default)]
    pub rotation: Option<RawElementRotation>,
    #[serde(default = "default_true")]
    pub shade: bool,
    pub faces: BTreeMap<FaceName, RawFace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawElementRotation {
    pub origin: [f64; 3],
    pub axis: FaceAxis,
    pub angle: f64,
    #[serde(default)]
    pub rescale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawFace {
    pub texture: String,
    #[serde(default)]
    pub uv: Option<[f64; 4]>,
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub cullface: Option<FaceName>,
    #[serde(default)]
    pub tintindex: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RawDisplayTransform {
    #[serde(default)]
    pub rotation: [f64; 3],
    #[serde(default)]
    pub translation: [f64; 3],
    #[serde(default = "default_scale")]
    pub scale: [f64; 3],
}

fn default_true() -> bool {
    true
}

fn default_scale() -> [f64; 3] {
    [1.0; 3]
}

impl From<RawElementRotation> for ElementRotation {
    fn from(rotation: RawElementRotation) -> Self {
        Self {
            origin: rotation.origin,
            axis: rotation.axis,
            angle: rotation.angle,
            rescale: rotation.rescale,
        }
    }
}

impl From<RawDisplayTransform> for DisplayTransform {
    fn from(transform: RawDisplayTransform) -> Self {
        Self {
            rotation: transform.rotation,
            translation: transform.translation,
            scale: transform.scale,
        }
    }
}

pub(crate) fn resolve_element<F, E>(
    element: RawElement,
    mut resolve_face: F,
) -> Result<ResolvedElement, E>
where
    F: FnMut(FaceName, RawFace, [f64; 3], [f64; 3]) -> Result<ResolvedFace, E>,
{
    let mut faces = BTreeMap::new();
    for (name, face) in element.faces {
        faces.insert(name, resolve_face(name, face, element.from, element.to)?);
    }
    Ok(ResolvedElement {
        from: element.from,
        to: element.to,
        rotation: element.rotation.map(Into::into),
        shade: element.shade,
        faces,
    })
}

pub(crate) fn default_uv(face: FaceName, from: [f64; 3], to: [f64; 3]) -> [f64; 4] {
    match face {
        FaceName::Down => [from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        FaceName::Up => [from[0], from[2], to[0], to[2]],
        FaceName::North => [16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        FaceName::South => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        FaceName::West => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        FaceName::East => [16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
    }
}

pub(crate) fn raw_face(texture: &str) -> RawFace {
    RawFace {
        texture: texture.to_owned(),
        uv: None,
        rotation: 0,
        cullface: None,
        tintindex: None,
    }
}
