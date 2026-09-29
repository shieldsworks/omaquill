//! A zip writer that stores entries uncompressed. That is all DOCX and
//! EPUB need (EPUB even requires its `mimetype` entry stored), and it keeps
//! project backups readable by any unzip.

use std::io::{self, Write};

pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    let mut crc = !0u32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

pub struct ZipWriter<W: Write> {
    out: W,
    written: u64,
    entries: Vec<Entry>,
}

fn too_big() -> io::Error {
    io::Error::other("zip entry larger than 4 GB")
}

impl<W: Write> ZipWriter<W> {
    pub fn new(out: W) -> Self {
        ZipWriter {
            out,
            written: 0,
            entries: Vec::new(),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| too_big())?;
        let offset = u32::try_from(self.written).map_err(|_| too_big())?;
        let crc = crc32(data);
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x04034b50u32.to_le_bytes());
        h.extend_from_slice(&10u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&0u16.to_le_bytes()); // time
        h.extend_from_slice(&0x21u16.to_le_bytes()); // date: 1980-01-01
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        self.put(&h)?;
        self.put(data)?;
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let start = u32::try_from(self.written).map_err(|_| too_big())?;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut h = Vec::with_capacity(46 + e.name.len());
            h.extend_from_slice(&0x02014b50u32.to_le_bytes());
            h.extend_from_slice(&0x031Eu16.to_le_bytes()); // made by: Unix
            h.extend_from_slice(&10u16.to_le_bytes());
            h.extend_from_slice(&0x0800u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&0x21u16.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attrs
            h.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
            h.extend_from_slice(&e.offset.to_le_bytes());
            h.extend_from_slice(e.name.as_bytes());
            self.put(&h)?;
        }
        let size = u32::try_from(self.written).map_err(|_| too_big())? - start;
        let count =
            u16::try_from(entries.len()).map_err(|_| io::Error::other("too many zip entries"))?;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x06054b50u32.to_le_bytes());
        end.extend_from_slice(&[0; 4]);
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&size.to_le_bytes());
        end.extend_from_slice(&start.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.put(&end)?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn unzip_reads_it() {
        let Ok(unzip) = which("unzip") else { return };
        let dir = std::env::temp_dir().join(format!("omaquill-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        let mut z = ZipWriter::new(std::fs::File::create(&path).unwrap());
        z.add("mimetype", b"application/epub+zip").unwrap();
        z.add("dir/été.txt", "hello\n".as_bytes()).unwrap();
        z.finish().unwrap();
        let out = std::process::Command::new(unzip)
            .arg("-t")
            .arg(&path)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn which(bin: &str) -> Result<std::path::PathBuf, ()> {
        std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join(bin))
                    .find(|p| p.exists())
            })
            .ok_or(())
    }
}
