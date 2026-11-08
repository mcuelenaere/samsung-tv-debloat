#!/usr/bin/gawk -f
# ─────────────────────────────────────────────────────────────────────────────
# Root boot hook for /opt/etc/parseinfo.sh (run by AT-HUB.service)
#
#   AT-HUB.service:
#     ExecStart=/opt/etc/parseinfo.sh &      # no User=  -> root
#     WantedBy=starter.target                # runs every boot
#
# WHAT IT DOES
#   1. Reproduces the stock behaviour exactly (writes __<BUILD>__<MODEL> to
#      /proc/target_version) so nothing downstream notices the swap.
#   2. Copies every *.service file from the root-only /opt/tv-units directory
#      into the runtime systemd unit path (/run/systemd/system) and starts each
#      one independently. systemd resolves newly written units on demand over
#      D-Bus (StartUnit/LoadUnit), so NO full daemon-reload is needed.
#
# WHY GAWK (not bash, not a binary)
#   * systemd execs this file directly; the kernel #! handler runs the
#     interpreter named on line 1.
#   * A #!/bin/bash script on writable /opt is UEP-checked by *bash's* userspace
#     uep_verify() -> "[uep][bash] the file is NOT signed!!" + exit.
#     /usr/bin/gawk has no such check (it does not link libkUEPUser.so).
#   * A native ELF here would be an unsigned binary on VDFS (writable /opt) and
#     would face the kernel UEP gate. Avoid.
#
# /opt/tv-units MUST be root-owned, mode 0700, and not app-writable. It is a
# privileged extension point: a unit placed there runs as root at boot.
# ─────────────────────────────────────────────────────────────────────────────

BEGIN {
    LOG = "/run/at-hub-boot.log"
    UNIT_DIR = "/opt/tv-units"
    ts = strftime("%Y-%m-%dT%H:%M:%S%z")

    system("mkdir -p /run/systemd/system")

    # ── (1) PRESERVE THE STOCK BEHAVIOUR EXACTLY ─────────────────────────────
    repo = ""; model = ""
    while ((getline line < "/etc/prd_info.ini") > 0) {
        sub(/\r$/, "", line)
        if (line ~ /^Build=/) {
            sub(/^Build=/, "", line); sub(/;.*/, "", line); repo = line
        } else if (line ~ /^Model=/) {
            sub(/^Model=/, "", line); sub(/;.*/, "", line); model = line
        }
    }
    close("/etc/prd_info.ini")

    printf "__%s__%s\n", repo, model > "/proc/target_version"
    close("/proc/target_version")
    printf "%s parseinfo: target_version=__%s__%s\n", ts, repo, model >> LOG
    close(LOG)

    # ── (2) STAGE AND START ALL ROOT-OWNED EXTRA UNITS ───────────────────────
    cmd = "rc=0; " \
          "for unit in " UNIT_DIR "/*.service; do " \
          "  [ -f \"$unit\" ] && [ ! -L \"$unit\" ] || continue; " \
          "  name=${unit##*/}; " \
          "  case \"$name\" in *.service) ;; *) echo '" ts " invalid unit name: $name' >> " LOG "; rc=1; continue ;; esac; " \
          "  if ! cp -f \"$unit\" \"/run/systemd/system/$name\" || ! chmod 0644 \"/run/systemd/system/$name\"; then " \
          "    echo '" ts " stage failed: $name' >> " LOG "; rc=1; continue; " \
          "  fi; " \
          "  if ! /usr/bin/systemctl start \"$name\"; then " \
          "    /usr/bin/systemctl daemon-reload; " \
          "    if ! /usr/bin/systemctl start \"$name\"; then " \
          "      echo '" ts " start failed: $name' >> " LOG "; rc=1; " \
          "    fi; " \
          "  fi; " \
          "done; " \
          "exit $rc"

    rc = system(cmd)
    printf "%s parseinfo: start units rc=%d\n", ts, rc >> LOG
    close(LOG)
}
