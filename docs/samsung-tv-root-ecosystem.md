# The `chris-ritsen/samsung-tv-root` Ecosystem — Research Notes

*Research compiled 2026-10-25, updated 2026-10-31 and 2026-11-04 from the GitHub
API, project READMEs, r/Tizen, and local clone state. Numbers are point-in-time
snapshots; links point at the primary evidence.*

---

## 0. TL;DR

`chris-ritsen/samsung-tv-root` is the root toolkit this project builds on. It
was published in August 2026, exploded in late October after its
model-independent `archive-root` route shipped (v0.0.9), and has spawned a
small ecosystem: boot-persistent ("untethered") root forks, debloat/privacy
findings, and several consumer-facing launcher projects that now out-star the
root toolkit itself. As of 2026-11-04: **514 stars, 57 forks, 45
issues/PRs, ~3,900 total release downloads**, master still at v0.0.12
(2026-10-23) with the maintainer triaging via his own open PR #44.
Upstream review capacity is the ecosystem's bottleneck — compatibility data
now accumulates faster in issues and downstream trackers than it lands in the
README.

---

## 1. The core project

Published August 2026 by Chris Ritsen, built largely with AI assistance
("about a day with Codex" for the QN90B, "three hours" for the QN90F). It
roots Samsung Tizen TVs via:

1. **Foothold — SVE-2025-50109**: Developer Mode exposes `sdbd` on TCP 26101;
   the daemon refuses shells but still accepts the internal
   `0 appinstall tpk <name>` command, whose package name is interpolated into
   a shell command unquoted → backtick substitution → command execution as
   `uid=901(sdk)`, SMACK `System`.
2. **Escalation, per-model kernel exploits** (original routes):
   - **QN90B**: `/dev/sdp_pqe_fdet` (Samsung display-processing device) mmap
     handler does `remap_pfn_range` on an unvalidated `vm_pgoff` → arbitrary
     physical RAM mapping → locate own `task_struct` via a unique `PR_SET_NAME`
     comm tag → rewrite `cred` uid/gid fields to 0.
   - **QN90F**: Mali-G510 r48p0 **CVE-2025-0072** (USER_IO UAF) → dangling
     pages reused as a Mali L3 page table aliased into CPU space → GPU page
     tables patched for arbitrary kernel memory R/W.
3. **Escalation — `archive-root` route (v0.0.9, 2026-10-21)**: model-independent.
   Uses the signed, capability-bearing `/usr/bin/dotnet`
   (`cap_setgid,cap_sys_admin=ei`), a private mount namespace bind-mounting a
   generated verification key over Samsung's launcher key, and Samsung's own
   signed archive launcher (`sdbd-tarlauncher`, `cap_setgid,cap_setuid=eip`) to
   run a staged script as root; HMAC-authenticated root agent calls back
   (`uid==euid==gid==egid==0` enforced). No kernel bug, no system file changes.
   This is the route the whole ecosystem runs on. It adapts a route first
   reported publicly on r/Tizen by u/motodrizzle (S90D owner =
   couchroot/splitcipher).
4. Extras: volatile UEP disable (unsigned native execution — see the
   [UEP notes](samsung-tv-uep.md)), host daemon
   re-acquiring root at each TV boot, remote remap/observe, HDMI/source/
   picture controls, screenshots/overlays (QN90F), SWU firmware-key extraction.

Root is **volatile by design**; persistence is achieved host-side (watchdog) or
by community forks (untethered, §5).

Releases: v0.0.1 (2026-08-17) → v0.0.8 (2026-09-20) → v0.0.9 archive-root
(2026-10-21) → v0.0.10 runtime detection / v0.0.11 alt CLR dir (2026-10-22) →
**v0.0.12 cold-SDB + frozen-OpenSSL fixes (2026-10-23, current)**. v0.0.12
had 3,152 downloads as of 2026-11-04.

---

## 2. Growth trajectory

| | 2026-10-25 | 2026-10-31 | 2026-11-04 |
|---|---|---|---|
| Stars / forks | 346 / 32 | 487 / 51 | **514 / 57** |
| Issues + PRs total | 20 (0 PRs) | 40 (3 PRs) | **45 (7 PRs)** |
| v0.0.12 downloads | 1,117 | 2,580 | **3,152** |
| Vetro stars | 86 | 222 | **261** |
| Master branch | v0.0.12 | v0.0.12 (silent since 10-23) | v0.0.12 (PR #44 opened 11-03) |

Collaboration model: originally **zero PRs** — evidence-first GitHub issues
(exact model/firmware/build + authenticated UID-0 JSON readback required for
the README compat table), Ritsen folding reports into releases within ~48 h.
From Oct 25 onward PRs became normal (7 to date). Notable: heavy AI-assisted
research culture across the community (Codex, DeepSeek V4.1 Flash on OpenCode,
Astra, Kimi).

---

## 3. Compatibility results (community-confirmed)

Legend: **owner-tested** = ran by Ritsen; **release** = unmodified public
release; **adapted** = local host-side changes; **downstream** = via a
downstream project/fork. All routes are SVE-2025-50109 foothold → archive-root
unless noted.

### Confirmed root

| Model | Firmware / build | Status / evidence |
|---|---|---|
| QN55QN90BAFXZA (QN90B) | `T-PTMAKUC-1602.3` | owner-tested; FDET + archive routes |
| QN75QN90FAFXZA (QN90F) | `T-RSMFAKUC-0090-1203.0` | owner-tested; Mali CVE-2025-0072 |
| QA55QN90FAUXEG (QN90F) | `T-RSMFUABC-0090-1301.0` | release v0.0.3 validation |
| QE55S95BATXXH (S95B) | `T-PTMDEUC-1720.7` | **my PR #21** (PontusM/.NET 3.1, FDET route) |
| QN55Q60BAFXZC (Q60B) | `T-NKLBAKUC-1720.7` | #1 (levkropp), #12, #34 (adapted) |
| UE50AU9072UXXH (AU9072) | `T-NKLADEUC-2301.1` | #5 (adapted, runtime fallback) |
| TQ43Q7F3AUXXC (Q7F3A) | `T-RSLFDEUC-0090` b. 202608061939 | #5 (ngxson comment) |
| QE77S90CATXXN (S90C) | `T-PTMCDEUC-2000.2` | #3 (adapted, direct-SDB wrapper) |
| UE43DU7100KXXU (DU7100) | `T-KSU2EDDEUC-0090-REL-202603190204` | #6 (adapted) |
| QN43LS03DAFXZA (Frame '24) | `T-PTMDAKUC-0090-2130.0` | #8 (downstream `quiet-tizen`) |
| UA43AU7000KXXD (AU7000) | `T-KSU2EUABC-REL-202607211852` | #9 — **release** v0.0.11, twice |
| QN85QN90AAFXZA (QN90A) | `T-NKM2AKUC-REL-202607211843` | #18 — **release** v0.0.12 |
| QN55QN85DBGXZD (QN85D) | `T-PTMDUABC-0090-2130.0` | #11 (sdbd 3.8.12, `vd_appinstall`) |
| UE43DU7100 / UN50DU6900 (DU-series) | `T-KSU2EDDEUC-2130.0` | #6, #16 (untethered proven, net6.0 retarget) |
| QN48S90FAEXZA (S90F) | `T-RSMFAKUC-0090-REL-202607141953` | #15 — **release** v0.0.12, first S90F |
| QE65Q64AAUXXN (Q64A) | `T-NKLADEUC-2310.0` | #17 (+ boot persistence, §5) |
| QE65Q60BAUXXN / GQ50Q60BAUXZG (Q60B) | `1720.7` | #12, #34 (untethered deployed) |
| QE43Q60AAUXZT (Q60A) | `T-NKLADEUC-2301.1` | #20 — **release** v0.0.12 |
| LS27BG652EUXEN (Odyssey G6 monitor!) | `T-PTMLWWC-1720.7` | #19 — **release** v0.0.12 |
| QE55LS03AAUXXN (Frame '21) | `2310.0` | #14 — release v0.0.12 (macOS relay) |
| QE55Q60CAUXRU (Q60C) | `T-NKLCEUC-0080-2032.1` | #39 |
| UE55AU7100KXXU (AU7100) | `T-KSU2EDEUC-2310.0` | #30 |
| UA85DU7700WXXY (DU7700) | `2130.0` | #32 |
| QE50LS01BAUXXH (Serif LS01B) | `T-PTMDEUC-REL-202602250226` | #35 — release, read-only sessions |
| QE65QN95A (QN95A) | `T-NKM2DEUC-2310.0` | #29 — release v0.0.12 + own untethered chain |
| QA85QN85AAUXZN (QN85A 85") | `T-NKM2DEUC`-class | #31 (AI-agent-applied fixes) |
| QA55QEF1AULXL / QN55QEF1AFXZX (QEF1A) | `T-RSLFUABC`/`T-RSLFAKUC-1300.9` | DemonKingSwarn repo; #23 |
| QA77S85FAEXXY (S85F AU) | `T-PTMFUABC-0090-1301.0` | PR #41 — release v0.0.12, 5 sessions |
| TQ55LS03FAUXXC (Frame '25) | `T-PTMFDEUC-0090-1301.0` | PR #45 — + watchdog tool |
| S90D / S95C / Q80A / Q70A / Q67B / Q65D / CU-series | various | via couchroot, Vetro, samsung-tv-tools trackers (downstream) |

### Boundaries (documented failures)

| Target | Result | Evidence |
|---|---|---|
| UN50M75HAGXZD (Tizen 10) | injection rejected; sdbd 3.8.12 plugin validates shell-command chars | #7 |
| 2020 NIKEM (Q70T etc.) | **no `/usr/bin/dotnet` host**; `dotnet-launcher` strips caps (`CapEff=0x0`) even with forged AUL context + partner cert → archive-root dead end; USB LPE route instead | #13, #2 |
| QE75Q90TATXXU (Q90T) | archive route failed; persistent root via Luminger USB LPE adaptation (different project) | #2 comments, tiz0wn |
| 2018 NU7100 (`T-KTM2LUABC-1360.0`) | foothold not present at all | #33 |
| 2020 TU-series (`T-KTSU2DEUC`, AU/Crystal UHD) | dotnet present but lacks effective SETGID/SYS_ADMIN | #40, #37 |
| UE55TU8070UXZG (Tizen 5.5) | no dotnet; sdbd 2.2.31; only launcher-remap goals | #38 |
| QE55Q60RATXXC (Tizen 5.0) | injection rejected instantly | #22 |
| S95F | archive-root works, but UEP blocks uploaded natives; workaround: .NET payload dlopens the *verified* `SWUMainApp` and calls its TrustZone client by symbol (TA accepts non-SWUMainApp callers — a finding in itself) | #28 |
| Tizen 4.0 / NU8000 | no SDB foothold; separate dashboard work in forks | #25–#27 |

### Recurring host-side gotchas (folded into releases or issues)

- Samsung's nonstandard `dotnet` apphost rejects `--list-runtimes` → v0.0.10
  runtime detection; alternative CLR directory → v0.0.11; first-call SDB
  timing false-negative → v0.0.12.
- Windows auto-creates a *blocking* firewall rule alongside the allow rule —
  block wins (#16).
- Frozen Linux binary needs host OpenSSL library path restore (v0.0.12).
- Staging-dir mkdir bug in the untethered chain on some models (#16, #34).

---

## 4. Debloat / privacy findings (directly relevant to this repo)

- **Ad/telemetry kill-switch markers** (#11 JoaoZaokk, #15 kvncaldwll, Tizen 9):
  `pisa-service.service` + `adagent-service.service` are gated by
  `ConditionPathExists=!/opt/usr/share/systemd/system/pisa-service`; the
  **content-analysis** daemon is gated by a *differently named* marker:
  `ConditionPathExists=!/opt/usr/share/systemd/system/canalysis-daemon`.
  Creating both markers disables PISA ads + adagent + canalysis; zero
  `NRestarts`, reversible with `rm`. Also stop the matching `.path` units —
  they watch the marker inode and re-activate the service.
- **`acr` (automatic content recognition)** is a *svcapp* launched by the
  app-control daemon, **not** systemd — it relaunches within seconds after
  being stopped. Durable kill requires renaming the binary:
  `mv .../com.samsung.tv.acr-service-app/bin/acr .../acr.disabled` (#15).
- **Preload packages cannot be uninstalled even as root** on Tizen 6.0
  (`pkgcmd -u` → `Operation not allowed [-4]`); Tizen 9's 84-package removal
  (DemonKingSwarn: 511 → 427 packages, no failed units) does **not** carry over
  (#5). Service-marker disabling is the durable route on Tizen 6.
- Read-only `/etc`/VDFS blocks drop-ins and `systemctl mask` on many builds;
  vendor `ConditionPathExists=` marker files are the general pattern.
- Vetro wraps all of this as reversible dashboard switches (incl. blocking ad
  domains on-device, disabling HbbTV/voice/ACR, and disabling firmware
  auto-update so root isn't removed behind your back).

---

## 5. Persistence / untethered landscape

Upstream root is volatile (host watchdog reacquires after each boot). The
community built several TV-side persistence routes:

| Route | Where | Notes |
|---|---|---|
| Boot-time .NET agent (replaced IL of an installed `dotnet-inhouse` app + on-boot flag; boot IL is **not** signature-checked; trigger lives on writable storage) + token-gated sdb bridge (Host PC IP stays `127.0.0.1`) + module manager | levkropp/samsung-tv-root-untethered (from #1) | Proven Q60B; deployed by others (#12, #34); proven on Tizen 9 with net6.0 retarget (#16). Manager v5.13 + telemetry module 1.2.3 + "immortal bridge" shipped |
| `ghservice` startup-slot stub TPK re-running archive-root at boot + token relay | #29 supersharp (QN95A); fork `samsung-tv-root-and-debloat` | second independent untethered chain |
| Gaming Hub startup slot boot agent + authenticated headless SDB bridge, .NET 3.1 + 6 builds | **PR #42** (kninetimmy) offered upstream; verified QN90B + U8000 | untethered work migrating into upstream |
| Vendor `AT-HUB.service` → `/opt/etc/parseinfo.sh` boot hook (writable VDFS, no `User=`), `#!/usr/bin/gawk -f` UEP bypass, `pkgcmd` WGT signature bypass | #17 danielyc (Tizen 6.0, NIKEL) | boot-persistent root + unsigned app install through the normal package manager |
| `tizen-system-env.service` sourcing a same-size-patched `/etc/profile.d` script | #13 cfloinc (NIKEM) | boot+0.74 s, no USB stick |
| Host-side watchdog reacquiring volatile root per boot | PR #45 (`archive-root-watchdog.py`); Vetro's TV-local reacquire | semi-persistent via host/TV-local watchdog |
| USB exFAT kernel LPE (Luminger) | Luminger/samsung-tv-usb-root; Q90T/Q70T adaptations | separate route for pre-dotnet platforms |

---

## 6. Ecosystem catalog (status 2026-11-04)

### Root tooling
- **chris-ritsen/samsung-tv-root** — 514★, 57 forks, v0.0.12. The core.
- **levkropp/samsung-tv-root-untethered** — 14★; boot agent, sdb bridge, module manager; last push 2026-10-31.
- **invaliddayta/samsung-tv-tools** — 21★; Linux/Nix S90C wrapper (root commands, remote keys, screenshots, cleanup); community reports S90D ×2, S95C.
- **DemonKingSwarn/samsung-tv-tizen-root** — 27★; Typst paper + evidence + debloat tooling for a 2026 Tizen 9 QEF1A (root via "DeepSeek V4.1 Flash (OpenCode)").
- **sebastianspicker/tiz0wn** — 15★; evidence-first research, Q90T persistent-root handover.
- **Luminger/samsung-tv-usb-root** — 14★; one-plug USB root for `T-MSMDEUC 1500.9` (pre-2021 route).
- **splitcipher/couchroot** — 19★; "Minimal Hub" launcher + root agent for QN90A & S90D; **origin of the archive-root route** (u/motodrizzle's r/Tizen S90D post).
- Notable forks: **ngxson** (Q7F3A runtime fix, 12★), **lzongren** (QN90D HDMI-wake branch), **supersharp** (QN95A debloat), **zeeshanhaque21** (Tizen 4.0 dashboard, withdrawn from upstream PR #27 to the fork).

### Consumer-facing launchers (now out-star the root toolkit)
- **andrearicci93/vetro** — 261★, 86 open issues; launcher + debloater + remapper + MQTT/Home Assistant; roots TV-locally so no host stays connected; installs via Apps2Samsung; roadmap (r/Tizen, 10-31): plugins, autoupdater, TV-side PIN (0.4.0). Releases v0.3.0→v0.3.5 in the last week.
- **NelsonEU/arn0-tv** — 258★, **75 forks**; custom home screen (Q70B, Tizen 6.5); code-dormant since 10-22; Home-button vconf gotcha (`db/appfw/home_appid` insufficient) tracked in its issues.
- **helalaou/AeroTizen** — 81★, 24 forks; "a better user interface for Samsung TVs"; created 10-19, actively pushed 11-04.
- **Apps2Samsung/Apps2Samsung** — 2,507★; the packaging/installation backbone (predates the scene, 2025-05); active daily.

### PR queue at upstream (open, 2026-11-04)
- **#21** — *mine*: S95B/PontusM .NET Core 3.1 support (FDET route on a QN90B sibling). Opened 10-25; **0 comments, CI still `action_required`** (first-time-contributor approval never granted).
- **#36** — docs: standalone `sdb` download for Linux (calin2k).
- **#41** — docs: S85F AU validation (roubochr).
- **#42** — TV-side boot-root + headless bridge (kninetimmy) — untethered work being offered upstream.
- **#44** — **maintainer's own**: archive-root diagnostics + setup/compat clarifications (+154/−21, 6 files), folding in #31/#37. First maintainer activity since 10-23; **nothing merged yet**.
- **#45** — Frame '25 report + host-side auto-root watchdog (jallehalvarsson-wq).

---

## 7. Timeline

- **2026-08-17** repo published (v0.0.1); v0.0.2–v0.0.8 through 09-20.
- **2026-10-09** levkropp proves untethered boot-root on Q60B (#1).
- **2026-10-15** S90C wrapper report (#3).
- **2026-10-21** v0.0.9 ships archive-root → compat report avalanche begins.
- **2026-10-19/20** AeroTizen and arn0-tv created; Vetro & couchroot publish (10-11).
- **2026-10-22/23** v0.0.10–v0.0.12 in 48 h, absorbing community fixes.
- **2026-10-23 → 11-03** maintainer silent; 25 untriaged reports/PRs accumulate; ecosystem self-organizes via forks + downstream trackers.
- **2026-10-25** my PR #21 opened (S95B support).
- **2026-10-26 → 10-31** first PR wave (#27 withdrawn-by-author, #36); #28–#40 reports; Vetro 86→222★; launchers take over the star growth.
- **2026-11-03** Ritsen returns via PR #44; PRs #41/#42/#45 queue up; untethered work (PR #42) heads upstream.
- **2026-11-04** master still v0.0.12; nothing merged.

## 8. Open questions / watch items

1. Will Ritsen merge #44 and clear the queue (#21, #36, #41, #42, #45)? The
   ecosystem either re-centralizes around upstream or forks become the
   de-facto mainline.
2. PR #42 upstreaming TV-side boot-root would turn persistence from a fork
   hack into a supported feature.
3. Vetro 0.4.0 (plugins/autoupdater/PIN) and whether dormant arn0-tv resumes.
4. For **this repo**: my PR #21 needs a maintainer ping; consider rebasing on
   top of #44's diagnostics changes if it lands.

---

## 9. Sources

- Upstream repo + issues/PRs: https://github.com/chris-ritsen/samsung-tv-root
  (README compat table, issues #1–#45 incl. PRs #21/#27/#36/#41/#42/#44/#45).
- Local clone `~/Projects/personal/samsung-tv-root` (2 commits ahead of
  upstream master at time of research; carries the S95B work of PR #21).
- r/Tizen threads: `1wl4e0n` (motodrizzle S90D launcher — archive-root route
  origin), `1x3cog3` + 10-31 roadmap post (Vetro).
- Downstream: andrearicci93/vetro, NelsonEU/arn0-tv, helalaou/AeroTizen,
  splitcipher/couchroot, invaliddayta/samsung-tv-tools,
  levkropp/samsung-tv-root-untethered, sebastianspicker/tiz0wn,
  Luminger/samsung-tv-usb-root, DemonKingSwarn/samsung-tv-tizen-root,
  Apps2Samsung/Apps2Samsung.
- GitHub REST API snapshots taken 2026-10-25, 2026-10-31, 2026-11-04.
