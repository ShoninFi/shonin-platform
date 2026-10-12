//! The program's address, decided at build time so that no local address is
//! ever committed: the committed `mainnet-id` or `devnet-id` with the
//! `mainnet` or `devnet` feature, otherwise the local deploy keypair's (see
//! `shonin-program-id`).

fn main() {
    shonin_program_id::emit("shonin_orca_driver");
}
