// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

//! LXD async operation wait helper.

use std::time::Duration;
use tracing::debug;

use crate::lxd::client::{LxdApiError, LxdClient};
use crate::lxd::models::Operation;

/// Wait for an LXD async operation to reach a terminal state.
///
/// Uses LXD's dedicated `/wait` endpoint (`GET /1.0/operations/{id}/wait?timeout=N`),
/// which blocks server-side until the operation completes or the timeout expires.
///
/// Returns the completed operation on success, or an error if the operation
/// fails or the wait itself times out.
pub async fn wait_for_operation(
    client: &LxdClient,
    operation_url: &str,
    timeout: Duration,
) -> Result<Operation, LxdApiError> {
    let wait_path = format!("{operation_url}/wait?timeout={}", timeout.as_secs());
    debug!(operation = %operation_url, timeout_secs = %timeout.as_secs(), "Waiting for LXD operation");

    // The HTTP connection must stay open at least as long as LXD holds it.
    // Add a 10-second buffer over the LXD-side wait timeout.
    let http_timeout = timeout + Duration::from_secs(10);
    let env = client
        .request_envelope(hyper::Method::GET, &wait_path, None, Some(http_timeout))
        .await?;

    if env.response_type == "error" {
        return Err(LxdApiError::Api {
            status: env.error_code,
            message: env.error,
        });
    }

    let op = env
        .parse_metadata::<Operation>()
        .map_err(|e| LxdApiError::Json(e.to_string()))?
        .ok_or_else(|| {
            LxdApiError::Json("missing operation metadata in wait response".to_string())
        })?;

    if op.is_failure() {
        return Err(LxdApiError::OperationFailed(if op.err.is_empty() {
            format!("operation {operation_url} failed with status {}", op.status)
        } else {
            op.err
        }));
    }

    Ok(op)
}
