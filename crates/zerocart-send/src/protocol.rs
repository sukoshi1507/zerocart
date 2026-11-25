// SPDX-License-Identifier: GPL-3.0-or-later
//! The messages and constants of LocalSend protocol v2.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;

pub const PORT: u16 = 53317;
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const MULTICAST_PORT: u16 = 53317;
/// Prefix of every endpoint.
pub const API: &str = "/api/localsend/v2/";
pub const PROTOCOL_VERSION: &str = "2.1";

/// Who a device is, as announced by multicast, answered by `register` and sent in `prepare-upload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// "https" or "http".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<bool>,
    /// Multicast only: true for an announcement (the others should answer), false for an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<bool>,
    /// The older name of `announce`, still sent by some apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement: Option<bool>,
}

impl DeviceInfo {
    /// The description of this device (`http_fields`: with the port and protocol, as in multicast and `register`).
    pub fn this_device(alias: &str, fingerprint: &str, http_fields: bool) -> Self {
        Self {
            alias: alias.to_string(),
            version: Some(PROTOCOL_VERSION.to_string()),
            device_model: Some("CardputerZero".to_string()),
            device_type: Some("mobile".to_string()),
            fingerprint: fingerprint.to_string(),
            port: http_fields.then_some(PORT),
            protocol: http_fields.then(|| "https".to_string()),
            download: Some(false),
            announce: None,
            announcement: None,
        }
    }

    /// An announcement asks the others to answer; an answer does not.
    pub fn is_announcement(&self) -> bool {
        self.announce.or(self.announcement).unwrap_or(false)
    }

    pub fn uses_https(&self) -> bool {
        self.protocol.as_deref() != Some("http")
    }
}

/// One file offered in `prepare-upload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub id: String,
    pub file_name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub file_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// For a text message: the text itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareUploadRequest {
    pub info: DeviceInfo,
    pub files: BTreeMap<String, FileMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    /// File id to upload token.
    pub files: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_device_has_the_fields_localsend_expects() {
        let d = DeviceInfo::this_device("ZeroCart", "ABCD", true);
        let v: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert_eq!(v["alias"], "ZeroCart");
        assert_eq!(v["version"], "2.1");
        assert_eq!(v["deviceModel"], "CardputerZero");
        assert_eq!(v["deviceType"], "mobile");
        assert_eq!(v["fingerprint"], "ABCD");
        assert_eq!((v["port"].as_u64(), v["protocol"].as_str(), v["download"].as_bool()), (Some(53317), Some("https"), Some(false)));
        assert!(v.get("announce").is_none(), "absent unless set");
        let bare = serde_json::to_value(DeviceInfo::this_device("ZeroCart", "ABCD", false)).unwrap();
        assert!(bare.get("port").is_none() && bare.get("protocol").is_none(), "the info endpoint has no port or protocol");
    }

    #[test]
    fn a_device_from_another_app_parses_with_missing_and_extra_fields() {
        let d: DeviceInfo = serde_json::from_str(
            r#"{"alias":"Pixel","version":"2.0","deviceModel":"Pixel 8","deviceType":"mobile","fingerprint":"F1","port":53317,"protocol":"https","download":true,"announce":true,"somethingNew":1}"#,
        )
        .unwrap();
        assert_eq!((d.alias.as_str(), d.port, d.announce, d.uses_https()), ("Pixel", Some(53317), Some(true), true));
        let minimal: DeviceInfo = serde_json::from_str(r#"{"alias":"X","fingerprint":"F2"}"#).unwrap();
        assert!(minimal.uses_https() && minimal.port.is_none());
        let plain: DeviceInfo = serde_json::from_str(r#"{"alias":"X","fingerprint":"F2","protocol":"http"}"#).unwrap();
        assert!(!plain.uses_https());
    }

    #[test]
    fn prepare_upload_messages_round_trip_in_localsends_field_names() {
        let body = r#"{"info":{"alias":"A","fingerprint":"F"},"files":{"f1":{"id":"f1","fileName":"a.bin","size":3,"fileType":"application/octet-stream","sha256":null,"preview":null,"metadata":{"modified":"2026-01-01T00:00:00Z"}}}}"#;
        let r: PrepareUploadRequest = serde_json::from_str(body).unwrap();
        assert_eq!(r.files["f1"].file_name, "a.bin");
        assert_eq!(r.files["f1"].size, 3);
        let ans = PrepareUploadResponse { session_id: "s".into(), files: [("f1".to_string(), "tok".to_string())].into() };
        assert_eq!(serde_json::to_string(&ans).unwrap(), r#"{"sessionId":"s","files":{"f1":"tok"}}"#);
    }
}
