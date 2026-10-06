use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Stage results on disk so late analysis failures emit no partial stdout and
/// cannot truncate an existing output. The buffer stays bounded as reports grow.
pub(super) struct StagedOutput {
    writer: Option<BufWriter<File>>,
    path: Option<PathBuf>,
}

impl StagedOutput {
    pub(super) fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let path = std::env::temp_dir().join(format!(
                "jsexec-output-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    #[cfg(unix)]
                    let path = {
                        std::fs::remove_file(&path)?;
                        None
                    };
                    #[cfg(not(unix))]
                    let path = Some(path);
                    return Ok(Self {
                        writer: Some(BufWriter::new(file)),
                        path,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create temporary output",
        ))
    }

    pub(super) fn writer(&mut self) -> &mut BufWriter<File> {
        self.writer.as_mut().expect("output writer is live")
    }

    pub(super) fn commit(mut self, output: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
        let writer = self.writer();
        writer.flush()?;
        writer.seek(SeekFrom::Start(0))?;
        if let Some(path) = output {
            let mut destination = BufWriter::new(File::create(path)?);
            io::copy(writer.get_mut(), &mut destination)?;
            destination.flush()?;
        } else {
            io::copy(writer.get_mut(), &mut io::stdout().lock())?;
        }
        Ok(())
    }
}

impl Drop for StagedOutput {
    fn drop(&mut self) {
        // Close before unlinking for Windows as well as Unix.
        drop(self.writer.take());
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}
