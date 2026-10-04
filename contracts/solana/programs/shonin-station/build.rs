//! The program's address, decided at build time so that no local or devnet
//! address is ever committed: the committed `mainnet-id` with the `mainnet`
//! feature, otherwise the local deploy keypair's (see `shonin-program-id`).

fn main() {
    shonin_program_id::emit("shonin_station");
}
