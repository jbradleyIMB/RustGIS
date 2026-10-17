//! Writing a `.gdbtable` and its `.gdbtablx`: the inverse of [`super::table`], for the writer
//! (and the reader's tests).

use rustgis_geom::Coord;

/// A field's type code in the field descriptors.
pub(super) mod code {
    pub const INT16: u8 = 0;
    pub const INT32: u8 = 1;
    pub const FLOAT64: u8 = 3;
    pub const TEXT: u8 = 4;
    pub const OBJECT_ID: u8 = 6;
    pub const GEOMETRY: u8 = 7;
    pub const BINARY: u8 = 8;
    pub const GUID: u8 = 10;
    pub const GLOBAL_ID: u8 = 11;
    pub const XML: u8 = 12;
    pub const INT64: u8 = 13;
}

/// A field to write.
#[derive(Clone, Debug)]
pub(super) struct FieldDef {
    pub name: String,
    pub code: u8,
    /// Its width: characters for text, bytes for numbers (ignored for other types).
    pub width: u32,
    /// The descriptor's flag byte: 1 nullable, 4 editable, 2 required (object and global IDs).
    pub flag: u8,
}

impl FieldDef {
    pub(super) fn new(name: impl Into<String>, code: u8, width: u32, nullable: bool) -> Self {
        let flag = match code {
            code::OBJECT_ID | code::GLOBAL_ID => 2,
            code::GEOMETRY => 6 | u8::from(nullable),
            _ => 4 | u8::from(nullable),
        };
        FieldDef { name: name.into(), code, width, flag }
    }

    fn nullable(&self) -> bool {
        self.flag & 1 != 0 && self.code != code::OBJECT_ID
    }
}

/// A geometry field's coordinate system and storage precision.
#[derive(Clone, Debug)]
pub(super) struct GeomDef {
    pub wkt: String,
    pub x_origin: f64,
    pub y_origin: f64,
    pub xy_scale: f64,
    pub xy_tolerance: f64,
    /// The features' extent (NaN for none).
    pub extent: [f64; 4],
    /// The spatial index's grid sizes (1 to 3).
    pub grids: Vec<f64>,
}

/// Z and M storage, the same for every geometry field written (two dimensions are written, but the
/// format stores these anyway).
const Z_ORIGIN: f64 = -100_000.0;
const Z_SCALE: f64 = 10_000.0;
const ZM_TOLERANCE: f64 = 0.001;

/// A value to write.
#[derive(Clone, Debug)]
pub(super) enum Out {
    Null,
    I16(i16),
    I32(i32),
    I64(i64),
    F64(f64),
    Text(String),
    /// A GUID's 16 bytes as stored (see [`guid_bytes`]).
    Guid([u8; 16]),
    /// A shape blob (see [`shape`]).
    Shape(Vec<u8>),
}

pub(super) fn varuint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub(super) fn varint(v: i64, out: &mut Vec<u8>) {
    let (neg, mut m) = (v < 0, v.unsigned_abs());
    let mut b = (m & 0x3f) as u8 | if neg { 0x40 } else { 0 };
    m >>= 6;
    while m != 0 {
        out.push(b | 0x80);
        b = (m & 0x7f) as u8;
        m >>= 7;
    }
    out.push(b);
}

fn utf16_name(s: &str, out: &mut Vec<u8>) {
    // Names are at most 160 characters, within the format's one-byte count.
    let u: Vec<u16> = s.encode_utf16().take(255).collect();
    out.push(u.len() as u8);
    out.extend(u.iter().flat_map(|c| c.to_le_bytes()));
}

/// Encodes a table: `geometry_type` 0 for none, 1 point, 2 multipoint, 3 polyline, 4 polygon; rows
/// are given object IDs from 1, and have a value for every field but the object ID.
pub(super) fn table(geometry_type: u8, fields: &[FieldDef], geom: Option<&GeomDef>, rows: &[Vec<Out>]) -> (Vec<u8>, Vec<u8>) {
    let mut desc = Vec::new();
    desc.extend(4u32.to_le_bytes());
    let flags: u32 = if geom.is_some() { 0x300 | u32::from(geometry_type) } else { 0x100 };
    desc.extend(flags.to_le_bytes());
    desc.extend((fields.len() as u16).to_le_bytes());
    for f in fields {
        utf16_name(&f.name, &mut desc);
        utf16_name("", &mut desc);
        desc.push(f.code);
        match f.code {
            code::OBJECT_ID => desc.extend([4, f.flag]),
            code::TEXT => {
                desc.extend(f.width.to_le_bytes());
                desc.extend([f.flag, 0]);
            }
            code::GEOMETRY => {
                let g = geom.cloned().unwrap_or(GeomDef {
                    wkt: String::new(),
                    x_origin: 0.0,
                    y_origin: 0.0,
                    xy_scale: 1.0,
                    xy_tolerance: 1.0,
                    extent: [f64::NAN; 4],
                    grids: vec![1.0],
                });
                desc.extend([0, f.flag]);
                let w: Vec<u8> = g.wkt.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
                desc.extend((w.len().min(u16::MAX as usize) as u16).to_le_bytes());
                desc.extend(w.get(..w.len().min(u16::MAX as usize & !1)).unwrap_or(&[]));
                // Origins and scales for M and Z follow (bits 1 and 2).
                desc.push(7);
                for v in
                    [g.x_origin, g.y_origin, g.xy_scale, Z_ORIGIN, Z_SCALE, Z_ORIGIN, Z_SCALE, g.xy_tolerance, ZM_TOLERANCE, ZM_TOLERANCE]
                {
                    desc.extend(v.to_le_bytes());
                }
                for v in g.extent {
                    desc.extend(v.to_le_bytes());
                }
                desc.push(0);
                let grids = if g.grids.is_empty() { vec![1.0] } else { g.grids.clone() };
                desc.extend((grids.len().min(3) as u32).to_le_bytes());
                for v in grids.iter().take(3) {
                    desc.extend(v.to_le_bytes());
                }
            }
            code::BINARY | code::XML => desc.extend([0, f.flag]),
            code::GUID | code::GLOBAL_ID => desc.extend([38, f.flag]),
            code::INT16 => desc.extend([2, f.flag, 0]),
            code::INT32 => desc.extend([4, f.flag, 0]),
            _ => desc.extend([8, f.flag, 0]),
        }
    }
    let nullable: Vec<bool> = fields.iter().map(FieldDef::nullable).collect();
    let null_bytes = nullable.iter().filter(|n| **n).count().div_ceil(8);
    let mut body = Vec::new();
    let mut offsets = Vec::with_capacity(rows.len());
    let start = 40 + 4 + desc.len() as u64;
    let mut max_row = 0usize;
    for row in rows {
        let mut r = vec![0u8; null_bytes];
        let mut bit = 0;
        let mut values = row.iter();
        for (f, n) in fields.iter().zip(&nullable) {
            if f.code == code::OBJECT_ID {
                continue;
            }
            let v = values.next().unwrap_or(&Out::Null);
            if *n {
                if matches!(v, Out::Null) {
                    if let Some(b) = r.get_mut(bit / 8) {
                        *b |= 1 << (bit % 8);
                    }
                    bit += 1;
                    continue;
                }
                bit += 1;
            }
            value(f, v, &mut r);
        }
        offsets.push(start + body.len() as u64);
        max_row = max_row.max(r.len());
        body.extend((r.len() as u32).to_le_bytes());
        body.extend(r);
    }
    let mut t = Vec::with_capacity(40 + 4 + desc.len() + body.len());
    t.extend(3u32.to_le_bytes());
    t.extend((rows.len() as u32).to_le_bytes());
    t.extend((max_row as u32).to_le_bytes());
    t.extend(5u32.to_le_bytes());
    t.extend([0u8; 8]);
    let len = (40 + 4 + desc.len() + body.len()) as u64;
    t.extend(len.to_le_bytes());
    t.extend(40u64.to_le_bytes());
    t.extend((desc.len() as u32).to_le_bytes());
    t.extend(desc);
    t.extend(body);
    let blocks = rows.len().div_ceil(1024).max(1);
    let mut x = Vec::with_capacity(16 + blocks * 1024 * 5 + 16);
    for v in [3u32, blocks as u32, rows.len() as u32, 5] {
        x.extend(v.to_le_bytes());
    }
    for i in 0..blocks * 1024 {
        let o = offsets.get(i).copied().unwrap_or(0);
        x.extend(o.to_le_bytes().get(..5).unwrap_or(&[0; 5]));
    }
    // The trailer: no bitmap (every block is stored), the blocks counted twice.
    for v in [0u32, blocks as u32, blocks as u32, 0] {
        x.extend(v.to_le_bytes());
    }
    (t, x)
}

/// A value as its field's type stores it (a value of another type is converted, or written as
/// zero when it can't be).
fn value(f: &FieldDef, v: &Out, r: &mut Vec<u8>) {
    let int = |v: &Out| -> i64 {
        match v {
            Out::I16(x) => i64::from(*x),
            Out::I32(x) => i64::from(*x),
            Out::I64(x) => *x,
            Out::F64(x) if x.is_finite() => *x as i64,
            _ => 0,
        }
    };
    match f.code {
        code::INT16 => r.extend((int(v).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16).to_le_bytes()),
        code::INT32 => r.extend((int(v).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32).to_le_bytes()),
        code::INT64 => r.extend(int(v).to_le_bytes()),
        code::FLOAT64 => {
            let x = match v {
                Out::F64(x) => *x,
                other => int(other) as f64,
            };
            r.extend(x.to_le_bytes());
        }
        code::TEXT | code::XML => {
            let s = match v {
                Out::Text(s) => s.as_str(),
                _ => "",
            };
            varuint(s.len() as u64, r);
            r.extend(s.as_bytes());
        }
        code::GUID | code::GLOBAL_ID => r.extend(match v {
            Out::Guid(g) => *g,
            _ => [0; 16],
        }),
        code::GEOMETRY | code::BINARY => {
            let b: &[u8] = match v {
                Out::Shape(b) => b,
                _ => &[],
            };
            varuint(b.len() as u64, r);
            r.extend(b);
        }
        _ => {}
    }
}

/// A shape's coordinates quantized at the geometry field's precision; `None` when one is outside
/// what the field can store (left of or below its origin, or non-finite).
fn quantize(c: Coord, g: &GeomDef) -> Option<(i64, i64)> {
    let x = ((c.x - g.x_origin) * g.xy_scale).round();
    let y = ((c.y - g.y_origin) * g.xy_scale).round();
    let max = (1u64 << 62) as f64;
    ((0.0..max).contains(&x) && (0.0..max).contains(&y)).then_some((x as i64, y as i64))
}

/// A point's blob.
pub(super) fn point(c: Coord, g: &GeomDef) -> Option<Vec<u8>> {
    let (x, y) = quantize(c, g)?;
    let mut b = vec![1];
    varuint(x as u64 + 1, &mut b);
    varuint(y as u64 + 1, &mut b);
    Some(b)
}

/// A multipoint's blob.
pub(super) fn multipoint(pts: &[Coord], g: &GeomDef) -> Option<Vec<u8>> {
    let q: Vec<(i64, i64)> = pts.iter().map(|c| quantize(*c, g)).collect::<Option<_>>()?;
    let mut b = vec![8];
    varuint(q.len() as u64, &mut b);
    bbox(&q, &mut b)?;
    deltas(&q, &mut b);
    Some(b)
}

/// A polyline's (shape type 3) or polygon's (5) blob, from its parts.
pub(super) fn parts(shape_type: u8, parts: &[Vec<Coord>], g: &GeomDef) -> Option<Vec<u8>> {
    let mut q: Vec<Vec<(i64, i64)>> =
        parts.iter().map(|p| p.iter().map(|c| quantize(*c, g)).collect::<Option<Vec<_>>>()).collect::<Option<_>>()?;
    // A vertex stored on the one before it makes a zero-length segment, which ArcGIS's Check
    // Geometry reports: dropped, and a part left too short to be a ring or a line with it.
    let min = if shape_type == 5 { 4 } else { 2 };
    for p in &mut q {
        p.dedup();
    }
    q.retain(|p| p.len() >= min);
    let all: Vec<(i64, i64)> = q.iter().flatten().copied().collect();
    if all.is_empty() {
        return None;
    }
    let mut b = vec![shape_type];
    varuint(all.len() as u64, &mut b);
    varuint(q.len() as u64, &mut b);
    bbox(&all, &mut b)?;
    for p in q.iter().take(q.len().saturating_sub(1)) {
        varuint(p.len() as u64, &mut b);
    }
    deltas(&all, &mut b);
    Some(b)
}

fn bbox(q: &[(i64, i64)], b: &mut Vec<u8>) -> Option<()> {
    let x0 = q.iter().map(|p| p.0).min()?;
    let y0 = q.iter().map(|p| p.1).min()?;
    let x1 = q.iter().map(|p| p.0).max()?;
    let y1 = q.iter().map(|p| p.1).max()?;
    for v in [x0, y0, x1 - x0, y1 - y0] {
        varuint(v as u64, b);
    }
    Some(())
}

fn deltas(q: &[(i64, i64)], b: &mut Vec<u8>) {
    let (mut px, mut py) = (0i64, 0i64);
    for (x, y) in q {
        varint(x - px, b);
        varint(y - py, b);
        (px, py) = (*x, *y);
    }
}
