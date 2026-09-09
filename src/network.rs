//! Locations on other machines: what a server address may look like, mounting one, and the
//! servers connected to before.
//!
//! Everything goes through GIO's virtual filesystem, so what can be reached is whatever
//! gvfs has a backend for. A system without gvfs supports no scheme but `file:`, and the
//! places that offer network locations ask `address_schemes()` before offering any.

use crate::gio::prelude::*;
use crate::gtk::prelude::*;
use crate::{gio, glib, gtk};

/// The schemes a server address can be typed with, in the order they are named to the user.
/// A share reached over HTTPS is WebDAV, which is `davs:`; plain `https:` is there for the
/// backends that read a web address as it stands.
const ADDRESS_SCHEMES: [&str; 11] = [
    "smb", "sftp", "ssh", "ftp", "ftps", "nfs", "dav", "davs", "afp", "http", "https",
];

/// The other schemes that still stand for another machine: mounts an online account made,
/// and network browsing itself. Hardware that speaks a protocol of its own, a phone over
/// MTP or a camera, is not here: it is a device, and belongs with the disks.
const OTHER_SCHEMES: [&str; 3] = ["google-drive", "dns-sd", "network"];

/// How many servers are kept in the list of the ones connected to before.
const MAX_SERVERS: usize = 10;

/// Where network browsing starts, when there is a backend for it.
pub const NETWORK_URI: &str = "network:///";

/// The address schemes this system can actually mount, in the order above. Empty where
/// gvfs is not installed, which is to say where no server can be reached at all.
pub fn address_schemes() -> Vec<&'static str> {
    let supported = gio::Vfs::default().supported_uri_schemes();
    ADDRESS_SCHEMES
        .into_iter()
        .filter(|scheme| supported.iter().any(|s| s == scheme))
        .collect()
}

/// Whether the virtual filesystem can open addresses of this scheme at all. Local files
/// always work; everything else is a gvfs backend that is installed or is not.
pub fn supports(scheme: &str) -> bool {
    scheme == "file"
        || gio::Vfs::default()
            .supported_uri_schemes()
            .iter()
            .any(|s| s == scheme)
}

/// Whether network browsing is on offer, `network:///` being a backend like any other.
pub fn can_browse() -> bool {
    gio::Vfs::default()
        .supported_uri_schemes()
        .iter()
        .any(|s| s == "network")
}

/// Whether `file` lives on another machine rather than on a disk of this one.
pub fn is_network(file: &gio::File) -> bool {
    file.uri_scheme().is_some_and(|scheme| {
        let scheme = scheme.as_str();
        ADDRESS_SCHEMES.contains(&scheme) || OTHER_SCHEMES.contains(&scheme)
    })
}

/// The location a typed address stands for, or None while what is typed is not one: an
/// address has the shape of an address, and a scheme this system can mount.
pub fn address(text: &str) -> Option<gio::File> {
    let (scheme, rest) = split_address(text)?;
    if !address_schemes().contains(&scheme.as_str()) {
        return None;
    }
    // Written out again rather than passed on as typed: a scheme in capitals is the same
    // scheme, and the backend is looked up by the one in the URI.
    Some(gio::File::for_uri(&format!("{scheme}://{rest}")))
}

/// The scheme, lowercased, and what follows it, while `text` has the shape of an address:
/// a scheme, `://`, and a host to go with it.
fn split_address(text: &str) -> Option<(String, &str)> {
    let (scheme, rest) = text.trim().split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    // Whatever comes before the share, the query or the fragment is the host, with the
    // user name in front of it where one was typed.
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or_default();
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_lowercase(), rest))
}

/// Mount whatever `file` is on, asking for a password through the window `parent` is in.
/// A location already mounted is nothing to do.
pub async fn mount(file: &gio::File, parent: &impl IsA<gtk::Widget>) -> Result<(), glib::Error> {
    let window = parent.root().and_downcast::<gtk::Window>();
    let op = gtk::MountOperation::new(window.as_ref());
    match file
        .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&op))
        .await
    {
        Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
        result => result,
    }
}

/// The servers connected to before, the most recent first.
pub fn servers() -> Vec<String> {
    crate::prefs::settings()
        .strv("network-servers")
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Put `uri` at the head of that list, dropping the oldest once it is full.
pub fn remember(uri: &str) {
    let mut servers = servers();
    servers.retain(|s| s != uri);
    servers.insert(0, uri.to_string());
    servers.truncate(MAX_SERVERS);
    save(&servers);
}

pub fn forget(uri: &str) {
    let mut servers = servers();
    servers.retain(|s| s != uri);
    save(&servers);
}

fn save(servers: &[String]) {
    let servers: Vec<&str> = servers.iter().map(String::as_str).collect();
    if let Err(e) = crate::prefs::settings().set_strv("network-servers", servers) {
        glib::g_warning!("spiral", "cannot save the server list: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_needs_a_scheme_and_a_host() {
        for text in [
            "",
            "server",
            "smb://",
            "://server",
            "smb:///share",
            "/srv/share",
        ] {
            assert!(
                split_address(text).is_none(),
                "“{text}” was taken for an address"
            );
        }
    }

    #[test]
    fn an_address_keeps_its_host_and_loses_the_case_of_its_scheme() {
        assert_eq!(
            split_address("  SMB://user@server/share  "),
            Some(("smb".to_string(), "user@server/share"))
        );
        assert_eq!(
            split_address("sftp://host"),
            Some(("sftp".to_string(), "host"))
        );
    }
}
