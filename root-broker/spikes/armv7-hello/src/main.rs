#[cfg(not(all(target_arch = "arm", target_env = "musl")))]
compile_error!("armv7-hello must be built for ARM musl");

fn main() {
    println!("armv7-hello: static ARMv7 musl Rust smoke test");
}
