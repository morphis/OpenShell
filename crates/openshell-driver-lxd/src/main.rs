// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

use openshell_core::VERSION;
use openshell_core::config::{DEFAULT_SSH_HANDSHAKE_SKEW_SECS, DEFAULT_SSH_PORT};
use openshell_core::proto::compute::v1::compute_driver_server::ComputeDriverServer;
use openshell_driver_lxd::config::{
    DEFAULT_BASE_IMAGE, DEFAULT_IMAGE_SERVER, DEFAULT_LXD_PROJECT, DEFAULT_LXD_SOCKET,
    DEFAULT_NETWORK_CIDR, DEFAULT_NETWORK_NAME, DEFAULT_OPERATION_TIMEOUT_SECS,
    DEFAULT_STORAGE_DRIVER, DEFAULT_STORAGE_POOL, DEFAULT_STORAGE_POOL_SIZE,
    DEFAULT_SUPERVISOR_PATH,
};
use openshell_driver_lxd::{ComputeDriverService, LxdComputeConfig, LxdComputeDriver};

#[derive(Parser)]
#[command(name = "openshell-driver-lxd")]
#[command(version = VERSION)]
struct Args {
    /// Address to bind the gRPC server on.
    #[arg(
        long,
        env = "OPENSHELL_COMPUTE_DRIVER_BIND",
        default_value = "127.0.0.1:50062"
    )]
    bind_address: SocketAddr,

    #[arg(long, env = "OPENSHELL_LOG_LEVEL", default_value = "info")]
    log_level: String,

    /// Path to the LXD Unix socket.
    #[arg(long, env = "OPENSHELL_LXD_SOCKET", default_value = DEFAULT_LXD_SOCKET)]
    lxd_socket: PathBuf,

    /// LXD project used to namespace all managed instances.
    #[arg(long, env = "OPENSHELL_LXD_PROJECT", default_value = DEFAULT_LXD_PROJECT)]
    lxd_project: String,

    /// Name of the ZFS storage pool.
    #[arg(long, env = "OPENSHELL_LXD_STORAGE_POOL", default_value = DEFAULT_STORAGE_POOL)]
    storage_pool: String,

    /// LXD storage driver ("zfs" or "dir").
    #[arg(long, env = "OPENSHELL_LXD_STORAGE_DRIVER", default_value = DEFAULT_STORAGE_DRIVER)]
    storage_driver: String,

    /// Size of the ZFS loopback pool (e.g. "50GiB"). Ignored for non-ZFS drivers.
    #[arg(long, env = "OPENSHELL_LXD_STORAGE_POOL_SIZE", default_value = DEFAULT_STORAGE_POOL_SIZE)]
    storage_pool_size: String,

    /// Name of the bridge network attached to sandbox VMs.
    #[arg(long, env = "OPENSHELL_LXD_NETWORK", default_value = DEFAULT_NETWORK_NAME)]
    network_name: String,

    /// Gateway IP and prefix for the bridge network (e.g. "10.100.200.1/24").
    #[arg(long, env = "OPENSHELL_LXD_NETWORK_CIDR", default_value = DEFAULT_NETWORK_CIDR)]
    network_cidr: String,

    /// LXD image alias for sandbox VMs.
    #[arg(long, env = "OPENSHELL_SANDBOX_IMAGE", default_value = DEFAULT_BASE_IMAGE)]
    base_image: String,

    /// Simplestreams image server URL.
    #[arg(long, env = "OPENSHELL_LXD_IMAGE_SERVER", default_value = DEFAULT_IMAGE_SERVER)]
    image_server: String,

    /// Host path of the openshell-sandbox supervisor binary.
    #[arg(long, env = "OPENSHELL_SUPERVISOR_PATH", default_value = DEFAULT_SUPERVISOR_PATH)]
    supervisor_path: PathBuf,

    /// Gateway gRPC endpoint the in-VM supervisor connects back to.
    #[arg(long, env = "OPENSHELL_GRPC_ENDPOINT")]
    grpc_endpoint: String,

    /// Shared secret for the SSH handshake.
    #[arg(long, env = "OPENSHELL_SSH_HANDSHAKE_SECRET")]
    ssh_handshake_secret: String,

    /// Maximum clock skew in seconds for SSH handshake validation.
    #[arg(
        long,
        env = "OPENSHELL_SSH_HANDSHAKE_SKEW_SECS",
        default_value_t = DEFAULT_SSH_HANDSHAKE_SKEW_SECS
    )]
    ssh_handshake_skew_secs: u64,

    /// SSH port the supervisor listens on inside each VM.
    #[arg(long, env = "OPENSHELL_SSH_PORT", default_value_t = DEFAULT_SSH_PORT)]
    ssh_port: u16,

    /// Timeout in seconds for LXD async operations (VM create, start, delete).
    #[arg(
        long,
        env = "OPENSHELL_LXD_OPERATION_TIMEOUT",
        default_value_t = DEFAULT_OPERATION_TIMEOUT_SECS
    )]
    operation_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    let config = LxdComputeConfig {
        socket_path: args.lxd_socket,
        lxd_project: args.lxd_project,
        storage_pool: args.storage_pool,
        storage_driver: args.storage_driver,
        storage_pool_size: args.storage_pool_size,
        network_name: args.network_name,
        network_cidr: args.network_cidr,
        base_image_alias: args.base_image,
        image_server: args.image_server,
        supervisor_path: args.supervisor_path,
        grpc_endpoint: args.grpc_endpoint,
        ssh_handshake_secret: args.ssh_handshake_secret,
        ssh_handshake_skew_secs: args.ssh_handshake_skew_secs,
        ssh_port: args.ssh_port,
        operation_timeout_secs: args.operation_timeout_secs,
    };

    let driver = LxdComputeDriver::new(config).await.into_diagnostic()?;

    info!(address = %args.bind_address, "Starting LXD compute driver");

    tonic::transport::Server::builder()
        .add_service(ComputeDriverServer::new(ComputeDriverService::new(driver)))
        .serve_with_shutdown(args.bind_address, async {
            tokio::signal::ctrl_c().await.ok();
            info!("Received shutdown signal, draining in-flight requests");
        })
        .await
        .into_diagnostic()
}
