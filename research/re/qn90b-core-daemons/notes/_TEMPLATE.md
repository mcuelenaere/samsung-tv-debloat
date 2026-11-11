# <daemon> — RE note

- **Path / unit:** `/usr/bin/<daemon>` — `<unit>.service`
- **Size / stripped:** `<n>` KB, stripped? `<y/n>`  ·  **Upstream?** `<upstream Tizen pkg / Samsung-proprietary / modified>`
- **Runs as / SMACK label:** `<root? User=? label>`

## What it does (2–4 lines)
<one-paragraph purpose; if upstream, say so and only note Samsung deltas>

## 1. Persistence surface
<does it exec/launch writable or root-owned content at boot? what paths? env/args it trusts? verdict: lead / none>

## 2. UEP / signature enforcement
<any signature/UEP/cert/verify logic? where (func/addr)? what it gates? bypass surface? verdict>

## 3. SMACK / privilege
<label it runs at; any exec of attacker-influenced input = confused deputy; cap handling>

## 4. Debloat
<safe to disable? ConditionPathExists marker? what breaks? verdict>

## Leads worth a deeper look
- <addr/func + one line> ...
