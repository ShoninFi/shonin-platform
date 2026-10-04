//! The program's address, decided at build time (see `shonin-program-id`).
//! A copy of the stand-in venue serves tests only and is never built for mainnet.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "shonin_local_venue_2 is never built for mainnet"
    );
    shonin_program_id::emit("shonin_local_venue_2");
}
