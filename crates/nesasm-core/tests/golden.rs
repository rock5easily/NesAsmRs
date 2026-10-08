//! Compares the assembler with C# results recorded by
//! `python tools/compat/compare.py --write-golden tools/compat/golden.json`,
//! so the compatibility cases also run on Linux and macOS.

use nesasm_core::{AssembleOptions, AssembleRequest, AssembleResult, ListLevel, SourceEncoding};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const PROJECT_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// 64-bit FNV-1a, as computed by compare.py.
fn fnv1a(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn options(flags: &[Value]) -> AssembleOptions {
    let mut options = AssembleOptions::default();
    for flag in flags {
        match flag.as_str().unwrap() {
            "--sjis" => options.encoding = SourceEncoding::Sjis,
            "--raw" => options.raw = true,
            "--autozp" => options.auto_zp = true,
            "--srec" => options.srec = true,
            "--list0" => options.list_level = ListLevel::Off,
            "--list1" => options.list_level = ListLevel::Brief,
            "--list3" => options.list_level = ListLevel::Full,
            "--mlist" => options.macro_listing = true,
            other => panic!("unknown flag {other}"),
        }
    }
    options
}

/// Writes a generated case and the shared auxiliary files into `dir`.
fn prepare(dir: &Path, source: &Value, aux: &Value) -> PathBuf {
    if let Some(file) = source.get("file") {
        return Path::new(PROJECT_ROOT).join(file.as_str().unwrap());
    }
    fs::create_dir_all(dir).unwrap();
    for (name, bytes) in aux.as_object().unwrap() {
        let bytes: Vec<u8> = serde_json::from_value(bytes.clone()).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
    }
    let input = dir.join("case.asm");
    if let Some(text) = source.get("text") {
        fs::write(&input, text.as_str().unwrap()).unwrap();
    } else {
        let text = source["sjis_text"].as_str().unwrap();
        fs::write(&input, encoding_rs::SHIFT_JIS.encode(text).0).unwrap();
    }
    input
}

fn check(
    name: &str,
    result: &AssembleResult,
    input: &Path,
    expected: &Value,
) -> Result<(), String> {
    let fail = |what: &str, a: &dyn std::fmt::Debug, b: &dyn std::fmt::Debug| {
        Err(format!("{name}: {what}: C# {a:?}, Rust {b:?}"))
    };
    let success = expected["success"].as_bool().unwrap();
    if success != result.success {
        return fail("success", &success, &result.diagnostics);
    }
    if !success {
        let errors: Vec<usize> = serde_json::from_value(expected["errors"].clone()).unwrap();
        let rust: BTreeSet<usize> = result
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.location.line)
            .collect();
        let rust: Vec<usize> = rust.into_iter().collect();
        return if errors == rust {
            Ok(())
        } else {
            fail("error lines", &errors, &result.diagnostics)
        };
    }
    for (field, data) in [("binary", &result.binary), ("map", &result.map)] {
        if expected[field] != fnv1a(data) {
            return fail(field, &expected[field], &fnv1a(data));
        }
    }
    let header: Vec<u8> = serde_json::from_value(expected["header"].clone()).unwrap();
    if header != result.header {
        return fail("header", &header, &result.header);
    }
    let symbols: BTreeMap<String, [u64; 3]> =
        serde_json::from_value(expected["symbols"].clone()).unwrap();
    let ignored: BTreeSet<String> =
        serde_json::from_value(expected["ignored_symbols"].clone()).unwrap_or_default();
    let rust_names: BTreeSet<&String> = result
        .symbols
        .keys()
        .filter(|n| !ignored.contains(*n))
        .collect();
    if rust_names != symbols.keys().collect() {
        return fail("symbol names", &symbols.keys(), &rust_names);
    }
    for (symbol, [value, bank, size]) in &symbols {
        let s = &result.symbols[symbol];
        let rust = [
            u64::from(s.value),
            u64::from(s.bank.number()),
            s.size as u64,
        ];
        if rust != [*value, *bank, *size] {
            return fail(&format!("symbol {symbol}"), &[value, bank, size], &rust);
        }
    }
    for (region, size) in expected["regions"].as_object().unwrap() {
        let rust = result.regions.get(region).and_then(|r| r.size);
        if size.as_i64() != rust {
            return fail(&format!("region {region}"), size, &rust);
        }
    }
    let header_line = format!("#[1]   {}", input.display());
    let listing = result
        .listing
        .as_deref()
        .map(|l| l.replace(&header_line, "#[1]   <input>"));
    if expected["listing"].as_str() != listing.as_deref() {
        return fail("listing", &expected["listing"], &listing);
    }
    if expected["srec"].as_str() != result.srec.as_deref() {
        return fail("srec", &expected["srec"], &result.srec);
    }
    Ok(())
}

#[test]
fn matches_recorded_csharp_results() {
    let golden_path = Path::new(PROJECT_ROOT).join("tools/compat/golden.json");
    let golden: Value = serde_json::from_str(&fs::read_to_string(golden_path).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    let cases = golden["cases"].as_array().unwrap();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input = prepare(
            &scratch.path().join(name),
            &case["source"],
            &golden["aux_files"],
        );
        let request = AssembleRequest {
            input: input.file_name().unwrap().into(),
            working_directory: input.parent().unwrap().into(),
            include_paths: vec![],
            allowed_root: None,
            options: options(case["flags"].as_array().unwrap()),
        };
        let result = nesasm_core::assemble(&request);
        if let Err(e) = check(name, &result, &request.input, &case["expected"]) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
