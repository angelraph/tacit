//! A tacit home directory: one per network, holding this user's keys and
//! wallets.
//!
//! ```text
//! <home>/config.json         network, servers, wallet birthday
//! <home>/seed                wallet seed (hex)
//! <home>/seat.key            X25519 key this user is known by in vault rooms (hex)
//! <home>/wallet.sqlite       the personal shielded wallet
//! <home>/vaults/<name>/      one per vault this user holds a seat in:
//!     share.json             seat number, FROST key share, group key, viewing key
//!     wallet.sqlite          the vault's view-and-build wallet
//! ```
//!
//! Secrets are stored unencrypted, which is fine for regtest and testnet and
//! not for real money; see docs/THREAT_MODEL.md.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tacit_core::envelope::SeatKey;
use tacit_vault::frost::keys::{KeyPackage, PublicKeyPackage};
use tacit_vault::session::{SeatPub, VaultShare};
use tacit_vault::Vault;
use tacit_wallet::relay::Relay;
use tacit_wallet::{connect, GrpcClient, Wallet};
use zcash_client_sqlite::AccountUuid;
use zcash_keys::keys::UnifiedSpendingKey;
use zip32::AccountId;

use crate::net::{Net, NetName};

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub network: String,
    /// Light wallet (lightwalletd gRPC) server.
    pub server: String,
    /// Tacit coordinator for vault rooms and the auction registry.
    pub coordinator: String,
    /// Chain height the personal wallet was created at.
    pub birthday: u32,
}

pub struct Home {
    pub dir: PathBuf,
    pub net: NetName,
    pub params: Net,
    pub cfg: Config,
}

/// `~/.tacit/<network>` unless told otherwise.
pub fn default_dir(net: NetName) -> PathBuf {
    let base = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".tacit").join(net.to_string())
}

fn write_secret(path: &Path, hex_value: &str) -> Result<()> {
    fs::write(path, hex_value).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn read_hex(path: &Path) -> Result<Vec<u8>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    hex::decode(text.trim()).with_context(|| format!("{} is not hex", path.display()))
}

impl Home {
    pub fn exists(dir: &Path) -> bool {
        dir.join("config.json").exists()
    }

    /// Writes a fresh home. The caller creates the wallet account.
    pub fn create(dir: PathBuf, net: NetName, cfg: Config, seed: &[u8]) -> Result<Self> {
        if Self::exists(&dir) {
            bail!("{} already holds a tacit home; pass --home to use another directory", dir.display());
        }
        fs::create_dir_all(dir.join("vaults"))?;
        write_secret(&dir.join("seed"), &hex::encode(seed))?;
        write_secret(&dir.join("seat.key"), &hex::encode(SeatKey::generate().to_bytes()))?;
        fs::write(dir.join("config.json"), serde_json::to_string_pretty(&cfg)?)?;
        Ok(Home { dir, net, params: net.params(), cfg })
    }

    pub fn open(dir: PathBuf, net: NetName) -> Result<Self> {
        let path = dir.join("config.json");
        let text = fs::read_to_string(&path)
            .map_err(|_| anyhow!("no tacit home at {}; run `tacit --network {net} init` first", dir.display()))?;
        let cfg: Config = serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        if cfg.network != net.to_string() {
            bail!("{} is a {} home, not {net}", dir.display(), cfg.network);
        }
        Ok(Home { dir, net, params: net.params(), cfg })
    }

    pub fn seed(&self) -> Result<Vec<u8>> {
        read_hex(&self.dir.join("seed"))
    }

    pub fn seat_key(&self) -> Result<SeatKey> {
        let b: [u8; 32] = read_hex(&self.dir.join("seat.key"))?
            .try_into()
            .map_err(|_| anyhow!("seat.key must be 32 bytes"))?;
        Ok(SeatKey::from_bytes(b))
    }

    pub fn usk(&self) -> Result<UnifiedSpendingKey> {
        UnifiedSpendingKey::from_seed(&self.params, &self.seed()?, AccountId::ZERO)
            .map_err(|e| anyhow!("deriving spending key: {e:?}"))
    }

    pub async fn client(&self) -> Result<GrpcClient> {
        connect(&self.cfg.server)
            .await
            .with_context(|| format!("can't reach the light wallet server at {}", self.cfg.server))
    }

    pub fn relay(&self) -> Relay {
        Relay::new(&self.cfg.coordinator)
    }

    pub fn wallet(&self) -> Result<Wallet<Net>> {
        Ok(Wallet::open(self.dir.join("wallet.sqlite"), self.params.clone())?)
    }

    /// The personal wallet and its one account.
    pub fn personal(&self) -> Result<(Wallet<Net>, AccountUuid)> {
        let wallet = self.wallet()?;
        let account = *wallet.accounts()?.first().ok_or_else(|| anyhow!("wallet has no account"))?;
        Ok((wallet, account))
    }

    fn vault_dir(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("vault names may only use letters, digits, - and _");
        }
        Ok(self.dir.join("vaults").join(name))
    }

    pub fn vault_names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = fs::read_dir(self.dir.join("vaults"))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("share.json").exists())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn ensure_new_vault(&self, name: &str) -> Result<()> {
        if self.vault_dir(name)?.join("share.json").exists() {
            bail!("you already have a vault called {name}");
        }
        Ok(())
    }

    pub fn save_vault(&self, name: &str, share: &VaultShare, seats: &[SeatPub], birthday: u32) -> Result<()> {
        let dir = self.vault_dir(name)?;
        fs::create_dir_all(&dir)?;
        let file = ShareFile {
            seat: share.seat,
            seats: seats.iter().map(hex::encode).collect(),
            key_package: hex::encode(share.key_package.serialize()?),
            public_key_package: hex::encode(share.vault.public_key_package().serialize()?),
            fvk: hex::encode(share.vault.fvk().to_bytes()),
            birthday,
        };
        write_secret(&dir.join("share.json"), &serde_json::to_string_pretty(&file)?)
    }

    pub fn load_vault(&self, name: &str) -> Result<VaultSeat> {
        let dir = self.vault_dir(name)?;
        let text = fs::read_to_string(dir.join("share.json")).map_err(|_| {
            anyhow!("no vault called {name}; `tacit vault list` shows the ones you hold a seat in")
        })?;
        let f: ShareFile = serde_json::from_str(&text)?;
        let pkp = PublicKeyPackage::deserialize(&hex::decode(&f.public_key_package)?)?;
        let fvk: [u8; 96] = hex::decode(&f.fvk)?.try_into().map_err(|_| anyhow!("bad viewing key in share.json"))?;
        let seats = f
            .seats
            .iter()
            .map(|s| hex::decode(s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| anyhow!("bad seat in share.json")))
            .collect::<Result<_>>()?;
        Ok(VaultSeat {
            name: name.to_string(),
            dir,
            seat: f.seat,
            seats,
            key_package: KeyPackage::deserialize(&hex::decode(&f.key_package)?)?,
            vault: Vault::from_parts(pkp, &fvk)?,
            birthday: f.birthday,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct ShareFile {
    seat: u16,
    seats: Vec<String>,
    key_package: String,
    public_key_package: String,
    fvk: String,
    birthday: u32,
}

/// This user's seat in one vault.
pub struct VaultSeat {
    pub name: String,
    dir: PathBuf,
    pub seat: u16,
    pub seats: Vec<SeatPub>,
    pub key_package: KeyPackage,
    pub vault: Vault,
    pub birthday: u32,
}

impl VaultSeat {
    /// The vault's own wallet, kept apart from the personal one so that
    /// payments between the two are scanned like any other.
    pub async fn wallet(&self, home: &Home, client: &mut GrpcClient) -> Result<(Wallet<Net>, AccountUuid)> {
        let mut wallet = Wallet::open(self.dir.join("wallet.sqlite"), home.params.clone())?;
        let account = match wallet.accounts()?.first() {
            Some(a) => *a,
            None => {
                let birthday = Wallet::<Net>::birthday_at(client, self.birthday as u64).await?;
                wallet.import_vault(&self.name, &self.vault, &birthday)?
            }
        };
        Ok((wallet, account))
    }
}
