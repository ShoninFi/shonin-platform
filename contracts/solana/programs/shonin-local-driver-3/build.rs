//! The program's address, decided at build time (see `shonin-program-id`).
//! A test driver serves tests only and is never built for mainnet.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "shonin_local_driver_3 is never built for mainnet"
    );
    shonin_program_id::emit("shonin_local_driver_3");
}
