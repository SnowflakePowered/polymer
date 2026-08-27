use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{
    DeviceAddr, Error, Result,
    bredr::hci::{Command, Event, HciHost, Opcode},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkKey {
    pub value: [u8; 16],
    pub key_type: u8,
}

pub trait LinkKeyStore: Send + Sync {
    fn get(&self, addr: DeviceAddr) -> Result<Option<LinkKey>>;
    fn put(&self, addr: DeviceAddr, key: LinkKey) -> Result<()>;
    fn remove(&self, addr: DeviceAddr) -> Result<()>;
}

pub struct FileLinkKeyStore {
    path: PathBuf,
    keys: Mutex<HashMap<DeviceAddr, LinkKey>>,
}

impl FileLinkKeyStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut keys = HashMap::new();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let mut fields = line.split_whitespace();
                    let (Some(addr), Some(key), Some(key_type)) =
                        (fields.next(), fields.next(), fields.next())
                    else {
                        continue;
                    };
                    if let (Ok(addr), Some(value), Ok(key_type)) = (
                        addr.parse(),
                        decode_key(key),
                        u8::from_str_radix(key_type, 16),
                    ) {
                        keys.insert(addr, LinkKey { value, key_type });
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            path,
            keys: Mutex::new(keys),
        })
    }

    fn save(&self, keys: &HashMap<DeviceAddr, LinkKey>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut entries: Vec<_> = keys.iter().collect();
        entries.sort_by_key(|(addr, _)| addr.bytes());
        let mut contents = String::from("# Snowglass Bluetooth BR/EDR link keys\n");
        for (addr, key) in entries {
            contents.push_str(&format!(
                "{addr} {} {:02x}\n",
                encode_key(key.value),
                key.key_type
            ));
        }
        let temporary = temporary_path(&self.path);
        fs::write(&temporary, contents)?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

impl LinkKeyStore for FileLinkKeyStore {
    fn get(&self, addr: DeviceAddr) -> Result<Option<LinkKey>> {
        Ok(self
            .keys
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .get(&addr)
            .copied())
    }

    fn put(&self, addr: DeviceAddr, key: LinkKey) -> Result<()> {
        let mut keys = self.keys.lock().map_err(|_| Error::ChannelClosed)?;
        keys.insert(addr, key);
        self.save(&keys)
    }

    fn remove(&self, addr: DeviceAddr) -> Result<()> {
        let mut keys = self.keys.lock().map_err(|_| Error::ChannelClosed)?;
        keys.remove(&addr);
        self.save(&keys)
    }
}

#[derive(Clone, Debug)]
pub enum PinCode {
    /// Adapter address, in the raw reversed order expected by a Wii Remote
    /// bonded using its red SYNC button.
    LocalAddress,
    /// Wii Remote address, in reversed order, for temporary 1+2 pairing.
    RemoteAddress,
    Fixed(Vec<u8>),
}

impl PinCode {
    fn resolve(&self, local: DeviceAddr, remote: DeviceAddr) -> Option<Vec<u8>> {
        let pin = match self {
            Self::LocalAddress => local.hci_bytes().to_vec(),
            Self::RemoteAddress => remote.hci_bytes().to_vec(),
            Self::Fixed(value) => value.clone(),
        };
        (pin.len() <= 16).then_some(pin)
    }
}

pub struct PairingAgent {
    host: Arc<HciHost>,
    keys: Arc<dyn LinkKeyStore>,
    pins: Mutex<HashMap<DeviceAddr, PinCode>>,
}

impl PairingAgent {
    pub fn new(host: Arc<HciHost>, keys: Arc<dyn LinkKeyStore>) -> Self {
        Self {
            host,
            keys,
            pins: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_pin(&self, addr: DeviceAddr, pin: PinCode) -> Result<()> {
        self.pins
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .insert(addr, pin);
        Ok(())
    }

    pub fn clear_pin(&self, addr: DeviceAddr) -> Result<()> {
        self.pins
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .remove(&addr);
        Ok(())
    }

    pub fn forget(&self, addr: DeviceAddr) -> Result<()> {
        self.clear_pin(addr)?;
        self.keys.remove(addr)
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut events = self.host.events();
        loop {
            self.handle(events.recv().await.map_err(|_| Error::ChannelClosed)?)
                .await?;
        }
    }

    pub async fn handle(&self, event: Event) -> Result<()> {
        match event {
            Event::LinkKeyRequest { addr } => {
                let command = match self.keys.get(addr)? {
                    Some(key) => Command::link_key_reply(addr, key.value),
                    None => Command::address_only(Opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY, addr),
                };
                self.host
                    .execute_checked(command, "Link Key Request Reply")
                    .await?;
            }
            Event::LinkKeyNotification {
                addr,
                key,
                key_type,
            } => self.keys.put(
                addr,
                LinkKey {
                    value: key,
                    key_type,
                },
            )?,
            Event::PinCodeRequest { addr } => {
                let pin = self
                    .pins
                    .lock()
                    .map_err(|_| Error::ChannelClosed)?
                    .get(&addr)
                    .cloned();
                let command = pin
                    .and_then(|pin| pin.resolve(self.host.info().address, addr))
                    .and_then(|pin| Command::pin_reply(addr, &pin))
                    .unwrap_or_else(|| {
                        Command::address_only(Opcode::PIN_CODE_REQUEST_NEGATIVE_REPLY, addr)
                    });
                self.host
                    .execute_checked(command, "PIN Code Request Reply")
                    .await?;
            }
            Event::IoCapabilityRequest { addr } => {
                self.host
                    .execute_checked(
                        Command::io_capability_reply(addr),
                        "IO Capability Request Reply",
                    )
                    .await?;
            }
            Event::UserConfirmationRequest { addr, .. } => {
                self.host
                    .execute_checked(
                        Command::address_only(Opcode::USER_CONFIRMATION_REQUEST_REPLY, addr),
                        "User Confirmation Request Reply",
                    )
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn encode_key(key: [u8; 16]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn decode_key(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut key = [0; 16];
    for (index, slot) in key.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(key)
}
fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".tmp");
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codec_round_trip() {
        let key = [0x5a; 16];
        assert_eq!(decode_key(&encode_key(key)), Some(key));
    }

    #[test]
    fn wiimote_pin_uses_raw_reversed_address() {
        let local = DeviceAddr::public([1, 2, 3, 4, 5, 6]);
        assert_eq!(
            PinCode::LocalAddress
                .resolve(local, DeviceAddr::public([0; 6]))
                .unwrap(),
            [6, 5, 4, 3, 2, 1]
        );
    }
}
