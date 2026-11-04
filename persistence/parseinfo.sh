#!/usr/bin/gawk -f
# ─────────────────────────────────────────────────────────────────────────────
# EXAMPLE drop-in replacement for /opt/etc/parseinfo.sh  (run by AT-HUB.service)
#
#   AT-HUB.service:
#     ExecStart=/opt/etc/parseinfo.sh &      # no User=  -> root
#     WantedBy=starter.target                # runs every boot
#
# ⚠  THIS IS AN EXAMPLE. It is NOT installed anywhere. Review it, test it
#    reversibly (see ./README.md), and keep a backup of the stock file.
#
# WHAT IT DOES
#   1. Reproduces the stock behaviour exactly (writes __<BUILD>__<MODEL> to
#      /proc/target_version) so nothing downstream notices the swap.
#   2. Injects ./dropbear.service into the runtime systemd unit path
#      (/run/systemd/system), then `daemon-reload` + `restart`. systemd (PID 1)
#      then owns dropbear: spawning, respawn (Restart=always), its own cgroup,
#      and logging. The hook itself can exit (no keep-alive needed).
#
# WHY GAWK (not bash, not a binary)
#   * systemd execs this file directly; the kernel #! handler runs the
#     interpreter named on line 1.
#   * A `#!/bin/bash` script on writable /opt is UEP-checked by *bash's* userspace
#     uep_verify() -> "[uep][bash] the file is NOT signed!!" + exit.
#     /usr/bin/gawk has no such check (it does not link libkUEPUser.so).
#   * A native ELF here would be an unsigned binary on VDFS (writable /opt) and
#     would face the *kernel* UEP gate. Avoid.
#   See  ../re/at-hub-persistence-analysis.md  and  ../docs/samsung-tv-secure-boot.md.
#
# WHY THIS UNIT/SYSTEMD APPROACH IS FEASIBLE (validated on-device 2026-11-04)
#   * The TV already creates transient units at runtime (Samsung's
#     tv-restart-sdbd.*), and the systemd D-Bus policy lets uid 0 do anything
#     (no polkit). A live test wrote a unit to /run/systemd/system, did
#     `daemon-reload` + `start`, logged in on an alternate port as uid 0, and
#     confirmed systemd respawned it after kill. See ./README.md.
#
# NOTE the payload is still an unsigned ELF, so the unit's ExecStart execs it
# from /run (tmpfs) — the placement UEP permits; /opt (VDFS) exec returns EPERM.
# ─────────────────────────────────────────────────────────────────────────────

BEGIN {
    LOG = "/opt/at-hub-boot.log"
    ts  = strftime("%Y-%m-%dT%H:%M:%S%z")

    # gawk's system() runs "/bin/sh -c <string>" — an inline command string, NOT a
    # script FILE, so bash's RW-script UEP check never fires for our own logic.
    system("echo '" ts " parseinfo(gawk): BEGIN' >> " LOG)

    # ── (1) PRESERVE THE STOCK BEHAVIOUR EXACTLY ─────────────────────────────
    # Original script body:
    #   Repoinfo=$(grep "Build=" /etc/prd_info.ini | cut -d "=" -f2 | cut -d";" -f1)
    #   REPO=${Repoinfo%*;}
    #   Modelinfo=$(grep "Model=" /etc/prd_info.ini | cut -d "=" -f2 | cut -d";" -f1)
    #   MODEL=${Modelinfo%*;}
    #   echo "__${REPO}__${MODEL}" > /proc/target_version
    # Replicate it byte-for-byte so nothing downstream notices the swap.
    repo = ""; model = ""
    while ((getline line < "/etc/prd_info.ini") > 0) {
        if (line ~ /^Build=/) {
            sub(/^Build=/, "", line); sub(/;.*/, "", line); repo  = line
        } else if (line ~ /^Model=/) {
            sub(/^Model=/, "", line); sub(/;.*/, "", line); model = line
        }
    }
    close("/etc/prd_info.ini")

    printf "__%s__%s\n", repo, model > "/proc/target_version"
    close("/proc/target_version")
    system("echo '" ts " parseinfo(gawk): target_version=__" repo "__" model "' >> " LOG)

    # ── (2) INJECT THE DROPBEAR UNIT AND LET SYSTEMD OWN IT ──────────────────
    # The unit (./dropbear.service -> /opt/systemd/dropbear.service) has an
    # ExecStartPre that stages the unsigned binary onto /run and generates the
    # host key; ExecStart runs it from /run on port 22.
    system("mkdir -p /run/systemd/system")

    rc = system("/bin/cp -f /opt/systemd/dropbear.service /run/systemd/system/dropbear.service")
    system("echo '" ts " parseinfo(gawk): stage unit rc=" rc "' >> " LOG)

    rc = system("/usr/bin/systemctl daemon-reload")
    system("echo '" ts " parseinfo(gawk): daemon-reload rc=" rc "' >> " LOG)

    rc = system("/usr/bin/systemctl restart dropbear.service")
    system("echo '" ts " parseinfo(gawk): start dropbear rc=" rc "' >> " LOG)

    # ── (3) DONE ─────────────────────────────────────────────────────────────
    # No keep-alive: dropbear now lives in its own unit/cgroup owned by PID 1, so
    # it is unaffected by AT-HUB exiting. (Stock AT-HUB also exits after a few s.)
}
