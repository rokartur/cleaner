use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

/// Per-entry error code, valid only in bulk calls; libc does not define it.
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VREG: u32 = 1;
const VDIR: u32 = 2;
/// More threads stop paying off on APFS: on an M1 Max, 8 threads and 16 measured the same.
const MAX_THREADS: usize = 8;

struct Folder {
    /// Index of the measured path this folder sits under.
    item: usize,
    path: PathBuf,
    /// The measured path's volume: a folder on another one is a mount, not part of the item.
    device: u64,
}

/// Allocated bytes per path as `du -skx` counts them (links 0, hard links once, no mounted volume such
/// as an iPhone under ~/Library/Developer/CoreDevice), and whether every folder could be read.
pub fn sizes(paths: &[PathBuf]) -> Vec<(u64, bool)> {
    let totals: Vec<AtomicU64> = paths.iter().map(|_| AtomicU64::new(0)).collect();
    let complete: Vec<AtomicBool> = paths.iter().map(|_| AtomicBool::new(true)).collect();
    let mut folders = Vec::new();
    for (item, path) in paths.iter().enumerate() {
        let Ok(meta) = std::fs::symlink_metadata(path) else { continue };
        if meta.is_dir() {
            folders.push(Folder { item, path: path.clone(), device: meta.dev() });
        } else if meta.is_file() {
            totals[item].store(meta.blocks() * 512, Ordering::Relaxed);
        }
    }

    // Threads take one folder at a time, so a single Xcode.app still spreads over all of them. The
    // pair counts folders waiting and folders being read: new ones can appear until both are zero.
    let queue = Mutex::new((folders, 0usize));
    let changed = Condvar::new();
    let linked = Mutex::new(HashSet::new());
    let threads = std::thread::available_parallelism().map_or(4, |count| count.get()).min(MAX_THREADS);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    let mut state = queue.lock().unwrap();
                    let folder = loop {
                        if let Some(folder) = state.0.pop() {
                            state.1 += 1;
                            break folder;
                        }
                        if state.1 == 0 {
                            return;
                        }
                        state = changed.wait(state).unwrap();
                    };
                    drop(state);

                    let mut found = Vec::new();
                    let (bytes, readable) = read_folder(&folder, &mut buffer, &mut found, &linked);
                    totals[folder.item].fetch_add(bytes, Ordering::Relaxed);
                    if !readable {
                        complete[folder.item].store(false, Ordering::Relaxed);
                    }

                    let mut state = queue.lock().unwrap();
                    let new_work = !found.is_empty();
                    state.0.extend(found);
                    state.1 -= 1;
                    if new_work || state.1 == 0 {
                        changed.notify_all();
                    }
                }
            });
        }
    });
    totals.iter().zip(&complete).map(|(total, complete)| (total.load(Ordering::Relaxed), complete.load(Ordering::Relaxed))).collect()
}

/// Sums one folder's files and adds its subfolders to `found`. One `getattrlistbulk` call fills a
/// buffer where `readdir` needs an `lstat` per entry: ~/Library/Developer took 1.3 s instead of 2.3 s.
fn read_folder(folder: &Folder, buffer: &mut [u8], found: &mut Vec<Folder>, linked: &Mutex<HashSet<(usize, u64)>>) -> (u64, bool) {
    let Ok(dir) = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(&folder.path) else { return (0, false) };
    let Ok(meta) = File::metadata(&dir) else { return (0, false) };
    if meta.dev() != folder.device {
        return (0, true);
    }
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | libc::ATTR_CMN_NAME | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };
    let (mut bytes, mut readable) = (0, true);
    loop {
        // SAFETY: both pointers stay valid for the call, and the kernel writes at most buffer.len() bytes.
        let count = unsafe { libc::getattrlistbulk(dir.as_raw_fd(), (&raw mut request).cast(), buffer.as_mut_ptr().cast(), buffer.len(), 0) };
        if count <= 0 {
            return (bytes, readable && count == 0);
        }
        let mut entry = 0;
        for _ in 0..count {
            // Layout per entry: length, the returned-attributes set, then each returned attribute
            // in bit order, except the error code, which comes first.
            let length: u32 = read(buffer, entry);
            let returned: libc::attribute_set_t = read(buffer, entry + 4);
            let mut at = entry + 4 + size_of::<libc::attribute_set_t>();
            let mut field = |set: u32, bit: u32, width: usize| {
                let start = at;
                if set & bit != 0 {
                    at += width;
                }
                (set & bit != 0).then_some(start)
            };
            let error = field(returned.commonattr, ATTR_CMN_ERROR, 4).map(|at| read::<u32>(buffer, at));
            let name = field(returned.commonattr, libc::ATTR_CMN_NAME, size_of::<libc::attrreference_t>());
            let kind = field(returned.commonattr, libc::ATTR_CMN_OBJTYPE, 4).map(|at| read::<u32>(buffer, at));
            let file_id = field(returned.commonattr, libc::ATTR_CMN_FILEID, 8).map(|at| read::<u64>(buffer, at));
            let links = field(returned.fileattr, libc::ATTR_FILE_LINKCOUNT, 4).map(|at| read::<u32>(buffer, at));
            let allocated = field(returned.fileattr, libc::ATTR_FILE_ALLOCSIZE, 8).map(|at| read::<i64>(buffer, at));
            readable &= error.is_none_or(|code| code == 0);

            match (kind, name) {
                (Some(VREG), _) => {
                    let counted = links.is_none_or(|links| links < 2) || file_id.is_none_or(|id| linked.lock().unwrap().insert((folder.item, id)));
                    if counted {
                        bytes += allocated.unwrap_or(0).max(0) as u64;
                    }
                }
                (Some(VDIR), Some(name)) => {
                    let reference: libc::attrreference_t = read(buffer, name);
                    let start = name.checked_add_signed(reference.attr_dataoffset as isize).expect("name offset");
                    // The length counts the trailing NUL.
                    let name = &buffer[start..start + (reference.attr_length as usize).saturating_sub(1)];
                    found.push(Folder { item: folder.item, path: folder.path.join(OsStr::from_bytes(name)), device: folder.device });
                }
                _ => {}
            }
            entry += length as usize;
        }
    }
}

/// The home volume's size and the bytes still free for the user, as `df` shows them.
pub fn disk_space() -> Option<(u64, u64)> {
    let home = std::ffi::CString::new(crate::scan::home().as_os_str().as_bytes()).ok()?;
    // SAFETY: statfs is plain data.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: home is NUL-terminated and stats is a writable statfs.
    if unsafe { libc::statfs(home.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = u64::from(stats.f_bsize);
    Some((stats.f_blocks * block, stats.f_bavail * block))
}

/// A plain-data field of a `getattrlistbulk` buffer, which packs fields without alignment.
fn read<T: Copy>(buffer: &[u8], at: usize) -> T {
    let bytes = &buffer[at..at + size_of::<T>()];
    // SAFETY: `bytes` holds size_of::<T>() initialized bytes, and every T read here is integers only.
    unsafe { bytes.as_ptr().cast::<T>().read_unaligned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_count_hard_links_once_and_links_as_zero() {
        let root = std::env::temp_dir().join(format!("cleaner-sizes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app/nested/deeper")).unwrap();
        std::fs::write(root.join("app/nested/deeper/data"), vec![7u8; 100_000]).unwrap();
        std::fs::hard_link(root.join("app/nested/deeper/data"), root.join("app/same-data")).unwrap();
        std::fs::write(root.join("app/small"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("app"), root.join("link")).unwrap();
        let block = |path: &str| std::fs::metadata(root.join(path)).unwrap().blocks() * 512;
        let (data, small) = (block("app/nested/deeper/data"), block("app/small"));

        let measured = sizes(&[root.join("app"), root.join("link"), root.join("app/small"), root.join("missing")]);
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(measured, [(data + small, true), (0, true), (small, true), (0, true)]);
    }
}
