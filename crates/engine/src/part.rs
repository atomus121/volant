//! 稀疏 part 文件：按偏移写，体积等于完整文件，但只占真正写过的盘。
//!
//! 为什么必须显式设稀疏：Windows 上先把文件 `set_len` 到 10 GiB，再在文件中间写 1 MiB，
//! NTFS 会**同步补零**前面那几 GiB（实测 2 GiB 处写 1 MiB 要 4.3 秒，而且盘上写了两倍的数据）。
//! 分段下载在第一秒内就会写文件中部，所以每个大文件都会踩这个坑。`FSCTL_SET_SPARSE` 之后
//! 就没这回事了。非 Windows 上 `ftruncate` 本来就是稀疏的，这个函数直接返回 false。
//!
//! 本体是引擎里唯一的 unsafe：一次 `DeviceIoControl`（以及它的测试里一次
//! `GetCompressedFileSizeW`），都在 `windows_impl` 里。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 太小的文件不值得设稀疏：补零本身也就几毫秒。
const MIN_SPARSE_SIZE: u64 = 5 << 20;

pub struct PartFile {
    file: File,
    path: PathBuf,
    /// 是否真的设上了稀疏属性。用 `AtomicBool` 而不是 `bool`，因为收尾时要能对一个
    /// 共享引用（`&PartFile`）清掉它，不必把句柄从 `Arc` 里抢出来。
    sparse: AtomicBool,
}

impl PartFile {
    /// 打开 or 新建 part 文件。
    ///
    /// `size = Some(n)`：文件体积先定成 n（分段下载要求每段都能 seek 进去）。
    /// `truncate = false`：续传，保留盘上已有内容。
    pub fn open(path: &Path, size: Option<u64>, truncate: bool) -> io::Result<Self> {
        Self::open_mode(path, size, truncate, false)
    }

    /// 原子创建一个新 part 文件。目标已存在时返回 `AlreadyExists`，调用方可以换下一个名字。
    ///
    /// 下载目标不能只靠 `exists()` 判断，否则两个同名任务会在检查和创建之间同时选中
    /// 同一个文件。`create_new(true)` 把“检查并占用”合成一个文件系统操作。
    pub fn create_new(path: &Path, size: Option<u64>) -> io::Result<Self> {
        Self::open_mode(path, size, false, true)
    }

    fn open_mode(
        path: &Path,
        size: Option<u64>,
        truncate: bool,
        create_new: bool,
    ) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        if create_new {
            opts.create_new(true);
        } else {
            opts.create(true);
        }
        let file = opts.open(path)?;
        let part = PartFile {
            file,
            path: path.to_path_buf(),
            sparse: AtomicBool::new(false),
        };
        if truncate {
            part.file.set_len(0)?;
        }
        // 先设稀疏，再定长度：反过来的话 set_len 会立刻把整段零填进去。
        if let Some(n) = size
            && n >= MIN_SPARSE_SIZE
        {
            part.sparse.store(
                windows_impl::set_sparse(&part.file, true),
                Ordering::Relaxed,
            );
        }
        if let Some(n) = size {
            part.file.set_len(n)?;
        }
        Ok(part)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 在 `offset` 处写 `buf`（不移动文件指针，多个 worker 可以同时写同一个句柄）。
    pub fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        let mut off = offset;
        let mut rest = buf;
        while !rest.is_empty() {
            let n = windows_impl::pwrite(&self.file, rest, off)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "写盘返回 0 字节"));
            }
            off += n as u64;
            rest = &rest[n..];
        }
        Ok(())
    }

    /// 让已写的字节真正落到盘上（状态文件必须在这之后写）。
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// 下完了：把稀疏属性去掉。此时每个字节都写过了，去掉属性不搬运数据，
    /// 但文件在资源管理器/压缩/备份工具眼里就是普通文件。
    pub fn finish(&self) {
        if self.sparse.swap(false, Ordering::Relaxed) {
            windows_impl::set_sparse(&self.file, false);
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::fs::File;
    use std::io;
    use std::os::windows::fs::FileExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FILE_SET_SPARSE_BUFFER, FSCTL_SET_SPARSE};

    pub fn pwrite(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.seek_write(buf, offset)
    }

    /// 设/清稀疏属性。文件系统不支持（FAT32、exFAT）时返回 false，下载照常进行，
    /// 只是会吃一次补零的代价。
    pub fn set_sparse(file: &File, sparse: bool) -> bool {
        let input = FILE_SET_SPARSE_BUFFER { SetSparse: sparse };
        let mut returned = 0u32;
        // SAFETY: `file` 持有句柄且活得比调用长；输入指针与长度描述栈上的 `input`；
        // 没有输出缓冲；`returned` 是有效的 u32；不传 OVERLAPPED，调用同步完成。
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                (&raw const input).cast(),
                size_of::<FILE_SET_SPARSE_BUFFER>() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        ok != 0
    }
}

#[cfg(not(windows))]
mod windows_impl {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileExt;

    pub fn pwrite(file: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
        file.write_at(buf, offset)
    }

    /// ftruncate 出来的文件本来就是稀疏的，不需要额外动作。
    pub fn set_sparse(_: &File, _: bool) -> bool {
        false
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    /// 文件真正在盘上占的字节（稀疏文件只算写过的部分）。
    fn on_drive(path: &Path) -> u64 {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut high = 0u32;
        // SAFETY: `wide` 是以 NUL 结尾的 UTF-16 路径且活得比调用长；`high` 是可写的 u32。
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == u32::MAX {
            let e = io::Error::last_os_error();
            assert_eq!(e.raw_os_error(), Some(0), "GetCompressedFileSizeW: {e}");
        }
        (u64::from(high) << 32) | u64::from(low)
    }

    #[test]
    fn a_part_file_written_far_in_stays_small_on_drive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.bin");
        let part = PartFile::open(&path, Some(4 << 30), true).unwrap();
        assert!(
            part.sparse.load(Ordering::Relaxed),
            "NTFS 上必须能设稀疏属性"
        );
        assert_ne!(
            part.file.metadata().unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
            0
        );
        // 在 2 GiB 处写 1 MiB。不稀疏的话 NTFS 会先同步补零 2 GiB。
        part.write_at(2 << 30, &[7u8; 1 << 20]).unwrap();
        part.sync().unwrap();
        let used = on_drive(&path);
        assert!(
            ((1 << 20)..(64 << 20)).contains(&used),
            "盘上只该占写过的部分，实际 {used} 字节"
        );
        // 读回来是写的那些字节，前面是零。
        let got = {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = File::open(&path).unwrap();
            f.seek(SeekFrom::Start(2 << 30)).unwrap();
            let mut b = vec![0u8; 16];
            f.read_exact(&mut b).unwrap();
            b
        };
        assert!(got.iter().all(|&b| b == 7));
    }

    #[test]
    fn small_files_are_not_marked_sparse() {
        let dir = tempfile::tempdir().unwrap();
        let part = PartFile::open(&dir.path().join("s.bin"), Some(1024), true).unwrap();
        assert!(!part.sparse.load(Ordering::Relaxed));
        assert_eq!(
            part.file.metadata().unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
            0
        );
    }

    #[test]
    fn write_at_does_not_disturb_other_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bin");
        let part = PartFile::open(&path, Some(1 << 20), true).unwrap();
        part.write_at(0, b"hello").unwrap();
        part.write_at(1000, b"world").unwrap();
        part.sync().unwrap();
        let all = std::fs::read(&path).unwrap();
        assert_eq!(&all[0..5], b"hello");
        assert_eq!(&all[1000..1005], b"world");
        assert!(all[5..1000].iter().all(|&b| b == 0));
        assert_eq!(all.len(), 1 << 20);
    }
}
