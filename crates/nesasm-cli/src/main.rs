use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult, SourceEncoding};
use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

struct Arguments {
    request: AssembleRequest,
    output: Option<PathBuf>,
    json: bool,
    check: bool,
    watch: bool,
    usage: usize,
}
fn parse(args: Vec<String>) -> Result<Option<Arguments>, String> {
    if args.iter().any(|a| ["-?", "--help"].contains(&a.as_str())) {
        return Ok(None);
    }
    let mut options = AssembleOptions::default();
    let mut input = None;
    let mut output = None;
    let mut includes = Vec::new();
    let mut json = false;
    let mut check = false;
    let mut watch = false;
    let mut usage = 0;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-s" => usage = 1,
            "-S" => usage = 2,
            "-m" => options.macro_listing = true,
            "-raw" => options.raw = true,
            "-autozp" => options.auto_zp = true,
            "-srec" => options.srec = true,
            "-wd" => options.warning_disabled = true,
            "-watch" => watch = true,
            "--json" => json = true,
            "--check" => check = true,
            "--output" => output = Some(PathBuf::from(args.next().ok_or("Missing output path")?)),
            "--include" => includes.push(PathBuf::from(args.next().ok_or("Missing include path")?)),
            "-e" => {
                options.encoding = match args
                    .next()
                    .ok_or("Missing encoding")?
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "UTF8" => SourceEncoding::Utf8,
                    "SJIS" => SourceEncoding::Sjis,
                    _ => return Err("Encoding must be UTF8 or SJIS".into()),
                }
            }
            "-l" => {
                let n = args
                    .next()
                    .ok_or("Missing listing level")?
                    .parse::<i32>()
                    .map_err(|_| "Invalid listing level")?;
                options.list_level = n.clamp(0, 3) as u8;
            }
            "-l0" => options.list_level = 0,
            "-l1" => options.list_level = 1,
            "-l2" => options.list_level = 2,
            "-l3" => options.list_level = 3,
            _ if arg.starts_with('-') => return Err(format!("Unknown option '{arg}'")),
            _ => {
                if input.is_some() {
                    return Err("Only one input file is accepted".into());
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }
    let mut input = input.ok_or("Need input file")?;
    if !input
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("asm"))
    {
        input = PathBuf::from(format!("{}.asm", input.display()));
    }
    if let Some(paths) = env::var_os("NES_INCLUDE") {
        includes.extend(env::split_paths(&paths).take(10));
    }
    if watch && json {
        return Err("--json cannot be combined with -watch".into());
    }
    Ok(Some(Arguments {
        request: AssembleRequest {
            input,
            working_directory: env::current_dir().map_err(|e| e.to_string())?,
            include_paths: includes,
            allowed_root: None,
            options,
        },
        output,
        json,
        check,
        watch,
        usage,
    }))
}
fn run(args: &Arguments) -> AssembleResult {
    let mut result = nesasm_core::assemble(&args.request);
    if !args.check
        && result.success
        && let Err(e) = nesasm_core::write_artifacts(
            &result,
            &args.request.input,
            args.output.as_deref(),
            &args.request.working_directory,
            None,
            &args.request.options,
        )
    {
        result.success = false;
        result.diagnostics.push(nesasm_core::Diagnostic {
            severity: nesasm_core::Severity::Error,
            code: "E_OUTPUT".into(),
            message: e,
            location: nesasm_core::SourceLocation {
                file: args
                    .output
                    .clone()
                    .unwrap_or_else(|| args.request.input.with_extension("nes")),
                line: 0,
                column: None,
            },
            expansion_trace: vec![],
        });
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&result).expect("result serialization")
        );
    } else {
        for d in &result.diagnostics {
            eprintln!(
                "{}:{}: {} [{}]: {}",
                d.location.file.display(),
                d.location.line,
                d.severity,
                d.code,
                d.message
            );
        }
        if result.success {
            println!("Assembled {} bytes", result.binary.len());
        }
        for r in result.regions.values() {
            if let Some(size) = r.size {
                println!("Region {}: {} bytes", r.name, size);
            } else {
                println!(
                    "Region {}: {} not found",
                    r.name,
                    if r.begin.is_none() {
                        "BEGINREGION"
                    } else {
                        "ENDREGION"
                    }
                );
            }
        }
        if args.usage > 0 {
            for bank in &result.banks {
                println!(
                    "Bank {:02X}: {} / {} bytes",
                    bank.bank, bank.used, bank.capacity
                );
            }
        }
        if args.usage > 1 {
            for s in result.symbols.values().filter(|s| s.location.line > 0) {
                println!("{:02X}:{:04X} {}", s.bank, s.value, s.name);
            }
        }
    }
    result
}
fn fingerprint(paths: &[PathBuf]) -> BTreeMap<PathBuf, Option<(SystemTime, u64)>> {
    paths
        .iter()
        .map(|p| {
            (
                p.clone(),
                fs::metadata(p)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|t| (t, m.len()))),
            )
        })
        .collect()
}
fn main() {
    let args = match parse(env::args().skip(1).collect()) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{}", nesasm_core::reference(Some("options")).unwrap());
            return;
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut result = run(&args);
    if args.watch {
        // Keyboard handling uses a separate thread, so polling works on all three OSes.
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                if send.send(line.unwrap_or_default()).is_err() {
                    break;
                }
            }
        });
        println!("Watching dependencies. H + Enter: help; R + Enter: rebuild; Q + Enter: quit.");
        let mut paths = result.dependencies.clone();
        paths.push(args.request.working_directory.join(&args.request.input));
        // Watch include directories too, to recover from previously missing dependencies.
        paths.push(args.request.working_directory.clone());
        paths.extend(args.request.include_paths.iter().map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                args.request.working_directory.join(p)
            }
        }));
        let mut previous = fingerprint(&paths);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let key = recv.try_recv().unwrap_or_default().to_ascii_uppercase();
            if key == "Q" {
                break;
            }
            if key == "H" {
                println!("H: help, R: rebuild, Q: quit (press Enter after key)");
            }
            let current = fingerprint(&paths);
            if current != previous || key == "R" {
                // Debounce bursts from editors replacing files.
                std::thread::sleep(Duration::from_millis(100));
                result = run(&args);
                for p in result.dependencies {
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
                previous = fingerprint(&paths);
            }
        }
    } else if !result.success {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn legacy_options() {
        let a = parse(vec![
            "-l".into(),
            "9".into(),
            "-e".into(),
            "SJIS".into(),
            "-raw".into(),
            "demo".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.request.options.list_level, 3);
        assert!(a.request.options.raw);
        assert_eq!(a.request.input, Path::new("demo.asm"));
    }
    #[test]
    fn missing_argument() {
        assert!(parse(vec!["-e".into()]).is_err());
    }
}
