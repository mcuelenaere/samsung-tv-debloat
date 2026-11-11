# Managed FDET payload

`FdetProbe.cs` and its project files are the managed payload ported from
`samsung-tv-root`. The one-shot root command uses the `srs` mode; unrelated
agent and display-control assemblies are intentionally not part of the setup
payload.

Build outputs are consumed from:

```text
out/modern/FdetProbe.dll
out/modern/FdetProbe.runtimeconfig.json
out/legacy/FdetProbe.dll
out/legacy/FdetProbe.runtimeconfig.json
```

`FdetProbe31.csproj` builds the .NET Core 3.1 variant from the same source.
The runtime is selected on the TV only after `readlink -f
/usr/share/dotnet.tizen/netcoreapp` positively identifies a 3.1 installation.
