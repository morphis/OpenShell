// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Typed structs for the LXD 1.0 REST API.

use std::collections::HashMap;

/// Standard LXD response envelope wrapping all API responses.
///
/// The `metadata` field is left as a raw `Value` to avoid serde generic-bound
/// issues; callers use `parse_metadata::<T>()` to get typed results.
#[derive(Debug, serde::Deserialize)]
pub struct LxdResponse {
    /// "sync", "async", or "error".
    #[serde(rename = "type")]
    pub response_type: String,
    /// HTTP-style status code (200 = success, 202 = async, 400 = failure, 404 = not found).
    #[allow(dead_code)]
    pub status_code: u16,
    /// For async responses, the URL of the created operation.
    #[serde(default)]
    pub operation: String,
    /// Response payload for sync responses; null for async/error responses.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Error message when type == "error".
    #[serde(default)]
    pub error: String,
    /// Numeric error code when type == "error".
    #[serde(default)]
    pub error_code: u16,
}

impl LxdResponse {
    /// Deserialize the `metadata` field into a typed value.
    pub fn parse_metadata<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<Option<T>, serde_json::Error> {
        self.metadata
            .as_ref()
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
    }
}

// ── Projects ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct ProjectsPost {
    pub name: String,
    #[serde(default)]
    pub config: HashMap<String, String>,
}

// ── Storage pools ─────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct StoragePoolsPost {
    pub name: String,
    pub driver: String,
    #[serde(default)]
    pub config: HashMap<String, String>,
}

#[derive(Debug, serde::Serialize)]
pub struct NetworksPost {
    pub name: String,
    #[serde(rename = "type", default)]
    pub network_type: String,
    #[serde(default)]
    pub config: HashMap<String, String>,
}

#[derive(Debug, serde::Serialize)]
pub struct InstancesPost {
    pub name: String,
    #[serde(rename = "type")]
    pub instance_type: String,
    pub source: InstanceSource,
    #[serde(default)]
    pub config: HashMap<String, String>,
    #[serde(default)]
    pub devices: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct InstanceSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub alias: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// PUT /1.0/instances/{name}/state — change instance power state.
#[derive(Debug, serde::Serialize)]
pub struct InstanceStatePut {
    pub action: String,
    pub timeout: u32,
    pub force: bool,
}

/// GET /1.0/instances/{name}/state — current instance state.
#[derive(Debug, serde::Deserialize)]
pub struct InstanceState {
    /// "Running", "Stopped", "Starting", "Stopping", etc.
    pub status: String,
    /// Network interface state. Populated when the instance is running.
    #[serde(default)]
    pub network: Option<HashMap<String, InterfaceState>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct InterfaceState {
    #[serde(default)]
    pub addresses: Vec<InterfaceAddress>,
    #[serde(default)]
    #[allow(dead_code)]
    pub state: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct InterfaceAddress {
    pub family: String,
    pub address: String,
    #[serde(default)]
    pub scope: String,
}

/// POST /1.0/instances/{name}/exec — run a command inside the instance.
#[derive(Debug, serde::Serialize)]
pub struct InstanceExecPost {
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub interactive: bool,
    #[serde(rename = "wait-for-websocket")]
    pub wait_for_websocket: bool,
    #[serde(rename = "record-output")]
    pub record_output: bool,
}

/// LXD async operation as returned by /1.0/operations/{id}/wait.
#[derive(Debug, serde::Deserialize)]
pub struct Operation {
    #[allow(dead_code)]
    pub id: String,
    /// "Running", "Success", "Failure", "Cancelled".
    pub status: String,
    /// Numeric status code: 103=Running, 200=Success, 400=Failure.
    pub status_code: u16,
    /// Error message when status is Failure.
    #[serde(default)]
    pub err: String,
}

impl Operation {
    #[allow(dead_code)]
    pub fn is_success(&self) -> bool {
        self.status == "Success" || self.status_code == 200
    }

    pub fn is_failure(&self) -> bool {
        self.status == "Failure" || self.status_code == 400
    }
}
