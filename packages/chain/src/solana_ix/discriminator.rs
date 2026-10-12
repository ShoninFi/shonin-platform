//! Each instruction's and account's discriminator, as Anchor makes it: the
//! first 8 bytes of `sha256("global:<handler>")` for an instruction and of
//! `sha256("account:<Type>")` for an account. Written out rather than hashed
//! at run time; the tests check each against the program's own.

// The vault program's instructions.
pub const ACTIVATE: [u8; 8] = [194, 203, 35, 100, 151, 55, 170, 82];
pub const RECORD_STATEMENT: [u8; 8] = [95, 31, 15, 135, 210, 14, 240, 115];
pub const REQUEST_DEPOSIT: [u8; 8] = [243, 202, 197, 215, 135, 97, 213, 109];
pub const REQUEST_REDEEM: [u8; 8] = [105, 49, 44, 38, 207, 241, 33, 173];
pub const SETTLE_CLAIM: [u8; 8] = [205, 203, 21, 66, 255, 231, 209, 155];
pub const CLAIM_INSTEAD: [u8; 8] = [200, 122, 155, 173, 62, 155, 240, 148];
pub const TAKE_PRO_RATA: [u8; 8] = [253, 153, 193, 248, 173, 162, 68, 207];
pub const TAKE_PRO_RATA_CLAIM: [u8; 8] = [224, 187, 162, 186, 238, 88, 176, 69];
pub const COLLECT_UNITS: [u8; 8] = [252, 155, 211, 122, 241, 25, 175, 129];
pub const REDEEM_CLOSED: [u8; 8] = [140, 145, 176, 135, 202, 47, 186, 219];
pub const CLAIM_CLOSED: [u8; 8] = [254, 56, 145, 210, 191, 133, 210, 86];
pub const SETTLE_CLOSED: [u8; 8] = [222, 130, 50, 42, 211, 141, 239, 243];
pub const COLLECT_HELD: [u8; 8] = [222, 246, 112, 28, 21, 142, 208, 242];
pub const CLOSE_REQUEST: [u8; 8] = [170, 46, 165, 120, 223, 102, 115, 2];
pub const CLOSE_CLAIM: [u8; 8] = [42, 177, 165, 35, 213, 179, 211, 19];
pub const CLOSE_HOLDER: [u8; 8] = [88, 22, 239, 228, 245, 64, 37, 78];
pub const FUND_TANK: [u8; 8] = [148, 141, 148, 80, 145, 14, 121, 127];
pub const KEEP: [u8; 8] = [243, 61, 149, 116, 246, 130, 184, 121];
pub const WITHDRAW_TANK: [u8; 8] = [217, 74, 105, 236, 47, 168, 186, 10];
pub const REQUEST_TANK_WITHDRAWAL: [u8; 8] = [142, 7, 128, 183, 152, 1, 101, 65];
pub const WITHDRAW_TANK_UNSIGNED: [u8; 8] = [23, 14, 30, 205, 116, 95, 37, 113];
pub const TOP_UP_FROM_SHARES: [u8; 8] = [233, 153, 197, 236, 181, 99, 193, 112];
pub const SET_TANK_KEEP: [u8; 8] = [28, 249, 212, 67, 104, 137, 160, 76];
pub const SET_ALLOWANCES: [u8; 8] = [188, 239, 124, 194, 195, 47, 229, 161];
pub const SET_IDLE_FLOOR: [u8; 8] = [195, 104, 134, 34, 161, 239, 240, 178];
pub const RETIRE: [u8; 8] = [44, 138, 153, 31, 222, 53, 21, 16];

// The publishing program's.
pub const SUBMIT: [u8; 8] = [88, 166, 102, 181, 162, 127, 170, 48];

// The vault program's accounts.
pub const RESTRICTED_ACCOUNT: [u8; 8] = [199, 149, 246, 4, 140, 61, 154, 126];
pub const FACTORY_ACCOUNT: [u8; 8] = [159, 68, 192, 61, 48, 249, 216, 202];
