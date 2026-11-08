#!/usr/bin/env bash
# Build the changed-content 0.1.1 upgrade of the brokerone test package.
# Same package ID and signer, different DLL/manifest -> different grant fingerprint.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1
OUTPUT="$SCRIPT_DIR/out/brokerone-0.1.1"
PKGROOT="$OUTPUT/pkgroot"
TPK="$OUTPUT/org.tvauth.spike.brokerone-0.1.1.tpk"

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet "$IMAGE" sh -eu -c '
        rm -rf /src/out/dotnet-brokerone-v2 /src/out/brokerone-0.1.1
        dotnet publish -c Release -o /src/out/dotnet-brokerone-v2 -p:DefineConstants=PROBE_ONE_V2
        mkdir -p /src/out/brokerone-0.1.1/pkgroot/bin /src/out/brokerone-0.1.1/pkgroot/shared/res
        cp /src/out/dotnet-brokerone-v2/BrokerProbe.dll \
           /src/out/dotnet-brokerone-v2/BrokerProbe.runtimeconfig.json \
           /src/out/dotnet-brokerone-v2/BrokerProbe.deps.json \
           /src/out/brokerone-0.1.1/pkgroot/bin/
        cp /src/broker-one-v2-manifest.xml /src/out/brokerone-0.1.1/pkgroot/tizen-manifest.xml
        cp /src/icon.png /src/out/brokerone-0.1.1/pkgroot/shared/res/org.tvauth.spike.brokerone.png
    '

rm -f "$TPK"
( cd "$PKGROOT" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$PKGROOT/bin" "$TPK"
shasum -a 256 "$TPK"
