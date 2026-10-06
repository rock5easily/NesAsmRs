use crate::{AssembleOptions, AssembleResult, SourceEncoding, resolve_path};
use schemars::JsonSchema;
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Artifact {
    pub kind: String,
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
    let mut binary = result.header.clone();
    binary.extend(&result.binary);
    let mut files = if options.srec {
        Vec::new()
    } else {
        vec![("rom", rom.clone(), binary)]
    };
    let encode = |text: &str| -> Result<Vec<u8>, String> {
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
            "listing",
            resolve_path(&rom.with_extension("lst"), base, root)?,
            encode(list)?,
        ));
    }
    if let Some(srec) = &result.srec {
        files.push((
            "srec",
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
    }
    for (_, path, _) in &files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut artifacts = Vec::new();
    for (kind, path, data) in files {
        // Write a sibling temporary file so a failed write cannot truncate an existing ROM.
        let name = path
            .file_name()
            .ok_or("Invalid output name")?
            .to_string_lossy();
        let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        let written = file.write_all(&data).and_then(|_| file.sync_all());
        drop(file);
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(e.to_string());
        }
        if let Err(e) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(e.to_string());
        }
        artifacts.push(Artifact {
            kind: kind.into(),
            path,
            size: data.len(),
        });
    }
    Ok(artifacts)
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
