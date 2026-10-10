mod common;

use common::Temp;

/// A PCX header for an image of `width` x `height` pixels.
fn header(bpp: u8, planes: u8, rle: bool, width: u16, height: u16, stride: u16) -> Vec<u8> {
    let mut data = vec![0u8; 128];
    data[0] = 10;
    data[1] = 5;
    data[2] = u8::from(rle);
    data[3] = bpp;
    data[8..10].copy_from_slice(&(width - 1).to_le_bytes());
    data[10..12].copy_from_slice(&(height - 1).to_le_bytes());
    data[65] = planes;
    data[66..68].copy_from_slice(&stride.to_le_bytes());
    data
}

/// A 16x16 8-bit image whose pixels are `x % 4`.
fn image() -> Vec<u8> {
    let mut data = header(8, 1, false, 16, 16, 16);
    for _ in 0..16 {
        data.extend((0..16).map(|x| x % 4));
    }
    data
}

#[test]
fn incchr_regions() {
    let t = Temp::new();
    t.write("image.pcx", image());
    // width,height: two tiles from the top left.
    let r = t.run("  .incchr \"image.pcx\", 2, 1\n");
    assert!(r.success, "{:?}", r.diagnostics);
    assert_eq!(r.binary[..8], [0x55; 8]);
    assert_eq!(r.binary[8..16], [0x33; 8]);
    assert_eq!(r.binary[16..32], r.binary[..16]);
    for operand in ["1", "0, 0, 1"] {
        t.fails(
            &format!("  .incchr \"image.pcx\", {operand}\n"),
            "INCCHR expects no coordinates, width,height, or x,y,width,height",
        );
    }
    t.fails(
        "  .incchr \"image.pcx\", missing, 1\n",
        "Undefined symbol 'missing'",
    );
    t.fails(
        "  .zp\n  .incchr \"image.pcx\"\n",
        "Data emission not allowed in RAM section",
    );
}

#[test]
fn invalid_pcx_files() {
    let t = Temp::new();
    let valid = || header(8, 1, false, 16, 16, 16);
    let mut reversed_x = valid();
    reversed_x[4] = 20;
    let mut reversed_y = valid();
    reversed_y[6] = 20;
    let mut encoding = valid();
    encoding[2] = 2;
    let mut short_run = header(8, 1, true, 16, 16, 16);
    short_run.push(0xc5);
    let mut empty_run = header(8, 1, true, 16, 16, 16);
    empty_run.extend([0xc0, 1]);
    // Five runs of 63 bytes are more than the 256 pixels of the image.
    let mut long_run = header(8, 1, true, 16, 16, 16);
    long_run.extend((0..5).flat_map(|_| [0xff, 0]));
    let cases = [
        (reversed_x, "Invalid PCX width"),
        (reversed_y, "Invalid PCX height"),
        (
            header(8, 1, false, 8, 8, 8),
            "PCX dimensions must be 16x16 through 1024x768",
        ),
        (
            header(8, 1, false, 16, 769, 16),
            "PCX dimensions must be 16x16 through 1024x768",
        ),
        (header(4, 1, false, 16, 16, 16), "Unsupported PCX format"),
        (encoding, "Unsupported PCX format"),
        (header(8, 1, false, 16, 16, 0), "Invalid PCX stride"),
        (header(8, 1, false, 16, 16, 8), "Invalid PCX stride"),
        (header(1, 2, false, 16, 16, 1), "Invalid PCX stride"),
        (valid(), "Truncated PCX data"),
        (short_run, "Truncated PCX run"),
        (empty_run, "Invalid PCX run"),
        (long_run, "Invalid PCX run"),
    ];
    for (data, needle) in cases {
        t.write("bad.pcx", &data);
        t.fails("  .incchr \"bad.pcx\"\n", needle);
    }
}
