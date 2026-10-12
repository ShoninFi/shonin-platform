//! The connection, as a state machine (Wallet-*, States-System, Handoff
//! "Wallet"): signed out → choosing → connecting (the hex ring, "Approve in
//! MetaMask", Cancel) → connected, or rejected, not installed, locked. Pure,
//! so every transition is tested natively; the browser's session feeds it
//! the wallet's answers and events.

use alloy_primitives::Address;

/// Which wallet: an announced browser wallet by its reverse-DNS id
/// (`io.metamask`), the legacy injected one, the recovery phrase, or the
/// development wallet. Only this id is ever remembered, never a phrase.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WalletId {
    Announced(String),
    Injected,
    Phrase,
    Dev,
}

impl WalletId {
    /// What is kept in storage to reconnect silently next time. The phrase
    /// wallet is never kept: its phrase lives in memory only, so a reload
    /// forgets it.
    pub fn remembered(&self) -> Option<String> {
        match self {
            Self::Announced(rdns) => Some(format!("eip6963:{rdns}")),
            Self::Injected => Some("injected".to_string()),
            Self::Dev => Some("dev".to_string()),
            Self::Phrase => None,
        }
    }

    pub fn from_remembered(text: &str) -> Option<Self> {
        match text {
            "injected" => Some(Self::Injected),
            "dev" => Some(Self::Dev),
            _ => {
                let rdns = text.strip_prefix("eip6963:")?;
                (!rdns.is_empty()
                    && rdns.len() <= 128
                    && rdns
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'))
                .then(|| Self::Announced(rdns.to_string()))
            }
        }
    }

    /// Whether the wallet lives in the browser (and can switch chains itself).
    pub fn is_browser(&self) -> bool {
        matches!(self, Self::Announced(_) | Self::Injected)
    }
}

/// Where the connection is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    SignedOut,
    /// The dialog is open, listing wallets.
    Choosing,
    /// Waiting for the person to approve in the wallet.
    Connecting {
        wallet: WalletId,
    },
    Connected {
        wallet: WalletId,
        account: Address,
        /// The chain the wallet is on; a local signer is on whichever chain
        /// it is asked to sign for.
        chain_id: Option<u64>,
    },
    /// Declined in the wallet.
    Rejected {
        wallet: WalletId,
    },
    /// Chosen, but not in this browser.
    NotInstalled {
        wallet: WalletId,
    },
    /// The wallet is locked, or gave no account.
    Locked {
        wallet: WalletId,
    },
}

impl Connection {
    pub fn account(&self) -> Option<Address> {
        match self {
            Self::Connected { account, .. } => Some(*account),
            _ => None,
        }
    }

    pub fn wallet(&self) -> Option<&WalletId> {
        match self {
            Self::Connecting { wallet }
            | Self::Connected { wallet, .. }
            | Self::Rejected { wallet }
            | Self::NotInstalled { wallet }
            | Self::Locked { wallet } => Some(wallet),
            Self::SignedOut | Self::Choosing => None,
        }
    }

    /// Whether the dialog is showing.
    pub fn dialog_open(&self) -> bool {
        matches!(
            self,
            Self::Choosing
                | Self::Connecting { .. }
                | Self::Rejected { .. }
                | Self::NotInstalled { .. }
                | Self::Locked { .. }
        )
    }
}

/// Why connecting didn't work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Rejected,
    NotInstalled,
    Locked,
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// "Connect wallet" pressed.
    Open,
    /// The dialog closed without connecting.
    Close,
    /// A wallet chosen in the dialog.
    Choose(WalletId),
    /// The wallet gave an account.
    Approved {
        account: Address,
        chain_id: Option<u64>,
    },
    Refused(Refusal),
    /// "Cancel" while connecting.
    Cancel,
    /// The wallet's accounts changed: the first is the one it offers now;
    /// none means it disconnected this site or locked.
    AccountsChanged(Option<Address>),
    ChainChanged(u64),
    /// The wallet disconnected, or the person pressed Disconnect.
    Disconnected,
    /// A silent reconnect found the remembered wallet still connected.
    Restored {
        wallet: WalletId,
        account: Address,
        chain_id: Option<u64>,
    },
}

/// The next state. Anything that makes no sense where the connection is
/// leaves it as it was: a late answer from a wallet whose request was
/// cancelled is ignored.
pub fn next(state: &Connection, event: Event) -> Connection {
    use Connection as C;
    match (state, event) {
        (C::Connected { .. }, Event::Open) => state.clone(),
        (_, Event::Open) => C::Choosing,
        (C::Connected { .. }, Event::Close) => state.clone(),
        (_, Event::Close) => C::SignedOut,
        (C::Connected { .. }, Event::Choose(_)) => state.clone(),
        (_, Event::Choose(wallet)) => C::Connecting { wallet },
        (C::Connecting { wallet }, Event::Approved { account, chain_id }) => C::Connected {
            wallet: wallet.clone(),
            account,
            chain_id,
        },
        (C::Connecting { wallet }, Event::Refused(why)) => {
            let wallet = wallet.clone();
            match why {
                Refusal::Rejected => C::Rejected { wallet },
                Refusal::NotInstalled => C::NotInstalled { wallet },
                Refusal::Locked => C::Locked { wallet },
            }
        }
        (C::Connecting { .. }, Event::Cancel) => C::Choosing,
        (
            C::Connected {
                wallet, chain_id, ..
            },
            Event::AccountsChanged(Some(account)),
        ) => C::Connected {
            wallet: wallet.clone(),
            account,
            chain_id: *chain_id,
        },
        (C::Connected { .. }, Event::AccountsChanged(None)) => C::SignedOut,
        (
            C::Connected {
                wallet, account, ..
            },
            Event::ChainChanged(chain_id),
        ) => C::Connected {
            wallet: wallet.clone(),
            account: *account,
            chain_id: Some(chain_id),
        },
        (C::Connected { .. } | C::Connecting { .. }, Event::Disconnected) => C::SignedOut,
        (
            C::SignedOut,
            Event::Restored {
                wallet,
                account,
                chain_id,
            },
        ) => C::Connected {
            wallet,
            account,
            chain_id,
        },
        _ => state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metamask() -> WalletId {
        WalletId::Announced("io.metamask".to_string())
    }

    fn connected(account: u8) -> Connection {
        Connection::Connected {
            wallet: metamask(),
            account: Address::repeat_byte(account),
            chain_id: Some(84532),
        }
    }

    #[test]
    fn choosing_a_wallet_and_approving_connects_it() {
        let state = next(&Connection::SignedOut, Event::Open);
        assert_eq!(state, Connection::Choosing);
        let state = next(&state, Event::Choose(metamask()));
        assert_eq!(state, Connection::Connecting { wallet: metamask() });
        let state = next(
            &state,
            Event::Approved {
                account: Address::repeat_byte(1),
                chain_id: Some(84532),
            },
        );
        assert_eq!(state, connected(1));
        assert!(!state.dialog_open());
    }

    #[test]
    fn each_refusal_has_its_own_state_and_trying_again_goes_back_to_choosing() {
        for (why, expected) in [
            (
                Refusal::Rejected,
                Connection::Rejected { wallet: metamask() },
            ),
            (
                Refusal::NotInstalled,
                Connection::NotInstalled { wallet: metamask() },
            ),
            (Refusal::Locked, Connection::Locked { wallet: metamask() }),
        ] {
            let state = next(
                &Connection::Connecting { wallet: metamask() },
                Event::Refused(why),
            );
            assert_eq!(state, expected);
            assert!(state.dialog_open());
            assert_eq!(next(&state, Event::Open), Connection::Choosing);
            assert_eq!(
                next(&state, Event::Choose(WalletId::Injected)),
                Connection::Connecting {
                    wallet: WalletId::Injected
                }
            );
        }
    }

    #[test]
    fn cancel_while_connecting_returns_to_the_list_and_a_late_approval_is_ignored() {
        let state = next(
            &Connection::Connecting { wallet: metamask() },
            Event::Cancel,
        );
        assert_eq!(state, Connection::Choosing);
        let late = next(
            &state,
            Event::Approved {
                account: Address::repeat_byte(1),
                chain_id: None,
            },
        );
        assert_eq!(late, Connection::Choosing);
    }

    #[test]
    fn a_changed_account_switches_identity_and_no_account_signs_out() {
        let state = next(
            &connected(1),
            Event::AccountsChanged(Some(Address::repeat_byte(2))),
        );
        assert_eq!(state.account(), Some(Address::repeat_byte(2)));
        assert_eq!(
            next(&state, Event::AccountsChanged(None)),
            Connection::SignedOut
        );
    }

    #[test]
    fn a_chain_change_is_followed_and_a_disconnect_signs_out() {
        let state = next(&connected(1), Event::ChainChanged(998));
        assert!(matches!(
            state,
            Connection::Connected {
                chain_id: Some(998),
                ..
            }
        ));
        assert_eq!(next(&state, Event::Disconnected), Connection::SignedOut);
    }

    #[test]
    fn a_silent_reconnect_only_restores_a_signed_out_page() {
        let restored = Event::Restored {
            wallet: metamask(),
            account: Address::repeat_byte(3),
            chain_id: Some(1),
        };
        assert_eq!(
            next(&Connection::SignedOut, restored.clone()).account(),
            Some(Address::repeat_byte(3))
        );
        // Someone already connecting isn't overridden.
        let connecting = Connection::Connecting {
            wallet: WalletId::Injected,
        };
        assert_eq!(next(&connecting, restored), connecting);
    }

    #[test]
    fn opening_the_dialog_while_connected_changes_nothing() {
        assert_eq!(next(&connected(1), Event::Open), connected(1));
        assert_eq!(next(&connected(1), Event::Close), connected(1));
    }

    #[test]
    fn only_a_wallet_id_is_remembered_never_the_phrase() {
        assert_eq!(
            metamask().remembered().as_deref(),
            Some("eip6963:io.metamask")
        );
        assert_eq!(WalletId::Phrase.remembered(), None);
        for id in [metamask(), WalletId::Injected, WalletId::Dev] {
            assert_eq!(
                WalletId::from_remembered(&id.remembered().unwrap()),
                Some(id)
            );
        }
        assert_eq!(WalletId::from_remembered("phrase"), None);
        assert_eq!(WalletId::from_remembered("eip6963:"), None);
        assert_eq!(WalletId::from_remembered("eip6963:<script>"), None);
    }
}
