//! Local AirPlay PIN pairings: one long-term sender identity and each paired
//! receiver's identifier/long-term key, keyed by the receiver's advertised
//! deviceID (a capability fact, never its name). Stored owner-only under the
//! user's config directory; never part of a support export.
use crate::{
    Error,
    hap::{self, Identity, Kind, Peer},
    session,
    wire::{self, Connection, dict},
};
use plist::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tokio::net::TcpStream;

const MAX_FILE: u64 = 64 * 1024;
const MAX_RECEIVERS: usize = 64;
pub(crate) const MODERN_AGENT: &str = "935.7.1";

pub(crate) struct Store {
    pub identity: Identity,
    pub receivers: BTreeMap<String, Peer>,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("receivers", &self.receivers.len())
            .finish_non_exhaustive()
    }
}

/// `$XDG_CONFIG_HOME/janus/airplay-pairings.plist`, else `~/.config/…`.
pub fn default_store_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("janus").join("airplay-pairings.plist"))
}

fn data(d: &plist::Dictionary, key: &str) -> Result<Vec<u8>, Error> {
    d.get(key)
        .and_then(Value::as_data)
        .map(<[u8]>::to_vec)
        .ok_or(Error::Protocol)
}
impl Store {
    pub(crate) fn load(path: &Path) -> Result<Option<Self>, Error> {
        let bytes = match std::fs::metadata(path) {
            Ok(m) if m.len() > MAX_FILE => return Err(Error::Protocol),
            Ok(_) => std::fs::read(path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let root = wire::decode(&bytes)?;
        let identity = root
            .get("identity")
            .and_then(Value::as_dictionary)
            .ok_or(Error::Protocol)?;
        let id = identity
            .get("id")
            .and_then(Value::as_string)
            .filter(|s| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or(Error::Protocol)?
            .to_owned();
        let seed = data(identity, "seed")?
            .try_into()
            .map_err(|_| Error::Protocol)?;
        let mut receivers = BTreeMap::new();
        if let Some(list) = root.get("receivers") {
            let list = list.as_dictionary().ok_or(Error::Protocol)?;
            if list.len() > MAX_RECEIVERS {
                return Err(Error::Protocol);
            }
            for (device, peer) in list {
                let peer = peer.as_dictionary().ok_or(Error::Protocol)?;
                let pid = data(peer, "id")?;
                if pid.is_empty() || pid.len() > 64 {
                    return Err(Error::Protocol);
                }
                let key = data(peer, "key")?.try_into().map_err(|_| Error::Protocol)?;
                receivers.insert(device.clone(), Peer { id: pid, key });
            }
        }
        Ok(Some(Self {
            identity: Identity { id, seed },
            receivers,
        }))
    }
    pub(crate) fn load_or_new(path: &Path) -> Result<Self, Error> {
        match Self::load(path)? {
            Some(store) => Ok(store),
            None => Ok(Self {
                identity: Identity::generate()?,
                receivers: BTreeMap::new(),
            }),
        }
    }
    /// Atomic replace; directory 0700 and file 0600 on Unix.
    pub(crate) fn save(&self, path: &Path) -> Result<(), Error> {
        if self.receivers.len() > MAX_RECEIVERS {
            return Err(Error::Protocol);
        }
        let receivers = self
            .receivers
            .iter()
            .map(|(device, peer)| {
                (
                    device.clone(),
                    dict([
                        ("id", Value::Data(peer.id.clone())),
                        ("key", Value::Data(peer.key.to_vec())),
                    ]),
                )
            })
            .collect::<plist::Dictionary>();
        let root = dict([
            (
                "identity",
                dict([
                    ("id", Value::String(self.identity.id.clone())),
                    ("seed", Value::Data(self.identity.seed.to_vec())),
                ]),
            ),
            ("receivers", Value::Dictionary(receivers)),
        ]);
        let bytes = wire::encode(&root)?;
        let dir = path.parent().ok_or(Error::Protocol)?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
        let temp = path.with_extension("plist.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }
}

/// Whether a stored pairing exists for this receiver's deviceID.
pub(crate) fn is_paired(device: &str) -> bool {
    default_store_path()
        .and_then(|p| Store::load(&p).ok().flatten())
        .is_some_and(|s| s.receivers.contains_key(device))
}

/// A receiver showing its one-time code, waiting for the user to type it.
/// The code is only valid on this connection.
pub struct PinPairing {
    connection: Connection<TcpStream>,
    kind: Kind,
    device: String,
    path: PathBuf,
    store: Store,
}
impl std::fmt::Debug for PinPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinPairing").finish_non_exhaustive()
    }
}
/// Explicit user action only: asks a PIN-pairing receiver to show its code.
pub async fn begin_pin_pairing(endpoints: &[SocketAddr]) -> Result<PinPairing, Error> {
    let path = default_store_path().ok_or(Error::NoPairingStore)?;
    let (mut connection, info, _) = session::connect(endpoints).await?;
    let kind = info.hap.ok_or(Error::Unsupported)?;
    if info.password {
        return Err(Error::PairingRequired);
    }
    let device = info.device.clone().ok_or(Error::InvalidResponse {
        stage: "GET /info",
        reason: "missing deviceID",
    })?;
    let store = Store::load_or_new(&path)?;
    connection.set_agent(MODERN_AGENT);
    hap::start_pin(&mut connection, kind, &store.identity).await?;
    Ok(PinPairing {
        connection,
        kind,
        device,
        path,
        store,
    })
}
impl PinPairing {
    /// Completes pair-setup with the code, proves it with a pair-verify on the
    /// same connection, then stores the receiver's long-term key.
    pub async fn finish(mut self, pin: &str) -> Result<(), Error> {
        let peer = hap::setup(&mut self.connection, self.kind, &self.store.identity, pin).await?;
        hap::verify(&mut self.connection, self.kind, &self.store.identity, &peer).await?;
        if !self.store.receivers.contains_key(&self.device)
            && self.store.receivers.len() >= MAX_RECEIVERS
        {
            return Err(Error::Protocol);
        }
        self.store.receivers.insert(self.device, peer);
        self.store.save(&self.path)
    }
}
