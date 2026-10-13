//! The disks: how full each filesystem on one is, from `statvfs`, and how much each disk reads
//! and writes, from `/proc/diskstats`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::reading::per_second;
use crate::mounts::MountTable;

/// How full a filesystem is, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filesystem {
    /// Where it is mounted.
    pub path: PathBuf,
    pub used: u64,
    /// What is left for the user; root may have a little more.
    pub free: u64,
}

/// Each filesystem on a disk, by the path it is mounted at.
pub fn filesystems() -> Vec<Filesystem> {
    // Network shares and automounts aren't asked: one that doesn't answer would hold up the
    // reading.
    let Ok(mounts) = MountTable::load() else {
        return Vec::new();
    };
    let filesystems = mounts.filesystems().iter().filter_map(|path| {
        let stat = rustix::fs::statvfs(path).ok()?;
        let block = stat.f_frsize;
        (stat.f_blocks > 0).then(|| Filesystem {
            path: path.clone(),
            used: stat.f_blocks.saturating_sub(stat.f_bfree) * block,
            free: stat.f_bavail * block,
        })
    });
    filesystems.collect()
}

/// Bytes a disk read and wrote: since it started, or each second between two readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIo {
    pub name: String,
    pub read: u64,
    pub written: u64,
}

impl DiskIo {
    /// What each disk in both `now` and `earlier` read and wrote each second in between,
    /// `elapsed` apart.
    pub fn rates(now: &[DiskIo], earlier: &[DiskIo], elapsed: Duration) -> Vec<DiskIo> {
        let rates = now.iter().filter_map(|disk| {
            let before = earlier.iter().find(|before| before.name == disk.name)?;
            Some(DiskIo {
                name: disk.name.clone(),
                read: per_second(disk.read, before.read, elapsed),
                written: per_second(disk.written, before.written, elapsed),
            })
        });
        rates.collect()
    }
}

/// What each disk read and wrote since it started.
pub fn io() -> Vec<DiskIo> {
    let Ok(diskstats) = fs::read_to_string("/proc/diskstats") else {
        return Vec::new();
    };
    let sys = Path::new("/sys");
    parse_diskstats(&diskstats)
        .into_iter()
        .filter(|disk| is_disk(sys, &disk.name))
        .collect()
}

/// Reads `/proc/diskstats` lines, e.g. `259 0 nvme0n1 164809 56540 9970730 57649 166751 163125
/// 9688074 …`: the sectors read are the 6th field and those written the 10th, in sectors of 512
/// bytes whatever the disk's own.
fn parse_diskstats(text: &str) -> Vec<DiskIo> {
    let disk = |line: &str| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let sectors = |n: usize| fields.get(n)?.parse::<u64>().ok();
        Some(DiskIo {
            name: fields.get(2)?.to_string(),
            read: sectors(5)? * 512,
            written: sectors(9)? * 512,
        })
    };
    text.lines().filter_map(disk).collect()
}

/// Whether the block device `name` is a disk holding something, as `sys` tells: a whole disk
/// (partitions aren't in its `block` folder), not one the kernel makes up (a loop device, zram,
/// device mapper or RAID, whose reads and writes are a disk's already), and not an empty drive.
fn is_disk(sys: &Path, name: &str) -> bool {
    let Ok(device) = fs::canonicalize(sys.join("block").join(name)) else {
        return false;
    };
    let sectors = fs::read_to_string(device.join("size"))
        .ok()
        .and_then(|size| size.trim().parse::<u64>().ok());
    !device.starts_with(sys.join("devices/virtual/block")) && sectors.is_some_and(|n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    #[test]
    fn reads_what_each_disk_read_and_wrote() {
        let disks = parse_diskstats(
            " 259       0 nvme0n1 164809 56540 9970730 57649 166751 163125 9688074 678851 0 \
             435528 905698 141033 0 1611257888 150823 14744 18374\n\
             253       0 zram0 75 0 2848 0 1925 0 15400 38 0 38 38 0 0 0 0 0 0\n\
             8 0 broken 1 2\n",
        );
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].name, "nvme0n1");
        assert_eq!(disks[0].read, 9_970_730 * 512);
        assert_eq!(disks[0].written, 9_688_074 * 512);
    }

    #[test]
    fn tells_disks_from_partitions_and_made_up_devices() {
        let sys = TestDir::new();
        let nvme = sys.dir("devices/pci/nvme0n1");
        std::fs::write(nvme.join("size"), "2000409264\n").unwrap();
        sys.symlink("block/nvme0n1", &nvme);
        let zram = sys.dir("devices/virtual/block/zram0");
        std::fs::write(zram.join("size"), "16777216\n").unwrap();
        sys.symlink("block/zram0", &zram);
        let empty = sys.dir("devices/pci/ata1/sr0");
        std::fs::write(empty.join("size"), "0\n").unwrap();
        sys.symlink("block/sr0", &empty);
        assert!(is_disk(sys.path(), "nvme0n1"));
        assert!(!is_disk(sys.path(), "zram0"));
        assert!(!is_disk(sys.path(), "sr0"));
        assert!(!is_disk(sys.path(), "nvme0n1p1"));
    }

    #[test]
    fn rates_are_per_second() {
        let disk = |read, written| DiskIo {
            name: "sda".into(),
            read,
            written,
        };
        let rates = DiskIo::rates(
            &[disk(3000, 1000)],
            &[disk(1000, 1000)],
            Duration::from_millis(500),
        );
        assert_eq!(rates, [disk(4000, 0)]);
    }

    #[test]
    fn reads_this_system() {
        // In a container, / is no disk's.
        for filesystem in filesystems() {
            assert!(filesystem.used + filesystem.free > 0, "{filesystem:?}");
        }
    }
}
