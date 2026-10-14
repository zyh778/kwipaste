//! mDNS / DNS-SD 广播与发现。
//!
//! 实例名就是设备 id；TXT 里放名称、平台和支持的协议版本范围，不放任何密钥或内容。
//! IPv4 与 IPv6 都广播；链路本地的 IPv6 地址带上收到它的网卡编号（scope id），否则连不上。
//! 协议版本不兼容的设备照样列出，由界面提示升级。

use std::net::{IpAddr, SocketAddr, SocketAddrV6};

use anyhow::{anyhow, Context};
use mdns_sd::{IfKind, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};

use super::protocol::{VersionRange, SERVICE_TYPE};
use crate::db::models::Platform;

const TXT_ID: &str = "id";
const TXT_NAME: &str = "name";
const TXT_PLATFORM: &str = "pf";
/// 支持的最高协议版本；1.x 只写了这一项（它只支持 1）。
const TXT_VERSION: &str = "v";
/// 支持的最低协议版本，v2 起才有，缺省等于最高版本。
const TXT_MIN_VERSION: &str = "vmin";

/// 发现到的一台设备。
#[derive(Debug, Clone)]
pub struct Discovered {
    pub device_id: String,
    pub name: String,
    pub platform: Platform,
    pub protocol: VersionRange,
    pub addresses: Vec<SocketAddr>,
}

pub enum DiscoveryEvent {
    Found(Discovered),
    Lost(String),
}

pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
    device_id: String,
    protocol: VersionRange,
    port: u16,
}

impl Discovery {
    /// 启动广播和浏览；`on_event` 在 mDNS 后台线程上回调。
    pub fn start(
        device_id: &str,
        name: &str,
        platform: Platform,
        protocol: VersionRange,
        port: u16,
        on_event: impl Fn(DiscoveryEvent) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new().map_err(mdns_err)?;
        daemon
            .disable_interface(vec![IfKind::LoopbackV4, IfKind::LoopbackV6])
            .map_err(mdns_err)?;

        let discovery = Self {
            daemon,
            fullname: String::new(),
            device_id: device_id.to_owned(),
            protocol,
            port,
        };
        let fullname = discovery.register(name, platform)?;
        let receiver = discovery.daemon.browse(SERVICE_TYPE).map_err(mdns_err)?;

        let own_id = device_id.to_owned();
        std::thread::Builder::new()
            .name("lan-sync-discovery".to_owned())
            .spawn(move || {
                // daemon 关闭后通道断开，线程随之结束。
                while let Ok(event) = receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(service) => {
                            let port = service.get_port();
                            let addresses = service
                                .get_addresses()
                                .iter()
                                .filter_map(|ip| socket_addr(ip, port));
                            let Some(found) = parse_service(
                                TxtRecord {
                                    id: service.get_property_val_str(TXT_ID),
                                    name: service.get_property_val_str(TXT_NAME),
                                    platform: service.get_property_val_str(TXT_PLATFORM),
                                    version: service.get_property_val_str(TXT_VERSION),
                                    min_version: service.get_property_val_str(TXT_MIN_VERSION),
                                },
                                addresses,
                            ) else {
                                continue;
                            };
                            if found.device_id != own_id {
                                on_event(DiscoveryEvent::Found(found));
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let Some(id) = instance_name(&fullname) else {
                                continue;
                            };
                            if id != own_id {
                                on_event(DiscoveryEvent::Lost(id));
                            }
                        }
                        _ => {}
                    }
                }
                log::debug!("lan sync discovery thread stopped");
            })
            .context("failed to spawn discovery thread")?;

        Ok(Self {
            fullname,
            ..discovery
        })
    }

    /// 改名后重新广播 TXT。
    pub fn update_name(&mut self, name: &str, platform: Platform) {
        if let Err(err) = self.daemon.unregister(&self.fullname) {
            log::warn!("mdns unregister before rename failed: {err}");
        }
        match self.register(name, platform) {
            Ok(fullname) => self.fullname = fullname,
            Err(err) => log::warn!("mdns re-register after rename failed: {err:#}"),
        }
    }

    /// 撤销广播并关闭 daemon；会短暂阻塞等待注销包发出。
    pub fn stop(self) {
        if let Ok(status) = self.daemon.unregister(&self.fullname) {
            let _ = status.recv_timeout(std::time::Duration::from_secs(1));
        }
        if let Ok(status) = self.daemon.shutdown() {
            let _ = status.recv_timeout(std::time::Duration::from_secs(1));
        }
    }

    fn register(&self, name: &str, platform: Platform) -> anyhow::Result<String> {
        let version = self.protocol.max.to_string();
        let min_version = self.protocol.min.to_string();
        let properties = [
            (TXT_ID, self.device_id.as_str()),
            (TXT_NAME, name),
            (TXT_PLATFORM, platform_tag(platform)),
            (TXT_VERSION, version.as_str()),
            (TXT_MIN_VERSION, min_version.as_str()),
        ];
        let host_name = format!("kwikpaste-{}.local.", self.device_id);
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &self.device_id,
            &host_name,
            "",
            self.port,
            &properties[..],
        )
        .map_err(mdns_err)?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        self.daemon.register(info).map_err(mdns_err)?;
        Ok(fullname)
    }
}

/// 广播里的 TXT 字段。
struct TxtRecord<'a> {
    id: Option<&'a str>,
    name: Option<&'a str>,
    platform: Option<&'a str>,
    version: Option<&'a str>,
    min_version: Option<&'a str>,
}

/// mDNS 解析出的地址转成能直接连接的地址。链路本地 IPv6 带上网卡编号，没有编号的连不上，丢掉。
fn socket_addr(ip: &ScopedIp, port: u16) -> Option<SocketAddr> {
    match ip {
        ScopedIp::V4(v4) => Some(SocketAddr::new(IpAddr::V4(*v4.addr()), port)),
        ScopedIp::V6(v6) => {
            let addr = *v6.addr();
            if !addr.is_unicast_link_local() {
                return Some(SocketAddr::new(IpAddr::V6(addr), port));
            }
            let scope = v6.scope_id().index;
            (scope != 0).then(|| SocketAddr::V6(SocketAddrV6::new(addr, port, 0, scope)))
        }
        _ => None,
    }
}

fn parse_service(
    txt: TxtRecord<'_>,
    addresses: impl Iterator<Item = SocketAddr>,
) -> Option<Discovered> {
    let device_id = txt.id?.trim();
    if device_id.is_empty() {
        return None;
    }
    let max = txt.version?.trim().parse::<u16>().ok()?;
    let min = match txt.min_version {
        Some(min) => min.trim().parse::<u16>().ok()?.min(max),
        None => max,
    };

    let platform = match txt.platform? {
        "macos" => Platform::Macos,
        "windows" => Platform::Windows,
        "linux" => Platform::Linux,
        _ => return None,
    };
    let mut unique: Vec<SocketAddr> = Vec::new();
    for addr in addresses {
        if addr.ip().is_loopback() || addr.ip().is_unspecified() || unique.contains(&addr) {
            continue;
        }
        unique.push(addr);
    }
    if unique.is_empty() {
        return None;
    }

    Some(Discovered {
        device_id: device_id.to_owned(),
        name: txt.name.unwrap_or(device_id).to_owned(),
        platform,
        protocol: VersionRange { min, max },
        addresses: unique,
    })
}

fn instance_name(fullname: &str) -> Option<String> {
    let suffix = format!(".{SERVICE_TYPE}");
    fullname.strip_suffix(&suffix).map(str::to_owned)
}

pub fn platform_tag(platform: Platform) -> &'static str {
    match platform {
        Platform::Macos => "macos",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    }
}

fn mdns_err(err: mdns_sd::Error) -> anyhow::Error {
    anyhow!("mdns: {err}")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn txt<'a>(version: Option<&'a str>, min_version: Option<&'a str>) -> TxtRecord<'a> {
        TxtRecord {
            id: Some("abc"),
            name: Some("MacBook"),
            platform: Some("macos"),
            version,
            min_version,
        }
    }

    /// Linux 的广播标签必须能被本版本发现并保持协议协商信息。
    #[test]
    fn discovers_linux_peer() {
        let mut record = txt(Some("3"), Some("2"));
        record.platform = Some("linux");
        let found =
            parse_service(record, ["192.168.1.8:41573".parse().unwrap()].into_iter()).unwrap();
        assert_eq!(found.platform, Platform::Linux);
        assert_eq!(platform_tag(found.platform), "linux");
    }

    #[test]
    fn parses_valid_service_and_drops_loopback() {
        let link_local = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 41573, 0, 7));
        let addresses = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41573),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 41573),
            "192.168.1.8:41573".parse().unwrap(),
            "192.168.1.8:41573".parse().unwrap(),
            link_local,
        ];

        let found = parse_service(txt(Some("3"), Some("2")), addresses.into_iter()).unwrap();

        assert_eq!(found.device_id, "abc");
        assert_eq!(found.platform, Platform::Macos);
        assert_eq!(found.protocol, VersionRange { min: 2, max: 3 });
        assert_eq!(
            found.addresses,
            vec!["192.168.1.8:41573".parse().unwrap(), link_local]
        );
    }

    /// 1.x 只写了 `v`，照样列出（界面提示版本不兼容）；版本读不懂的不列。
    #[test]
    fn reads_version_ranges_from_old_and_new_devices() {
        let addresses = || vec!["192.168.1.8:41573".parse().unwrap()].into_iter();

        let old = parse_service(txt(Some("1"), None), addresses()).unwrap();
        assert_eq!(old.protocol, VersionRange { min: 1, max: 1 });

        assert!(parse_service(txt(Some("x"), None), addresses()).is_none());
        assert!(parse_service(txt(None, Some("2")), addresses()).is_none());
    }

    #[test]
    fn instance_name_strips_service_type() {
        assert_eq!(
            instance_name("abc._kwikpaste._tcp.local.").as_deref(),
            Some("abc")
        );
        assert_eq!(instance_name("abc._other._tcp.local."), None);
    }
}
