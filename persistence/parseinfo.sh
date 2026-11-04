#!/usr/bin/gawk -f
# ─────────────────────────────────────────────────────────────────────────────
# EXAMPLE drop-in replacement for /opt/etc/parseinfo.sh  (run by AT-HUB.service)
#
#   AT-HUB.service:
#     ExecStart=/opt/etc/parseinfo.sh &      # no User= -> root
#     WantedBy=starter.target                # runs every boot
#
# ⚠  THIS IS AN EXAMPLE. It is NOT installed anywhere. Review it, test it
#    reversibly (see ./README.md), and keep a backup of the stock file.
#
# WHY GAWK (not bash, not a binary):
#   * systemd execs this file directly; the kernel's #! handler then runs the
#     interpreter named on line 1.
#   * A `#!/bin/bash` script on writable /opt is UEP-checked by *bash's* userspace
#     uep_verify() -> "[uep][bash] the file is NOT signed!!" + exit.
#     /usr/bin/gawk has no such check (it does not link libkUEPUser.so).
#   * A native ELF here would be an unsigned binary on VDFS (writable /opt) and
#     would face the *kernel* UEP gate (SfdUepVerifyExecutableBinary). Avoid.
#   See  ../re/at-hub-persistence-analysis.md  and  ../docs/samsung-tv-secure-boot.md.
# ─────────────────────────────────────────────────────────────────────────────

BEGIN {
    LOG = "/opt/at-hub-boot.log"
    ts  = strftime("%Y-%m-%dT%H:%M:%S%z")

    # gawk's system() runs "/bin/sh -c <string>" — an inline command string, NOT a
    # script file. So bash's RW-script UEP check never fires for our own logic.
    system("echo '" ts " parseinfo(gawk): BEGIN' >> " LOG)

    # ── (1) PRESERVE THE STOCK BEHAVIOUR EXACTLY ─────────────────────────────
    # The original script's entire job is:
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

    # ── (2) LAUNCH DROPBEAR ──────────────────────────────────────────────────
    # The payload binary is UNSIGNED, so it is executed from /run (tmpfs,
    # SMACK System::Run) — the placement that is known to work. Executing the
    # same binary directly from /opt (VDFS, User::App::Shared) returned EPERM
    # (the UEP exec gate; NOT SMACK — `User` holds rwxat on both labels).
    #
    # Everything below runs via `sh -c` (no script file), so no script UEP check.
    launch = \
        "D=/run/tv-sshd; " \
        "S=/home/owner/share/tmp/sdk_tools/dropbear; " \
        "K=/opt/tv-sshd/dropbear_ed25519_host_key; " \
        "mkdir -p $D /opt/tv-sshd; " \
        "cp -f $S $D/dropbearmulti; chmod 755 $D/dropbearmulti; " \
        "ln -sf $D/dropbearmulti $D/dropbear; " \
        "ln -sf $D/dropbearmulti $D/dropbearkey; " \
        "[ -s $K ] || $D/dropbearkey -t ed25519 -f $K; chmod 600 $K; " \
        "[ -s /opt/tv-sshd/authorized_keys ] || echo 'MISSING authorized_keys' >&2; " \
        "/usr/bin/setsid $D/dropbear -D /opt/tv-sshd -r $K -s -p 22 " \
        "-P $D/dropbear.pid -E </dev/null >>$D/dropbear.log 2>&1 & " \
        "sleep 2; printf 'dropbear='; cat $D/dropbear.pid 2>/dev/null || printf 'none'; printf '\\n'"

    rc = system("(" launch ") >> " LOG " 2>&1")
    system("echo '" ts " parseinfo(gawk): launch rc=" rc "' >> " LOG)

    # ── (3) STAY RESIDENT ────────────────────────────────────────────────────
    # AT-HUB.service is Type=simple with the default KillMode=control-group:
    # when this ExecStart process exits, systemd tears down the whole cgroup and
    # kills the dropbear we just started. So block forever (nothing Requires=
    # AT-HUB, so keeping it "active" is harmless).
    while (1) system("sleep 300")
}
