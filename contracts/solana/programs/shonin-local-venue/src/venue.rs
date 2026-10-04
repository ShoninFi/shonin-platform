use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};
use shonin_common::loader;

/// The venue's settings: one per deployment.
pub const VENUE_SEED: &[u8] = b"venue";
/// The venue's dollars, held for every account.
pub const VAULT_SEED: &[u8] = b"vault";
/// One market, by its id.
pub const MARKET_SEED: &[u8] = b"market";
/// One master's account.
pub const ACCOUNT_SEED: &[u8] = b"account";

/// Sizes carry 18 decimals.
pub const SIZE_SCALE: i128 = 1_000_000_000_000_000_000;
pub const BPS: i128 = 10_000;
/// The most markets one account holds at once, and the most orders one
/// placement carries: an account's whole book has to be read by whoever
/// values it, inside one instruction.
pub const MAX_HELD: usize = 8;
pub const MAX_ORDERS: usize = 8;
/// What a withdrawal that keeps an account's margin ratio may round by: four
/// dollar-units per market it may hold. Each market's order rounds its
/// dollars, and its position's value at the mark rounds, by less than a unit
/// each (two per market), and its notional in the gross figure the ratio is
/// measured against rounds by less than one more (three per market). Four per
/// market covers that with room, so a leaver's close of part of an account
/// keeps its ratio wherever the full close's would.
pub const RATIO_ROUNDING: i128 = 4 * MAX_HELD as i128;

#[program]
pub mod shonin_local_venue {
    use super::*;

    /// Sets the venue up, once, signed by the key that deployed it.
    pub fn initialize(ctx: Context<Initialize>, operator: Pubkey, margin_bps: u64) -> Result<()> {
        require!(
            loader::is_upgrade_authority(
                &ctx.accounts.program,
                Some(&ctx.accounts.program_data),
                ctx.accounts.deployer.key,
            ),
            VenueError::NotTheDeployer
        );
        ctx.accounts.venue.set_inner(Venue {
            operator,
            mint: ctx.accounts.mint.key(),
            vault: ctx.accounts.vault.key(),
            margin_bps,
            bump: ctx.bumps.venue,
            vault_bump: ctx.bumps.vault,
        });
        Ok(())
    }

    // Markets and prices --------------------------------------------------

    pub fn list_market(
        ctx: Context<ListMarket>,
        id: [u8; 32],
        is_perp: bool,
        price: u64,
    ) -> Result<()> {
        require!(id != [0; 32], VenueError::NoSuchMarket);
        ctx.accounts.market.set_inner(Market {
            id,
            is_perp,
            mark: price,
            oracle: price,
            bid: price,
            ask: price,
            funding_index: 0,
            funding_rate: 0,
            funding_updated_at: now()?,
            bump: ctx.bumps.market,
        });
        emit!(PricesSet {
            market: id,
            mark: price,
            oracle: price,
            bid: price,
            ask: price,
        });
        Ok(())
    }

    pub fn set_prices(ctx: Context<Operate>, quote: Quote) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require!(market.id == quote.market, VenueError::NoSuchMarket);
        market.set_prices(&quote);
        Ok(())
    }

    /// Longs pay shorts `paid_per_unit` dollars for each unit of size held.
    pub fn accrue_funding(ctx: Context<Operate>, paid_per_unit: i128) -> Result<()> {
        ctx.accounts.market.accrue(paid_per_unit)
    }

    /// Publish what longs are paying shorts, per unit of size per day.
    pub fn set_funding(ctx: Context<Operate>, rate_per_day: i128) -> Result<()> {
        ctx.accounts.market.set_rate(rate_per_day);
        Ok(())
    }

    /// Everything a market operator has to say in one instruction: the
    /// markets it names follow as writable remaining accounts, each once, in
    /// any order.
    pub fn post<'info>(
        ctx: Context<'_, '_, 'info, 'info, Post<'info>>,
        quotes: Vec<Quote>,
        funding: Vec<Funding>,
    ) -> Result<()> {
        for info in ctx.remaining_accounts {
            let mut market = load_market(info)?;
            let id = market.id;
            for quote in quotes.iter().filter(|quote| quote.market == id) {
                market.set_prices(quote);
            }
            for entry in funding.iter().filter(|entry| entry.market == id) {
                market.set_rate(entry.rate_per_day);
                market.accrue(entry.paid_per_unit)?;
            }
            require!(info.is_writable, VenueError::NoSuchMarket);
            let mut data = info.try_borrow_mut_data()?;
            market.try_serialize(&mut &mut data[..])?;
        }
        Ok(())
    }

    // Accounts ------------------------------------------------------------

    /// Opens the master's account. The master signs, so nobody can open one
    /// in someone else's name; whoever pays its rent gets it back when it is
    /// closed.
    pub fn open_account(ctx: Context<OpenAccount>) -> Result<()> {
        ctx.accounts.account.set_inner(MarginAccount {
            master: ctx.accounts.master.key(),
            payer: ctx.accounts.payer.key(),
            cash: 0,
            positions: Vec::new(),
            ratio_value: 0,
            ratio_gross: 0,
            bump: ctx.bumps.account,
        });
        emit!(AccountOpened {
            master: ctx.accounts.master.key(),
        });
        Ok(())
    }

    /// Closes an empty account and refunds its rent to whoever paid it.
    pub fn close_account(ctx: Context<CloseAccount>) -> Result<()> {
        let account = &ctx.accounts.account;
        require!(
            account.cash == 0 && account.positions.is_empty(),
            VenueError::NotEmpty
        );
        emit!(AccountClosed {
            master: account.master,
        });
        Ok(())
    }

    // Dollars in and out --------------------------------------------------

    /// From a token account the master owns, signed by the master.
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        token::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.master_dollars.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.master.to_account_info(),
                },
            ),
            amount,
            ctx.accounts.mint.decimals,
        )?;
        let account = &mut ctx.accounts.account;
        account.cash = account
            .cash
            .checked_add(amount as i128)
            .ok_or(VenueError::Overflow)?;
        emit!(Deposited {
            master: account.master,
            amount,
        });
        Ok(())
    }

    /// To a token account the master owns, and nowhere else: there is no
    /// instruction that sends an account's dollars anywhere but back to its
    /// master, which is the whole of the custody claim. The markets the
    /// account holds follow as remaining accounts, so what is left is valued.
    ///
    /// What is left must cover the initial margin on the perps still open,
    /// or else keep the account's margin ratio where it stood before its last
    /// order (within the rounding): so taking out the proceeds of closing a
    /// fraction of every position goes through wherever taking out the
    /// proceeds of closing all of them would, even below the initial margin.
    pub fn withdraw<'info>(
        ctx: Context<'_, '_, 'info, 'info, Withdraw<'info>>,
        amount: u64,
    ) -> Result<()> {
        let account = &mut ctx.accounts.account;
        let markets = held_markets(account, ctx.remaining_accounts)?;
        account.settle_all(&markets)?;
        account.cash = account
            .cash
            .checked_sub(amount as i128)
            .ok_or(VenueError::Overflow)?;
        let left = account.value_at(&markets, PriceKind::Mark)?;
        require!(account.cash >= 0 && left >= 0, VenueError::WouldGoNegative);
        let gross = account.gross_perp_notional(&markets, None)?;
        let initial = left.checked_mul(BPS).ok_or(VenueError::Overflow)?
            >= gross
                .checked_mul(ctx.accounts.venue.margin_bps as i128)
                .ok_or(VenueError::Overflow)?;
        require!(
            initial || account.keeps_ratio(left, gross)?,
            VenueError::WouldGoNegative
        );
        let venue = &ctx.accounts.venue;
        let seeds: &[&[u8]] = &[VENUE_SEED, &[venue.bump]];
        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.master_dollars.to_account_info(),
                    authority: ctx.accounts.venue.to_account_info(),
                },
                &[seeds],
            ),
            amount,
            ctx.accounts.mint.decimals,
        )?;
        emit!(Withdrawn {
            master: account.master,
            amount,
        });
        Ok(())
    }

    // Trading -------------------------------------------------------------

    /// Places `orders` for the master's account, signed by the master. Every
    /// market the account holds, and every market an order names, follow as
    /// remaining accounts in any order; one an order names that was never
    /// listed is passed at its address, empty, and the order is refused.
    /// Answers with each order's fill, in the return data.
    pub fn place<'info>(
        ctx: Context<'_, '_, 'info, 'info, Place<'info>>,
        orders: Vec<OrderRequest>,
    ) -> Result<()> {
        require!(orders.len() <= MAX_ORDERS, VenueError::TooMany);
        let account = &mut ctx.accounts.account;
        let book = Book::read(ctx.remaining_accounts)?;
        let held = book.held(account)?;
        // The account's value at the mark is worked out once, here, and kept
        // up to date order by order, exactly: an order changes only its own
        // market's position and the cash.
        let mut value = account.value_at(&held, PriceKind::Mark)?;
        account.ratio_value = value;
        account.ratio_gross = account.gross_perp_notional(&held, None)?;
        let margin_bps = ctx.accounts.venue.margin_bps as i128;
        let mut fills = Vec::with_capacity(orders.len());
        for order in &orders {
            fills.push(place_one(account, &book, &mut value, order, margin_bps)?);
        }
        set_return_data(&fills.try_to_vec()?);
        Ok(())
    }
}

// The book -------------------------------------------------------------------

/// The markets an instruction was given, by id.
struct Book {
    markets: Vec<Market>,
}

impl Book {
    /// Every remaining account that is one of this venue's markets, checked to
    /// be at its own address. An empty account is a market never listed.
    fn read(accounts: &[AccountInfo]) -> Result<Self> {
        let mut markets: Vec<Market> = Vec::with_capacity(accounts.len());
        for info in accounts {
            if info.owner != &crate::ID || info.data_is_empty() {
                continue;
            }
            let market = load_market(info)?;
            require!(
                !markets.iter().any(|seen| seen.id == market.id),
                VenueError::NoSuchMarket
            );
            markets.push(market);
        }
        Ok(Self { markets })
    }

    fn get(&self, id: &[u8; 32]) -> Option<&Market> {
        self.markets.iter().find(|market| &market.id == id)
    }

    /// The markets an account holds, in its order; every one must be here.
    fn held(&self, account: &MarginAccount) -> Result<Vec<Market>> {
        account
            .positions
            .iter()
            .map(|position| {
                self.get(&position.market)
                    .cloned()
                    .ok_or_else(|| error!(VenueError::MissingMarket))
            })
            .collect()
    }
}

/// A market account of this venue, at the address its id derives.
pub fn load_market(info: &AccountInfo) -> Result<Market> {
    load_market_of(&crate::ID, info)
}

/// A market account of the venue deployed at `program`, at the address its
/// id derives there.
pub fn load_market_of(program: &Pubkey, info: &AccountInfo) -> Result<Market> {
    require_keys_eq!(*info.owner, *program, VenueError::NoSuchMarket);
    let market = {
        let data = info.try_borrow_data()?;
        Market::try_deserialize(&mut &data[..])?
    };
    let address =
        Pubkey::create_program_address(&[MARKET_SEED, &market.id, &[market.bump]], program)
            .map_err(|_| error!(VenueError::NoSuchMarket))?;
    require_keys_eq!(address, *info.key, VenueError::NoSuchMarket);
    Ok(market)
}

/// The markets `account` holds, from `accounts`, which must be exactly those,
/// in the account's order.
fn held_markets(account: &MarginAccount, accounts: &[AccountInfo]) -> Result<Vec<Market>> {
    let book = Book::read(accounts)?;
    book.held(account)
}

/// Places one order. `value` is the account's value at the mark, which
/// every market it holds is in `book` to give (checked once, before the
/// first order), and is brought up to date when the order fills.
fn place_one(
    account: &mut MarginAccount,
    book: &Book,
    value: &mut i128,
    order: &OrderRequest,
    margin_bps: i128,
) -> Result<Fill> {
    let master = account.master;
    let Some(market) = book.get(&order.market).cloned() else {
        return Ok(reject(master, order.market, "no such market"));
    };
    if order.size == 0 {
        return Ok(reject(master, order.market, "zero size"));
    }
    // Settling moves the funding the position owes from its value into the
    // cash, which leaves the account's value as it was.
    account.settle(&market)?;
    let current = account.size_in(&market.id);

    let mut size = i128::try_from(order.size).map_err(|_| error!(VenueError::Overflow))?;
    if order.reduce_only {
        if current == 0 {
            return Ok(reject(master, order.market, "nothing to reduce"));
        }
        let shrinks = if order.is_buy {
            current < 0
        } else {
            current > 0
        };
        if !shrinks {
            return Ok(reject(master, order.market, "would not reduce"));
        }
        size = size.min(current.abs());
    }

    let price = if order.is_buy { market.ask } else { market.bid };
    let crosses = if order.is_buy {
        price <= order.limit_price
    } else {
        price >= order.limit_price
    };
    if !crosses {
        // Nobody was there at that price. The venue would have taken the
        // order, so this is the market's answer rather than the venue's.
        emit!(Unfilled {
            master,
            market: order.market,
        });
        return Ok(Fill {
            accepted: false,
            reason: "no fill at that price".to_string(),
            ..Fill::default()
        });
    }

    // What the account would look like afterwards. Nothing is written until
    // every check has read it, so a refused order leaves no trace and the
    // orders beside it keep their fills. The dollars round against the
    // order: a buy pays for every fraction of a dollar-unit it takes, and a
    // sale is paid only the whole dollar-units it brings.
    let notional = if order.is_buy {
        mul_scale_up(size, price as i128)?
    } else {
        mul_scale(size, price as i128)?
    };
    let at_mark = mul_scale(size, market.mark as i128)?;
    let next_cash = if order.is_buy {
        account.cash.checked_sub(notional)
    } else {
        account.cash.checked_add(notional)
    }
    .ok_or(VenueError::Overflow)?;
    let next_size = if order.is_buy {
        current.checked_add(size)
    } else {
        current.checked_sub(size)
    }
    .ok_or(VenueError::Overflow)?;
    let next_value = if order.is_buy {
        value.checked_add(at_mark - notional)
    } else {
        value.checked_add(notional - at_mark)
    }
    .ok_or(VenueError::Overflow)?;

    // Spot can't be borrowed: no buying into debt, no selling what isn't
    // held. A sale always raises the cash, so it is never refused for it.
    if !market.is_perp && (next_size < 0 || (order.is_buy && next_cash < 0)) {
        return Ok(reject(
            master,
            order.market,
            "spot cannot be borrowed against",
        ));
    }
    // An order that only reduces a position is always taken, as venues take
    // reduce-only orders: closing can't be held up by the margin a position
    // needed to be opened.
    if !order.reduce_only && next_value < 0 {
        return Ok(reject(
            master,
            order.market,
            "would leave the account underwater",
        ));
    }
    if market.is_perp && !order.reduce_only {
        // The book holds every market the account holds and this one.
        let gross = account.gross_perp_notional(&book.markets, Some((&market.id, next_size)))?;
        let enough = next_value
            .checked_mul(BPS)
            .zip(gross.checked_mul(margin_bps))
            .is_some_and(|(have, need)| have >= need);
        if !enough {
            return Ok(reject(master, order.market, "not enough margin"));
        }
    }

    // The value exactly as `value_at` would read it now: the cash's change,
    // and this market's position at the mark in place of what it was (its
    // funding is settled, so it owes none). `next_value` above is the
    // venue's estimate for its checks, which rounds the order's notional at
    // the mark on its own.
    let was = mul_scale(current, market.mark as i128)?;
    let will_be = mul_scale(next_size, market.mark as i128)?;
    *value = value
        .checked_sub(account.cash)
        .and_then(|v| v.checked_sub(was))
        .and_then(|v| v.checked_add(next_cash))
        .and_then(|v| v.checked_add(will_be))
        .ok_or(VenueError::Overflow)?;
    account.cash = next_cash;
    account.set_size(&market, next_size)?;
    emit!(Filled {
        master,
        market: order.market,
        is_buy: order.is_buy,
        reduce_only: order.reduce_only,
        size: size as u128,
        price,
        notional: notional as u128,
    });
    Ok(Fill {
        accepted: true,
        reason: String::new(),
        size: size as u128,
        price,
        notional: notional as u128,
    })
}

/// An order the venue would not take. The batch it came in carries on, so a
/// strategy submitting several orders learns which of them landed.
fn reject(master: Pubkey, market: [u8; 32], reason: &str) -> Fill {
    emit!(Rejected {
        master,
        market,
        reason: reason.to_string(),
    });
    Fill {
        accepted: false,
        reason: reason.to_string(),
        ..Fill::default()
    }
}

/// `size * price / 1e18`, truncated toward zero as the EVM venue's signed
/// division is.
pub fn mul_scale(size: i128, price: i128) -> Result<i128> {
    Ok(size
        .checked_mul(price)
        .ok_or(VenueError::Overflow)?
        .checked_div(SIZE_SCALE)
        .ok_or(VenueError::Overflow)?)
}

/// `size * price / 1e18` for a size and price that are never negative,
/// rounded up: what a buy costs.
pub fn mul_scale_up(size: i128, price: i128) -> Result<i128> {
    require!(size >= 0 && price >= 0, VenueError::Overflow);
    let product = size.checked_mul(price).ok_or(VenueError::Overflow)?;
    Ok(product.div_euclid(SIZE_SCALE) + i128::from(product.rem_euclid(SIZE_SCALE) != 0))
}

fn now() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}

// State ----------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct Venue {
    /// Moves prices and accrues funding. A real venue is its own market;
    /// here the development chain plays one.
    pub operator: Pubkey,
    /// The dollar.
    pub mint: Pubkey,
    /// The venue's dollar account, holding every account's cash.
    pub vault: Pubkey,
    /// What a perp position has to be backed by, in basis points of its
    /// notional. The venue's own limit, not the platform's: how much leverage
    /// to use inside it is a strategy's own decision.
    pub margin_bps: u64,
    pub bump: u8,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Market {
    pub id: [u8; 32],
    pub is_perp: bool,
    /// What the venue values a position at.
    pub mark: u64,
    /// What the venue's own price source says, which bounds an exit.
    pub oracle: u64,
    pub bid: u64,
    pub ask: u64,
    /// Dollars per unit of size that longs have paid shorts since the market
    /// opened. A position's own share of it is settled when it next moves.
    pub funding_index: i128,
    /// What longs are paying shorts now, per unit of size per day.
    pub funding_rate: i128,
    pub funding_updated_at: u64,
    pub bump: u8,
}

impl Market {
    fn set_prices(&mut self, quote: &Quote) {
        self.mark = quote.mark;
        self.oracle = quote.oracle;
        self.bid = quote.bid;
        self.ask = quote.ask;
        emit!(PricesSet {
            market: self.id,
            mark: quote.mark,
            oracle: quote.oracle,
            bid: quote.bid,
            ask: quote.ask,
        });
    }

    fn accrue(&mut self, paid_per_unit: i128) -> Result<()> {
        self.funding_index = self
            .funding_index
            .checked_add(paid_per_unit)
            .ok_or(VenueError::Overflow)?;
        self.funding_updated_at = now()?;
        emit!(FundingAccrued {
            market: self.id,
            paid_per_unit,
            funding_index: self.funding_index,
        });
        Ok(())
    }

    fn set_rate(&mut self, rate_per_day: i128) {
        self.funding_rate = rate_per_day;
        emit!(FundingSet {
            market: self.id,
            rate_per_day,
        });
    }
}

/// What one account holds in one market.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub struct Position {
    pub market: [u8; 32],
    /// Positive is long, negative is short; never zero while listed.
    pub size: i128,
    /// The funding index this position last settled at.
    pub funding_index: i128,
}

/// One master's account at the venue.
#[account]
#[derive(InitSpace)]
pub struct MarginAccount {
    pub master: Pubkey,
    /// Who paid its rent, and gets it back when it closes.
    pub payer: Pubkey,
    pub cash: i128,
    /// The positions open now, one per market; a flat one is forgotten.
    #[max_len(MAX_HELD)]
    pub positions: Vec<Position>,
    /// Its margin ratio as it stood before its last order, as its value and
    /// its gross perp notional at the mark then: a withdrawal may take it
    /// back down to that ratio, never below.
    pub ratio_value: i128,
    pub ratio_gross: i128,
    pub bump: u8,
}

/// Which price a position is valued at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PriceKind {
    /// The venue's mark.
    Mark,
    /// What a sale would fetch: longs at the bid, shorts at the ask.
    Exit,
}

impl MarginAccount {
    pub fn size_in(&self, market: &[u8; 32]) -> i128 {
        self.positions
            .iter()
            .find(|position| &position.market == market)
            .map_or(0, |position| position.size)
    }

    /// Cash, plus what every position is worth, less the funding it owes but
    /// hasn't yet settled. `markets` are the account's markets, in its order.
    pub fn value_at(&self, markets: &[Market], kind: PriceKind) -> Result<i128> {
        require!(
            markets.len() == self.positions.len(),
            VenueError::MissingMarket
        );
        let mut value = self.cash;
        for (position, market) in self.positions.iter().zip(markets) {
            require!(position.market == market.id, VenueError::MissingMarket);
            let price = match kind {
                PriceKind::Mark => market.mark,
                PriceKind::Exit if position.size > 0 => market.bid,
                PriceKind::Exit => market.ask,
            };
            let owed = mul_scale(
                position.size,
                market
                    .funding_index
                    .checked_sub(position.funding_index)
                    .ok_or(VenueError::Overflow)?,
            )?;
            value = value
                .checked_add(mul_scale(position.size, price as i128)?)
                .and_then(|value| value.checked_sub(owed))
                .ok_or(VenueError::Overflow)?;
        }
        Ok(value)
    }

    /// What every open perp position is worth at mark, added up without
    /// regard to sign; with one market's size replaced by what a pending
    /// order would leave there. `markets` holds every market named.
    pub fn gross_perp_notional(
        &self,
        markets: &[Market],
        replace: Option<(&[u8; 32], i128)>,
    ) -> Result<i128> {
        let mut gross: i128 = 0;
        let mut add = |size: i128, market: &Market| -> Result<()> {
            if market.is_perp && size != 0 {
                gross = gross
                    .checked_add(mul_scale(size.abs(), market.mark as i128)?)
                    .ok_or(VenueError::Overflow)?;
            }
            Ok(())
        };
        for position in &self.positions {
            let market = markets
                .iter()
                .find(|market| market.id == position.market)
                .ok_or(VenueError::MissingMarket)?;
            let size = match replace {
                Some((id, size)) if id == &position.market => size,
                _ => position.size,
            };
            add(size, market)?;
        }
        if let Some((id, size)) = replace {
            if !self.positions.iter().any(|position| &position.market == id) {
                let market = markets
                    .iter()
                    .find(|market| &market.id == id)
                    .ok_or(VenueError::MissingMarket)?;
                add(size, market)?;
            }
        }
        Ok(gross)
    }

    /// Whether leaving `left` against `gross` keeps the margin ratio the
    /// account had before its last order, less [`RATIO_ROUNDING`]. An account
    /// that held no perps then, or was worth nothing, has no ratio to keep.
    fn keeps_ratio(&self, left: i128, gross: i128) -> Result<bool> {
        if self.ratio_gross <= 0 || self.ratio_value <= 0 {
            return Ok(false);
        }
        let have = left
            .checked_add(RATIO_ROUNDING)
            .and_then(|have| have.checked_mul(self.ratio_gross))
            .ok_or(VenueError::Overflow)?;
        let need = self
            .ratio_value
            .checked_mul(gross)
            .ok_or(VenueError::Overflow)?;
        Ok(have >= need)
    }

    /// Settles the funding a position owes or is owed into cash.
    fn settle(&mut self, market: &Market) -> Result<()> {
        let mut cash = self.cash;
        if let Some(position) = self
            .positions
            .iter_mut()
            .find(|position| position.market == market.id)
        {
            if position.funding_index != market.funding_index {
                let owed = mul_scale(
                    position.size,
                    market
                        .funding_index
                        .checked_sub(position.funding_index)
                        .ok_or(VenueError::Overflow)?,
                )?;
                cash = cash.checked_sub(owed).ok_or(VenueError::Overflow)?;
                position.funding_index = market.funding_index;
            }
        }
        self.cash = cash;
        Ok(())
    }

    fn settle_all(&mut self, markets: &[Market]) -> Result<()> {
        for market in markets {
            self.settle(market)?;
        }
        Ok(())
    }

    /// Sets the position in `market`: a new one is listed, a flat one
    /// forgotten, so the account lists only what it holds now.
    fn set_size(&mut self, market: &Market, size: i128) -> Result<()> {
        let at = self
            .positions
            .iter()
            .position(|position| position.market == market.id);
        match (at, size) {
            (Some(at), 0) => {
                self.positions.swap_remove(at);
            }
            (Some(at), size) => self.positions[at].size = size,
            (None, 0) => {}
            (None, size) => {
                require!(self.positions.len() < MAX_HELD, VenueError::TooMany);
                self.positions.push(Position {
                    market: market.id,
                    size,
                    funding_index: market.funding_index,
                });
            }
        }
        Ok(())
    }
}

/// One order, as a strategy submits it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderRequest {
    pub market: [u8; 32],
    pub is_buy: bool,
    /// 18 decimals.
    pub size: u128,
    /// The worst price the order may fill at.
    pub limit_price: u64,
    /// Only ever shrinks a position, and never past flat.
    pub reduce_only: bool,
}

/// What the venue did with one order.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Fill {
    pub accepted: bool,
    /// Why it was not accepted, when it was not.
    pub reason: String,
    pub size: u128,
    pub price: u64,
    pub notional: u128,
}

/// One market's prices, as the operator posts them.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub market: [u8; 32],
    pub mark: u64,
    pub oracle: u64,
    pub bid: u64,
    pub ask: u64,
}

/// What longs are paying shorts in one market, and what has accrued since
/// the last posting.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Funding {
    pub market: [u8; 32],
    pub rate_per_day: i128,
    pub paid_per_unit: i128,
}

// Addresses ------------------------------------------------------------------

pub fn venue_address() -> Pubkey {
    venue_address_of(&crate::ID)
}

pub fn vault_address() -> Pubkey {
    vault_address_of(&crate::ID)
}

pub fn market_address(id: &[u8; 32]) -> Pubkey {
    market_address_of(&crate::ID, id)
}

pub fn account_address(master: &Pubkey) -> Pubkey {
    account_address_of(&crate::ID, master)
}

/// The same addresses at the venue deployed at `program`.
pub fn venue_address_of(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VENUE_SEED], program).0
}

pub fn vault_address_of(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VAULT_SEED], program).0
}

pub fn market_address_of(program: &Pubkey, id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[MARKET_SEED, id], program).0
}

pub fn account_address_of(program: &Pubkey, master: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ACCOUNT_SEED, master.as_ref()], program).0
}

// Accounts -------------------------------------------------------------------

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub deployer: Signer<'info>,
    #[account(
        init,
        payer = deployer,
        space = 8 + Venue::INIT_SPACE,
        seeds = [VENUE_SEED],
        bump,
    )]
    pub venue: Account<'info, Venue>,
    #[account(
        init,
        payer = deployer,
        seeds = [VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = venue,
        token::token_program = token_program,
    )]
    pub vault: Account<'info, TokenAccount>,
    pub mint: Account<'info, Mint>,
    /// CHECK: this program's own account, whose upgrade authority is read.
    #[account(address = crate::ID)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: checked against `program` by `loader::is_upgrade_authority`.
    pub program_data: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(id: [u8; 32])]
pub struct ListMarket<'info> {
    #[account(mut)]
    pub operator: Signer<'info>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump, has_one = operator @ VenueError::NotOperator)]
    pub venue: Account<'info, Venue>,
    #[account(
        init,
        payer = operator,
        space = 8 + Market::INIT_SPACE,
        seeds = [MARKET_SEED, id.as_ref()],
        bump,
    )]
    pub market: Account<'info, Market>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Operate<'info> {
    pub operator: Signer<'info>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump, has_one = operator @ VenueError::NotOperator)]
    pub venue: Account<'info, Venue>,
    #[account(mut, seeds = [MARKET_SEED, market.id.as_ref()], bump = market.bump)]
    pub market: Account<'info, Market>,
}

#[derive(Accounts)]
pub struct Post<'info> {
    pub operator: Signer<'info>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump, has_one = operator @ VenueError::NotOperator)]
    pub venue: Account<'info, Venue>,
}

#[derive(Accounts)]
pub struct OpenAccount<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub master: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + MarginAccount::INIT_SPACE,
        seeds = [ACCOUNT_SEED, master.key().as_ref()],
        bump,
    )]
    pub account: Account<'info, MarginAccount>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseAccount<'info> {
    pub master: Signer<'info>,
    #[account(
        mut,
        seeds = [ACCOUNT_SEED, master.key().as_ref()],
        bump = account.bump,
        has_one = payer,
        close = payer,
    )]
    pub account: Account<'info, MarginAccount>,
    /// CHECK: whoever paid the rent, as the account records.
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    pub master: Signer<'info>,
    #[account(
        mut,
        seeds = [ACCOUNT_SEED, master.key().as_ref()],
        bump = account.bump,
    )]
    pub account: Account<'info, MarginAccount>,
    #[account(
        mut,
        token::mint = mint,
        token::authority = master,
    )]
    pub master_dollars: Account<'info, TokenAccount>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump, has_one = vault, has_one = mint)]
    pub venue: Account<'info, Venue>,
    #[account(mut)]
    pub vault: Account<'info, TokenAccount>,
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    pub master: Signer<'info>,
    #[account(
        mut,
        seeds = [ACCOUNT_SEED, master.key().as_ref()],
        bump = account.bump,
    )]
    pub account: Account<'info, MarginAccount>,
    /// Only a dollar account the master owns.
    #[account(
        mut,
        token::mint = mint,
        token::authority = master,
    )]
    pub master_dollars: Account<'info, TokenAccount>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump, has_one = vault, has_one = mint)]
    pub venue: Account<'info, Venue>,
    #[account(mut)]
    pub vault: Account<'info, TokenAccount>,
    pub mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Place<'info> {
    pub master: Signer<'info>,
    #[account(
        mut,
        seeds = [ACCOUNT_SEED, master.key().as_ref()],
        bump = account.bump,
    )]
    pub account: Account<'info, MarginAccount>,
    #[account(seeds = [VENUE_SEED], bump = venue.bump)]
    pub venue: Account<'info, Venue>,
}

// Events ---------------------------------------------------------------------

#[event]
pub struct AccountOpened {
    pub master: Pubkey,
}

#[event]
pub struct AccountClosed {
    pub master: Pubkey,
}

#[event]
pub struct Deposited {
    pub master: Pubkey,
    pub amount: u64,
}

#[event]
pub struct Withdrawn {
    pub master: Pubkey,
    pub amount: u64,
}

#[event]
pub struct Filled {
    pub master: Pubkey,
    pub market: [u8; 32],
    pub is_buy: bool,
    pub reduce_only: bool,
    pub size: u128,
    pub price: u64,
    pub notional: u128,
}

#[event]
pub struct Rejected {
    pub master: Pubkey,
    pub market: [u8; 32],
    pub reason: String,
}

/// An immediate-or-cancel order that found nobody at its price. Not a
/// refusal: the venue would have taken it, and the market moved.
#[event]
pub struct Unfilled {
    pub master: Pubkey,
    pub market: [u8; 32],
}

#[event]
pub struct PricesSet {
    pub market: [u8; 32],
    pub mark: u64,
    pub oracle: u64,
    pub bid: u64,
    pub ask: u64,
}

#[event]
pub struct FundingAccrued {
    pub market: [u8; 32],
    pub paid_per_unit: i128,
    pub funding_index: i128,
}

#[event]
pub struct FundingSet {
    pub market: [u8; 32],
    pub rate_per_day: i128,
}

#[error_code]
pub enum VenueError {
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("Only the market operator may do this")]
    NotOperator,
    #[msg("No such market")]
    NoSuchMarket,
    #[msg("A market the account holds was not passed")]
    MissingMarket,
    #[msg("It would leave the account negative or short of margin")]
    WouldGoNegative,
    #[msg("Too many markets or orders")]
    TooMany,
    #[msg("The account still holds cash or positions")]
    NotEmpty,
    #[msg("Arithmetic overflow")]
    Overflow,
}
