mod common;

use common::{Project, Watch};
use std::fs;

#[test]
fn keys_show_help_and_rebuild() {
    let p = Project::new();
    p.write("t.asm", "  .db 1\n");
    let mut watch = Watch::start(&p, &["-watch", "t.asm"]);
    watch.expect("Assembled");
    assert!(watch.expect("Watching dependencies.").contains("Q + Enter"));
    watch.send("h");
    assert_eq!(
        watch.expect("H:"),
        "H: help, R: rebuild, Q: quit (press Enter after key)"
    );
    // R rebuilds without a change; the ROM is written again.
    fs::remove_file(p.path("t.nes")).unwrap();
    watch.send("R");
    watch.expect("Assembled");
    assert!(p.path("t.nes").exists());
    watch.quit();
}

#[test]
fn failed_builds_recover_when_a_missing_file_appears() {
    let p = Project::new();
    p.write("t.asm", "  .include \"later.asm\"\n");
    let mut watch = Watch::start(&p, &["-watch", "t.asm"]);
    watch.ready();
    // The project directory is watched, so a new file triggers a rebuild.
    p.write("later.asm", "  .db 7\n");
    watch.expect("Assembled");
    assert_eq!(fs::read(p.path("t.nes")).unwrap()[16], 7);
    // The new dependency is watched from now on.
    watch.ready();
    p.write("later.asm", "  .db 8, 9\n");
    watch.expect("Assembled");
    assert_eq!(&fs::read(p.path("t.nes")).unwrap()[16..18], &[8, 9]);
    watch.quit();
}

#[test]
fn include_directories_are_watched() {
    let p = Project::new();
    fs::create_dir(p.path("lib")).unwrap();
    p.write("t.asm", "  .include \"lib.asm\"\n");
    let lib = p.path("lib");
    let lib = lib.to_str().unwrap();
    // One absolute and one relative include directory.
    let mut watch = Watch::start(
        &p,
        &["-watch", "--include", lib, "--include", "other", "t.asm"],
    );
    watch.ready();
    p.write("lib/lib.asm", "  .db 3\n");
    watch.expect("Assembled");
    assert_eq!(fs::read(p.path("t.nes")).unwrap()[16], 3);
    watch.quit();
}
