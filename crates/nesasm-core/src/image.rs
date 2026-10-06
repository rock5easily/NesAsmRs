pub(crate) fn packed_tile(rows: [u32; 8], validate: bool) -> Result<Vec<u8>, String> {
    let mut tile = vec![0; 16];
    // Preserve the C# port's byte-pointer semantics: the packed reader consumes
    // the first eight bytes of the little-endian u32 row buffer, one byte per row.
    for (y, row) in rows
        .into_iter()
        .take(2)
        .flat_map(u32::to_le_bytes)
        .enumerate()
    {
        let row = u32::from(row);
        for x in 0..8 {
            let pixel = (row >> ((7 - x) * 4)) & 15;
            if validate && pixel > 3 {
                return Err("Incorrect pixel color index".into());
            }
            tile[y] |= ((pixel & 1) as u8) << (7 - x);
            tile[y + 8] |= (((pixel >> 1) & 1) as u8) << (7 - x);
        }
    }
    Ok(tile)
}

pub(crate) fn pcx_tiles(data: &[u8], args: &[usize]) -> Result<Vec<u8>, String> {
    if data.len() < 128 || data[0] != 10 {
        return Err("Invalid PCX header".into());
    }
    let short = |i| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    let width = short(8).checked_sub(short(4)).ok_or("Invalid PCX width")? + 1;
    let width = (width + 1) & !1;
    let height = short(10)
        .checked_sub(short(6))
        .ok_or("Invalid PCX height")?
        + 1;
    if !(16..=1024).contains(&width) || !(16..=768).contains(&height) {
        return Err("PCX dimensions must be 16x16 through 1024x768".into());
    }
    let bpp = data[3];
    let planes = data[65] as usize;
    let stride = short(66);
    if !((bpp == 8 && planes == 1) || (bpp == 1 && (1..=4).contains(&planes))) || data[2] > 1 {
        return Err("Unsupported PCX format".into());
    }
    if stride == 0 || stride > 2048 || stride * 8 < width && bpp == 1 || stride < width && bpp == 8
    {
        return Err("Invalid PCX stride".into());
    }
    let expected = stride * planes * height;
    let mut decoded = Vec::with_capacity(expected);
    let mut pos = 128;
    while decoded.len() < expected {
        let mut value = *data.get(pos).ok_or("Truncated PCX data")?;
        pos += 1;
        let count = if data[2] == 1 && value & 0xc0 == 0xc0 {
            let n = (value & 63) as usize;
            value = *data.get(pos).ok_or("Truncated PCX run")?;
            pos += 1;
            n
        } else {
            1
        };
        if count == 0 || decoded.len() + count > expected {
            return Err("Invalid PCX run".into());
        }
        decoded.extend(std::iter::repeat_n(value, count));
    }
    let mut pixels = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            if bpp == 8 {
                pixels[y * width + x] = decoded[y * stride + x];
            } else {
                for plane in 0..planes {
                    pixels[y * width + x] |=
                        ((decoded[(y * planes + plane) * stride + x / 8] >> (7 - x % 8)) & 1)
                            << plane;
                }
            }
        }
    }
    let (x, y, w, h) = match args {
        [] => (0, 0, width / 8, height / 8),
        [w, h] => (0, 0, *w, *h),
        [x, y, w, h] => (*x, *y, *w, *h),
        _ => return Err("INCCHR expects no coordinates, width,height, or x,y,width,height".into()),
    };
    if x.checked_add(w.checked_mul(8).ok_or("PCX range overflow")?)
        .is_none_or(|end| end > width)
        || y.checked_add(h.checked_mul(8).ok_or("PCX range overflow")?)
            .is_none_or(|end| end > height)
    {
        return Err("PCX coordinates out of range".into());
    }
    let mut output = Vec::new();
    for ty in 0..h {
        for tx in 0..w {
            let mut tile = [0u8; 16];
            for py in 0..8 {
                for px in 0..8 {
                    let p = pixels[(y + ty * 8 + py) * width + x + tx * 8 + px] & 3;
                    tile[py] |= (p & 1) << (7 - px);
                    tile[py + 8] |= ((p >> 1) & 1) << (7 - px);
                }
            }
            output.extend(tile);
        }
    }
    Ok(output)
}
