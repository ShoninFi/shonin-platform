//! The stand-in venue's driver, for local chains and tests.
//!
//! It is built on the same interface a real venue's driver is
//! (`shonin-driver`), so every path a real one takes runs here first: the
//! allowlist a station pins at creation, valuing the station's account at the
//! mark and at what a sale would fetch, the band checked on both sides of the
//! venue's own price, a leaver's fraction closed inside the band, and the
//! freed dollars swept home. It holds nothing and calls nothing; it reads the
//! venue's accounts and answers.
//!
//! **Its accounts**, after the station's authority at the venue (0) and that
//! authority's dollar account (1): the venue program, the venue's settings,
//! the venue's dollar account, the dollar's mint, the token program, the
//! authority's account at the venue, and then markets: every market that
//! account holds must be among them, each
//! once. Each is checked to be exactly the venue's account it stands for, so
//! nobody can value or close the station with a position left out or a
//! look-alike passed in. A market the account no longer holds may stay in the
//! list and is ignored: a close that flattens a position makes the account
//! forget its market, and the sweep that follows in the same transaction is
//! given the same accounts.
//!
//! Its reading and preparing ([`logic`]) take the venue's program as an
//! argument, so the same logic drives a copy of the stand-in venue deployed
//! at another address; this program drives the stand-in itself.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;
use anchor_lang::{Discriminator, InstructionData};
use shonin_driver::{
    Allowlist, Class, Close, Custody, Description, ExitPath, Meta, Own, Plan, Position, Prepared,
    Rule, Valuation, ValuationKind, BPS, MAX_RETURN_DATA, WAD,
};
use shonin_local_venue as venue;

include!(concat!(env!("OUT_DIR"), "/program_id.rs"));

/// The name `shonin.toml` writes for it.
pub const NAME: [u8; 32] = *b"local\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// Where each of its accounts stands among the ones it is given, counted from
/// the first after the authority's two.
pub mod layout {
    pub const VENUE_PROGRAM: usize = 0;
    pub const VENUE: usize = 1;
    pub const VAULT: usize = 2;
    pub const MINT: usize = 3;
    pub const TOKEN_PROGRAM: usize = 4;
    pub const ACCOUNT: usize = 5;
    pub const MARKETS: usize = 6;
}

/// Where each account the allowlist pins stands among its sets.
pub mod fixed {
    /// The venue's settings.
    pub const SETTINGS: u8 = 0;
    /// The venue's dollar account.
    pub const VAULT: u8 = 1;
}

/// The station's authority at the venue and its dollar account, which come
/// first.
pub const STATION: u8 = 0;
pub const STATION_DOLLARS: u8 = 1;

#[program]
pub mod shonin_local_driver {
    use super::*;

    /// The venue's program and the instructions a station may make there.
    pub fn allowlist(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::allowlist(&venue::ID))
    }

    /// The stand-in venue's terms exclude nobody; the vault's baseline still
    /// applies.
    pub fn restricted(_ctx: Context<Empty>) -> Result<()> {
        answer(&Vec::<[u8; 32]>::new())
    }

    pub fn describe(_ctx: Context<Empty>) -> Result<()> {
        answer(&logic::description())
    }

    pub fn value<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::valuation(&book, band_bps)?)
    }

    pub fn close<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        fraction: u64,
        band_bps: u16,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::closing(&book, fraction, band_bps, 2)?)
    }

    /// A stopped station's unwind of everything: the stand-in sells it all
    /// in one transaction, so the same as a close of it all.
    pub fn unwind<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        band_bps: u16,
    ) -> Result<()> {
        close(ctx, WAD, band_bps)
    }

    pub fn sweep<'info>(
        ctx: Context<'_, '_, 'info, 'info, Read<'info>>,
        amount: u64,
    ) -> Result<()> {
        let book = logic::read(
            &venue::ID,
            ctx.accounts.authority.key,
            ctx.remaining_accounts,
        )?;
        answer(&logic::sweep(&book, amount, 2))
    }
}

/// Leaves `value` as the program's answer, which must fit the return data.
pub fn answer(value: &impl AnchorSerialize) -> Result<()> {
    let bytes = value.try_to_vec()?;
    require!(bytes.len() <= MAX_RETURN_DATA, DriverError::TooLarge);
    set_return_data(&bytes);
    Ok(())
}

#[derive(Accounts)]
pub struct Empty {}

#[derive(Accounts)]
pub struct Read<'info> {
    /// CHECK: the station's authority at the venue, whose account there is
    /// derived from its address.
    pub authority: UncheckedAccount<'info>,
    /// CHECK: the authority's dollar account, where a sweep sends the
    /// dollars; the venue itself checks the authority owns it.
    pub authority_dollars: UncheckedAccount<'info>,
}

/// The driver's reading and preparing, apart from its entrypoint, so a test
/// driver can wrap it.
pub mod logic {
    use super::*;

    /// What the station's authority holds at the venue, read from accounts
    /// each checked to be the one it stands for.
    pub struct Book {
        pub account: Option<venue::MarginAccount>,
        /// The market of each position the account holds, in its order.
        pub markets: Vec<venue::Market>,
        /// How many market accounts were given, held or not.
        pub given: usize,
    }

    impl Book {
        fn value_at(&self, kind: venue::PriceKind) -> Result<u64> {
            let Some(account) = &self.account else {
                return Ok(0);
            };
            let value = account.value_at(&self.markets, kind)?;
            if value <= 0 {
                return Ok(0);
            }
            u64::try_from(value).map_err(|_| error!(DriverError::Overflow))
        }
    }

    /// Reads `authority`'s account at the venue deployed at `program` from
    /// `accounts`, laid out as [`layout`] says. Only the account is read: the
    /// authority's dollar account holds nothing of the station's between its
    /// calls.
    pub fn read(program: &Pubkey, authority: &Pubkey, accounts: &[AccountInfo]) -> Result<Book> {
        use layout::*;
        require!(accounts.len() >= MARKETS, DriverError::WrongAccounts);
        let is = |at: usize, key: &Pubkey| accounts[at].key == key;
        // An account the venue made is checked at the address its own bump
        // derives, which is one derivation, rather than searched for: what a
        // driver costs then doesn't depend on the addresses it was given.
        let derived = |seeds: &[&[u8]]| {
            Pubkey::create_program_address(seeds, program)
                .map_err(|_| error!(DriverError::WrongAccounts))
        };
        require!(is(VENUE_PROGRAM, program), DriverError::WrongAccounts);
        let settings = {
            let info = &accounts[VENUE];
            require_keys_eq!(*info.owner, *program, DriverError::WrongAccounts);
            let data = info.try_borrow_data()?;
            venue::Venue::try_deserialize(&mut &data[..])?
        };
        require!(
            is(VENUE, &derived(&[venue::VENUE_SEED, &[settings.bump]])?)
                && is(VAULT, &settings.vault)
                && is(MINT, &settings.mint)
                && is(TOKEN_PROGRAM, &anchor_spl::token::ID),
            DriverError::WrongAccounts
        );
        let info = &accounts[ACCOUNT];
        let account = if info.owner == program && !info.data_is_empty() {
            let data = info.try_borrow_data()?;
            let account = venue::MarginAccount::try_deserialize(&mut &data[..])?;
            require!(
                account.master == *authority
                    && is(
                        ACCOUNT,
                        &derived(&[venue::ACCOUNT_SEED, authority.as_ref(), &[account.bump]])?
                    ),
                DriverError::WrongAccounts
            );
            Some(account)
        } else {
            // None opened yet: it must be where one would be.
            require!(
                is(ACCOUNT, &venue::account_address_of(program, authority)),
                DriverError::WrongAccounts
            );
            None
        };
        // Every market given is one of the venue's, at its own address, and
        // given once; every market the account holds is among them. One left
        // out is refused, not valued at nothing.
        let given = &accounts[MARKETS..];
        let mut loaded: Vec<venue::Market> = Vec::with_capacity(given.len());
        for info in given {
            let market = venue::load_market_of(program, info)?;
            require!(
                !loaded.iter().any(|seen| seen.id == market.id),
                DriverError::WrongAccounts
            );
            loaded.push(market);
        }
        let held = account.as_ref().map_or(&[][..], |a| &a.positions[..]);
        let mut markets = Vec::with_capacity(held.len());
        for position in held {
            let market = loaded
                .iter()
                .find(|market| market.id == position.market)
                .ok_or(DriverError::WrongAccounts)?;
            markets.push(market.clone());
        }
        Ok(Book {
            account,
            markets,
            given: given.len(),
        })
    }

    /// The instructions a station may make at the venue deployed at
    /// `program`, with the venue's settings and its dollar account pinned
    /// wherever they stand. Its markets are left open: only the venue's
    /// operator lists one.
    pub fn allowlist(program: &Pubkey) -> Allowlist {
        let own = |index: u8, own: Own| Position { index, own };
        let account = || Own::StationAddress(venue::ACCOUNT_SEED.to_vec());
        let settings = || Own::Fixed(fixed::SETTINGS);
        let vault = || Own::Fixed(fixed::VAULT);
        Allowlist {
            venue: *program,
            fixed: vec![
                vec![venue::venue_address_of(program)],
                vec![venue::vault_address_of(program)],
            ],
            rules: vec![
                // payer, master, account, system program
                Rule {
                    discriminator: venue::instruction::OpenAccount::DISCRIMINATOR.to_vec(),
                    class: Class::Trade,
                    positions: vec![own(1, Own::Station), own(2, account())],
                },
                // master, account, payer
                Rule {
                    discriminator: venue::instruction::CloseAccount::DISCRIMINATOR.to_vec(),
                    class: Class::Exit,
                    positions: vec![own(0, Own::Station), own(1, account())],
                },
                // master, account, master's dollars, settings, vault, ...
                Rule {
                    discriminator: venue::instruction::Deposit::DISCRIMINATOR.to_vec(),
                    class: Class::Trade,
                    positions: vec![
                        own(0, Own::Station),
                        own(1, account()),
                        own(2, Own::StationDollars),
                        own(3, settings()),
                        own(4, vault()),
                    ],
                },
                Rule {
                    discriminator: venue::instruction::Withdraw::DISCRIMINATOR.to_vec(),
                    class: Class::Exit,
                    positions: vec![
                        own(0, Own::Station),
                        own(1, account()),
                        own(2, Own::StationDollars),
                        own(3, settings()),
                        own(4, vault()),
                    ],
                },
                // master, account, settings, markets ...
                Rule {
                    discriminator: venue::instruction::Place::DISCRIMINATOR.to_vec(),
                    class: Class::Trade,
                    positions: vec![own(0, Own::Station), own(1, account()), own(2, settings())],
                },
            ],
        }
    }

    pub fn description() -> Description {
        Description {
            name: NAME,
            custody: Custody::Station,
            exit_path: ExitPath::Contract,
            valuation: ValuationKind::OnChain,
        }
    }

    /// Whether a market is no place to close a position on that side: its
    /// best price is worse than the band from the venue's own price, or its
    /// mark, bid or ask sits outside the band on either side. A valuation
    /// there would price everyone at a figure nobody trusts, and a bid pushed
    /// up would charge a deposit a premium nobody could sell at. At the full
    /// band, no market ever is.
    pub fn outside(market: &venue::Market, is_long: bool, band_bps: u16) -> bool {
        let band = band_bps as u64;
        if band >= BPS {
            return false;
        }
        let low = shonin_driver::mul_div_up(market.oracle, BPS - band, BPS);
        let high = shonin_driver::mul_div(market.oracle, BPS + band, BPS);
        let best = if is_long {
            market.bid < low
        } else {
            market.ask > high
        };
        best || [market.mark, market.bid, market.ask]
            .iter()
            .any(|price| *price < low || *price > high)
    }

    pub fn valuation(book: &Book, band_bps: u16) -> Result<Valuation> {
        let outside = book
            .account
            .iter()
            .flat_map(|account| account.positions.iter())
            .zip(&book.markets)
            .any(|(position, market)| outside(market, position.size > 0, band_bps));
        Ok(Valuation {
            mark: book.value_at(venue::PriceKind::Mark)?,
            exit: book.value_at(venue::PriceKind::Exit)?,
            outside,
        })
    }

    /// The answer to `close`: the plan, with the account valued at the same
    /// band from the same book the plan was prepared from.
    pub fn closing(book: &Book, fraction: u64, band_bps: u16, base: u8) -> Result<Close> {
        Ok(Close {
            value: valuation(book, band_bps)?,
            plan: close(book, fraction, band_bps, base)?,
        })
    }

    /// Closes `fraction` of every position, each no worse than the band from
    /// the venue's own price: one reduce-only order per market, a long sold
    /// at the bid and a short bought back at the ask. Nothing, and `outside`,
    /// when any market is outside the band. `base` is where the driver's own
    /// accounts start among those it was given.
    pub fn close(book: &Book, fraction: u64, band_bps: u16, base: u8) -> Result<Plan> {
        let Some(account) = &book.account else {
            return Ok(Plan::default());
        };
        let band = band_bps as u64;
        let mut orders = Vec::with_capacity(account.positions.len());
        for (position, market) in account.positions.iter().zip(&book.markets) {
            let is_long = position.size > 0;
            let open = position.size.unsigned_abs();
            let size = fraction_of(open, fraction).min(open);
            if size == 0 {
                continue;
            }
            // The full band accepts any price: a stopped station that has
            // waited long enough sells at whatever the market offers.
            let limit_price = if band >= BPS {
                if is_long {
                    0
                } else {
                    u64::MAX
                }
            } else {
                if outside(market, is_long, band_bps) {
                    return Ok(Plan {
                        outside: true,
                        ..Plan::default()
                    });
                }
                if is_long {
                    shonin_driver::mul_div_up(market.oracle, BPS - band, BPS)
                } else {
                    shonin_driver::mul_div(market.oracle, BPS + band, BPS)
                }
            };
            orders.push(venue::OrderRequest {
                market: position.market,
                is_buy: !is_long,
                size,
                limit_price,
                reduce_only: true,
            });
        }
        if orders.is_empty() {
            return Ok(Plan::default());
        }
        let at = |offset: usize| base + offset as u8;
        let mut accounts = vec![
            Meta {
                index: STATION,
                writable: false,
            },
            Meta {
                index: at(layout::ACCOUNT),
                writable: true,
            },
            Meta {
                index: at(layout::VENUE),
                writable: false,
            },
        ];
        accounts.extend((0..book.given).map(|i| Meta {
            index: at(layout::MARKETS + i),
            writable: false,
        }));
        // One order per market, each of whose proceeds the venue rounds down.
        let count = orders.len() as u8;
        Ok(Plan {
            outside: false,
            instructions: vec![Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts,
                data: venue::instruction::Place { orders }.data(),
            }],
            orders: count,
        })
    }

    /// `ceil(open * fraction / WAD)`, worked in two parts so that no product
    /// can outgrow 128 bits: the whole units of `WAD`, then the remainder.
    pub fn fraction_of(open: u128, fraction: u64) -> u128 {
        let (wad, fraction) = (WAD as u128, fraction as u128);
        (open / wad) * fraction + ((open % wad) * fraction).div_ceil(wad)
    }

    /// Brings `amount` of the account's dollars back to the authority's dollar
    /// account, from where the station takes them home, after a close has
    /// freed them, or as much cash as the account has.
    pub fn sweep(book: &Book, amount: u64, base: u8) -> Plan {
        let cash = book.account.as_ref().map_or(0, |account| account.cash);
        if cash <= 0 {
            return Plan::default();
        }
        let amount = (amount as u128).min(cash as u128) as u64;
        if amount == 0 {
            return Plan::default();
        }
        let at = |offset: usize| base + offset as u8;
        let mut accounts = vec![
            Meta {
                index: STATION,
                writable: false,
            },
            Meta {
                index: at(layout::ACCOUNT),
                writable: true,
            },
            Meta {
                index: STATION_DOLLARS,
                writable: true,
            },
            Meta {
                index: at(layout::VENUE),
                writable: false,
            },
            Meta {
                index: at(layout::VAULT),
                writable: true,
            },
            Meta {
                index: at(layout::MINT),
                writable: false,
            },
            Meta {
                index: at(layout::TOKEN_PROGRAM),
                writable: false,
            },
        ];
        accounts.extend((0..book.given).map(|i| Meta {
            index: at(layout::MARKETS + i),
            writable: false,
        }));
        Plan {
            outside: false,
            instructions: vec![Prepared {
                program: at(layout::VENUE_PROGRAM),
                accounts,
                data: venue::instruction::Withdraw { amount }.data(),
            }],
            orders: 0,
        }
    }
}

#[error_code]
pub enum DriverError {
    #[msg("An account is not the venue's account it stands for, or one is missing")]
    WrongAccounts,
    #[msg("The answer is larger than the return data holds")]
    TooLarge,
    #[msg("Arithmetic overflow")]
    Overflow,
}
