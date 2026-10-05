//! The program's address, decided at build time so that no local address is
//! ever committed (see `shonin-program-id`).

fn main() {
    shonin_program_id::emit("shonin_publishing");
}
