//! The program's address, decided at build time (see `shonin-program-id`).
//! A program for tests only, never built for mainnet.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "shonin_test_vault is never built for mainnet"
    );
    shonin_program_id::emit("shonin_test_vault");
}
