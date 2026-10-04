//! The program's address, decided at build time (see `shonin-program-id`).
//! A program for tests only, never built for mainnet. It is the stand-in
//! venue's code replaced, so it is built at the stand-in venue's address.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "shonin_test_hostile_venue is never built for mainnet"
    );
    shonin_program_id::emit("shonin_local_venue");
}
