#!/usr/bin/env bash
# Legacy managed .NET approval UI build. The native Rust UI in `rust/` is the
# production implementation; this is kept for reference and fallback.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
IMAGE=mcr.microsoft.com/dotnet/sdk:3.1
OUTPUT="$SCRIPT_DIR/out/tv-auth-ui-dotnet"
PKGROOT="$OUTPUT/pkgroot"
TPK="$OUTPUT/org.tvauth.ui-0.1.0.dotnet.tpk"

docker run --rm --platform linux/amd64 \
    -v "$SCRIPT_DIR:/src" -w /src/dotnet "$IMAGE" sh -eu -c '
        rm -rf /src/out/tv-auth-ui-dotnet
        dotnet publish -c Release -o /src/out/tv-auth-ui-dotnet/publish
        mkdir -p /src/out/tv-auth-ui-dotnet/pkgroot/bin /src/out/tv-auth-ui-dotnet/pkgroot/shared/res
        cp /src/out/tv-auth-ui-dotnet/publish/TvAuthUi.dll \
           /src/out/tv-auth-ui-dotnet/publish/TvAuthUi.runtimeconfig.json \
           /src/out/tv-auth-ui-dotnet/publish/TvAuthUi.deps.json \
           /src/out/tv-auth-ui-dotnet/pkgroot/bin/
        cp /src/dotnet/tizen-manifest.xml /src/out/tv-auth-ui-dotnet/pkgroot/tizen-manifest.xml
        cp /src/dotnet/icon.png \
           /src/out/tv-auth-ui-dotnet/pkgroot/shared/res/org.tvauth.ui.png
    '

rm -f "$TPK"
( cd "$PKGROOT" && zip -q -X -r "$TPK" tizen-manifest.xml bin shared )
ls -l "$PKGROOT/bin" "$TPK"
shasum -a 256 "$TPK"
