//! The program's address, decided at build time so that no local or devnet
//! address is ever committed (see `shonin-program-id`).

fn main() {
    shonin_program_id::emit("shonin_profiles");
}
