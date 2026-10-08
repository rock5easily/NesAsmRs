use crate::{AssembleOptions, AssembleResult, SourceEncoding, resolve_path};
use schemars::JsonSchema;
use serde::Serialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Rom,
    Listing,
    Srec,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub size: usize,
}

pub fn write_artifacts(
    result: &AssembleResult,
    input: &Path,
    output: Option<&Path>,
    base: &Path,
    root: Option<&Path>,
    options: &AssembleOptions,
) -> Result<Vec<Artifact>, String> {
    if !result.success {
        return Err("Cannot write artifacts for a failed assembly".into());
    }
    let rom = resolve_path(output.unwrap_or(&input.with_extension("nes")), base, root)?;
    if let Some(root) = root {
        check_rooted_output(&rom, root)?;
    }
    let mut files = if options.srec {
        Vec::new()
    } else {
        let mut binary = Vec::with_capacity(result.header.len() + result.binary.len());
        binary.extend_from_slice(&result.header);
        binary.extend_from_slice(&result.binary);
        vec![(ArtifactKind::Rom, rom.clone(), binary)]
    };
    let encode = |text: &str| -> Result<Vec<u8>, String> {
        // Text artifacts use the platform line ending, like the C# WriteLine output.
        let native;
        let text = if cfg!(windows) {
            native = text.replace('\n', "\r\n");
            native.as_str()
        } else {
            text
        };
        match options.encoding {
            SourceEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
            SourceEncoding::Sjis => {
                let (bytes, _, errors) = encoding_rs::SHIFT_JIS.encode(text);
                if errors {
                    Err("Listing cannot be encoded as SJIS".into())
                } else {
                    Ok(bytes.into_owned())
                }
            }
        }
    };
    if let Some(list) = &result.listing {
        files.push((
            ArtifactKind::Listing,
            resolve_path(&rom.with_extension("lst"), base, root)?,
            encode(list)?,
        ));
    }
    if let Some(srec) = &result.srec {
        files.push((
            ArtifactKind::Srec,
            resolve_path(&rom.with_extension("s28"), base, root)?,
            encode(srec)?,
        ));
    }
    let mut destinations = std::collections::BTreeSet::new();
    for (_, path, _) in &files {
        if !destinations.insert(path) {
            return Err("Artifact output paths collide".into());
        }
        if result.dependencies.contains(path) {
            return Err("Output would overwrite an input dependency".into());
        }
        if path.is_dir() {
            return Err(format!("Output path is a directory: {}", path.display()));
        }
    }
    for (_, path, _) in &files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut staged = Vec::with_capacity(files.len());
    for (kind, path, data) in files {
        staged.push(StagedArtifact::write(kind, path, &data).map_err(|e| e.to_string())?);
    }
    staged
        .into_iter()
        .map(|artifact| artifact.commit().map_err(|e| e.to_string()))
        .collect()
}

/// Restricts root-confined (MCP) output to ROM artifact names outside hidden
/// directories, so a request cannot replace source, configuration or VCS files.
/// The listing and S-record paths are derived from the ROM path.
fn check_rooted_output(rom: &Path, root: &Path) -> Result<(), String> {
    let extension = rom
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("nes" | "bin")) {
        return Err("Output file must have a .nes or .bin extension".into());
    }
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let relative = rom
        .strip_prefix(&root)
        .map_err(|_| "Path is outside the project root")?;
    if relative
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return Err("Output path must not contain hidden files or directories".into());
    }
    Ok(())
}

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Owns its temporary file, including cleanup on errors and unwinding.
struct StagedArtifact {
    kind: ArtifactKind,
    path: PathBuf,
    temp: PathBuf,
    size: usize,
    file: Option<fs::File>,
}

impl StagedArtifact {
    fn write(kind: ArtifactKind, path: PathBuf, data: &[u8]) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid output name"))?
            .to_string_lossy();
        let (temp, file) = loop {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let temp = path.with_file_name(format!(".{name}.{}.{id}.tmp", std::process::id()));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(file) => break (temp, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let mut staged = Self {
            kind,
            path,
            temp,
            size: data.len(),
            file: Some(file),
        };
        let file = staged.file.as_mut().expect("newly created staging file");
        file.write_all(data)?;
        file.sync_all()?;
        Ok(staged)
    }

    fn commit(mut self) -> io::Result<Artifact> {
        // Windows requires the file to be closed before renaming it.
        drop(self.file.take());
        fs::rename(&self.temp, &self.path)?;
        Ok(Artifact {
            kind: self.kind,
            path: std::mem::take(&mut self.path),
            size: self.size,
        })
    }
}

impl Drop for StagedArtifact {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.temp);
    }
}

pub(crate) fn srec(binary: &[u8], map: &[u8]) -> String {
    let mut text = String::new();
    for bank in 0..binary.len() / 8192 {
        let mut pos = bank * 8192;
        let end = pos + 8192;
        while pos < end {
            if map[pos] == 255 {
                pos += 1;
                continue;
            }
            let start = pos;
            while pos < end && map[pos] != 255 && pos - start < 32 {
                pos += 1;
            }
            let count = (pos - start + 4) as u8;
            let mut sum = count
                .wrapping_add((start >> 16) as u8)
                .wrapping_add((start >> 8) as u8)
                .wrapping_add(start as u8);
            text.push_str(&format!("S2{count:02X}{start:06X}"));
            for b in &binary[start..pos] {
                sum = sum.wrapping_add(*b);
                text.push_str(&format!("{b:02X}"));
            }
            text.push_str(&format!("{:02X}\n", !sum));
        }
    }
    let address = ((map[0] >> 5) as usize) << 13;
    let sum = 4u8
        .wrapping_add((address >> 8) as u8)
        .wrapping_add(address as u8);
    text.push_str(&format!("S804{address:06X}{:02X}", !sum));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_staging_files_are_distinct_and_cleaned_up() {
        let root = std::env::temp_dir().join(format!("nesasm-staging-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("result.nes");
        let staged = std::thread::scope(|scope| {
            let handles = (0..4)
                .map(|_| {
                    let path = path.clone();
                    scope.spawn(move || StagedArtifact::write(ArtifactKind::Rom, path, &[0x42]))
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(fs::read_dir(&root).unwrap().count(), 4);
        drop(staged);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
    }
}
