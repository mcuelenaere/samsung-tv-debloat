#!/usr/bin/env bash
# Build the managed Tizen .NET equivalent of the native EFL popup spike.
# This avoids UEP's rejection of unsigned native ELFs on writable app storage.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1
OUTPUT="$SCRIPT_DIR/out/popupdotnet"
PKGROOT="$SCRIPT_DIR/out/popupdotnet/pkgroot"
TPK="$OUTPUT/org.tvauth.spike.popupdotnet-0.1.1.tpk"

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet "$IMAGE" sh -eu -c '
        rm -rf /src/out/popupdotnet
        dotnet publish -c Release -o /src/out/popupdotnet/publish
        mkdir -p /src/out/popupdotnet/pkgroot/bin /src/out/popupdotnet/pkgroot/shared/res
        cp /src/out/popupdotnet/publish/EflPopupDotnet.dll \
           /src/out/popupdotnet/publish/EflPopupDotnet.runtimeconfig.json \
           /src/out/popupdotnet/publish/EflPopupDotnet.deps.json \
           /src/out/popupdotnet/pkgroot/bin/
        cp /src/dotnet/tizen-manifest.xml /src/out/popupdotnet/pkgroot/tizen-manifest.xml
        cp /src/dotnet/popup-icon.png \
           /src/out/popupdotnet/pkgroot/shared/res/org.tvauth.spike.popupdotnet.png
    '

rm -f "$TPK"
( cd "$PKGROOT" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$PKGROOT/bin" "$TPK"
shasum -a 256 "$TPK"
