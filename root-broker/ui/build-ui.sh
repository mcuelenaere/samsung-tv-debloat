#!/usr/bin/env bash
# Build the managed Tizen .NET approval and grant-manager UI.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1
OUTPUT="$SCRIPT_DIR/out/tv-auth-ui"
PKGROOT="$OUTPUT/pkgroot"
TPK="$OUTPUT/org.tvauth.ui-0.1.0.tpk"

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet "$IMAGE" sh -eu -c '
        rm -rf /src/out/tv-auth-ui
        dotnet publish -c Release -o /src/out/tv-auth-ui/publish
        mkdir -p /src/out/tv-auth-ui/pkgroot/bin /src/out/tv-auth-ui/pkgroot/shared/res
        cp /src/out/tv-auth-ui/publish/TvAuthUi.dll \
           /src/out/tv-auth-ui/publish/TvAuthUi.runtimeconfig.json \
           /src/out/tv-auth-ui/publish/TvAuthUi.deps.json \
           /src/out/tv-auth-ui/pkgroot/bin/
        cp /src/dotnet/tizen-manifest.xml /src/out/tv-auth-ui/pkgroot/tizen-manifest.xml
        cp /src/dotnet/icon.png \
           /src/out/tv-auth-ui/pkgroot/shared/res/org.tvauth.ui.png
    '

rm -f "$TPK"
( cd "$PKGROOT" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$PKGROOT/bin" "$TPK"
shasum -a 256 "$TPK"
