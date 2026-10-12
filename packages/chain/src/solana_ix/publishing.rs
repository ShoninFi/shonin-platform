//! Paying to publish: the author's one instruction in publishing. The build
//! service registers the strategy and the reviewer reviews it; the author
//! pays the fee and names the upload.

use super::discriminator as d;
use super::{Instruction, Pubkey, address, data, ids, r, w, ws};

/// The author pays publishing's fee from its own dollar account to the
/// platform's, for the upload whose hash is `upload`, and so takes the
/// strategy id `next` (publishing's `count` plus one, read from its config:
/// the submission's address names it, so a payment racing another fails
/// rather than taking a different id). `mint`, `token_program` and
/// `platform` are publishing config's.
pub fn submit(
    author: &Pubkey,
    upload: [u8; 32],
    next: u64,
    mint: &Pubkey,
    token_program: &Pubkey,
    platform: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: ids::PUBLISHING,
        accounts: vec![
            ws(*author),
            w(address::publishing_config()),
            w(address::submission(next)),
            r(*mint),
            w(address::associated_token(author, mint, token_program)),
            w(address::associated_token(platform, mint, token_program)),
            r(*token_program),
            r(ids::ASSOCIATED_TOKEN),
            r(ids::SYSTEM),
        ],
        data: data(d::SUBMIT, &upload),
    }
}
