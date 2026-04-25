// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

//! High-level LXD API endpoint wrappers used by the driver.

use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info};

use crate::lxd::client::{LxdApiError, LxdClient};
use crate::lxd::models::{
    InstanceExecPost, InstanceState, InstanceStatePut, InstancesPost, NetworksPost, Operation,
    ProjectsPost, StoragePoolsPost,
};
use crate::lxd::operations::wait_for_operation;

const API: &str = "/1.0";

/// Ensure the named LXD project exists. Idempotent (409 is swallowed).
pub async fn ensure_project(client: &LxdClient, name: &str) -> Result<(), LxdApiError> {
    let body = ProjectsPost {
        name: name.to_string(),
        config: {
            let mut m = HashMap::new();
            // Inherit global images and profiles so the project can use
            // the simplestreams image cache without duplicating it.
            m.insert("features.images".to_string(), "false".to_string());
            m.insert("features.profiles".to_string(), "false".to_string());
            m
        },
    };
    match client.post(&format!("{API}/projects"), &body).await {
        Ok(_) => {
            info!(project = %name, "Created LXD project");
            Ok(())
        }
        Err(LxdApiError::Conflict(_)) => {
            debug!(project = %name, "LXD project already exists");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Ensure the named ZFS storage pool exists with a loopback backing file.
/// Idempotent (409 is swallowed). `size` is a LXD quantity string, e.g. "50GiB".
pub async fn ensure_storage_pool(
    client: &LxdClient,
    name: &str,
    driver: &str,
    size: &str,
) -> Result<(), LxdApiError> {
    let mut config = HashMap::new();
    if driver == "zfs" && !size.is_empty() {
        // Empty source causes LXD to create a loopback file automatically.
        config.insert("size".to_string(), size.to_string());
    }
    let body = StoragePoolsPost {
        name: name.to_string(),
        driver: driver.to_string(),
        config,
    };
    match client.post(&format!("{API}/storage-pools"), &body).await {
        Ok(_) => {
            info!(pool = %name, driver = %driver, size = %size, "Created LXD storage pool");
            Ok(())
        }
        Err(LxdApiError::Conflict(_)) => {
            debug!(pool = %name, "LXD storage pool already exists");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Ensure a bridge network exists with NAT and a static CIDR.
/// Idempotent (409 Conflict and LXD's non-standard 400 "already exists" are both swallowed).
/// `cidr` is the gateway address, e.g. "10.100.200.1/24".
pub async fn ensure_network(client: &LxdClient, name: &str, cidr: &str) -> Result<(), LxdApiError> {
    let mut config = HashMap::new();
    config.insert("ipv4.address".to_string(), cidr.to_string());
    config.insert("ipv4.nat".to_string(), "true".to_string());
    config.insert("ipv6.address".to_string(), "none".to_string());
    let body = NetworksPost {
        name: name.to_string(),
        network_type: "bridge".to_string(),
        config,
    };
    match client.post(&format!("{API}/networks"), &body).await {
        Ok(_) => {
            info!(network = %name, cidr = %cidr, "Created LXD bridge network");
            Ok(())
        }
        Err(LxdApiError::Conflict(_)) => {
            debug!(network = %name, "LXD network already exists");
            Ok(())
        }
        // LXD 5.x returns error_code 400 (not 409) for duplicate network creation.
        Err(LxdApiError::Api { message, .. }) if message.contains("already exists") => {
            debug!(network = %name, "LXD network already exists");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Create an instance and return the operation URL for the caller to wait on.
pub async fn create_instance(
    client: &LxdClient,
    req: &InstancesPost,
    project: &str,
) -> Result<String, LxdApiError> {
    let env = client
        .post(&format!("{API}/instances?project={project}"), req)
        .await?;
    Ok(env.operation)
}

/// Start an instance and return the operation URL.
pub async fn start_instance(
    client: &LxdClient,
    name: &str,
    project: &str,
) -> Result<String, LxdApiError> {
    let body = InstanceStatePut {
        action: "start".to_string(),
        timeout: 60,
        force: false,
    };
    let env = client
        .put(
            &format!("{API}/instances/{name}/state?project={project}"),
            &body,
        )
        .await?;
    Ok(env.operation)
}

/// Stop an instance and return the operation URL.
pub async fn stop_instance(
    client: &LxdClient,
    name: &str,
    project: &str,
    timeout_secs: u32,
) -> Result<String, LxdApiError> {
    let body = InstanceStatePut {
        action: "stop".to_string(),
        timeout: timeout_secs,
        force: false,
    };
    let env = client
        .put(
            &format!("{API}/instances/{name}/state?project={project}"),
            &body,
        )
        .await?;
    Ok(env.operation)
}

/// Delete an instance and return the operation URL.
pub async fn delete_instance(
    client: &LxdClient,
    name: &str,
    project: &str,
) -> Result<String, LxdApiError> {
    let env = client
        .delete(&format!("{API}/instances/{name}?project={project}"))
        .await?;
    Ok(env.operation)
}

/// Get the current state of an instance.
pub async fn get_instance_state(
    client: &LxdClient,
    name: &str,
    project: &str,
) -> Result<InstanceState, LxdApiError> {
    client
        .get::<InstanceState>(&format!("{API}/instances/{name}/state?project={project}"))
        .await
}

/// List all instance names in the given project.
///
/// Returns the short names (last path segment of each URL in the response list).
#[allow(dead_code)]
pub async fn list_instances(client: &LxdClient, project: &str) -> Result<Vec<String>, LxdApiError> {
    let urls: Vec<String> = client
        .get::<Vec<String>>(&format!("{API}/instances?project={project}"))
        .await?;
    Ok(urls
        .into_iter()
        .filter_map(|url| url.split('/').next_back().map(str::to_string))
        .collect())
}

/// Execute a command inside an instance (fire-and-forget for daemons).
///
/// Uses `wait-for-websocket: false` so the command starts immediately without
/// needing WebSocket connections for stdin/stdout/stderr. Returns the operation
/// URL; callers that need the exit code can wait on it.
pub async fn exec_in_instance(
    client: &LxdClient,
    name: &str,
    command: Vec<String>,
    env: HashMap<String, String>,
    project: &str,
) -> Result<String, LxdApiError> {
    let body = InstanceExecPost {
        command,
        environment: env,
        interactive: false,
        wait_for_websocket: false,
        record_output: false,
    };
    let resp = client
        .post(
            &format!("{API}/instances/{name}/exec?project={project}"),
            &body,
        )
        .await?;
    Ok(resp.operation)
}

/// Create an instance and wait for the creation operation to complete.
pub async fn create_instance_and_wait(
    client: &LxdClient,
    req: &InstancesPost,
    project: &str,
    op_timeout: Duration,
) -> Result<Operation, LxdApiError> {
    let op_url = create_instance(client, req, project).await?;
    wait_for_operation(client, &op_url, op_timeout).await
}

/// Start an instance and wait for it to reach Running state.
pub async fn start_instance_and_wait(
    client: &LxdClient,
    name: &str,
    project: &str,
    op_timeout: Duration,
) -> Result<Operation, LxdApiError> {
    let op_url = start_instance(client, name, project).await?;
    wait_for_operation(client, &op_url, op_timeout).await
}

/// Stop an instance and wait for the operation to complete.
pub async fn stop_instance_and_wait(
    client: &LxdClient,
    name: &str,
    project: &str,
    timeout_secs: u32,
    op_timeout: Duration,
) -> Result<Operation, LxdApiError> {
    let op_url = stop_instance(client, name, project, timeout_secs).await?;
    wait_for_operation(client, &op_url, op_timeout).await
}

/// Delete an instance and wait for the operation to complete.
pub async fn delete_instance_and_wait(
    client: &LxdClient,
    name: &str,
    project: &str,
    op_timeout: Duration,
) -> Result<Operation, LxdApiError> {
    let op_url = delete_instance(client, name, project).await?;
    wait_for_operation(client, &op_url, op_timeout).await
}

/// Poll instance state until the VM has a global IPv4 address or the timeout expires.
///
/// LXD 5.x does not expose an `agent` field in instance state; presence of a
/// routable IPv4 address is the reliable signal that the VM is up and reachable.
pub async fn wait_for_agent(
    client: &LxdClient,
    name: &str,
    project: &str,
    timeout: Duration,
) -> Result<InstanceState, LxdApiError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut interval = tokio::time::interval(Duration::from_secs(3));

    loop {
        interval.tick().await;

        if tokio::time::Instant::now() > deadline {
            return Err(LxdApiError::Timeout(timeout));
        }

        match get_instance_state(client, name, project).await {
            Ok(state) if extract_ipv4(&state).is_some() => {
                debug!(instance = %name, "VM has IPv4 address, ready");
                return Ok(state);
            }
            Ok(state) => {
                debug!(instance = %name, status = %state.status, "Waiting for VM network");
            }
            Err(e) => {
                debug!(instance = %name, error = %e, "Error polling instance state, retrying");
            }
        }
    }
}

/// Wait for cloud-init to finish inside the VM.
///
/// First waits for the LXD agent (IPv4 address), then polls for the
/// cloud-init `boot-finished` marker file via the LXD files API. Returns
/// the instance state so callers can extract the VM IP.
pub async fn wait_for_cloud_init(
    client: &LxdClient,
    name: &str,
    project: &str,
    timeout: Duration,
) -> Result<InstanceState, LxdApiError> {
    // First wait for the agent and network.
    let state = wait_for_agent(client, name, project, timeout).await?;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut interval = tokio::time::interval(Duration::from_secs(5));

    loop {
        interval.tick().await;

        if tokio::time::Instant::now() > deadline {
            return Err(LxdApiError::Timeout(timeout));
        }

        match client
            .file_exists(name, "/var/lib/cloud/instance/boot-finished", project)
            .await
        {
            Ok(true) => {
                info!(instance = %name, "cloud-init finished, VM fully booted");
                return Ok(state);
            }
            Ok(false) => {
                debug!(instance = %name, "cloud-init still running");
            }
            Err(e) => {
                debug!(instance = %name, error = %e, "cloud-init poll failed, retrying");
            }
        }
    }
}

/// Extract the first global IPv4 address from a running instance state.
pub fn extract_ipv4(state: &InstanceState) -> Option<String> {
    state.network.as_ref().and_then(|net| {
        net.values().find_map(|iface| {
            iface.addresses.iter().find_map(|addr| {
                if addr.family == "inet" && addr.scope == "global" {
                    Some(addr.address.clone())
                } else {
                    None
                }
            })
        })
    })
}
