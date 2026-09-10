//! What there is to tell about the disk a folder is on: the mount it is under, from the
//! kernel's list of mounts, and the volume and the drive behind that, from UDisks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gettextrs::gettext;

use crate::{gio, glib};

const UDISKS: &str = "org.freedesktop.UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";
const TABLE: &str = "org.freedesktop.UDisks2.PartitionTable";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const NVME: &str = "org.freedesktop.UDisks2.NVMe.Controller";
/// UDisks answers at once or not at all; the page does without it after this long.
const TIMEOUT_MS: i32 = 2_000;

/// A mount as the kernel lists it.
pub struct Mount {
    /// What is mounted: a device node for a disk, a name for the rest ("tmpfs").
    pub source: String,
    pub point: PathBuf,
    pub fstype: String,
    pub options: Vec<String>,
}

impl Mount {
    /// The value of `key=value` among the options.
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find_map(|o| o.strip_prefix(key)?.strip_prefix('='))
    }

    pub fn read_only(&self) -> bool {
        self.options.iter().any(|o| o == "ro")
    }
}

/// The mount `path` is under: the one whose mount point is the longest leading part of
/// it, the last listed where one is mounted over another.
pub fn mount_of(path: &Path) -> Option<Mount> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    // Spaces and the like come escaped as octal, "\040" for a space.
    let unescape = |s: &str| s.replace("\\040", " ").replace("\\011", "\t");
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let source = unescape(fields.next()?);
            let point = PathBuf::from(unescape(fields.next()?));
            let fstype = fields.next()?.to_string();
            let options = fields.next()?.split(',').map(String::from).collect();
            path.starts_with(&point).then_some(Mount {
                source,
                point,
                fstype,
                options,
            })
        })
        .max_by_key(|m| m.point.as_os_str().len())
}

/// The volume behind a device, as UDisks knows it.
#[derive(Default)]
pub struct Volume {
    /// "btrfs", "ext4", "FAT32" and the like.
    pub format: Option<String>,
    pub label: Option<String>,
    /// "LUKS2" for a volume that is an unlocked encrypted one.
    pub encryption: Option<String>,
    /// The number of the partition that holds it, and the name the table gives it.
    pub partition: Option<(u32, String)>,
    /// "GPT" or "MBR".
    pub table: Option<String>,
    pub drive: Option<Drive>,
}

pub struct Drive {
    pub model: String,
    /// What kind of drive, in words: "NVMe SSD", "Hard disk, 7200 RPM", "USB flash drive".
    pub kind: String,
    pub size: u64,
}

type Props = HashMap<String, glib::Variant>;
type Objects = HashMap<String, HashMap<String, Props>>;

/// What UDisks knows of the volume `device` is. `None` without UDisks, or for a device it
/// does not list, which is anything that is not a block device.
pub async fn volume_of(device: &str) -> Option<Volume> {
    let bus = gio::bus_get_future(gio::BusType::System).await.ok()?;
    let reply = bus
        .call_future(
            Some(UDISKS),
            "/org/freedesktop/UDisks2",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            None,
            None,
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
        )
        .await
        .ok()?;
    let objects = objects(&reply.child_value(0));
    // The node the mount names is often a link, /dev/mapper/name to /dev/dm-2.
    let real = std::fs::canonicalize(device)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let names_it = |block: &Props| {
        ["Device", "PreferredDevice"]
            .iter()
            .filter_map(|key| bytestring(block.get(*key)?))
            .chain(
                block
                    .get("Symlinks")
                    .into_iter()
                    .flat_map(|v| v.iter().filter_map(|s| bytestring(&s)).collect::<Vec<_>>()),
            )
            .any(|n| n == device || Some(&n) == real.as_ref())
    };
    let ifaces = objects
        .values()
        .find(|ifaces| ifaces.get(BLOCK).is_some_and(names_it))?;
    let block = &ifaces[BLOCK];
    let mut volume = Volume {
        format: format(block),
        label: string(block, "IdLabel"),
        ..Default::default()
    };
    // An unlocked encrypted volume sits on the one that holds it, which is what is on a
    // partition of a drive.
    let mut holder = ifaces;
    if let Some(backing) = path(block, "CryptoBackingDevice").and_then(|p| objects.get(&p))
        && let Some(backing_block) = backing.get(BLOCK)
    {
        volume.encryption = Some(match string(backing_block, "IdVersion") {
            Some(version) => format!("LUKS{version}"),
            None => "LUKS".to_string(),
        });
        holder = backing;
    }
    if let Some(partition) = holder.get(PARTITION) {
        let number = partition
            .get("Number")
            .and_then(|v| v.get::<u32>())
            .unwrap_or(0);
        volume.partition = Some((number, string(partition, "Name").unwrap_or_default()));
        volume.table = path(partition, "Table")
            .and_then(|t| objects.get(&t))
            .and_then(|i| i.get(TABLE))
            .and_then(|t| string(t, "Type"))
            .map(|t| match t.as_str() {
                "dos" => "MBR".to_string(),
                other => other.to_uppercase(),
            });
    }
    volume.drive = holder
        .get(BLOCK)
        .and_then(|b| path(b, "Drive"))
        .and_then(|d| objects.get(&d))
        .and_then(drive);
    Some(volume)
}

fn drive(ifaces: &HashMap<String, Props>) -> Option<Drive> {
    let d = ifaces.get(DRIVE)?;
    let flag = |key: &str| d.get(key).and_then(|v| v.get::<bool>()).unwrap_or(false);
    let model = [string(d, "Vendor"), string(d, "Model")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let media = string(d, "Media").unwrap_or_default();
    let bus = string(d, "ConnectionBus").unwrap_or_default();
    let rotational = flag("Rotational");
    let rate = d
        .get("RotationRate")
        .and_then(|v| v.get::<i32>())
        .unwrap_or(0);
    let kind = if ifaces.contains_key(NVME) {
        gettext("NVMe SSD")
    } else if media.starts_with("optical") {
        gettext("Optical drive")
    } else if media.starts_with("flash_sd") || bus == "sdio" {
        gettext("SD card")
    } else if bus == "usb" && media == "thumb" {
        gettext("USB flash drive")
    } else if bus == "usb" && rotational {
        gettext("USB hard disk")
    } else if bus == "usb" {
        gettext("USB drive")
    } else if rotational && rate > 0 {
        gettext("Hard disk, %d RPM").replace("%d", &rate.to_string())
    } else if rotational {
        gettext("Hard disk")
    } else {
        gettext("SSD")
    };
    let size = d.get("Size").and_then(|v| v.get::<u64>()).unwrap_or(0);
    Some(Drive { model, kind, size })
}

/// The format of what is on a block: its filesystem, with the version where that is the
/// part people know it by (FAT32).
fn format(block: &Props) -> Option<String> {
    let kind = string(block, "IdType")?;
    Some(match (kind.as_str(), string(block, "IdVersion")) {
        ("vfat", Some(version)) => version,
        ("vfat", None) => "FAT".to_string(),
        _ => kind,
    })
}

/// `a{oa{sa{sv}}}` as maps.
fn objects(v: &glib::Variant) -> Objects {
    let entries = |v: &glib::Variant| -> Vec<(String, glib::Variant)> {
        v.iter()
            .filter_map(|e| Some((e.child_value(0).str()?.to_string(), e.child_value(1))))
            .collect()
    };
    entries(v)
        .into_iter()
        .map(|(path, ifaces)| {
            let ifaces = entries(&ifaces)
                .into_iter()
                .map(|(name, props)| {
                    let props = entries(&props)
                        .into_iter()
                        .map(|(key, value)| (key, value.as_variant().unwrap_or(value)))
                        .collect();
                    (name, props)
                })
                .collect();
            (path, ifaces)
        })
        .collect()
}

/// A string property, when it is there and says something.
fn string(props: &Props, key: &str) -> Option<String> {
    props
        .get(key)?
        .str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// An object path property that points at something.
fn path(props: &Props, key: &str) -> Option<String> {
    string(props, key).filter(|p| p != "/")
}

/// A NUL-terminated byte string property, as UDisks gives device names.
fn bytestring(v: &glib::Variant) -> Option<String> {
    let bytes = v.fixed_array::<u8>().ok()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(bytes).into_owned())
}
