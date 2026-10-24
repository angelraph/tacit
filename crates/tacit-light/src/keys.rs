//! An Ironwood account derived from a seed, and address encoding.

use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey};
use zcash_address::unified::{self, Container, Encoding};
use zcash_address::{ToAddress, ZcashAddress};
use zcash_protocol::consensus::Parameters;

use crate::{invalid, Net, Result};

/// One ZIP 32 account. Tacit uses only its Ironwood keys.
#[derive(Clone)]
pub struct Account {
    net: Net,
    sk: SpendingKey,
    fvk: FullViewingKey,
}

impl Account {
    /// Account 0 of `seed` (at least 32 bytes) on `net`.
    pub fn from_seed(net: Net, seed: &[u8]) -> Result<Self> {
        if seed.len() < 32 {
            return Err(invalid("a seed needs at least 32 bytes"));
        }
        let sk = SpendingKey::from_zip32_seed(seed, net.coin_type(), zip32::AccountId::ZERO).map_err(invalid)?;
        let fvk = FullViewingKey::from(&sk);
        Ok(Account { net, sk, fvk })
    }

    pub fn net(&self) -> Net {
        self.net
    }

    pub fn fvk(&self) -> &FullViewingKey {
        &self.fvk
    }

    pub fn ask(&self) -> SpendAuthorizingKey {
        SpendAuthorizingKey::from(&self.sk)
    }

    /// The `j`th receiving address. Each one is unlinkable to the others,
    /// which is what refund addresses in sealed bids rely on.
    pub fn address(&self, j: u32) -> orchard::Address {
        self.fvk.address_at(j, Scope::External)
    }

    /// The address change goes to.
    pub fn change_address(&self) -> orchard::Address {
        self.fvk.address_at(0u32, Scope::Internal)
    }

    /// The `j`th receiving address as a unified address string.
    pub fn unified_address(&self, j: u32) -> String {
        encode_address(self.net, &self.address(j))
    }

    /// The account's unified full viewing key with just its Ironwood part.
    pub fn ufvk(&self) -> String {
        encode_fvk(self.net, &self.fvk)
    }
}

/// Encodes an Ironwood receiver as a unified address.
pub fn encode_address(net: Net, addr: &orchard::Address) -> String {
    encode_raw_address(net, &addr.to_raw_address_bytes())
}

pub fn encode_raw_address(net: Net, raw: &[u8; 43]) -> String {
    let ua = unified::Address::try_from_items(vec![unified::Receiver::Orchard(*raw)])
        .expect("a single Ironwood receiver is a valid unified address");
    ZcashAddress::from_unified(net.network_type(), ua).encode()
}

/// The Ironwood receiver inside a unified address on `net`.
pub fn decode_address(net: Net, s: &str) -> Result<orchard::Address> {
    let raw = decode_raw_address(net, s)?;
    Option::from(orchard::Address::from_raw_address_bytes(&raw)).ok_or_else(|| invalid("not a valid Ironwood receiver"))
}

pub fn decode_raw_address(net: Net, s: &str) -> Result<[u8; 43]> {
    let (network, ua) = unified::Address::decode(s).map_err(|e| invalid(format!("not a unified address: {e}")))?;
    if network != net.network_type() {
        return Err(invalid(format!("that address is for another network, not {}", net.name())));
    }
    ua.items()
        .into_iter()
        .find_map(|r| match r {
            unified::Receiver::Orchard(raw) => Some(raw),
            _ => None,
        })
        .ok_or_else(|| invalid("the address has no shielded (Ironwood) receiver"))
}

pub fn encode_fvk(net: Net, fvk: &FullViewingKey) -> String {
    let ufvk = unified::Ufvk::try_from_items(vec![unified::Fvk::Orchard(fvk.to_bytes())])
        .expect("a single Ironwood viewing key is a valid unified viewing key");
    ufvk.encode(&net.network_type())
}

/// The Ironwood viewing key inside a unified full viewing key.
pub fn decode_fvk(net: Net, s: &str) -> Result<FullViewingKey> {
    let (network, ufvk) = unified::Ufvk::decode(s).map_err(|e| invalid(format!("not a viewing key: {e}")))?;
    if network != net.network_type() {
        return Err(invalid(format!("that viewing key is for another network, not {}", net.name())));
    }
    ufvk.items()
        .into_iter()
        .find_map(|f| match f {
            unified::Fvk::Orchard(b) => FullViewingKey::from_bytes(&b),
            _ => None,
        })
        .ok_or_else(|| invalid("the viewing key has no Ironwood part"))
}

/// The address a vault receives deposits at: the default (index 0) address
/// of its viewing key, the same one `tacit-wallet` hands out.
pub fn vault_address(fvk: &FullViewingKey) -> orchard::Address {
    fvk.address_at(0u32, Scope::External)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_round_trip() {
        let a = Account::from_seed(Net::Test, &[7; 32]).unwrap();
        let ua = a.unified_address(3);
        assert!(ua.starts_with("utest1"));
        assert_eq!(decode_address(Net::Test, &ua).unwrap(), a.address(3));
        assert!(decode_address(Net::Main, &ua).is_err());
        let fvk = decode_fvk(Net::Test, &a.ufvk()).unwrap();
        assert_eq!(fvk.to_bytes(), a.fvk().to_bytes());
    }
}

#[cfg(test)]
mod print {
    /// `cargo test -p tacit-light print_mainnet_address -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn print_mainnet_address() {
        let a = super::Account::from_seed(crate::Net::Main, &[42; 32]).unwrap();
        println!("{}", a.unified_address(0));
    }
}
