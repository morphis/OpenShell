// SPDX-FileCopyrightText: Copyright (c) 2026 Canonical Ltd.
// SPDX-License-Identifier: Apache-2.0

//! LXD compute driver — core lifecycle implementation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;

use openshell_core::ComputeDriverError;
use openshell_core::VERSION;
use openshell_core::proto::compute::v1::{
    DriverCondition, DriverSandbox, DriverSandboxStatus, GetCapabilitiesResponse,
    WatchSandboxesDeletedEvent, WatchSandboxesEvent, WatchSandboxesSandboxEvent,
    watch_sandboxes_event,
};
use tracing::{debug, info, warn};

use crate::config::LxdComputeConfig;
use crate::lxd::api;
use crate::lxd::client::{LxdApiError, LxdClient, sanitize_instance_name};
use crate::lxd::models::InstancesPost;

pub type WatchStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<WatchSandboxesEvent, ComputeDriverError>> + Send>,
>;

impl From<LxdApiError> for ComputeDriverError {
    fn from(e: LxdApiError) -> Self {
        match e {
            LxdApiError::Conflict(_) => Self::AlreadyExists,
            LxdApiError::NotFound(msg) => Self::Message(format!("not found: {msg}")),
            other => Self::Message(other.to_string()),
        }
    }
}

/// In-memory record of a managed sandbox instance.
#[derive(Debug, Clone)]
struct SandboxRecord {
    /// Last known snapshot of this sandbox.
    sandbox: DriverSandbox,
    /// LXD instance name (derived from sandbox.name).
    vm_name: String,
}

/// LXD compute driver managing sandbox VMs via the LXD REST API.
#[derive(Clone)]
pub struct LxdComputeDriver {
    client: Arc<LxdClient>,
    config: Arc<LxdComputeConfig>,
    /// Registry keyed by `DriverSandbox.name` (the gateway-assigned name).
    sandboxes: Arc<RwLock<HashMap<String, SandboxRecord>>>,
}

impl std::fmt::Debug for LxdComputeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LxdComputeDriver")
            .field("socket_path", &self.config.socket_path)
            .field("lxd_project", &self.config.lxd_project)
            .field("network_name", &self.config.network_name)
            .finish()
    }
}

impl LxdComputeDriver {
    /// Create a new driver, verify LXD connectivity, and bootstrap infrastructure.
    pub async fn new(config: LxdComputeConfig) -> Result<Self, LxdApiError> {
        if !config.socket_path.exists() {
            warn!(
                path = %config.socket_path.display(),
                "LXD socket not found — is LXD running? \
                 Set OPENSHELL_LXD_SOCKET to override."
            );
        }

        let client = Arc::new(LxdClient::new(config.socket_path.clone()));

        client.ping().await?;
        info!("Connected to LXD");

        // Ensure required infrastructure exists.
        api::ensure_project(&client, &config.lxd_project).await?;
        api::ensure_storage_pool(
            &client,
            &config.storage_pool,
            &config.storage_driver,
            &config.storage_pool_size,
        )
        .await?;
        api::ensure_network(&client, &config.network_name, &config.network_cidr).await?;

        info!(
            project = %config.lxd_project,
            storage_pool = %config.storage_pool,
            network = %config.network_name,
            "LXD infrastructure ready"
        );

        Ok(Self {
            client,
            config: Arc::new(config),
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    #[allow(clippy::unused_async)]
    pub async fn capabilities(&self) -> Result<GetCapabilitiesResponse, ComputeDriverError> {
        Ok(GetCapabilitiesResponse {
            driver_name: "lxd".to_string(),
            driver_version: VERSION.to_string(),
            default_image: self.config.base_image_alias.clone(),
            supports_gpu: false,
        })
    }

    #[allow(clippy::unused_async)]
    pub async fn validate_sandbox_create(
        &self,
        sandbox: &DriverSandbox,
    ) -> Result<(), ComputeDriverError> {
        if sandbox.name.is_empty() {
            return Err(ComputeDriverError::Precondition(
                "sandbox name must not be empty".to_string(),
            ));
        }
        // Validate that the name can be sanitized to a valid LXD instance name.
        sanitize_instance_name(&sandbox.name)
            .map_err(|e| ComputeDriverError::Precondition(e.to_string()))?;

        let spec = sandbox.spec.as_ref().ok_or_else(|| {
            ComputeDriverError::Precondition("sandbox spec is required".to_string())
        })?;
        if spec.gpu {
            return Err(ComputeDriverError::Precondition(
                "LXD driver does not support GPU sandboxes".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn get_sandbox(
        &self,
        sandbox_name: &str,
    ) -> Result<Option<DriverSandbox>, ComputeDriverError> {
        let registry = self.sandboxes.read().await;
        Ok(registry.get(sandbox_name).map(|r| r.sandbox.clone()))
    }

    pub async fn list_sandboxes(&self) -> Result<Vec<DriverSandbox>, ComputeDriverError> {
        let registry = self.sandboxes.read().await;
        Ok(registry.values().map(|r| r.sandbox.clone()).collect())
    }

    pub async fn create_sandbox(&self, sandbox: &DriverSandbox) -> Result<(), ComputeDriverError> {
        self.validate_sandbox_create(sandbox).await?;

        let sandbox_id = sandbox.id.clone();
        let sandbox_name = sandbox.name.clone();
        let vm_name = sanitize_instance_name(&sandbox_name)
            .map_err(|e| ComputeDriverError::Precondition(e.to_string()))?;

        // Reject if already tracked.
        {
            let registry = self.sandboxes.read().await;
            if registry.contains_key(&sandbox_name) {
                return Err(ComputeDriverError::AlreadyExists);
            }
        }

        info!(
            sandbox_id = %sandbox_id,
            sandbox_name = %sandbox_name,
            vm_name = %vm_name,
            "Creating LXD VM sandbox"
        );

        let image = sandbox
            .spec
            .as_ref()
            .and_then(|s| s.template.as_ref())
            .map(|t| t.image.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.config.base_image_alias);

        let op_timeout = Duration::from_secs(self.config.operation_timeout_secs);

        // Build the supervisor env early so we can embed it in cloud-init.
        let supervisor_env = self.build_supervisor_env(&sandbox_id, &sandbox_name);

        // Build instance creation request — cloud-init will install the
        // systemd units, env file, create the sandbox user, and enable the
        // path watcher that auto-starts the supervisor when the binary lands.
        let instance_req = self.build_instance_request(
            &vm_name,
            image,
            &sandbox_id,
            &sandbox_name,
            &supervisor_env,
        );

        // 1. Create the VM.
        api::create_instance_and_wait(
            &self.client,
            &instance_req,
            &self.config.lxd_project,
            op_timeout,
        )
        .await
        .map_err(ComputeDriverError::from)?;
        debug!(vm_name = %vm_name, "VM created");

        // 2. Start the VM.
        api::start_instance_and_wait(&self.client, &vm_name, &self.config.lxd_project, op_timeout)
            .await
            .map_err(ComputeDriverError::from)?;
        debug!(vm_name = %vm_name, "VM started");

        // 3. Wait for cloud-init to finish inside the VM. This ensures the
        //    system is fully booted, the `sandbox` user exists (created by
        //    the cloud-init users directive), and the filesystem is ready.
        let state = api::wait_for_cloud_init(
            &self.client,
            &vm_name,
            &self.config.lxd_project,
            op_timeout,
        )
        .await
        .map_err(ComputeDriverError::from)?;
        debug!(vm_name = %vm_name, "VM fully booted (cloud-init done)");

        // 4. Push the supervisor binary into the VM. Cloud-init already
        //    installed the systemd path unit that watches for this file —
        //    once the binary lands, systemd starts the supervisor service.
        self.push_supervisor(&vm_name).await?;
        debug!(vm_name = %vm_name, "Supervisor binary pushed (systemd path unit will start it)");

        // 6. Extract VM IP for the agent_fd.
        let vm_ip = api::extract_ipv4(&state);
        let agent_fd = vm_ip
            .map(|ip| format!("{ip}:{}", self.config.ssh_port))
            .unwrap_or_default();

        // 7. Build the sandbox snapshot and add to registry.
        let driver_sandbox = DriverSandbox {
            id: sandbox_id.clone(),
            name: sandbox_name.clone(),
            namespace: self.config.lxd_project.clone(),
            spec: sandbox.spec.clone(),
            status: Some(DriverSandboxStatus {
                sandbox_name: sandbox_name.clone(),
                instance_id: vm_name.clone(),
                agent_fd,
                sandbox_fd: String::new(),
                conditions: vec![provisioning_condition()],
                deleting: false,
            }),
        };

        {
            let mut registry = self.sandboxes.write().await;
            registry.insert(
                sandbox_name.clone(),
                SandboxRecord {
                    sandbox: driver_sandbox,
                    vm_name,
                },
            );
        }

        info!(sandbox_id = %sandbox_id, sandbox_name = %sandbox_name, "Sandbox created");
        Ok(())
    }

    pub async fn stop_sandbox(&self, sandbox_name: &str) -> Result<(), ComputeDriverError> {
        let vm_name = {
            let registry = self.sandboxes.read().await;
            registry
                .get(sandbox_name)
                .map(|r| r.vm_name.clone())
                .ok_or_else(|| {
                    ComputeDriverError::Message(format!("sandbox not found: {sandbox_name}"))
                })?
        };

        let op_timeout = Duration::from_secs(self.config.operation_timeout_secs);
        api::stop_instance_and_wait(
            &self.client,
            &vm_name,
            &self.config.lxd_project,
            30,
            op_timeout,
        )
        .await
        .map_err(ComputeDriverError::from)?;

        // Update status in registry.
        let mut registry = self.sandboxes.write().await;
        if let Some(record) = registry.get_mut(sandbox_name)
            && let Some(status) = record.sandbox.status.as_mut()
        {
            status.conditions = vec![stopped_condition()];
        }

        info!(sandbox_name = %sandbox_name, vm_name = %vm_name, "Sandbox stopped");
        Ok(())
    }

    pub async fn delete_sandbox(
        &self,
        sandbox_id: &str,
        sandbox_name: &str,
    ) -> Result<bool, ComputeDriverError> {
        let vm_name = {
            let registry = self.sandboxes.read().await;
            registry.get(sandbox_name).map(|r| r.vm_name.clone())
        };

        let Some(vm_name) = vm_name else {
            debug!(sandbox_id = %sandbox_id, sandbox_name = %sandbox_name, "Sandbox not found for delete");
            return Ok(false);
        };

        let op_timeout = Duration::from_secs(self.config.operation_timeout_secs);

        // Mark as deleting in the registry so the watch poller emits a delete
        // event once the LXD instance disappears, rather than silently losing
        // track of the sandbox.
        {
            let mut registry = self.sandboxes.write().await;
            if let Some(record) = registry.get_mut(sandbox_name)
                && let Some(status) = record.sandbox.status.as_mut()
            {
                status.deleting = true;
            }
        }

        // Force-stop first (ignore error if already stopped).
        if let Err(e) = api::stop_instance_and_wait(
            &self.client,
            &vm_name,
            &self.config.lxd_project,
            10,
            op_timeout,
        )
        .await
        {
            debug!(vm_name = %vm_name, error = %e, "Stop before delete failed (may already be stopped)");
        }

        // Delete the instance.
        api::delete_instance_and_wait(&self.client, &vm_name, &self.config.lxd_project, op_timeout)
            .await
            .map_err(ComputeDriverError::from)?;

        // Do not remove from registry here. The watch poller will detect the
        // LXD instance is gone (NotFound), emit a WatchSandboxesDeletedEvent,
        // and remove the entry. This ensures the gateway receives the delete
        // event promptly instead of waiting for the reconciler.

        info!(sandbox_id = %sandbox_id, sandbox_name = %sandbox_name, vm_name = %vm_name, "Sandbox deleted");
        Ok(true)
    }

    /// Return a stream of sandbox observations.
    ///
    /// Emits initial snapshots for all known sandboxes, then polls LXD every
    /// 5 seconds to detect state changes and deletions. The stream terminates
    /// when the receiver is dropped.
    #[allow(clippy::unused_async)]
    pub async fn watch_sandboxes(&self) -> Result<WatchStream, ComputeDriverError> {
        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<WatchSandboxesEvent, ComputeDriverError>>(256);
        let client = Arc::clone(&self.client);
        let config = Arc::clone(&self.config);
        let sandboxes = Arc::clone(&self.sandboxes);

        tokio::spawn(async move {
            // Emit initial snapshots from registry.
            {
                let registry = sandboxes.read().await;
                for record in registry.values() {
                    if tx
                        .send(Ok(sandbox_event(record.sandbox.clone())))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            // Poll for state changes.
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            // Track last known conditions per vm_name to detect changes.
            let mut last_status: HashMap<String, String> = HashMap::new();

            loop {
                interval.tick().await;
                if tx.is_closed() {
                    break;
                }

                // Snapshot current registry.
                let snapshot: Vec<SandboxRecord> = {
                    let registry = sandboxes.read().await;
                    registry.values().cloned().collect()
                };

                for record in &snapshot {
                    let is_deleting = record
                        .sandbox
                        .status
                        .as_ref()
                        .is_some_and(|s| s.deleting);

                    match api::get_instance_state(&client, &record.vm_name, &config.lxd_project)
                        .await
                    {
                        Ok(state) => {
                            // Skip status updates for entries being deleted; the
                            // next poll will see NotFound and emit a delete event.
                            if is_deleting {
                                continue;
                            }

                            let new_status = state.status.clone();
                            let prev = last_status
                                .get(&record.vm_name)
                                .cloned()
                                .unwrap_or_default();

                            if new_status != prev {
                                last_status.insert(record.vm_name.clone(), new_status.clone());

                                let vm_ip = api::extract_ipv4(&state);
                                let agent_fd = vm_ip
                                    .map(|ip| format!("{ip}:{}", config.ssh_port))
                                    .unwrap_or_default();

                                let conditions = if new_status == "Running" {
                                    vec![provisioning_condition()]
                                } else {
                                    vec![stopped_condition_with_status(&new_status)]
                                };

                                let updated = DriverSandbox {
                                    id: record.sandbox.id.clone(),
                                    name: record.sandbox.name.clone(),
                                    namespace: record.sandbox.namespace.clone(),
                                    spec: record.sandbox.spec.clone(),
                                    status: Some(DriverSandboxStatus {
                                        sandbox_name: record.sandbox.name.clone(),
                                        instance_id: record.vm_name.clone(),
                                        agent_fd,
                                        sandbox_fd: String::new(),
                                        conditions,
                                        deleting: false,
                                    }),
                                };

                                // Update registry.
                                {
                                    let mut reg = sandboxes.write().await;
                                    if let Some(r) = reg.get_mut(&record.sandbox.name) {
                                        r.sandbox = updated.clone();
                                    }
                                }

                                if tx.send(Ok(sandbox_event(updated))).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(LxdApiError::NotFound(_)) => {
                            // Instance disappeared — emit delete event.
                            let sandbox_id = record.sandbox.id.clone();
                            let sandbox_name = record.sandbox.name.clone();
                            let vm_name = record.vm_name.clone();

                            if is_deleting {
                                debug!(
                                    sandbox_name = %sandbox_name,
                                    vm_name = %vm_name,
                                    "Deleted instance removed from LXD"
                                );
                            } else {
                                warn!(
                                    sandbox_name = %sandbox_name,
                                    vm_name = %vm_name,
                                    "Instance unexpectedly removed from LXD"
                                );
                            }

                            {
                                let mut reg = sandboxes.write().await;
                                reg.remove(&sandbox_name);
                            }
                            last_status.remove(&vm_name);

                            let event = WatchSandboxesEvent {
                                payload: Some(watch_sandboxes_event::Payload::Deleted(
                                    WatchSandboxesDeletedEvent { sandbox_id },
                                )),
                            };
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            debug!(
                                vm_name = %record.vm_name,
                                error = %e,
                                "Error polling instance state in watch loop"
                            );
                        }
                    }
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    /// Push the supervisor binary from host into the VM.
    async fn push_supervisor(&self, vm_name: &str) -> Result<(), ComputeDriverError> {
        let data = tokio::fs::read(&self.config.supervisor_path)
            .await
            .map_err(|e| {
                ComputeDriverError::Message(format!(
                    "failed to read supervisor binary {}: {e}",
                    self.config.supervisor_path.display()
                ))
            })?;

        // Push the env file first so it's ready before the path unit
        // triggers the service (the path unit watches for the binary).
        // Directories /opt/openshell, /run/openshell, /etc/openshell are
        // created by cloud-init.
        self.client
            .push_file(
                vm_name,
                "/opt/openshell/supervisor",
                data,
                0,
                0,
                0o755,
                &self.config.lxd_project,
            )
            .await
            .map_err(ComputeDriverError::from)?;

        Ok(())
    }

    fn build_supervisor_env(
        &self,
        sandbox_id: &str,
        sandbox_name: &str,
    ) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("OPENSHELL_SANDBOX_ID".to_string(), sandbox_id.to_string());
        env.insert("OPENSHELL_SANDBOX".to_string(), sandbox_name.to_string());
        env.insert(
            "OPENSHELL_ENDPOINT".to_string(),
            self.config.grpc_endpoint.clone(),
        );
        env.insert(
            "OPENSHELL_SSH_SOCKET_PATH".to_string(),
            "/run/openshell/ssh.sock".to_string(),
        );
        env.insert(
            "OPENSHELL_SSH_HANDSHAKE_SECRET".to_string(),
            self.config.ssh_handshake_secret.clone(),
        );
        env.insert(
            "OPENSHELL_SSH_HANDSHAKE_SKEW_SECS".to_string(),
            self.config.ssh_handshake_skew_secs.to_string(),
        );
        // Keep the entrypoint process alive indefinitely so the supervisor
        // doesn't exit when the default /bin/bash returns immediately in a
        // non-interactive exec context.
        env.insert(
            "OPENSHELL_SANDBOX_COMMAND".to_string(),
            "tail -f /dev/null".to_string(),
        );
        env.insert("OPENSHELL_LOG_LEVEL".to_string(), "info".to_string());
        env
    }

    fn build_instance_request(
        &self,
        vm_name: &str,
        image: &str,
        sandbox_id: &str,
        sandbox_name: &str,
        supervisor_env: &HashMap<String, String>,
    ) -> InstancesPost {
        let mut config = HashMap::new();
        config.insert(
            "user.user-data".to_string(),
            cloud_init_user_data(supervisor_env),
        );
        // Tag the instance so we can identify it as managed by this driver.
        config.insert("user.openshell.managed".to_string(), "true".to_string());
        config.insert(
            "user.openshell.sandbox_id".to_string(),
            sandbox_id.to_string(),
        );
        config.insert(
            "user.openshell.sandbox_name".to_string(),
            sandbox_name.to_string(),
        );

        let mut root_device = HashMap::new();
        root_device.insert("type".to_string(), "disk".to_string());
        root_device.insert("path".to_string(), "/".to_string());
        root_device.insert("pool".to_string(), self.config.storage_pool.clone());

        let mut eth0_device = HashMap::new();
        eth0_device.insert("type".to_string(), "nic".to_string());
        eth0_device.insert("nictype".to_string(), "bridged".to_string());
        eth0_device.insert("parent".to_string(), self.config.network_name.clone());
        eth0_device.insert("name".to_string(), "eth0".to_string());

        let mut devices = HashMap::new();
        devices.insert("root".to_string(), root_device);
        devices.insert("eth0".to_string(), eth0_device);

        InstancesPost {
            name: vm_name.to_string(),
            instance_type: "virtual-machine".to_string(),
            source: crate::lxd::models::InstanceSource {
                source_type: "image".to_string(),
                alias: image.to_string(),
                server: Some(self.config.image_server.clone()),
                protocol: Some("simplestreams".to_string()),
                mode: Some("pull".to_string()),
            },
            config,
            devices,
            profiles: vec!["default".to_string()],
        }
    }
}

fn sandbox_event(sandbox: DriverSandbox) -> WatchSandboxesEvent {
    WatchSandboxesEvent {
        payload: Some(watch_sandboxes_event::Payload::Sandbox(
            WatchSandboxesSandboxEvent {
                sandbox: Some(sandbox),
            },
        )),
    }
}

fn provisioning_condition() -> DriverCondition {
    DriverCondition {
        r#type: "Ready".to_string(),
        status: "False".to_string(),
        reason: "Starting".to_string(),
        message: "VM is running, waiting for supervisor connection".to_string(),
        last_transition_time: String::new(),
    }
}

fn stopped_condition() -> DriverCondition {
    DriverCondition {
        r#type: "Ready".to_string(),
        status: "False".to_string(),
        reason: "VmStopped".to_string(),
        message: "VM is stopped".to_string(),
        last_transition_time: String::new(),
    }
}

fn stopped_condition_with_status(status: &str) -> DriverCondition {
    DriverCondition {
        r#type: "Ready".to_string(),
        status: "False".to_string(),
        reason: "VmNotRunning".to_string(),
        message: format!("VM status: {status}"),
        last_transition_time: String::new(),
    }
}

fn cloud_init_user_data(env: &HashMap<String, String>) -> String {
    // Build sorted env file content for the systemd EnvironmentFile.
    let mut env_lines: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    env_lines.sort();
    let env_content = env_lines.join("\n");

    format!(
        "\
#cloud-config
users:
  - default
  - name: sandbox
    system: true
    shell: /bin/bash

write_files:
  - path: /etc/openshell/supervisor.env
    permissions: '0600'
    content: |
{env_file}
  - path: /etc/systemd/system/openshell-supervisor.service
    permissions: '0644'
    content: |
      [Unit]
      Description=OpenShell Sandbox Supervisor
      After=network-online.target
      Wants=network-online.target

      [Service]
      Type=simple
      EnvironmentFile=/etc/openshell/supervisor.env
      ExecStart=/opt/openshell/supervisor
      Restart=on-failure
      RestartSec=2
      RuntimeDirectory=openshell
      RuntimeDirectoryPreserve=yes

      [Install]
      WantedBy=multi-user.target
  - path: /etc/systemd/system/openshell-supervisor.path
    permissions: '0644'
    content: |
      [Unit]
      Description=Watch for OpenShell supervisor binary

      [Path]
      PathExists=/opt/openshell/supervisor

      [Install]
      WantedBy=multi-user.target

runcmd:
  - mkdir -p /opt/openshell /run/openshell /etc/openshell
  - chmod 700 /run/openshell
  - systemctl daemon-reload
  - systemctl enable --now openshell-supervisor.path
",
        env_file = env_content
            .lines()
            .map(|l| format!("      {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
