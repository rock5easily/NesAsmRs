use crate::{AssembleRequest, SourceEncoding, SourceLocation};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    rc::Rc,
};

/// One logical source line (continuations joined), ready for assembly.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub text: String,
    /// Shared by every line of the file.
    pub file: Rc<Path>,
    pub line: usize,
    /// Include and macro call sites leading to this line, outermost first.
    pub trace: Rc<[SourceLocation]>,
    pub expanded: bool,
}

impl Line {
    pub fn location(&self) -> SourceLocation {
        SourceLocation {
            file: self.file.to_path_buf(),
            line: self.line,
            column: None,
        }
    }
}

/// A decoded source file: line numbers and texts with continuations joined.
pub(crate) type SourceText = Rc<[(usize, String)]>;

/// Resolve existing paths or an output path whose nearest ancestor exists.
/// Canonicalization follows symlinks and Windows junctions before checking root.
pub fn resolve_path(path: &Path, base: &Path, root: Option<&Path>) -> Result<PathBuf, String> {
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    // `missing/..` would be resolved lexically on Windows but fail elsewhere;
    // reject it everywhere so all platforms agree.
    let mut prefix = PathBuf::new();
    for component in full.components() {
        if component == Component::ParentDir && !prefix.exists() {
            return Err("Invalid path: '..' after a directory that does not exist".into());
        }
        prefix.push(component);
    }
    let mut ancestor = full.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(ancestor.file_name().ok_or("Invalid path")?.to_owned());
        ancestor = ancestor.parent().ok_or("Invalid path")?;
    }
    let mut resolved = canonicalize(ancestor)?;
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    if let Some(root) = root {
        let root = canonicalize(root)?;
        if !resolved.starts_with(root) {
            return Err("Path is outside the project root".into());
        }
    }
    Ok(resolved)
}

/// `fs::canonicalize` without the Windows verbatim prefix (`\\?\`) when the
/// path stays valid without it, so diagnostics show ordinary paths that editors
/// recognize. Root checks compare paths produced by this same function.
pub(crate) fn canonicalize(path: &Path) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        const MAX_PATH: usize = 260;
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            if let Some(share) = rest.strip_prefix(r"UNC\") {
                if share.len() + 2 < MAX_PATH {
                    return Ok(PathBuf::from(format!(r"\\{share}")));
                }
            } else if rest.len() < MAX_PATH && rest.as_bytes().get(1) == Some(&b':') {
                return Ok(PathBuf::from(rest));
            }
        }
    }
    Ok(path)
}

/// Input files: the request's in-memory files, which take precedence, and
/// the file system.
pub(crate) struct Files {
    /// In-memory files by resolved path.
    memory: HashMap<PathBuf, Rc<[u8]>>,
}

impl Files {
    /// Resolves the in-memory file names like other paths of the request:
    /// relative to the working directory and within the allowed root.
    pub fn new(request: &AssembleRequest) -> Result<Self, String> {
        let mut memory = HashMap::new();
        for (name, data) in &request.files {
            let path = resolve_path(
                name,
                &request.working_directory,
                request.allowed_root.as_deref(),
            )
            .map_err(|e| format!("In-memory file '{}': {e}", name.display()))?;
            memory.insert(path, Rc::from(data.as_slice()));
        }
        Ok(Self { memory })
    }

    fn exists(&self, path: &Path) -> bool {
        self.memory.contains_key(path) || path.is_file()
    }

    /// Finds `name` in the working directory, then in the include paths.
    pub fn find(&self, request: &AssembleRequest, name: &Path) -> Result<PathBuf, String> {
        let mut bases = vec![request.working_directory.clone()];
        bases.extend(request.include_paths.iter().map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                request.working_directory.join(p)
            }
        }));
        // A base that rejects the path does not stop the search; report it only
        // when no other base provides the file.
        let mut rejected = None;
        for base in bases {
            match resolve_path(name, &base, request.allowed_root.as_deref()) {
                Ok(candidate) if self.exists(&candidate) => return Ok(candidate),
                Ok(_) => {}
                Err(e) => {
                    rejected.get_or_insert(e);
                }
            }
            if name.is_absolute() {
                break;
            }
        }
        Err(rejected.unwrap_or_else(|| format!("Cannot open file '{}'", name.display())))
    }

    pub fn len(&self, path: &Path) -> Result<u64, String> {
        match self.memory.get(path) {
            Some(data) => Ok(data.len() as u64),
            None => Ok(fs::metadata(path).map_err(|e| e.to_string())?.len()),
        }
    }

    /// Reads at most `limit` bytes; a longer file returns `limit + 1` bytes so
    /// the caller can report it as too large.
    pub fn read(&self, path: &Path, limit: u64) -> Result<Vec<u8>, String> {
        if let Some(data) = self.memory.get(path) {
            let end = data.len().min(
                usize::try_from(limit)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
            );
            return Ok(data[..end].to_vec());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    /// Reads `size` bytes at `offset`; the range must lie within the file.
    pub fn read_range(&self, path: &Path, offset: u64, size: usize) -> Result<Vec<u8>, String> {
        if let Some(data) = self.memory.get(path) {
            let start = usize::try_from(offset).map_err(|_| "Range out of bounds")?;
            return data
                .get(start..start + size)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| "Range out of bounds".into());
        }
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

/// Reads and decodes a source file, joining `\` continuation lines.
pub(crate) fn read_source(
    request: &AssembleRequest,
    files: &Files,
    path: &Path,
) -> Result<SourceText, String> {
    const SOURCE_LIMIT: u64 = 1024 * 1024;
    let bytes = files.read(path, SOURCE_LIMIT)?;
    if bytes.len() as u64 > SOURCE_LIMIT {
        return Err("Source exceeds 1 MiB".into());
    }
    let text = match request.options.encoding {
        SourceEncoding::Utf8 => {
            String::from_utf8(bytes).map_err(|_| "Invalid UTF-8; use SJIS for legacy sources")?
        }
        SourceEncoding::Sjis => {
            let (text, _, errors) = encoding_rs::SHIFT_JIS.decode(&bytes);
            if errors {
                return Err("Invalid SJIS source".into());
            }
            text.into_owned()
        }
    };
    let mut lines: Vec<(usize, String)> = Vec::new();
    let mut continuation = false;
    for (index, text) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let content = strip_comment(text);
        let next_continuation = content.trim_end().ends_with('\\');
        let piece = if next_continuation {
            content.trim_end().trim_end_matches('\\')
        } else {
            text
        };
        if continuation {
            let (_, last) = lines.last_mut().ok_or("Invalid continuation")?;
            last.push(' ');
            last.push_str(piece.trim());
        } else {
            lines.push((index + 1, piece.into()));
        }
        continuation = next_continuation;
    }
    if continuation {
        return Err("Unterminated line continuation".into());
    }
    Ok(lines.into())
}

/// Lines of a decoded file, as seen from one include site.
pub(crate) fn lines(
    source: &SourceText,
    file: &Rc<Path>,
    trace: &Rc<[SourceLocation]>,
) -> Vec<Line> {
    source
        .iter()
        .map(|(line, text)| Line {
            text: text.clone(),
            file: Rc::clone(file),
            line: *line,
            trace: Rc::clone(trace),
            expanded: false,
        })
        .collect()
}

pub(crate) fn strip_comment(text: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && ch == '\\' {
            // `\"` inside a string does not end it, as in the C# DB parser.
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && (ch == '"' || ch == '\'') {
            quote = Some(ch);
        } else if quote.is_none() && ch == ';' {
            return &text[..i];
        }
    }
    text
}

pub(crate) fn arguments(text: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote = None;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if quote == Some('"') && ch == '\\' {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_some() {
            continue;
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ['(', '[', '{'].contains(&ch) {
            depth += 1;
        } else if [')', ']', '}'].contains(&ch) {
            depth -= 1;
        } else if ch == ',' && depth == 0 {
            result.push(text[start..i].trim().into());
            start = i + 1;
        }
        if depth < 0 {
            return Err("Unbalanced parentheses".into());
        }
    }
    if quote.is_some() || depth != 0 {
        return Err("Unterminated string or parentheses".into());
    }
    if !text.trim().is_empty() {
        result.push(text[start..].trim().into());
    }
    if result.iter().any(String::is_empty) {
        return Err("Missing argument".into());
    }
    Ok(result)
}

pub(crate) fn macro_arguments(text: &str) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    for arg in arguments(text)? {
        let arg = if arg.starts_with('{') && arg.ends_with('}') {
            arg[1..arg.len() - 1].to_owned()
        } else {
            arg
        };
        if let Some(last) = args.last_mut()
            && ["x", "y", "x++", "y++"].contains(&arg.to_ascii_lowercase().as_str())
        {
            last.push(',');
            last.push_str(&arg);
        } else {
            args.push(arg);
        }
    }
    Ok(args)
}

pub(crate) fn quoted(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        Ok(text[1..text.len() - 1].into())
    } else {
        Err("Expected a quoted string".into())
    }
}
