//! Surfaces declared as MCP resources: `uri` is the `surface://` uri, `name` the
//! label, `_meta["omp/surface"]` the pixel size and mode; documents set `mimeType`.

use loa_contracts::surface::{SurfaceInfo, SurfaceMode, SurfaceUri};
use serde::Deserialize;
use serde_json::{Value, json};

/// `_meta` key of the surface fields on a resource.
pub const META_KEY: &str = "omp/surface";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("resource is not an object with `uri` and `name`: {0}")]
    Resource(serde_json::Error),
    #[error("surface resource {uri} lacks `_meta.\"omp/surface\"` with width, height and mode")]
    Meta { uri: String },
    #[error("unknown surface mode `{0}`")]
    Mode(String),
    #[error(transparent)]
    Uri(#[from] loa_contracts::surface::SurfaceError),
}

/// The MCP resource that declares `info`.
pub fn resource(info: &SurfaceInfo) -> Value {
    let meta = json!({ "width": info.width, "height": info.height, "mode": info.mode.as_str() });
    let mut resource = json!({ "uri": info.uri.as_str(), "name": info.name, "_meta": { META_KEY: meta } });
    if let Some(mime) = &info.mime {
        resource["mimeType"] = json!(mime);
    }
    resource
}

#[derive(Deserialize)]
struct ResourceJson {
    uri: String,
    name: String,
    #[serde(rename = "mimeType")]
    mime: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize)]
struct SurfaceMeta {
    width: u32,
    height: u32,
    mode: String,
}

/// The surface a resource declares; `Ok(None)` for resources that are not `surface://` uris.
pub fn parse_resource(resource: &Value) -> Result<Option<SurfaceInfo>, McpError> {
    let r = ResourceJson::deserialize(resource).map_err(McpError::Resource)?;
    if !r.uri.starts_with(&format!("{}://", loa_contracts::surface::SCHEME)) {
        return Ok(None);
    }
    let uri: SurfaceUri = r.uri.parse()?;
    let meta = r.meta.as_ref().and_then(|m| m.get(META_KEY)).and_then(|m| SurfaceMeta::deserialize(m).ok());
    let meta = meta.ok_or_else(|| McpError::Meta { uri: r.uri.clone() })?;
    let mode = SurfaceMode::parse(&meta.mode).ok_or(McpError::Mode(meta.mode))?;
    Ok(Some(SurfaceInfo { uri, name: r.name, width: meta.width, height: meta.height, mode, mime: r.mime }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_resources_round_trip() {
        let info = SurfaceInfo {
            uri: "surface://docs/a.pdf".parse().unwrap(),
            name: "a.pdf".into(),
            width: 64,
            height: 48,
            mode: SurfaceMode::Document,
            mime: Some("application/pdf".into()),
        };
        assert_eq!(parse_resource(&resource(&info)).unwrap(), Some(info));
        assert_eq!(parse_resource(&json!({ "uri": "file:///notes.txt", "name": "n" })).unwrap(), None);
        assert!(matches!(parse_resource(&json!({ "uri": "surface://t/x", "name": "x" })), Err(McpError::Meta { .. })));
    }
}
