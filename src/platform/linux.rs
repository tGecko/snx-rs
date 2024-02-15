use std::{collections::HashMap, os::fd::AsRawFd, time::Duration};

use anyhow::anyhow;
use secret_service::{EncryptionType, SecretService};
use tokio::net::UdpSocket;
use tracing::{debug, warn};
use zbus::{dbus_proxy, zvariant, Connection};

pub use xfrm::XfrmConfigurator as IpsecImpl;

use crate::platform::{UdpEncap, UdpSocketExt};

pub mod net;
pub mod xfrm;

const UDP_ENCAP_ESPINUDP: libc::c_int = 2; // from /usr/include/linux/udp.h

#[async_trait::async_trait]
impl UdpSocketExt for UdpSocket {
    fn set_encap(&self, encap: UdpEncap) -> anyhow::Result<()> {
        let stype: libc::c_int = match encap {
            UdpEncap::EspInUdp => UDP_ENCAP_ESPINUDP,
        };

        unsafe {
            let rc = libc::setsockopt(
                self.as_raw_fd(),
                libc::SOL_UDP,
                libc::UDP_ENCAP,
                &stype as *const libc::c_int as _,
                std::mem::size_of::<libc::c_int>() as _,
            );
            if rc != 0 {
                Err(anyhow!("Cannot set UDP_ENCAP socket option, error code: {}", rc))
            } else {
                Ok(())
            }
        }
    }

    fn set_no_check(&self, flag: bool) -> anyhow::Result<()> {
        let disable: libc::c_int = flag.into();
        unsafe {
            let rc = libc::setsockopt(
                self.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NO_CHECK,
                &disable as *const libc::c_int as _,
                std::mem::size_of::<libc::c_int>() as _,
            );
            if rc != 0 {
                Err(anyhow!("Cannot set SO_NO_CHECK socket option, error code: {}", rc))
            } else {
                Ok(())
            }
        }
    }

    async fn send_receive(&self, data: &[u8], timeout: Duration) -> anyhow::Result<Vec<u8>> {
        super::udp_send_receive(self, data, timeout).await
    }
}

pub fn new_tun_config() -> tun::Configuration {
    let mut config = tun::Configuration::default();

    config.platform(|config| {
        config.packet_information(true);
    });

    config
}

pub async fn acquire_password(user_name: &str) -> anyhow::Result<String> {
    let props = HashMap::from([("snx-rs.username", user_name)]);

    debug!("Attempting to acquire password from the keychain");

    let ss = SecretService::connect(EncryptionType::Dh).await;
    if let Ok(ref ss) = ss {
        if let Ok(search_items) = ss.search_items(props.clone()).await {
            if let Some(item) = search_items.unlocked.first() {
                if let Ok(secret) = item.get_secret().await {
                    debug!("Acquired user password from the keychain");
                    return Ok(String::from_utf8_lossy(&secret).into_owned());
                }
            }
        }
    }

    Err(anyhow!("No password in the keychain"))
}

pub async fn store_password(user_name: &str, password: &str) -> anyhow::Result<()> {
    let props = HashMap::from([("snx-rs.username", user_name)]);

    let ss = SecretService::connect(EncryptionType::Dh).await;
    let collection = match ss {
        Ok(ref ss) => match ss.get_default_collection().await {
            Ok(collection) => {
                if let Ok(true) = collection.is_locked().await {
                    debug!("Unlocking secret collection");
                    let _ = collection.unlock().await;
                }
                Some(collection)
            }
            Err(e) => {
                warn!("{}", e);
                None
            }
        },
        Err(ref e) => {
            warn!("{}", e);
            None
        }
    };

    if let Some(collection) = collection {
        debug!("Attempting to store user password in the keychain");
        if let Err(e) = collection
            .create_item(
                &format!("snx-rs - {}", user_name),
                props,
                password.as_bytes(),
                true,
                "text/plain",
            )
            .await
        {
            warn!("Warning: cannot store user password in the keychain: {}", e);
        }
    }

    Ok(())
}

#[dbus_proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<String, zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

pub async fn send_notification(summary: &str, message: &str) -> anyhow::Result<()> {
    let connection = Connection::session().await?;
    let proxy = NotificationsProxy::new(&connection).await?;
    proxy
        .notify(
            "SNX-RS VPN client",
            0,
            "emblem-error",
            summary,
            message,
            &[],
            HashMap::default(),
            10000,
        )
        .await?;
    Ok(())
}
