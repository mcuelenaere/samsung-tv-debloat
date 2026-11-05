#!/usr/bin/env bash
# Build the two unsigned managed broker-protocol test TPKs.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet "$IMAGE" sh -eu -c '
        for item in brokerone:PROBE_ONE brokertwo:PROBE_TWO; do
            package=${item%%:*}
            define=${item##*:}
            output="/src/out/dotnet-$package"
            pkgroot="/src/out/$package/pkgroot"
            rm -rf "$output" "$pkgroot"
            dotnet publish -c Release -o "$output" -p:DefineConstants="$define"
            mkdir -p "$pkgroot/bin" "$pkgroot/shared/res"
            cp "$output/BrokerProbe.dll" "$output/BrokerProbe.runtimeconfig.json" "$output/BrokerProbe.deps.json" "$pkgroot/bin/"
            cp "/src/$([ "$package" = brokerone ] && echo broker-one || echo broker-two)-manifest.xml" "$pkgroot/tizen-manifest.xml"
            cp /src/icon.png "$pkgroot/shared/res/org.tvauth.spike.$package.png"
        done
    '

for package in brokerone brokertwo; do
    pkgroot="$SCRIPT_DIR/out/$package/pkgroot"
    tpk="$SCRIPT_DIR/out/$package/org.tvauth.spike.$package-0.1.0.tpk"
    rm -f "$tpk"
    ( cd "$pkgroot" && zip -q -X -r "$tpk" tizen-manifest.xml bin shared )
    ls -l "$pkgroot/bin" "$tpk"
done
