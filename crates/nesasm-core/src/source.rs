use crate::{AssembleRequest, SourceEncoding, SourceLocation};
use std::{
    fs,
    io::Read,
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

pub(crate) fn find_file(request: &AssembleRequest, name: &Path) -> Result<PathBuf, String> {
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
            Ok(candidate) if candidate.is_file() => return Ok(candidate),
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

/// Reads and decodes a source file, joining `\` continuation lines.
pub(crate) fn read_source(request: &AssembleRequest, path: &Path) -> Result<SourceText, String> {
    const SOURCE_LIMIT: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take((SOURCE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > SOURCE_LIMIT {
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
