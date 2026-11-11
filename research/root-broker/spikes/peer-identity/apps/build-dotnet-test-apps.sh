#!/usr/bin/env bash
# Build the two managed .NET peer-identity probe TPK roots. The resulting
# packages still need Tizen author/distributor signing before installation.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1

for command in zip; do
    command -v "$command" >/dev/null || {
        echo "missing required command: $command" >&2
        exit 1
    }
done

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet-peer "$IMAGE" sh -eu -c '
        for item in peerone:PROBE_ONE peertwo:PROBE_TWO; do
            package=${item%%:*}
            define=${item##*:}
            output="/src/out/dotnet-$package"
            rm -rf "$output"
            dotnet publish -c Release -o "$output" -p:DefineConstants="$define"

            pkgroot="/src/out/$package/pkgroot-dotnet"
            rm -rf "$pkgroot"
            mkdir -p "$pkgroot/bin" "$pkgroot/shared/res"
            cp "$output/PeerProbe.dll" "$output/PeerProbe.runtimeconfig.json" \
                "$output/PeerProbe.deps.json" "$pkgroot/bin/"
            cp "/src/$([ "$package" = peerone ] && echo peer-one || echo peer-two)-dotnet-manifest.xml" \
                "$pkgroot/tizen-manifest.xml"
            cp /src/peer-icon.png "$pkgroot/shared/res/org.tvauth.spike.$package.png"
        done
    '

for package in peerone peertwo; do
    pkgroot="$SCRIPT_DIR/out/$package/pkgroot-dotnet"
    output="$SCRIPT_DIR/out/$package/org.tvauth.spike.$package-0.1.4.tpk"
    rm -f "$output"
    ( cd "$pkgroot" && zip -q -X -r "$output" tizen-manifest.xml bin shared )
    ls -l "$pkgroot/bin" "$output"
done
