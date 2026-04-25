// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use openshell_core::config::{DEFAULT_SSH_HANDSHAKE_SKEW_SECS, DEFAULT_SSH_PORT};

pub const DEFAULT_LXD_SOCKET: &str = "/var/snap/lxd/common/lxd/unix.socket";
pub const DEFAULT_LXD_PROJECT: &str = "openshell";
pub const DEFAULT_STORAGE_POOL: &str = "openshell-pool";
pub const DEFAULT_STORAGE_DRIVER: &str = "zfs";
pub const DEFAULT_STORAGE_POOL_SIZE: &str = "50GiB";
pub const DEFAULT_NETWORK_NAME: &str = "openshell-br0";
pub const DEFAULT_NETWORK_CIDR: &str = "10.100.200.1/24";
pub const DEFAULT_BASE_IMAGE: &str = "24.04";
pub const DEFAULT_IMAGE_SERVER: &str = "https://cloud-images.ubuntu.com/releases";
pub const DEFAULT_SUPERVISOR_PATH: &str = "/opt/openshell/supervisor";
pub const DEFAULT_OPERATION_TIMEOUT_SECS: u64 = 600;

#[derive(Clone)]
pub struct LxdComputeConfig {
    /// Path to the LXD Unix socket.
    pub socket_path: PathBuf,
    /// LXD project used to namespace all managed instances.
    pub lxd_project: String,
    /// Name of the ZFS storage pool for instance root disks.
    pub storage_pool: String,
    /// LXD storage driver ("zfs" or "dir").
    pub storage_driver: String,
    /// Size of the loopback file when using the ZFS driver.
    pub storage_pool_size: String,
    /// Name of the bridge network attached to instances.
    pub network_name: String,
    /// Gateway address and prefix length for the bridge (e.g. "10.100.200.1/24").
    pub network_cidr: String,
    /// LXD image alias for new sandbox VMs.
    pub base_image_alias: String,
    /// Simplestreams image server URL.
    pub image_server: String,
    /// Host path of the openshell-sandbox supervisor binary to inject into VMs.
    pub supervisor_path: PathBuf,
    /// Gateway gRPC endpoint the in-VM supervisor connects back to.
    pub grpc_endpoint: String,
    /// Shared secret for the SSH handshake.
    pub ssh_handshake_secret: String,
    /// Maximum clock skew in seconds for SSH handshake validation.
    pub ssh_handshake_skew_secs: u64,
    /// SSH port the supervisor listens on inside the VM.
    pub ssh_port: u16,
    /// Timeout in seconds for LXD async operations (VM create, start, delete).
    pub operation_timeout_secs: u64,
}

impl Default for LxdComputeConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from(DEFAULT_LXD_SOCKET),
            lxd_project: DEFAULT_LXD_PROJECT.to_string(),
            storage_pool: DEFAULT_STORAGE_POOL.to_string(),
            storage_driver: DEFAULT_STORAGE_DRIVER.to_string(),
            storage_pool_size: DEFAULT_STORAGE_POOL_SIZE.to_string(),
            network_name: DEFAULT_NETWORK_NAME.to_string(),
            network_cidr: DEFAULT_NETWORK_CIDR.to_string(),
            base_image_alias: DEFAULT_BASE_IMAGE.to_string(),
            image_server: DEFAULT_IMAGE_SERVER.to_string(),
            supervisor_path: PathBuf::from(DEFAULT_SUPERVISOR_PATH),
            grpc_endpoint: String::new(),
            ssh_handshake_secret: String::new(),
            ssh_handshake_skew_secs: DEFAULT_SSH_HANDSHAKE_SKEW_SECS,
            ssh_port: DEFAULT_SSH_PORT,
            operation_timeout_secs: DEFAULT_OPERATION_TIMEOUT_SECS,
        }
    }
}

impl std::fmt::Debug for LxdComputeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LxdComputeConfig")
            .field("socket_path", &self.socket_path)
            .field("lxd_project", &self.lxd_project)
            .field("storage_pool", &self.storage_pool)
            .field("storage_driver", &self.storage_driver)
            .field("storage_pool_size", &self.storage_pool_size)
            .field("network_name", &self.network_name)
            .field("network_cidr", &self.network_cidr)
            .field("base_image_alias", &self.base_image_alias)
            .field("image_server", &self.image_server)
            .field("supervisor_path", &self.supervisor_path)
            .field("grpc_endpoint", &self.grpc_endpoint)
            .field("ssh_handshake_secret", &"[REDACTED]")
            .field("ssh_handshake_skew_secs", &self.ssh_handshake_skew_secs)
            .field("ssh_port", &self.ssh_port)
            .field("operation_timeout_secs", &self.operation_timeout_secs)
            .finish()
    }
}
