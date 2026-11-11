# eventd — RE note

- **Path / unit:** `/usr/bin/eventd` — `eventd.service`
- **Size / stripped:** 34 KB, stripped: **y**  ·  **Upstream?** Upstream-ish Tizen (`eventd-0.1`); small GLib/GDBus
- **Runs as / SMACK label:** **root** (unit has no `User=`/label). `After=deviced.service`, `Restart=always`. No `[Install]` section in the shipped unit (pulled in by another target's `.wants`).

## What it does (2–4 lines)
On-demand event launcher. Parses D-Bus-activation config files under `/usr/lib/systemd/eventd/`, registers vconf keys and connectivity events (net connect dns/gateway/http/phy, USB mount, system-idle), and when an event fires it asks **systemd** (`org.freedesktop.systemd1.Manager.EnqueueUnitJob`) to start the mapped unit. Pure plumbing; **no telemetry, no network upload, no URLs.**

## 1. Persistence surface — none (config-bounded)
Enqueues systemd unit jobs named by config files in **`/usr/lib/systemd/eventd/`** (root-owned rootfs). If that dir is writable you could make eventd start an arbitrary unit on a network/USB event — but that requires rootfs write and a crafted unit, i.e. you already have root. No exec of writable content, no shell-out. Trusts only root-owned config + vconf.

## 2. UEP / signature enforcement
None (not applicable).

## 3. SMACK / privilege
Runs root. Confused-deputy is bounded: it can only `EnqueueUnitJob` for units named in `/usr/lib/systemd/eventd/` config. `prctl` imported (likely thread naming). No cap juggling.

## 4. Debloat — leave enabled (plumbing, not telemetry)
- **Collects/transmits:** nothing. It's an event→systemd dispatcher.
- **What breaks if disabled:** on-demand services that rely on eventd triggers — e.g. USB-mount-activated units and network-connectivity-activated units may not auto-start. Could cause subtle "feature doesn't wake up when I plug in X / connect network" bugs. Not a data-collection daemon, so **low debloat value, non-trivial breakage risk.**
- **Marker trick:** no `ConditionPathExists`. (Could add one, but no reason to.)
- **Recommendation:** **do NOT mask** as part of debloat. Leave it. If you insist on trimming, mask only after confirming nothing you use depends on `/usr/lib/systemd/eventd/*` mappings; not worth it.

## Leads worth a deeper look
- `ls /usr/lib/systemd/eventd/` on the TV to enumerate exactly which units it can trigger (confirms blast radius before touching it).
