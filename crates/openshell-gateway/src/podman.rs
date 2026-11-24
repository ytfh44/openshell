// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Podman compute driver launch.
//!
//! The `openshell-driver-podman` subprocess owns its
//! `[openshell.drivers.podman]` table. This module reads only the
//! gateway-owned `driver_dir` key, forwards the rest as TOML text on stdin,
//! and launches the driver through [`crate::launcher`].

use std::path::{Path, PathBuf};

use openshell_core::{Error, Result};
use openshell_server::{
    AcquiredRemoteDriverEndpoint, DetectedDriver, DriverDetection, DriverDetectionContext,
};

use crate::launcher::{
    self, ConfigureInputs, LaunchOutcome, LaunchRequest, PreflightFailure, StartOutcome, Started,
};

/// How many times an explicitly selected Podman is started while its socket is
/// unreachable, and how long to wait between attempts. This keeps the bounded
/// tolerance the in-process driver had.
const PODMAN_START_ATTEMPTS: u32 = 6;
const PODMAN_START_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

const DRIVER_BIN_NAME: &str = "openshell-driver-podman";
const DRIVER_BIN_HINT: &str = "install it under [openshell.drivers.podman].driver_dir, a conventional libexec path such as ~/.local/libexec/openshell, /usr/libexec/openshell, or /usr/local/libexec{,/openshell}, or place it next to the gateway binary";

/// The gateway's view of the `[openshell.drivers.podman]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodmanLaunchPlan {
    /// Where to look for the driver binary before the conventional locations.
    pub driver_dir: Option<PathBuf>,
    /// The table to forward, without gateway-owned keys.
    pub table: String,
}

impl PodmanLaunchPlan {
    /// Split gateway-owned keys from the table the driver will parse.
    ///
    /// `gateway_driver_dir` is `driver_dir` from `[openshell.gateway]`. A
    /// `driver_dir` in the driver table is deprecated: it is removed with a
    /// warning and used only when the gateway section sets none.
    pub fn from_table(
        mut table: toml::map::Map<String, toml::Value>,
        gateway_driver_dir: Option<&Path>,
    ) -> Result<Self> {
        let legacy_driver_dir = table
            .remove("driver_dir")
            .map(|value| {
                tracing::warn!(
                    "[openshell.drivers.podman].driver_dir is deprecated; set driver_dir in [openshell.gateway]"
                );
                value.as_str().map(PathBuf::from).ok_or_else(|| {
                    Error::config("[openshell.drivers.podman].driver_dir must be a string")
                })
            })
            .transpose()?;
        let driver_dir = gateway_driver_dir
            .map(Path::to_path_buf)
            .or(legacy_driver_dir);
        launcher::strip_gateway_owned_keys(&mut table);
        // `main` accepted `gateway_port` and overwrote it with the gateway's
        // listening port. The driver now rejects the key, so the gateway drops
        // it, warning once, and never treats it as an override.
        if table.remove("gateway_port").is_some() {
            tracing::warn!(
                "[openshell.drivers.podman].gateway_port is ignored: the gateway supplies its listening port; remove it"
            );
        }
        let table = toml::to_string(&table)
            .map_err(|error| Error::config(format!("invalid podman driver table: {error}")))?;
        Ok(Self { driver_dir, table })
    }

    fn binary(&self) -> Result<PathBuf> {
        launcher::resolve_binary(
            DRIVER_BIN_NAME,
            &launcher::driver_search_dirs(self.driver_dir.as_deref()),
            DRIVER_BIN_HINT,
        )
    }
}

/// The private directory for the launched driver's socket.
fn socket_dir() -> PathBuf {
    openshell_core::paths::openshell_state_dir().map_or_else(
        |_| PathBuf::from("target/openshell/run/podman"),
        |dir| dir.join("run").join("podman"),
    )
}

/// Validate the table with the driver's own `preflight`, which contacts no
/// backend and creates no state.
pub fn validate(plan: &PodmanLaunchPlan) -> Result<()> {
    launcher::preflight(&plan.binary()?, &plan.table).map_err(|failure| match failure {
        PreflightFailure::InvalidConfig(reason) => Error::config(format!(
            "invalid [openshell.drivers.podman] table: {reason}"
        )),
        PreflightFailure::Error(reason) => Error::execution(format!(
            "the podman compute driver failed its preflight: {reason}"
        )),
    })
}

/// Decide whether Podman can serve with the operator's table by starting the
/// driver, which checks the table and its socket before it binds. The running
/// driver is kept: if Podman is selected, this same process is configured.
pub fn detect(context: &DriverDetectionContext<'_>) -> DriverDetection {
    detect_in(context, &socket_dir())
}

fn detect_in(context: &DriverDetectionContext<'_>, socket_dir: &Path) -> DriverDetection {
    let plan = match PodmanLaunchPlan::from_table(context.table.clone(), context.driver_dir) {
        Ok(plan) => plan,
        Err(error) => return DriverDetection::InvalidConfig(error.to_string()),
    };
    let binary = match plan.binary() {
        Ok(binary) => binary,
        Err(error) => {
            tracing::warn!(%error, "skipping the podman compute driver: its binary cannot be used");
            return DriverDetection::Unavailable(error.to_string());
        }
    };
    match launcher::start(&LaunchRequest {
        name: "podman",
        binary: &binary,
        table: &plan.table,
        socket_dir,
        log_level: context.gateway_log_level,
    }) {
        Ok(StartOutcome::Started(started)) => {
            DriverDetection::Available(Some(DetectedDriver::new(started)))
        }
        Ok(StartOutcome::Unavailable(reason)) => DriverDetection::Unavailable(reason),
        Ok(StartOutcome::InvalidConfig(reason)) => DriverDetection::InvalidConfig(reason),
        // A crash, a timeout, or a launch error is not an absent backend.
        Err(error) => DriverDetection::Failed(error.to_string()),
    }
}

/// Launch the Podman compute driver, configure it, and return a gRPC channel
/// connected to it with a handle that stops it when dropped. `detected` is the
/// driver that auto-detection already started, which is configured instead of
/// starting another.
#[tracing::instrument(
    name = "driver.launch",
    skip_all,
    fields(otel.name = "driver.launch", otel.status_code = tracing::field::Empty, driver.name = "podman")
)]
pub async fn spawn(
    plan: &PodmanLaunchPlan,
    detected: Option<Started>,
    gateway_log_level: &str,
    inputs: &ConfigureInputs<'_>,
) -> Result<AcquiredRemoteDriverEndpoint> {
    let configure = inputs.request();
    let launched = if let Some(started) = detected {
        launcher::finish(started, configure, launcher::exit_gateway_on_driver_exit()).await?
    } else {
        let binary = plan.binary()?;
        // A named driver tolerates a Podman socket that is briefly down, such
        // as `podman.socket` restarting during a package upgrade.
        let outcome = launcher::launch_tolerating_unavailable(
            LaunchRequest {
                name: "podman",
                binary: &binary,
                table: &plan.table,
                socket_dir: &socket_dir(),
                log_level: gateway_log_level,
            },
            configure,
            launcher::exit_gateway_on_driver_exit(),
            launcher::UnavailableRetry {
                attempts: PODMAN_START_ATTEMPTS,
                delay: PODMAN_START_RETRY_DELAY,
            },
        )
        .await?;
        match outcome {
            LaunchOutcome::Launched(launched) => launched,
            LaunchOutcome::InvalidConfig(reason) => {
                return Err(Error::config(format!(
                    "invalid [openshell.drivers.podman] table: {reason}"
                )));
            }
            LaunchOutcome::Unavailable(reason) => {
                return Err(Error::execution(format!(
                    "the podman compute driver is unavailable: {reason}"
                )));
            }
        }
    };
    Ok(AcquiredRemoteDriverEndpoint::managed(
        "podman",
        launched.channel,
        launched.process,
    ))
}

#[cfg(test)]
mod tests;
