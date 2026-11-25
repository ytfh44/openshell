// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! When a launched driver stops serving.

use std::future::Future;
use std::time::Duration;

use openshell_driver_launch::Shutdown;

/// How long a stop signal from elsewhere defers to the gateway.
///
/// It is shorter than the 90 seconds a service manager gives a unit to stop.
pub const EXTERNAL_STOP_GRACE: Duration = Duration::from_secs(30);

/// The future a launched driver's server stops on.
///
/// It completes when the gateway releases the driver with `Shutdown`, which
/// the parent guard also sets when the gateway is gone. A stop signal from
/// elsewhere, such as the SIGTERM a service manager sends to every process of
/// the gateway's unit, does not stop the driver at once: the gateway may still
/// need it to stop sandboxes. `signal` completes when such a signal arrives,
/// and the driver then waits up to `grace` for the gateway before it stops.
pub async fn stop_when_released(
    shutdown: Shutdown,
    signal: impl Future<Output = ()>,
    grace: Duration,
) {
    tokio::select! {
        () = shutdown.requested() => tracing::info!("the gateway released the driver"),
        () = async {
            signal.await;
            tracing::info!(
                grace_secs = grace.as_secs(),
                "stop signal received; waiting for the gateway to release the driver"
            );
            tokio::time::sleep(grace).await;
        } => tracing::warn!("the gateway did not release the driver in time; stopping"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use openshell_driver_launch::Shutdown;

    use super::stop_when_released;

    const GRACE: Duration = Duration::from_secs(30);

    #[tokio::test(start_paused = true)]
    async fn a_release_stops_the_driver() {
        let shutdown = Shutdown::new();
        shutdown.request();
        stop_when_released(shutdown, std::future::pending(), GRACE).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_signal_waits_for_the_release() {
        let shutdown = Shutdown::new();
        let stopped = tokio::spawn(stop_when_released(
            shutdown.clone(),
            std::future::ready(()),
            GRACE,
        ));

        tokio::time::sleep(Duration::from_secs(29)).await;
        assert!(!stopped.is_finished(), "stopped before the grace period");

        shutdown.request();
        stopped.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_signal_stops_the_driver_after_the_grace_period() {
        let stopped = tokio::spawn(stop_when_released(
            Shutdown::new(),
            std::future::ready(()),
            GRACE,
        ));

        tokio::time::sleep(GRACE + Duration::from_secs(1)).await;

        assert!(
            stopped.is_finished(),
            "still running after the grace period"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_signal_and_no_release_keeps_the_driver_running() {
        let stopped = tokio::spawn(stop_when_released(
            Shutdown::new(),
            std::future::pending(),
            GRACE,
        ));

        tokio::time::sleep(GRACE * 10).await;

        assert!(!stopped.is_finished());
    }
}
