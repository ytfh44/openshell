// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Shared building blocks for gateway-launched compute drivers.
//!
//! A launched driver reads its operator table from stdin, reports start-up
//! failures with a stable exit code and one JSON line on stdout, and receives
//! gateway metadata through [`launch`]'s `Configure` RPC.

mod admission;
mod endpoint;
mod gateway;
mod guard;
mod start;
mod table;
mod telemetry;
mod trust;

pub use admission::admission_from_json;
pub use endpoint::{endpoint_uses_tls, validate_grpc_endpoint};
pub use gateway::GatewayContext;
#[cfg(unix)]
pub use guard::watch_parent;
pub use openshell_driver_launch as launch;
pub use start::{EXIT_CONFIG, EXIT_UNAVAILABLE, StartKind, StartReport, StartResult};
pub use table::{MAX_TABLE_BYTES, read_stdin_table, read_table};
pub use telemetry::{deferred_export_config, supervisor_otlp};
pub use trust::load_network_trust;
