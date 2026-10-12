//! The Solana connection, as a state machine beside the EVM one: signed out
//! → connecting (the hex ring, "Approve in Phantom", Cancel) → connected, or
//! rejected, not installed, locked. Pure, so every transition is tested
//! natively; the session feeds it the wallet's answers and events.

use chain::solana_ix::Pubkey;

/// Which Solana wallet: a Wallet Standard wallet by its name, Phantom's
/// older injected provider, the recovery phrase, or the development wallet.
/// Only this id is ever remembered, never a phrase.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SolWalletId {
    Standard(String),
    Legacy,
    Phrase,
    Dev,
}

impl SolWalletId {
    /// What is kept in storage to reconnect silently next time; never the
    /// phrase wallet.
    pub fn remembered(&self) -> Option<String> {
        match self {
            Self::Standard(name) => Some(format!("standard:{name}")),
            Self::Legacy => Some("legacy".to_string()),
            Self::Dev => Some("dev".to_string()),
            Self::Phrase => None,
        }
    }

    pub fn from_remembered(text: &str) -> Option<Self> {
        match text {
            "legacy" => Some(Self::Legacy),
            "dev" => Some(Self::Dev),
            _ => {
                let name = text.strip_prefix("standard:")?;
                (!name.is_empty()
                    && name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')))
                .then(|| Self::Standard(name.to_string()))
            }
        }
    }

    /// Whether the wallet lives in the browser (and so may not offer the
    /// vault's cluster).
    pub fn is_browser(&self) -> bool {
        matches!(self, Self::Standard(_) | Self::Legacy)
    }
}

/// Where the Solana connection is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolConnection {
    SignedOut,
    Connecting {
        wallet: SolWalletId,
    },
    Connected {
        wallet: SolWalletId,
        account: Pubkey,
    },
    Rejected {
        wallet: SolWalletId,
    },
    NotInstalled {
        wallet: SolWalletId,
    },
    Locked {
        wallet: SolWalletId,
    },
}

impl SolConnection {
    pub fn account(&self) -> Option<Pubkey> {
        match self {
            Self::Connected { account, .. } => Some(*account),
            _ => None,
        }
    }

    pub fn wallet(&self) -> Option<&SolWalletId> {
        match self {
            Self::SignedOut => None,
            Self::Connecting { wallet }
            | Self::Connected { wallet, .. }
            | Self::Rejected { wallet }
            | Self::NotInstalled { wallet }
            | Self::Locked { wallet } => Some(wallet),
        }
    }

    /// Whether the connect dialog shows this state instead of the list.
    pub fn in_dialog(&self) -> bool {
        matches!(
            self,
            Self::Connecting { .. }
                | Self::Rejected { .. }
                | Self::NotInstalled { .. }
                | Self::Locked { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Rejected,
    NotInstalled,
    Locked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Choose(SolWalletId),
    Approved(Pubkey),
    Refused(Refusal),
    /// "Cancel" while connecting, or back to the list.
    Cancel,
    /// The wallet's account changed: the one it offers now, or none.
    AccountChanged(Option<Pubkey>),
    Disconnected,
    /// A silent reconnect found the remembered wallet still connected.
    Restored {
        wallet: SolWalletId,
        account: Pubkey,
    },
}

/// The next state. Anything that makes no sense where the connection is
/// leaves it as it was: a late answer from a cancelled request is ignored.
pub fn next(state: &SolConnection, event: Event) -> SolConnection {
    use SolConnection as C;
    match (state, event) {
        (C::Connected { .. }, Event::Choose(_)) => state.clone(),
        (_, Event::Choose(wallet)) => C::Connecting { wallet },
        (C::Connecting { wallet }, Event::Approved(account)) => C::Connected {
            wallet: wallet.clone(),
            account,
        },
        (C::Connecting { wallet }, Event::Refused(why)) => {
            let wallet = wallet.clone();
            match why {
                Refusal::Rejected => C::Rejected { wallet },
                Refusal::NotInstalled => C::NotInstalled { wallet },
                Refusal::Locked => C::Locked { wallet },
            }
        }
        (
            C::Connecting { .. } | C::Rejected { .. } | C::NotInstalled { .. } | C::Locked { .. },
            Event::Cancel,
        ) => C::SignedOut,
        (C::Connected { wallet, .. }, Event::AccountChanged(Some(account))) => C::Connected {
            wallet: wallet.clone(),
            account,
        },
        (C::Connected { .. }, Event::AccountChanged(None)) => C::SignedOut,
        (C::Connected { .. } | C::Connecting { .. }, Event::Disconnected) => C::SignedOut,
        (C::SignedOut, Event::Restored { wallet, account }) => C::Connected { wallet, account },
        _ => state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phantom() -> SolWalletId {
        SolWalletId::Standard("Phantom".to_string())
    }

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    #[test]
    fn choosing_and_approving_connects_and_a_late_approval_after_cancel_is_ignored() {
        let state = next(&SolConnection::SignedOut, Event::Choose(phantom()));
        assert_eq!(state, SolConnection::Connecting { wallet: phantom() });
        let connected = next(&state, Event::Approved(key(1)));
        assert_eq!(connected.account(), Some(key(1)));
        let cancelled = next(&state, Event::Cancel);
        assert_eq!(cancelled, SolConnection::SignedOut);
        assert_eq!(
            next(&cancelled, Event::Approved(key(1))),
            SolConnection::SignedOut
        );
    }

    #[test]
    fn a_refusal_has_its_own_state_and_choosing_again_starts_over() {
        let state = next(
            &SolConnection::Connecting { wallet: phantom() },
            Event::Refused(Refusal::Rejected),
        );
        assert_eq!(state, SolConnection::Rejected { wallet: phantom() });
        assert!(state.in_dialog());
        assert_eq!(
            next(&state, Event::Choose(SolWalletId::Legacy)),
            SolConnection::Connecting {
                wallet: SolWalletId::Legacy
            }
        );
    }

    #[test]
    fn an_account_change_switches_identity_and_none_signs_out() {
        let state = SolConnection::Connected {
            wallet: phantom(),
            account: key(1),
        };
        let switched = next(&state, Event::AccountChanged(Some(key(2))));
        assert_eq!(switched.account(), Some(key(2)));
        assert_eq!(
            next(&switched, Event::AccountChanged(None)),
            SolConnection::SignedOut
        );
        assert_eq!(
            next(&switched, Event::Disconnected),
            SolConnection::SignedOut
        );
    }

    #[test]
    fn a_silent_reconnect_only_restores_a_signed_out_page() {
        let restored = Event::Restored {
            wallet: phantom(),
            account: key(3),
        };
        assert_eq!(
            next(&SolConnection::SignedOut, restored.clone()).account(),
            Some(key(3))
        );
        let connecting = SolConnection::Connecting {
            wallet: SolWalletId::Dev,
        };
        assert_eq!(next(&connecting, restored), connecting);
    }

    #[test]
    fn only_a_wallet_id_is_remembered_never_the_phrase() {
        assert_eq!(SolWalletId::Phrase.remembered(), None);
        for id in [phantom(), SolWalletId::Legacy, SolWalletId::Dev] {
            assert_eq!(
                SolWalletId::from_remembered(&id.remembered().unwrap()),
                Some(id)
            );
        }
        assert_eq!(SolWalletId::from_remembered("standard:"), None);
        assert_eq!(SolWalletId::from_remembered("standard:<script>"), None);
        assert_eq!(SolWalletId::from_remembered("phrase"), None);
    }
}
