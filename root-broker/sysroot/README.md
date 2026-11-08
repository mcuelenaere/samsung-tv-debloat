# Minimal TV sysroot

`fetch-tv-sysroot.sh` copies the small set of ARMv7 shared libraries directly referenced by the Rust/EFL UI and spike into the ignored `tv-6.5/` directory.

The TV has no native headers or `pkg-config` records, so this is intentionally a link/ABI sysroot, not a complete SDK. Manual FFI declarations remain based on the public headers documented by the spike. Transitive EFL dependencies are resolved by the TV at runtime; local inspection of the copied direct libraries establishes that the required symbols and SONAMEs exist.

Collected target: `T-PTMDEUC-REL-202602250226`. The direct libraries are ARM
EABI5 **soft-float** (`0x5000200`), use SONAMEs matching the expected unversioned
link names, export every function declared by the EFL spike, and require at most
`GLIBC_2.29`; the TV supplies glibc 2.30. The script verifies each copied file's
recorded SHA-256.

The native approval UI additionally links `libecore.so.1.25.1`
(`ecore_timer_add`) and `libecore_wl2.so.1.25.1`
(`ecore_wl2_window_type_set`), collected with the same checksums
(`49727cc5…` for ecore and `304b5ca9…` for ecore_wl2).

The generated `tv-6.5/` tree is ignored and must not be committed. It contains
no keys or credentials. Copying these files is a read-only operation on the
device. Do not run the script unless the user has explicitly authorized
read-only TV access.
