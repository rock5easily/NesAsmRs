use crate::{AssembleOptions, AssembleResult, SourceEncoding, resolve_path};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::Serialize;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Rom,
    Listing,
    Srec,
    /// Generated source (disassembly).
    Source,
    /// CHR data included by a generated source.
    Chr,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
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
    check_not_hidden(rom, root)
}

fn check_not_hidden(path: &Path, root: &Path) -> Result<(), String> {
    let root = crate::source::canonicalize(root)?;
    let relative = path
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

/// What [`export_disassembly`] wrote and removed.
#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct DisassemblyExport {
    /// Files written, the main source first.
    pub artifacts: Vec<Artifact>,
    /// Unedited bank files of the replaced main source that the new source
    /// no longer includes, such as the decimal names of older versions
    /// (`_bank10.asm` for bank 10, now `_bank0A.asm`) or banks a ROM hack no
    /// longer changes.
    pub removed: Vec<PathBuf>,
    /// Such files that could not be removed.
    pub warnings: Vec<String>,
}

/// Writes the source files and CHR file of a disassembly; see
/// [`export_disassembly`], which also reports the files it removed.
pub fn write_disassembly(
    disassembly: &crate::disasm::Disassembly,
    base: &Path,
    root: Option<&Path>,
    rom: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Artifact>, String> {
    export_disassembly(disassembly, base, root, rom, cancel).map(|export| export.artifacts)
}

/// Writes the source files and CHR file of a disassembly, relative to
/// `base` and within `root` when set. The main source must end in `.asm`
/// (bank files and the CHR file derive from its name). Existing files are
/// replaced only if they are generated sources not edited since (or the CHR
/// file of one), so hand-written or edited files are never lost: the main
/// file only by a main file, and a bank file by the file of the same bank,
/// or by any bank file when the replaced main file included it (the whole
/// set is generated again). Bank files the replaced main file included that
/// are not written again are removed, if they were not edited either. When
/// `cancel` is set before the files are renamed into place, nothing is
/// written.
pub fn export_disassembly(
    disassembly: &crate::disasm::Disassembly,
    base: &Path,
    root: Option<&Path>,
    rom: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<DisassemblyExport, String> {
    use crate::disasm::{SourceRole, source_includes, source_role};
    let main = disassembly.source_path();
    if !main
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("asm"))
    {
        return Err("Source output must have a .asm extension".into());
    }
    let rom = resolve_path(rom, base, root)?;
    let mut files: Vec<(ArtifactKind, PathBuf, Vec<u8>, Option<SourceRole>)> = Vec::new();
    for (path, text) in disassembly.files() {
        let role = source_role(text);
        // Like the listing, sources use the platform line ending.
        let text = if cfg!(windows) {
            text.replace('\n', "\r\n")
        } else {
            text.clone()
        };
        files.push((
            ArtifactKind::Source,
            resolve_path(path, base, root)?,
            text.into_bytes(),
            role,
        ));
    }
    if let Some((path, data)) = disassembly.chr_file() {
        files.push((
            ArtifactKind::Chr,
            resolve_path(path, base, root)?,
            data.to_vec(),
            None,
        ));
    }
    let main = files[0].1.clone();
    let main_text = match fs::read(&main) {
        // Text that is not UTF-8 was not generated (or was edited).
        Ok(text) => Some(String::from_utf8(text).unwrap_or_default()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let main_replaceable = main_text.as_deref().and_then(source_role) == Some(SourceRole::Main);
    // The bank files generated with the main file that is replaced, as
    // resolved (to compare with the files written) and as named in their
    // directory. A link is left out: resolving it names its target, which may
    // belong to another source and must not be replaced or removed with this one.
    let previous: Vec<(PathBuf, PathBuf)> = main_text
        .as_deref()
        .filter(|_| main_replaceable)
        .map(source_includes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let path = Path::new(&path);
            let resolved = resolve_path(path, base, root).ok()?;
            let named = in_directory(path, base, root)?;
            let file = fs::symlink_metadata(&named).is_ok_and(|m| m.file_type().is_file());
            file.then_some((resolved, named))
        })
        .collect();
    for (kind, path, _, role) in &files {
        if let Some(root) = root {
            check_not_hidden(path, root)?;
        }
        if *path == rom {
            return Err("Output would overwrite the ROM".into());
        }
        // A link must not lead to another kind of file, such as a ROM.
        let expected = if *kind == ArtifactKind::Chr {
            "chr"
        } else {
            "asm"
        };
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(expected))
        {
            return Err(format!(
                "Output path {} is not a .{expected} file",
                path.display()
            ));
        }
        if path.is_dir() {
            return Err(format!("Output path is a directory: {}", path.display()));
        }
        if !path.exists() {
            continue;
        }
        let replaceable = match kind {
            ArtifactKind::Source => {
                let text = fs::read(path).map_err(|e| e.to_string())?;
                let existing = std::str::from_utf8(&text).ok().and_then(source_role);
                match (existing, role) {
                    (Some(existing), Some(role)) if existing == *role => true,
                    (Some(SourceRole::Bank(_)), Some(SourceRole::Bank(_)))
                        if previous.iter().any(|(resolved, _)| resolved == path) =>
                    {
                        true
                    }
                    (Some(SourceRole::Bank(other)), Some(SourceRole::Bank(bank))) => {
                        return Err(format!(
                            "{} exists and is the generated file of bank {other}, not of bank {bank} \
                             (bank files are named in hexadecimal; older versions used decimal); \
                             remove it or choose another output path",
                            path.display()
                        ));
                    }
                    _ => false,
                }
            }
            _ => {
                let data = fs::read(path).map_err(|e| e.to_string())?;
                main_replaceable
                    && main_text
                        .as_deref()
                        .is_some_and(|text| crate::disasm::is_unedited_chr(text, &data))
            }
        };
        if !replaceable {
            return Err(format!(
                "{} exists and is not an unedited generated file; choose another output path",
                path.display()
            ));
        }
    }
    // Bank files of the replaced main file that are not written again, in
    // the directory of the main file. Directories are compared as resolved
    // themselves: on Windows, a resolved file path keeps the `\\?\` prefix
    // when it is too long without it, which its directory may not need.
    let prefix = format!(
        "{}_bank",
        main.file_stem().unwrap_or_default().to_string_lossy()
    );
    let main_directory = in_directory(disassembly.source_path(), base, root)
        .and_then(|path| path.parent().map(Path::to_owned));
    let stale: Vec<PathBuf> = previous
        .into_iter()
        .filter(|(resolved, _)| files.iter().all(|(_, p, _, _)| p != resolved))
        .map(|(_, named)| named)
        .filter(|path| {
            main_directory.is_some()
                && path.parent() == main_directory.as_deref()
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("asm"))
                && root.is_none_or(|root| check_not_hidden(path, root).is_ok())
                && fs::read(path).is_ok_and(|text| {
                    std::str::from_utf8(&text)
                        .ok()
                        .and_then(source_role)
                        .is_some_and(|role| matches!(role, SourceRole::Bank(_)))
                })
        })
        .collect();
    for (_, path, _, _) in &files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut staged = Vec::with_capacity(files.len());
    for (kind, path, data, _) in files {
        staged.push(StagedArtifact::write(kind, path, &data).map_err(|e| e.to_string())?);
    }
    // Staged files are removed when dropped.
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled before writing; nothing was written".into());
    }
    let artifacts = staged
        .into_iter()
        .map(|artifact| artifact.commit().map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut export = DisassemblyExport {
        artifacts,
        ..DisassemblyExport::default()
    };
    for path in stale {
        match fs::remove_file(&path) {
            Ok(()) => export.removed.push(path),
            Err(e) => export
                .warnings
                .push(format!("Could not remove {}: {e}", path.display())),
        }
    }
    Ok(export)
}

/// `path` relative to `base`, with its directory resolved but not its last
/// component, which may be a link.
fn in_directory(path: &Path, base: &Path, root: Option<&Path>) -> Option<PathBuf> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Some(
        resolve_path(directory, base, root)
            .ok()?
            .join(path.file_name()?),
    )
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
    use std::fmt::Write as _;
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
            let _ = write!(text, "S2{count:02X}{start:06X}");
            for b in &binary[start..pos] {
                sum = sum.wrapping_add(*b);
                let _ = write!(text, "{b:02X}");
            }
            let _ = writeln!(text, "{:02X}", !sum);
        }
    }
    let address = ((map[0] >> 5) as usize) << 13;
    let sum = 4u8
        .wrapping_add((address >> 8) as u8)
        .wrapping_add(address as u8);
    let _ = write!(text, "S804{address:06X}{:02X}", !sum);
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

    #[test]
    fn staging_skips_existing_temporary_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.nes");
        // Occupy the next names; another test may take some of the ids first.
        let next = NEXT_TEMP_ID.load(Ordering::Relaxed);
        for id in next..next + 16 {
            let name = format!(".result.nes.{}.{id}.tmp", std::process::id());
            fs::write(dir.path().join(name), b"taken").unwrap();
        }
        let staged = StagedArtifact::write(ArtifactKind::Rom, path.clone(), &[1]).unwrap();
        assert!(NEXT_TEMP_ID.load(Ordering::Relaxed) >= next + 16);
        let artifact = staged.commit().unwrap();
        assert_eq!((artifact.path, artifact.size), (path.clone(), 1));
        assert_eq!(fs::read(path).unwrap(), [1]);
    }

    #[test]
    fn staging_errors() {
        let dir = tempfile::tempdir().unwrap();
        let invalid = StagedArtifact::write(ArtifactKind::Rom, PathBuf::from(".."), &[1]);
        assert_eq!(invalid.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        let missing = dir.path().join("missing").join("result.nes");
        assert!(StagedArtifact::write(ArtifactKind::Rom, missing, &[1]).is_err());
        // A directory created at the destination after staging makes the
        // rename fail; the temporary file is still removed.
        let path = dir.path().join("result.nes");
        let staged = StagedArtifact::write(ArtifactKind::Rom, path.clone(), &[1]).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(staged.commit().is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
