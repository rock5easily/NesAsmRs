use crate::{AssembleRequest, SourceEncoding, SourceLocation};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub text: String,
    pub location: SourceLocation,
    pub trace: Rc<[SourceLocation]>,
    pub expanded: bool,
}

/// Resolve existing paths or an output path whose nearest ancestor exists.
/// Canonicalization follows symlinks and Windows junctions before checking root.
pub fn resolve_path(path: &Path, base: &Path, root: Option<&Path>) -> Result<PathBuf, String> {
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    let mut ancestor = full.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(ancestor.file_name().ok_or("Invalid path")?.to_owned());
        ancestor = ancestor.parent().ok_or("Invalid path")?;
    }
    let mut resolved = fs::canonicalize(ancestor).map_err(|e| e.to_string())?;
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    if let Some(root) = root {
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        if !resolved.starts_with(root) {
            return Err("Path is outside the project root".into());
        }
    }
    Ok(resolved)
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
    for base in bases {
        let candidate = resolve_path(name, &base, request.allowed_root.as_deref())?;
        if candidate.is_file() {
            return Ok(candidate);
        }
        if name.is_absolute() {
            break;
        }
    }
    Err(format!("Cannot open file '{}'", name.display()))
}

pub(crate) fn read_lines(
    request: &AssembleRequest,
    path: &Path,
    trace: &Rc<[SourceLocation]>,
) -> Result<Vec<Line>, String> {
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
    let mut lines: Vec<Line> = Vec::new();
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
            let last = lines.last_mut().ok_or("Invalid continuation")?;
            last.text.push(' ');
            last.text.push_str(piece.trim());
        } else {
            lines.push(Line {
                text: piece.into(),
                location: SourceLocation {
                    file: path.into(),
                    line: index + 1,
                    column: None,
                },
                trace: Rc::clone(trace),
                expanded: false,
            });
        }
        continuation = next_continuation;
    }
    if continuation {
        return Err("Unterminated line continuation".into());
    }
    Ok(lines)
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
        if !args.is_empty() && ["x", "y", "x++", "y++"].contains(&arg.to_ascii_lowercase().as_str())
        {
            let last = args.last_mut().unwrap();
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
