//! Locations on other machines: what a server address may look like, mounting one, and the
//! servers connected to before.
//!
//! Everything goes through GIO's virtual filesystem, so what can be reached is whatever
//! gvfs has a backend for. A system without gvfs supports no scheme but `file:`, and the
//! places that offer network locations ask `address_schemes()` before offering any.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::gio::prelude::*;
use crate::gtk::prelude::*;
use crate::{gio, glib, gtk};

/// The schemes a server address can be typed with, in the order they are named to the user.
/// A share reached over HTTPS is WebDAV, which is `davs:`; plain `https:` is there for the
/// backends that read a web address as it stands.
const ADDRESS_SCHEMES: [&str; 12] = [
    "smb", "sftp", "ssh", "ftp", "ftps", "ftpis", "nfs", "dav", "davs", "afp", "http", "https",
];

/// The other schemes that still stand for another machine: mounts an online account made,
/// WebDAV found by name on the local network, and network browsing itself. Hardware that
/// speaks a protocol of its own, a phone over MTP or a camera, is not here: it is a device,
/// and belongs with the disks.
const OTHER_SCHEMES: [&str; 6] = [
    "google-drive",
    "onedrive",
    "dav+sd",
    "davs+sd",
    "dns-sd",
    "network",
];

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

/// Filesystems the kernel mounts from another machine, or that a FUSE program fills from
/// one: a share mounted from fstab, or a cloud drive mounted by rclone, rather than through
/// gvfs.
const NETWORK_FILESYSTEMS: [&str; 11] = [
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "ceph",
    "glusterfs",
    "davfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.s3fs",
];

/// Whether the mount at `root` is on another machine: an address gvfs mounted, or a local
/// path the kernel mounted a share or a cloud drive at.
pub fn is_network_mount(root: &gio::File, mounts: &MountTable) -> bool {
    is_network(root)
        || (root.is_native()
            && root
                .path()
                .and_then(|path| mounts.at(&path))
                .is_some_and(|m| NETWORK_FILESYSTEMS.contains(&m.fstype.as_str())))
}

pub struct KernelMount {
    pub fstype: String,
    /// The filesystem's own options, as the kernel lists them.
    pub options: String,
}

/// The kernel's table of mounts, read once for all the mounts looked up at a time.
pub struct MountTable(String);

impl MountTable {
    pub fn read() -> Self {
        Self(std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default())
    }

    /// What is mounted at `path`: the last one mounted there, which hides those before it.
    pub fn at(&self, path: &std::path::Path) -> Option<KernelMount> {
        mount_in(&self.0, path.to_str()?)
    }
}

fn mount_in(table: &str, at: &str) -> Option<KernelMount> {
    table.lines().rev().find_map(|line| {
        let (fields, rest) = line.split_once(" - ")?;
        let point = fields
            .split(' ')
            .nth(4)?
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let mut rest = rest.split(' ');
        let fstype = rest.next()?;
        // Past the source.
        let options = rest.nth(1).unwrap_or_default();
        (point == at).then(|| KernelMount {
            fstype: fstype.to_string(),
            options: options.to_string(),
        })
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
/// A location already mounted is nothing to do. Cancelling `cancellable` gives the attempt
/// up. gvfs goes on with a mount its client has left, and asks for the password when it
/// gets that far; `wanted` says whether anyone still waits for this attempt, and a question
/// nobody waits for is turned down.
pub async fn mount(
    file: &gio::File,
    parent: &impl IsA<gtk::Widget>,
    cancellable: &gio::Cancellable,
    wanted: impl Fn() -> bool + 'static,
) -> Result<(), glib::Error> {
    let window = parent.root().and_downcast::<gtk::Window>();
    let op = gtk::MountOperation::new(window.as_ref());
    let server = server_of(file);
    let _waiting = Waiting::start(&server, Rc::new(wanted));
    for signal in ["ask-password", "ask-question"] {
        let server = server.clone();
        op.connect_local(signal, false, move |values| {
            if !Waiting::for_server(&server) {
                let op = values[0].get::<gio::MountOperation>().unwrap();
                op.reply(gio::MountOperationResult::Aborted);
                op.stop_signal_emission_by_name(signal);
            }
            None
        });
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    file.mount_enclosing_volume(
        gio::MountMountFlags::NONE,
        Some(&op),
        Some(cancellable),
        move |result| {
            let _ = tx.send(result);
        },
    );
    match rx.await {
        Ok(Err(e)) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
        Ok(result) => result,
        Err(_) => Err(glib::Error::new(gio::IOErrorEnum::Cancelled, "")),
    }
}

/// The protocol an entry of `network:///` is reached by, as the tag that names it there:
/// the page lists a machine's Samba server and its NFS exports under much the same names.
/// None for anything else.
pub fn protocol_tag(info: &gio::FileInfo) -> Option<&'static str> {
    if crate::file_utils::file_of(info).uri_scheme()? != "network" {
        return None;
    }
    scheme_tag(&crate::file_utils::target_of(info)?.uri_scheme()?)
}

/// The secure variant of a protocol is named apart, so an encrypted server shows as one.
/// Protocol names are the same in every language.
fn scheme_tag(scheme: &str) -> Option<&'static str> {
    Some(match scheme {
        "smb" => "SMB",
        "nfs" => "NFS",
        "sftp" => "SFTP",
        "ftp" => "FTP",
        "dav" | "dav+sd" => "DAV",
        "davs" | "davs+sd" => "DAVS",
        "afp" => "AFP",
        _ => return None,
    })
}

/// Where `file`, just mounted, is found. A backend may register its mount under an address
/// of its own: gvfs's NFS backend keeps no port, while `network:///` lists every export with
/// one, so the export is never found by the address it was asked for, and mounting it again
/// only answers that it already is. The address without the port is then where it is. An
/// error is the location still not mounted.
pub async fn mounted_at(file: &gio::File) -> Result<gio::File, glib::Error> {
    let Some(error) = not_mounted(file).await else {
        return Ok(file.clone());
    };
    match without_port(file) {
        Some(bare) if not_mounted(&bare).await.is_none() => Ok(bare),
        _ => Err(error),
    }
}

async fn not_mounted(file: &gio::File) -> Option<glib::Error> {
    file.query_info_future(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .err()
    .filter(|e| e.matches(gio::IOErrorEnum::NotMounted))
}

/// `file` with the port left out of its address, or None where it names none.
fn without_port(file: &gio::File) -> Option<gio::File> {
    let flags = glib::UriFlags::ENCODED;
    let parsed = glib::Uri::parse(&file.uri(), flags).ok()?;
    if parsed.port() == -1 {
        return None;
    }
    let bare = glib::Uri::build(
        flags,
        &parsed.scheme(),
        parsed.userinfo().as_deref(),
        parsed.host().as_deref(),
        -1,
        &parsed.path(),
        parsed.query().as_deref(),
        parsed.fragment().as_deref(),
    );
    Some(gio::File::for_uri(&bare.to_str()))
}

/// The server a location is on, as far as its address tells: the scheme, the user, the host
/// and the port.
fn server_of(file: &gio::File) -> String {
    let uri = file.uri();
    match glib::Uri::parse(&uri, glib::UriFlags::NONE) {
        Ok(parsed) => format!(
            "{}://{}@{}:{}",
            parsed.scheme(),
            parsed.userinfo().unwrap_or_default(),
            parsed.host().unwrap_or_default(),
            parsed.port()
        ),
        Err(_) => uri.to_string(),
    }
}

type Wanted = Rc<dyn Fn() -> bool>;

thread_local! {
    static WAITING: RefCell<Vec<(u64, String, Wanted)>> = const { RefCell::new(Vec::new()) };
    static NEXT_WAITING: Cell<u64> = const { Cell::new(0) };
}

/// A mount under way, for as long as its attempt lasts. gvfs mounts a server once for all
/// who ask for it at the same time and puts its questions to whichever asked first, which
/// may be one nobody waits for any more while another still does.
struct Waiting(u64);

impl Waiting {
    fn start(server: &str, wanted: Wanted) -> Self {
        let id = NEXT_WAITING.replace(NEXT_WAITING.get() + 1);
        WAITING.with(|w| w.borrow_mut().push((id, server.to_string(), wanted)));
        Self(id)
    }

    /// Whether anyone still waits for a mount of `server`.
    fn for_server(server: &str) -> bool {
        let wanted: Vec<Wanted> = WAITING.with(|w| {
            w.borrow()
                .iter()
                .filter(|(_, s, _)| s == server)
                .map(|(_, _, wanted)| wanted.clone())
                .collect()
        });
        wanted.iter().any(|wanted| wanted())
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.with(|w| w.borrow_mut().retain(|(id, _, _)| *id != self.0));
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
    fn locations_on_one_server_share_it() {
        let server = |uri: &str| server_of(&gio::File::for_uri(uri));
        assert_eq!(server("sftp://host/srv"), server("sftp://host/home/a"));
        assert_ne!(server("sftp://host/"), server("sftp://host:2222/"));
        assert_ne!(server("sftp://host/"), server("sftp://me@host/"));
        assert_ne!(server("sftp://host/"), server("ftp://host/"));
    }

    #[test]
    fn an_entry_of_the_network_page_is_tagged_by_its_target() {
        let entry = |at: &str, target: &str| {
            let info = gio::FileInfo::new();
            info.set_file_type(gio::FileType::Shortcut);
            info.set_attribute_string("standard::target-uri", target);
            info.set_attribute_object("standard::file", &gio::File::for_uri(at));
            protocol_tag(&info)
        };
        let listed = "network:///dnssd-server-x";
        assert_eq!(entry(listed, "nfs://host:2049/srv/nfs/bak"), Some("NFS"));
        assert_eq!(entry(listed, "smb://host.local:445/"), Some("SMB"));
        assert_eq!(
            entry(listed, "davs+sd://host._webdavs._tcp.local/"),
            Some("DAVS")
        );
        assert_eq!(entry(listed, "gopher://host/"), None);
        assert_eq!(entry("computer:///root.link", "file:///"), None);
    }

    #[test]
    fn an_address_without_its_port_keeps_the_rest() {
        let bare = |uri: &str| without_port(&gio::File::for_uri(uri)).map(|f| f.uri().to_string());
        assert_eq!(
            bare("nfs://thinkcentre.local:2049/srv/nfs/my%20camera").as_deref(),
            Some("nfs://thinkcentre.local/srv/nfs/my%20camera")
        );
        assert_eq!(
            bare("nfs://me@[fd8b::1]:2049/srv/nfs").as_deref(),
            Some("nfs://me@[fd8b::1]/srv/nfs")
        );
        assert_eq!(bare("nfs://thinkcentre.local/srv/nfs/camera"), None);
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

    #[test]
    fn a_mount_is_read_from_the_last_line_for_its_point() {
        let table = "\
29 1 0:26 / / rw - btrfs /dev/mapper/root rw
80 29 0:50 / /home/u/Google\\040Drive rw - tmpfs tmpfs rw
81 80 0:51 / /home/u/Google\\040Drive rw,nosuid - fuse.rclone gdrive: rw
";
        let drive = mount_in(table, "/home/u/Google Drive").unwrap();
        assert_eq!(drive.fstype, "fuse.rclone");
        assert_eq!(drive.options, "rw");
        assert_eq!(mount_in(table, "/").unwrap().fstype, "btrfs");
        assert!(mount_in(table, "/home/u").is_none());
    }
}
