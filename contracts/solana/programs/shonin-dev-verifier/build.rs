//! The program's address, decided at build time (see `shonin-program-id`).
//! There is no mainnet build: a development checker trusts one key instead of
//! the hardware, and has no place on a chain that holds anyone's money.

fn main() {
    assert!(
        std::env::var_os("CARGO_FEATURE_MAINNET").is_none(),
        "the development attestation checker is never built for mainnet"
    );
    shonin_program_id::emit("shonin_dev_verifier");
}
