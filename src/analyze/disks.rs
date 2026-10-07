//! Which disk each mount is on, read from `/proc/self/mountinfo` and `/sys`, so the scan stays
//! on the disk `/` is on.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::disk::unescape_octal;

/// Filesystems whose files are on another computer.
const NETWORK: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "ncpfs",
    "afs",
    "9p",
    "ceph",
    "glusterfs",
    "lustre",
    "davfs",
    "sshfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.s3fs",
    "fuse.gvfsd-fuse",
];

/// Where a mount's files are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    /// On the disk `/` is on: scanned.
    SameDisk,
    OtherDisk,
    UsbDisk,
    /// On another computer. Never even looked at: a share that doesn't answer would keep the
    /// scan waiting.
    Network,
    /// Mounted when first used, and not mounted yet: looking would mount it.
    Automount,
    /// Kept in memory (`/tmp`, `/run`), made up by the kernel (`/proc`, `/sys`), or an image
    /// file mounted from a disk, whose space is counted where the file is.
    NotADisk,
}

impl Mount {
    /// Whether the scan goes into the mount, when it scans `other_disks` too or not.
    pub fn is_scanned(self, other_disks: bool) -> bool {
        match self {
            Mount::SameDisk => true,
            Mount::OtherDisk | Mount::UsbDisk => other_disks,
            Mount::Network | Mount::Automount | Mount::NotADisk => false,
        }
    }
}

/// Every mount point, and where its files are.
#[derive(Debug, Default)]
pub struct MountTable(HashMap<PathBuf, Mount>);

impl MountTable {
    pub fn load() -> io::Result<Self> {
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        Ok(Self::parse(&mountinfo, &disks))
    }

    /// Reads `mountinfo`, where `disks` says which disks a mount is on.
    fn parse(mountinfo: &str, disks: &dyn Fn(&Source) -> Vec<Disk>) -> Self {
        let mounts: Vec<(PathBuf, Source)> = mountinfo.lines().filter_map(Source::read).collect();
        // A mount hides what was mounted on the same path before it.
        let root = mounts
            .iter()
            .rev()
            .find(|(path, _)| path == Path::new("/"))
            .map(|(_, source)| disks(source))
            .unwrap_or_default();
        let mounts = mounts
            .into_iter()
            .map(|(path, source)| {
                let mount = classify(&source, &disks(&source), &root);
                (path, mount)
            })
            .collect();
        Self(mounts)
    }

    /// What is mounted on `path`; `None` when it isn't a mount point.
    pub fn get(&self, path: &Path) -> Option<Mount> {
        self.0.get(path).copied()
    }

    /// The mount points the scan goes into, when it scans `other_disks` too or not.
    pub fn scanned(&self, other_disks: bool) -> Vec<PathBuf> {
        self.0
            .iter()
            .filter(|(_, mount)| mount.is_scanned(other_disks))
            .map(|(path, _)| path.clone())
            .collect()
    }

    #[cfg(test)]
    pub fn from_mounts(mounts: impl IntoIterator<Item = (PathBuf, Mount)>) -> Self {
        Self(mounts.into_iter().collect())
    }
}

/// What mountinfo says a mount is mounted from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    /// The filesystem's type, e.g. `ext4`.
    fstype: String,
    /// The filesystem's device number (major, minor); major 0 for one with no block device of
    /// its own, like btrfs, which gives one to each subvolume.
    device: (u32, u32),
    /// What it is mounted from, e.g. `/dev/nvme0n1p2`.
    source: String,
}

impl Source {
    /// Reads a mountinfo line, e.g.
    /// `36 35 98:0 / /mnt rw,noatime master:1 - ext4 /dev/sda1 rw`.
    fn read(line: &str) -> Option<(PathBuf, Source)> {
        let fields: Vec<&str> = line.split(' ').collect();
        let (major, minor) = fields.get(2)?.split_once(':')?;
        let path = unescape_octal(fields.get(4)?);
        // Optional fields come before the `-`; the filesystem's own after it.
        let dash = fields.iter().skip(6).position(|&field| field == "-")? + 6;
        let source = Source {
            fstype: fields.get(dash + 1)?.to_string(),
            device: (major.parse().ok()?, minor.parse().ok()?),
            source: unescape_octal(fields.get(dash + 2)?)
                .to_string_lossy()
                .into_owned(),
        };
        Some((path, source))
    }
}

/// A disk, as far as telling disks apart goes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Disk {
    /// A whole disk, by its folder in `/sys/devices`.
    Block(PathBuf),
    /// A ZFS pool, by name: its datasets have no block device of their own.
    Pool(String),
}

impl Disk {
    /// A loop device, a RAM disk or the like: its files are in memory, or in an image file kept
    /// on a disk. NVMe disks reached through several paths are under /sys/devices/virtual too,
    /// but not under its block folder.
    fn is_virtual(&self) -> bool {
        matches!(self, Disk::Block(sys) if sys.starts_with("/sys/devices/virtual/block"))
    }

    fn is_usb(&self) -> bool {
        matches!(self, Disk::Block(sys) if sys
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with("usb")))
    }
}

/// Where a mount with `source`, on `disks`, has its files, when `/` is on `root`.
fn classify(source: &Source, disks: &[Disk], root: &[Disk]) -> Mount {
    if NETWORK.contains(&source.fstype.as_str()) {
        return Mount::Network;
    }
    if source.fstype == "autofs" {
        return Mount::Automount;
    }
    let disks: Vec<&Disk> = disks.iter().filter(|disk| !disk.is_virtual()).collect();
    if disks.is_empty() {
        Mount::NotADisk
    } else if disks.iter().any(|disk| root.contains(disk)) {
        Mount::SameDisk
    } else if disks.iter().any(|disk| disk.is_usb()) {
        Mount::UsbDisk
    } else {
        Mount::OtherDisk
    }
}

/// The disks a mount's files are on, as `/sys` says: none for a mount not on a disk.
fn disks(source: &Source) -> Vec<Disk> {
    if source.fstype == "zfs" {
        let pool = source.source.split('/').next().unwrap_or_default();
        return vec![Disk::Pool(pool.to_string())];
    }
    let devices = match source.device {
        (0, _) => source.source.split(':').filter_map(block_device).collect(),
        device => vec![device],
    };
    devices
        .into_iter()
        .flat_map(|(major, minor)| {
            whole_disks(Path::new(&format!("/sys/dev/block/{major}:{minor}")), 0)
        })
        .collect()
}

/// The device number of the block device at `path`, e.g. `/dev/mapper/root`.
fn block_device(path: &str) -> Option<(u32, u32)> {
    if !path.starts_with("/dev/") {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    let device = meta.rdev();
    meta.file_type()
        .is_block_device()
        .then(|| (rustix::fs::major(device), rustix::fs::minor(device)))
}

/// The whole disks under the block device whose folder in `/sys` is `sys`: the disk itself, the
/// disk a partition is on, or the disks under an encrypted volume, LVM or RAID.
fn whole_disks(sys: &Path, depth: usize) -> Vec<Disk> {
    let Ok(sys) = fs::canonicalize(sys) else {
        return Vec::new();
    };
    if depth >= 8 {
        return vec![Disk::Block(sys)];
    }
    // The folder above a partition is its device, itself maybe made of others: md0 for md0p1.
    if sys.join("partition").exists()
        && let Some(device) = sys.parent()
    {
        return whole_disks(device, depth + 1);
    }
    let under: Vec<PathBuf> = fs::read_dir(sys.join("slaves"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    if under.is_empty() {
        return vec![Disk::Block(sys)];
    }
    under
        .iter()
        .flat_map(|device| whole_disks(device, depth + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    const MOUNTINFO: &str = "\
41 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
56 41 259:1 / /boot rw,relatime shared:115 - vfat /dev/nvme0n1p1 rw
44 41 0:25 / /sys rw,nosuid shared:5 - sysfs sysfs rw
51 41 0:28 / /run rw,nosuid,nodev shared:12 - tmpfs tmpfs rw,mode=755
60 41 8:17 / /mnt/my\\040data rw - ext4 /dev/sdb1 rw
61 51 8:33 / /run/media/me/STICK rw - vfat /dev/sdc1 rw
62 41 0:50 / /mnt/nas rw - nfs4 nas:/export rw
63 41 0:51 / /efi rw - autofs systemd-1 rw,fd=43
64 41 0:52 /@home /home rw - btrfs /dev/nvme0n1p3 rw
65 41 7:0 / /mnt/iso ro - iso9660 /dev/loop0 ro
";

    fn table() -> MountTable {
        let disk = |path: &str| vec![Disk::Block(path.into())];
        let nvme = "/sys/devices/pci0000:00/nvme/nvme0/nvme0n1";
        MountTable::parse(MOUNTINFO, &|source: &Source| match source.source.as_str() {
            "/dev/nvme0n1p1" | "/dev/nvme0n1p2" | "/dev/nvme0n1p3" => disk(nvme),
            "/dev/sdb1" => disk("/sys/devices/pci0000:00/ata2/host1/target1:0:0/block/sdb"),
            "/dev/sdc1" => disk("/sys/devices/pci0000:00/usb2/2-1/host6/block/sdc"),
            "/dev/loop0" => disk("/sys/devices/virtual/block/loop0"),
            _ => Vec::new(),
        })
    }

    #[test]
    fn tells_where_each_mount_is() {
        let table = table();
        let at = |path: &str| table.get(Path::new(path));
        assert_eq!(at("/"), Some(Mount::SameDisk));
        assert_eq!(at("/boot"), Some(Mount::SameDisk));
        assert_eq!(at("/home"), Some(Mount::SameDisk));
        assert_eq!(at("/mnt/my data"), Some(Mount::OtherDisk));
        assert_eq!(at("/run/media/me/STICK"), Some(Mount::UsbDisk));
        assert_eq!(at("/mnt/nas"), Some(Mount::Network));
        assert_eq!(at("/efi"), Some(Mount::Automount));
        assert_eq!(at("/sys"), Some(Mount::NotADisk));
        assert_eq!(at("/run"), Some(Mount::NotADisk));
        assert_eq!(at("/mnt/iso"), Some(Mount::NotADisk));
        assert_eq!(at("/mnt"), None);
        let mut scanned = table.scanned(false);
        scanned.sort();
        assert_eq!(
            scanned,
            [Path::new("/"), Path::new("/boot"), Path::new("/home")]
        );
        assert_eq!(table.scanned(true).len(), 5);
    }

    #[test]
    fn nvme_disks_with_several_paths_are_disks() {
        let mountinfo = "\
41 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
56 41 259:1 / /boot rw - vfat /dev/nvme0n1p1 rw
";
        let head = "/sys/devices/virtual/nvme-subsystem/nvme-subsys0/nvme0n1";
        let table = MountTable::parse(mountinfo, &|_: &Source| vec![Disk::Block(head.into())]);
        assert_eq!(table.get(Path::new("/boot")), Some(Mount::SameDisk));
    }

    #[test]
    fn the_last_mount_on_a_path_counts() {
        let mountinfo = "\
41 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
63 41 0:51 / /efi rw - autofs systemd-1 rw
70 63 259:1 / /efi rw - vfat /dev/nvme0n1p1 rw
";
        let table = MountTable::parse(mountinfo, &|source: &Source| {
            if source.source.starts_with("/dev/nvme0n1") {
                vec![Disk::Pool("one disk".into())]
            } else {
                Vec::new()
            }
        });
        assert_eq!(table.get(Path::new("/efi")), Some(Mount::SameDisk));
    }

    #[test]
    fn reads_the_source_of_a_line() {
        let (path, source) = Source::read(
            "36 35 98:0 /mnt1 /mnt\\0402 rw,noatime master:1 shared:2 - ext3 /dev/root rw",
        )
        .unwrap();
        assert_eq!(path, Path::new("/mnt 2"));
        assert_eq!(source.fstype, "ext3");
        assert_eq!(source.device, (98, 0));
        assert_eq!(source.source, "/dev/root");
        assert_eq!(Source::read("garbage"), None);
    }

    #[test]
    fn finds_the_disks_under_partitions_and_volumes() {
        let sys = TestDir::new();
        let disk = sys.dir("devices/pci/nvme0n1");
        sys.file("devices/pci/nvme0n1/nvme0n1p2/partition", 1);
        let crypt = sys.dir("devices/virtual/block/dm-0");
        sys.symlink(
            "devices/virtual/block/dm-0/slaves/nvme0n1p2",
            &disk.join("nvme0n1p2"),
        );
        assert_eq!(whole_disks(&disk, 0), [Disk::Block(disk.clone())]);
        assert_eq!(
            whole_disks(&disk.join("nvme0n1p2"), 0),
            [Disk::Block(disk.clone())]
        );
        assert_eq!(whole_disks(&crypt, 0), [Disk::Block(disk.clone())]);
        assert!(whole_disks(&sys.path().join("missing"), 0).is_empty());
        // A partition of a RAID is on the RAID's disks.
        let other = sys.dir("devices/pci/sda");
        sys.file("devices/pci/sda/sda1/partition", 1);
        let raid = sys.dir("devices/virtual/block/md0");
        sys.file("devices/virtual/block/md0/md0p1/partition", 1);
        sys.symlink(
            "devices/virtual/block/md0/slaves/nvme0n1p2",
            &disk.join("nvme0n1p2"),
        );
        sys.symlink("devices/virtual/block/md0/slaves/sda1", &other.join("sda1"));
        let mut disks = whole_disks(&raid.join("md0p1"), 0);
        disks.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        assert_eq!(disks, [Disk::Block(disk), Disk::Block(other)]);
    }

    #[test]
    fn reads_the_mounts_of_this_system() {
        let table = MountTable::load().unwrap();
        assert!(table.get(Path::new("/")).is_some());
        assert_eq!(table.get(Path::new("/proc")), Some(Mount::NotADisk));
    }
}
