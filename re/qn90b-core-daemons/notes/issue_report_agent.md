# issue_report_agent — RE note

- **Path / unit:** `/bin/issue_report_agent` — `issue_report_agent.service`
- **Size / stripped:** 1197 KB, stripped: **y**  ·  **Upstream?** Samsung-proprietary (`AT-HUB-1.8`, namespace `ira::`); heavy C++ (boost, `deviceinfo`)
- **Runs as / SMACK label:** **root** (no `User=`, no `SmackProcessLabel=`). `WantedBy=starter.target.wants` **and** `vdtizen-bootseq-PNP-starter.target.wants` (symlinked into TV boot sequence). `Restart=always`.

## What it does (2–4 lines)
Crash / issue-report collector ("IRA"). On a fault (or a remote-key combo, or syspopup/AppStatus/power-off D-Bus signals) it runs a policy engine (`TCPolicyManager`, XML policies in `/opt/usr/share/dlog/*.xml`, `tvs-ht/data/rmpolicy.xml`, `hitron_ira_policy.xml`) that decides which logs to grab: coredumps, VLogs, HitComp dumps, HT dumps, kernel emergency klog, `vdfs.dump.tgz`, streamline. It encrypts the dumps and **uploads them to a server** (or writes them to an inserted USB stick, gated by a security file).

## 1. Persistence surface — minor leads
- Executes helper **shell scripts as root**: `ira_encrypt.sh`, `/opt/usr/share/HitComp/Hitcomp_buffer.sh`, plus many `system()`/`popen` `rm -rf`, `/bin/touch`, `/bin/echo`, `launch_app com.samsung.hitcomp DUMP`. Via `CUtil::exec_cmd`. **All targets are fixed `/opt/usr/share` / `/opt/data` rootfs paths** → root-exec-of-writable only if that overlay is writable on this set (worth confirming; if `/opt/usr/share/HitComp/` is writable, dropping a trojan `Hitcomp_buffer.sh` = root at next fault).
- **USB dump activation** is gated: requires a security file on the USB whose content matches the device **HWDUID** (`"Security Mismatch, Don't activate USB dump"`), so not arbitrary.
- Reads policy XML and `save_folder_path`/`send_issue_files` from vconf, not from writable exec content. No boot-time exec of attacker content found.

## 2. UEP / signature enforcement
No code-signing/UEP gate. `head_verify` / `verify_log_buff` / `"can not verify ermg klog body"` = integrity check of the *emergency klog it ingests*, not of executables. `encrypt_corefile` / `CSSMMode::encryptVlog` = encrypt outbound dumps (privacy of upload), skipped on debug/perf images and on 32-bit (`"System is a 32Bit machine. Not encrypting core file"`).

## 3. SMACK / privilege
Runs **root**. `setSmackPermissions` → shells `chsmack -a "User::App::Shared"` on its work dirs (so apps can hand it files). Large root shell-exec surface, but command strings are constructed from fixed paths + policy data, not raw external input.

## 4. Debloat — **uploads crash data; safe to mask**
- **Collects/transmits:** core/VLog/HitComp/HT/kernel-emergency dumps per fault policy, encrypted. Uploaders: `CFileSenderSSM::uploadFiles`, `CFileSenderSmast::sendToWebServer`; log `"file collector send to server (MAX 15 minute)"`, `"upload info url: %s"`, `"Curl Sending on pc command = %s"`. Uses `upload_dod` / `no_upload_dod` marker files per folder.
- **WHERE:** upload URL is **dynamic** (`"upload info url: %s"` — resolved at runtime from vconf/policy, e.g. `db/aub/send_aub_files_path`, `db/comss/*`; no hardcoded host in the binary). Identifiers gathered: `db/comss/hwduid`, `modelid`, `countrycode`, factory KPI. Alternate sink = USB stick.
- **What breaks if disabled:** automatic crash-dump capture & upload, remote-management debug-log collection (`rmh_setdebug`), the user key-combo "capture an issue report" feature, USB log dumps. Nothing in normal TV viewing. This is a pure telemetry/field-diagnostics daemon → **prime debloat target**.
- **Marker trick:** unit has **no `ConditionPathExists`**. Soft-neuter possible via vconf `db/issue_report_agent/send_issue_files=0`, but it's symlinked into two boot-seq targets, so mask is cleaner.
- **Recommendation:** `systemctl mask issue_report_agent.service` (persistent, on overlay/rootfs). Remove/empty the `*.wants` symlinks under `starter.target.wants` and `vdtizen-bootseq-PNP-starter.target.wants`. Eliminates a large root shell-exec surface too.

## Leads worth a deeper look
- Is `/opt/usr/share/HitComp/` (holds `Hitcomp_buffer.sh`, `ira_encrypt.sh`) writable on this SET? If yes → root persistence via script replacement.
- `CFileSenderSmast::sendToWebServer` / `CFileSenderSSM::uploadFiles` — resolve the actual upload host + what device IDs go with the dump.
- `key_combo_cb` (`IRAIssueReportAgent.h:86`) — which remote key combo triggers a report (useful/annoying to know).
