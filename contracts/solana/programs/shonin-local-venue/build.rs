//! The program's address, decided at build time (see `shonin-program-id`).
//! The stand-in venue serves local chains and tests and is never built for mainnet.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "shonin_local_venue is never built for mainnet"
    );
    shonin_program_id::emit("shonin_local_venue");
}
