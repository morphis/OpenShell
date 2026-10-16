#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Snap wrapper for openshell-gateway. The system service supplies explicit
# compatibility paths; the user service uses OpenShell's XDG defaults.

set -eu

# An out-of-tree compute driver connected over the `compute-driver` content
# interface publishes a descriptor in the shared directory. Turn it into the
# compute-driver selection the gateway understands, unless the operator has
# already chosen a driver. The socket path is fixed by the interface contract;
# only the driver's name is taken from the descriptor, and it is validated the
# same way the gateway validates a configured name.
if [ -z "${OPENSHELL_COMPUTE_DRIVER:-}" ] && [ -n "${SNAP_COMMON:-}" ]; then
    driver_conf="${SNAP_COMMON}/compute-driver/driver.conf"
    if [ -f "$driver_conf" ]; then
        driver_name="$(sed -n 's/^[[:space:]]*driver_name[[:space:]]*=[[:space:]]*//p' "$driver_conf" | head -n 1)"
        case "$driver_name" in
            '' | *[!A-Za-z0-9_-]*)
                driver_name=""
                ;;
        esac
        if [ -n "$driver_name" ]; then
            export OPENSHELL_COMPUTE_DRIVER="$driver_name"
            export OPENSHELL_COMPUTE_DRIVER_SOCKET="${SNAP_COMMON}/compute-driver/openshell-driver.sock"
        fi
    fi
fi

if [ -z "${OPENSHELL_GATEWAY_CONFIG:-}" ] \
    && [ -n "${OPENSHELL_SNAP_CONFIG_FILE:-}" ] \
    && { [ -e "$OPENSHELL_SNAP_CONFIG_FILE" ] || [ -L "$OPENSHELL_SNAP_CONFIG_FILE" ]; }
then
    export OPENSHELL_GATEWAY_CONFIG="$OPENSHELL_SNAP_CONFIG_FILE"
fi

if [ -z "${OPENSHELL_LOCAL_TLS_DIR:-}" ]; then
    if [ -z "${XDG_STATE_HOME:-}" ]; then
        echo "openshell-gateway: OPENSHELL_LOCAL_TLS_DIR or XDG_STATE_HOME is required" >&2
        exit 1
    fi
    export OPENSHELL_LOCAL_TLS_DIR="${XDG_STATE_HOME}/openshell/tls"
fi

"${SNAP}/bin/openshell-gateway" config preflight -- "$@"

# Generate the local TLS bundle and the JWT bundle used for launch-scoped
# supervisor credentials; generate-certs is idempotent and preserves an
# existing bundle.
"${SNAP}/bin/openshell-gateway" generate-certs \
    --output-dir "$OPENSHELL_LOCAL_TLS_DIR" \
    --server-san host.openshell.internal

exec "${SNAP}/bin/openshell-gateway" "$@"
