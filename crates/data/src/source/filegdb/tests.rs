use super::*;
use crate::{Dataset, FieldType, Geometry};
use rustgis_geom::Coord;
use std::collections::HashMap;

/// A field for the test encoder: name, type code, nullable.
type FieldSpec = (&'static str, u8, bool);

/// A value for the test encoder.
#[derive(Clone, Debug)]
enum In {
    Null,
    I16(i16),
    I32(i32),
    I64(i64),
    F64(f64),
    Text(&'static str),
    Days(f64),
    Offset(f64, i16),
    Guid([u8; 16]),
    Bytes(Vec<u8>),
    Shape(Vec<u8>),
}

const SCALE: f64 = 1e6;
const ORIGIN: f64 = -400.0;

fn varuint(mut v: u64, out: &mut Vec<u8>) {
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

fn varint(v: i64, out: &mut Vec<u8>) {
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

fn utf16(s: &str, out: &mut Vec<u8>) {
    let u: Vec<u16> = s.encode_utf16().collect();
    out.push(u.len() as u8);
    out.extend(u.iter().flat_map(|c| c.to_le_bytes()));
}

fn q(v: f64) -> i64 {
    ((v - ORIGIN) * SCALE).round() as i64
}

/// A point blob.
fn point(x: f64, y: f64) -> Vec<u8> {
    let mut b = vec![1];
    varuint(q(x) as u64 + 1, &mut b);
    varuint(q(y) as u64 + 1, &mut b);
    b
}

/// A polyline (3) or polygon (5) blob from its parts.
fn parts(code: u64, parts: &[&[(f64, f64)]]) -> Vec<u8> {
    let pts: Vec<(f64, f64)> = parts.iter().flat_map(|p| p.iter().copied()).collect();
    let mut b = Vec::new();
    varuint(code, &mut b);
    varuint(pts.len() as u64, &mut b);
    varuint(parts.len() as u64, &mut b);
    let (x0, y0) = pts.iter().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
    let (x1, y1) = pts.iter().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
    for v in [q(x0), q(y0), q(x1) - q(x0), q(y1) - q(y0)] {
        varuint(v as u64, &mut b);
    }
    for p in &parts[..parts.len() - 1] {
        varuint(p.len() as u64, &mut b);
    }
    let (mut px, mut py) = (0i64, 0i64);
    for (x, y) in pts {
        varint(q(x) - px, &mut b);
        varint(q(y) - py, &mut b);
        (px, py) = (q(x), q(y));
    }
    b
}

/// A table's `.gdbtable` and `.gdbtablx`: rows by object ID (`None` for a deleted one).
fn table(geometry_type: u8, wkt: &str, fields: &[FieldSpec], rows: &[Option<Vec<In>>]) -> (Vec<u8>, Vec<u8>) {
    let mut desc = Vec::new();
    desc.extend(4u32.to_le_bytes());
    let flags: u32 = if geometry_type == 0 { 0x100 } else { 0x300 | u32::from(geometry_type) };
    desc.extend(flags.to_le_bytes());
    desc.extend((fields.len() as u16 + 1).to_le_bytes());
    utf16("OBJECTID", &mut desc);
    utf16("", &mut desc);
    desc.extend([6, 4, 2]);
    for (name, code, nullable) in fields {
        utf16(name, &mut desc);
        utf16("", &mut desc);
        desc.push(*code);
        let flag = if *nullable { 5 } else { 4 };
        match code {
            4 => {
                desc.extend(255u32.to_le_bytes());
                desc.extend([flag, 0]);
            }
            7 => {
                desc.extend([0, if *nullable { 7 } else { 6 }]);
                let w: Vec<u8> = wkt.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
                desc.extend((w.len() as u16).to_le_bytes());
                desc.extend(w);
                desc.push(1);
                // No extent: the reader works it out from the rows.
                for v in [ORIGIN, ORIGIN, SCALE, 0.001, f64::NAN, f64::NAN, f64::NAN, f64::NAN] {
                    desc.extend(v.to_le_bytes());
                }
                desc.push(0);
                desc.extend(1u32.to_le_bytes());
                desc.extend(1.0f64.to_le_bytes());
            }
            8 | 12 => desc.extend([0, flag]),
            10 | 11 => desc.extend([38, flag]),
            _ => desc.extend([8, flag, 0]),
        }
    }
    let nullable: Vec<bool> = fields.iter().map(|f| f.2).collect();
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    let start = 40 + 4 + desc.len() as u64;
    for row in rows {
        let Some(values) = row else {
            offsets.push(0u64);
            continue;
        };
        let mut r = vec![0u8; nullable.iter().filter(|n| **n).count().div_ceil(8)];
        let mut bit = 0;
        for (v, (_, code, n)) in values.iter().zip(fields) {
            if *n {
                if matches!(v, In::Null) {
                    r[bit / 8] |= 1 << (bit % 8);
                }
                bit += 1;
            }
            match v {
                In::Null => {}
                In::I16(x) => r.extend(x.to_le_bytes()),
                In::I32(x) => r.extend(x.to_le_bytes()),
                In::I64(x) => r.extend(x.to_le_bytes()),
                In::F64(x) if *code == 2 => r.extend((*x as f32).to_le_bytes()),
                In::F64(x) | In::Days(x) => r.extend(x.to_le_bytes()),
                In::Offset(d, m) => {
                    r.extend(d.to_le_bytes());
                    r.extend(m.to_le_bytes());
                }
                In::Text(s) => {
                    varuint(s.len() as u64, &mut r);
                    r.extend(s.as_bytes());
                }
                In::Guid(g) => r.extend(g),
                In::Bytes(b) | In::Shape(b) => {
                    varuint(b.len() as u64, &mut r);
                    r.extend(b);
                }
            }
        }
        offsets.push(start + body.len() as u64);
        body.extend((r.len() as u32).to_le_bytes());
        body.extend(r);
    }
    let mut t = Vec::new();
    t.extend(3u32.to_le_bytes());
    t.extend((rows.iter().flatten().count() as u32).to_le_bytes());
    t.extend([0u8; 16]);
    t.extend(0u64.to_le_bytes());
    t.extend(40u64.to_le_bytes());
    t.extend((desc.len() as u32).to_le_bytes());
    t.extend(desc);
    t.extend(body);
    let len = t.len() as u64;
    t[24..32].copy_from_slice(&len.to_le_bytes());
    let blocks = rows.len().div_ceil(1024);
    let mut x = Vec::new();
    for v in [3u32, blocks as u32, rows.len() as u32, 5] {
        x.extend(v.to_le_bytes());
    }
    for i in 0..blocks * 1024 {
        x.extend(&offsets.get(i).copied().unwrap_or(0).to_le_bytes()[..5]);
    }
    x.extend([0u8; 16]);
    (t, x)
}

/// A geodatabase's files: a catalog naming `tables` (name, `.gdbtable`, `.gdbtablx`), each at
/// object ID 9 onward, after the system tables.
fn gdb(tables: Vec<(&'static str, (Vec<u8>, Vec<u8>))>) -> HashMap<String, Vec<u8>> {
    let system = [
        "GDB_SystemCatalog",
        "GDB_DBTune",
        "GDB_SpatialRefs",
        "GDB_Items",
        "GDB_ItemTypes",
        "GDB_ItemRelationships",
        "GDB_ItemRelationshipTypes",
    ];
    let mut rows: Vec<Option<Vec<In>>> = system.iter().map(|n| Some(vec![In::Text(n), In::I32(0)])).collect();
    // The replica log has no table of its own.
    rows.push(Some(vec![In::Text("GDB_ReplicaLog"), In::I32(2)]));
    rows.extend(tables.iter().map(|(n, _)| Some(vec![In::Text(n), In::I32(0)])));
    let mut files = HashMap::new();
    let (t, x) = table(0, "", &[("Name", 4, false), ("FileFormat", 1, false)], &rows);
    files.insert("a00000001.gdbtable".to_string(), t);
    files.insert("a00000001.gdbtablx".to_string(), x);
    for (i, (_, (t, x))) in tables.into_iter().enumerate() {
        files.insert(format!("a{:08x}.gdbtable", i + 9), t);
        files.insert(format!("a{:08x}.gdbtablx", i + 9), x);
    }
    files
}

fn open(files: &HashMap<String, Vec<u8>>, layer: Option<&str>) -> Result<FileGdbSource> {
    FileGdbSource::open(Arc::new(Mem(files.clone())), layer)
}

const WGS84: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

fn sample() -> HashMap<String, Vec<u8>> {
    let fields: &[FieldSpec] = &[
        ("SHAPE", 7, true),
        ("name", 4, true),
        ("small", 0, true),
        ("count", 1, true),
        ("big", 13, true),
        ("ratio", 2, true),
        ("value", 3, true),
        ("when", 5, true),
        ("day", 14, true),
        ("time", 15, true),
        ("stamp", 16, true),
        ("blob", 8, true),
        ("id", 10, true),
        ("notes", 12, true),
    ];
    let guid = [0x78, 0x56, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x12, 0x34, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
    let full = vec![
        In::Shape(point(10.5, -20.25)),
        In::Text("Zürich ✓"),
        In::I16(-7),
        In::I32(70_000),
        In::I64(4_503_599_627_370_497),
        In::F64(1.5),
        In::F64(2.25),
        In::Days(45_293.127_835_648_15),
        In::Days(36_925.0),
        In::Days(0.551_562_5),
        In::Offset(45_418.297_326_388_885, -300),
        In::Bytes(vec![1, 2, 3]),
        In::Guid(guid),
        In::Text("<x/>"),
    ];
    let empty = vec![In::Null; fields.len()];
    let pts = table(
        1,
        WGS84,
        fields,
        &[
            Some(full),
            Some(empty),
            None,
            Some(vec![In::Shape(point(30.0, 40.0))].into_iter().chain(std::iter::repeat_n(In::Null, fields.len() - 1)).collect()),
        ],
    );
    let outer: &[(f64, f64)] = &[(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0), (0.0, 0.0)];
    let hole: &[(f64, f64)] = &[(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0), (2.0, 2.0)];
    let island: &[(f64, f64)] = &[(20.0, 20.0), (20.0, 25.0), (25.0, 25.0), (20.0, 20.0)];
    let polys = table(4, WGS84, &[("SHAPE", 7, true)], &[Some(vec![In::Shape(parts(5, &[outer, hole, island]))])]);
    let lines =
        table(3, WGS84, &[("SHAPE", 7, true)], &[Some(vec![In::Shape(parts(3, &[&[(0.0, 0.0), (1.0, 1.0)], &[(5.0, 5.0), (6.0, 7.0)]]))])]);
    let lookup = table(
        0,
        "",
        &[("code", 1, false), ("label", 4, true)],
        &[Some(vec![In::I32(1), In::Text("one")]), Some(vec![In::I32(2), In::Null])],
    );
    let attach = table(0, "", &[("DATA", 8, true)], &[]);
    gdb(vec![("places", pts), ("areas", polys), ("roads", lines), ("lookup", lookup), ("places__ATTACH", attach)])
}

#[test]
fn every_field_type() {
    let files = sample();
    let src = open(&files, Some("places")).unwrap();
    let info = src.info();
    assert_eq!(info.format, "File Geodatabase");
    assert_eq!(info.geometry_type, GeometryType::Point);
    assert_eq!(info.count, Some(3), "the deleted row is left out");
    let names: Vec<(&str, FieldType)> = info.fields.iter().map(|f| (f.name.as_str(), f.ty)).collect();
    // The object ID is each feature's id, and binary fields are left out.
    assert_eq!(
        names,
        [
            ("name", FieldType::Text),
            ("small", FieldType::Integer),
            ("count", FieldType::Integer),
            ("big", FieldType::Integer),
            ("ratio", FieldType::Double),
            ("value", FieldType::Double),
            ("when", FieldType::Text),
            ("day", FieldType::Text),
            ("time", FieldType::Text),
            ("stamp", FieldType::Text),
            ("id", FieldType::Text),
            ("notes", FieldType::Text),
        ]
    );
    let b = src.read(&Request::all()).unwrap();
    assert!(b.complete);
    assert_eq!(b.skipped, 0);
    let ids: Vec<u64> = b.features.iter().map(|f| f.id).collect();
    assert_eq!(ids, [1, 2, 4]);
    let f = &b.features[0];
    let Some(Geometry::Point(c)) = f.geometry else { panic!("{:?}", f.geometry) };
    assert!((c.x - 10.5).abs() < 1e-9 && (c.y + 20.25).abs() < 1e-9);
    assert_eq!(
        f.attrs,
        [
            Value::Text("Zürich ✓".into()),
            Value::Int(-7),
            Value::Int(70_000),
            Value::Int(4_503_599_627_370_497),
            Value::Float(1.5),
            Value::Float(2.25),
            Value::Text("2024-01-02T03:04:05".into()),
            Value::Text("2001-02-03".into()),
            Value::Text("13:14:15".into()),
            Value::Text("2024-05-06T07:08:09-05:00".into()),
            Value::Text("{12345678-1234-1234-1234-123456789ABC}".into()),
            Value::Text("<x/>".into()),
        ]
    );
    // All nulls, a null shape included.
    assert!(b.features[1].geometry.is_none());
    assert!(b.features[1].attrs.iter().all(|v| *v == Value::Null));
}

#[test]
fn polygons_lines_and_tables() {
    let files = sample();
    let polys = open(&files, Some("AREAS")).unwrap();
    assert_eq!(polys.info().geometry_type, GeometryType::Polygon);
    let b = polys.read(&Request::all()).unwrap();
    let Some(Geometry::Polygon(p)) = &b.features[0].geometry else { panic!() };
    // The hole stays with its outer ring; the island is a polygon of its own.
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].len(), 2);
    assert_eq!(p[0][1][1], Coord::new(4.0, 2.0));
    let roads = open(&files, Some("roads")).unwrap().read(&Request::all()).unwrap();
    let Some(Geometry::Line(l)) = &roads.features[0].geometry else { panic!() };
    assert_eq!(l.len(), 2);
    assert_eq!(l[1][1], Coord::new(6.0, 7.0));
    let lookup = open(&files, Some("lookup")).unwrap();
    assert_eq!(lookup.info().geometry_type, GeometryType::None);
    let b = lookup.read(&Request::all()).unwrap();
    assert_eq!(b.features[1].attrs, [Value::Int(2), Value::Null]);
    // A table has no extent: a request for one still gives every row.
    let some = lookup.read(&Request { extent: Some(Extent::new(0.0, 0.0, 1.0, 1.0)), limit: 10 }).unwrap();
    assert_eq!(some.features.len(), 2);
}

#[test]
fn catalog_leaves_out_system_tables_and_attachments() {
    let files = sample();
    let mem = Mem(files.clone());
    let names: Vec<String> = catalog(&mem).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["places", "areas", "roads", "lookup"]);
    // The first is the default, and an unknown name is refused.
    assert_eq!(open(&files, None).unwrap().info().name, "places");
    assert!(open(&files, Some("nope")).is_err());
    // A table whose files are missing is left out.
    let mut gone = files.clone();
    gone.remove("a0000000a.gdbtable");
    let names: Vec<String> = catalog(&Mem(gone)).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["places", "roads", "lookup"]);
    assert!(open(&HashMap::new(), None).is_err());
}

#[test]
fn extent_requests_and_samples() {
    let fields: &[FieldSpec] = &[("SHAPE", 7, true), ("n", 1, false)];
    // 50 × 40 points, past a block of the row index.
    let rows: Vec<Option<Vec<In>>> =
        (0..2000).map(|i| Some(vec![In::Shape(point(-100.0 + (i % 50) as f64, -20.0 + (i / 50) as f64)), In::I32(i)])).collect();
    let files = gdb(vec![("grid", table(1, WGS84, fields, &rows))]);
    let src = open(&files, None).unwrap();
    assert_eq!(src.info().count, Some(2000));
    let e = src.info().extent;
    assert!((e.min_x + 100.0).abs() < 1e-6 && (e.max_y - 19.0).abs() < 1e-6, "{e:?}");
    let b = src.read(&Request { extent: Some(Extent::new(-90.5, -10.5, -80.5, -0.5)), limit: 10_000 }).unwrap();
    assert_eq!(b.features.len(), 10 * 10);
    assert!(b.complete);
    let b = src.read(&Request { extent: None, limit: 100 }).unwrap();
    assert_eq!(b.features.len(), 100);
    assert_eq!(b.matched, 2000);
    assert!(!b.complete);
    // Spread over the whole table, not the first hundred.
    assert!(b.features.last().unwrap().id > 1900);
    let none = src.read(&Request { extent: Some(Extent::new(100.0, 50.0, 110.0, 60.0)), limit: 10 }).unwrap();
    assert!(none.features.is_empty());
}

#[test]
fn projected_coordinates_come_out_as_lonlat() {
    let utm = r#"PROJCS["WGS_1984_UTM_Zone_18N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-75.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
    // The test encoder's origin and scale fit metres too, at this size.
    let (t, x) = table(1, utm, &[("SHAPE", 7, true)], &[Some(vec![In::Shape(point(500.0, 100.0))])]);
    let src = open(&gdb(vec![("p", (t, x))]), None).unwrap();
    let b = src.read(&Request::all()).unwrap();
    let Some(Geometry::Point(c)) = b.features[0].geometry else { panic!() };
    // 499.5 km west of the central meridian, just north of the equator.
    assert!(c.x < -79.0 && c.x > -80.0 && c.y > 0.0 && c.y < 0.01, "{c:?}");
}

#[test]
fn zipped_geodatabases() {
    let files = sample();
    let entries: Vec<(String, Vec<u8>)> = files.iter().map(|(n, b)| (format!("data/sample.gdb/{n}"), b.clone())).collect();
    let mut with_other = entries.clone();
    with_other.push(("readme.txt".into(), b"hi".to_vec()));
    let zip = crate::zip::write(&with_other).unwrap();
    assert_eq!(zip_layers(&zip).unwrap(), ["places", "areas", "roads", "lookup"]);
    let src = FileGdbSource::open_zip(&zip, Some("roads")).unwrap();
    assert_eq!(src.read(&Request::all()).unwrap().features.len(), 1);
    // Every layer read whole, as a zip of Shapefiles is, up to the cap.
    let read = read_zip(&zip, 3).unwrap().unwrap();
    assert_eq!(read.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["places", "areas", "roads"]);
    assert!(read[0].1.messages.iter().any(|m| m.contains("lookup")), "{:?}", read[0].1.messages);
    let plain = crate::zip::write(&[("a.txt".into(), b"x".to_vec())]).unwrap();
    assert!(FileGdbSource::open_zip(&plain, None).is_err());
    assert!(read_zip(&plain, 3).unwrap().is_none());
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn folders_on_disk() {
    let dir = std::env::temp_dir().join(format!("rustgis-filegdb-{}.gdb", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (n, b) in sample() {
        std::fs::write(dir.join(n), b).unwrap();
    }
    let with_slash = format!("{}/", dir.display());
    assert!(is_gdb_name(&with_slash) && is_gdb_name("C:\\data\\x.GDB\\") && !is_gdb_name("x.gdbtable"));
    assert_eq!(layers(Path::new(&with_slash)).unwrap().len(), 4);
    let src = FileGdbSource::open_path(Path::new(&with_slash), Some("places")).unwrap();
    assert_eq!(src.read(&Request::all()).unwrap().features.len(), 3);
    assert!(FileGdbSource::open_path(&dir.join("missing.gdb"), None).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn damaged_files_never_panic() {
    let files = sample();
    for name in ["a00000001.gdbtable", "a00000001.gdbtablx", "a00000009.gdbtable", "a00000009.gdbtablx", "a0000000a.gdbtable"] {
        let whole = files[name].clone();
        for n in (0..whole.len()).step_by(3) {
            let mut cut = files.clone();
            cut.insert(name.into(), whole[..n].to_vec());
            for layer in [None, Some("areas")] {
                if let Ok(src) = open(&cut, layer) {
                    let _ = src.read(&Request::all());
                }
            }
        }
        // Bytes flipped, a few at a time.
        for at in (0..whole.len()).step_by(7) {
            let mut bad = files.clone();
            let mut b = whole.clone();
            b[at] ^= 0xff;
            bad.insert(name.into(), b);
            if let Ok(src) = open(&bad, None) {
                let _ = src.read(&Request::all());
            }
        }
    }
}

/// Writes what the reader reads from the geodatabases in `RUSTGIS_FILEGDB_FIXTURES` (separated
/// by `;`) as JSON lines to `RUSTGIS_FILEGDB_DUMP`, for comparing with GDAL by hand; at most
/// `RUSTGIS_FILEGDB_LIMIT` features a layer, spread over it.
#[test]
#[ignore = "needs geodatabases written by GDAL or ArcGIS"]
#[cfg(not(target_arch = "wasm32"))]
fn dump_fixtures() {
    use std::io::Write;
    let dirs = std::env::var("RUSTGIS_FILEGDB_FIXTURES").unwrap();
    let mut out = std::fs::File::create(std::env::var("RUSTGIS_FILEGDB_DUMP").unwrap()).unwrap();
    for dir in dirs.split(';') {
        for layer in layers(Path::new(dir)).unwrap() {
            let src = match FileGdbSource::open_path(Path::new(dir), Some(&layer)) {
                Ok(s) => s,
                Err(e) => {
                    writeln!(out, "{}", serde_json::json!({"gdb": dir, "layer": layer, "error": e.to_string()})).unwrap();
                    continue;
                }
            };
            let info = src.info().clone();
            let limit = std::env::var("RUSTGIS_FILEGDB_LIMIT").ok().and_then(|l| l.parse().ok()).unwrap_or(super::super::MAX_LIMIT);
            let batch = src.read(&Request { extent: None, limit }).unwrap();
            let fields: Vec<String> = info.fields.iter().map(|f| format!("{}:{:?}", f.name, f.ty)).collect();
            writeln!(
                out,
                "{}",
                serde_json::json!({"gdb": dir, "layer": layer, "fields": fields, "count": info.count, "crs": info.crs,
                    "extent": [info.extent.min_x, info.extent.min_y, info.extent.max_x, info.extent.max_y],
                    "notes": info.notes, "skipped": batch.skipped, "geometry_type": format!("{:?}", info.geometry_type)})
            )
            .unwrap();
            for f in batch.features {
                // The geometry summed up: its vertices' count and extent, and every 97th vertex.
                let mut coords = Vec::new();
                if let Some(g) = &f.geometry {
                    g.for_each_coord(|c| coords.push(c));
                }
                let e = f.geometry.as_ref().map(|g| g.extent()).filter(|e| !e.is_empty());
                let sample: Vec<[f64; 2]> = coords.iter().step_by(97).map(|c| [c.x, c.y]).collect();
                let geometry = f.geometry.as_ref().map(|g| {
                    serde_json::json!({"kind": format!("{:?}", g.kind()), "vertices": coords.len(), "sample": sample,
                        "extent": e.map(|e| [e.min_x, e.min_y, e.max_x, e.max_y])})
                });
                writeln!(out, "{}", serde_json::json!({"gdb": dir, "layer": layer, "id": f.id, "attrs": f.attrs, "geometry": geometry}))
                    .unwrap();
            }
        }
    }
}

/// Datasets covering what the writer does: every field type, every kind of geometry (and both
/// windings of polygon rings), mixed geometry, a table, awkward names and nulls.
fn writer_sample() -> Vec<Dataset> {
    use crate::GeometryType as G;
    let mut pts = Dataset::new(
        "Cities & Towns",
        G::Point,
        vec![
            crate::Field::new("name", FieldType::Text),
            crate::Field::new("pop", FieldType::Integer),
            crate::Field::new("big", FieldType::Integer),
            crate::Field::new("ratio", FieldType::Double),
            crate::Field::new("capital", FieldType::Boolean),
            crate::Field::new("1st choice", FieldType::Text),
            crate::Field::new("select", FieldType::Text),
            crate::Field::new("OBJECTID", FieldType::Integer),
            crate::Field::new("Name", FieldType::Text),
        ],
    );
    pts.push(
        Some(Geometry::Point(Coord::new(-77.0365, 38.8977))),
        vec![
            Value::Text("Washington ✓".into()),
            Value::Int(689_545),
            Value::Int(4_503_599_627_370_497),
            Value::Float(0.25),
            Value::Bool(true),
            Value::Text("yes".into()),
            Value::Text("a".into()),
            Value::Int(7),
            Value::Text("dup".into()),
        ],
    );
    pts.push(None, vec![Value::Text("Nowhere".into())]);
    pts.push(
        Some(Geometry::Point(Coord::new(2.3522, 48.8566))),
        vec![Value::Null, Value::Int(-5), Value::Null, Value::Float(f64::NAN), Value::Bool(false)],
    );
    let mut lines = Dataset::new("roads", G::Polyline, vec![crate::Field::new("k", FieldType::Integer)]);
    lines.push(
        Some(Geometry::Line(vec![
            vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)],
            vec![Coord::new(5.0, 5.0), Coord::new(6.0, 7.0), Coord::new(8.0, 7.5)],
        ])),
        vec![Value::Int(1)],
    );
    let mut polys = Dataset::new("parcels", G::Polygon, vec![crate::Field::new("k", FieldType::Integer)]);
    // Outer counter-clockwise and its hole clockwise (GeoJSON's winding), then an island
    // already in Esri's winding, not closed.
    let outer = vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 10.0), Coord::new(0.0, 0.0)];
    let hole = vec![Coord::new(2.0, 2.0), Coord::new(2.0, 4.0), Coord::new(4.0, 4.0), Coord::new(4.0, 2.0), Coord::new(2.0, 2.0)];
    let island = vec![Coord::new(20.0, 20.0), Coord::new(20.0, 25.0), Coord::new(25.0, 25.0)];
    polys.push(Some(Geometry::Polygon(vec![vec![outer, hole], vec![island]])), vec![Value::Int(1)]);
    let mut mixed = Dataset::new("mixed", G::None, vec![crate::Field::new("k", FieldType::Integer)]);
    mixed.push(Some(Geometry::MultiPoint(vec![Coord::new(1.0, 1.0), Coord::new(2.0, 2.0)])), vec![Value::Int(1)]);
    mixed.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(3.0, 3.0)]])), vec![Value::Int(2)]);
    mixed.push(None, vec![Value::Int(3)]);
    let mut table =
        Dataset::new("lookup", G::None, vec![crate::Field::new("code", FieldType::Integer), crate::Field::new("label", FieldType::Text)]);
    table.push(None, vec![Value::Int(1), Value::Text("one".into())]);
    table.push(None, vec![Value::Int(2), Value::Null]);
    vec![pts, lines, polys, mixed, table]
}

#[test]
fn written_geodatabases_read_back() {
    let sample = writer_sample();
    let refs: Vec<&Dataset> = sample.iter().collect();
    let (files, report) = write::encode(&refs).unwrap();
    let names: Vec<&str> = report.classes.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Cities___Towns", "roads", "parcels", "mixed_points", "mixed_lines", "lookup"]);
    assert_eq!(report.classes.iter().map(|c| c.1).collect::<Vec<_>>(), [3, 1, 1, 2, 1, 2]);
    assert!(report.notes.iter().any(|n| n.contains("1st choice written as F_1st_choice")), "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("select written as select_")), "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("OBJECTID written as OBJECTID_1")), "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("Name written as Name_1")), "{:?}", report.notes);
    let files: HashMap<String, Vec<u8>> = files.into_iter().collect();
    assert_eq!(files["gdb"], [5, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(catalog(&Mem(files.clone())).unwrap().into_iter().map(|e| e.name).collect::<Vec<_>>(), names);

    let pts = open(&files, Some("Cities___Towns")).unwrap();
    let info = pts.info().clone();
    assert_eq!(info.geometry_type, GeometryType::Point);
    let fields: Vec<(&str, FieldType)> = info.fields.iter().map(|f| (f.name.as_str(), f.ty)).collect();
    assert_eq!(
        fields,
        [
            ("name", FieldType::Text),
            ("pop", FieldType::Integer),
            ("big", FieldType::Integer),
            ("ratio", FieldType::Double),
            ("capital", FieldType::Integer),
            ("F_1st_choice", FieldType::Text),
            ("select_", FieldType::Text),
            ("OBJECTID_1", FieldType::Integer),
            ("Name_1", FieldType::Text),
        ]
    );
    let b = pts.read(&Request::all()).unwrap();
    assert_eq!(b.features.len(), 3);
    let Some(Geometry::Point(c)) = b.features[0].geometry else { panic!() };
    assert!((c.x + 77.0365).abs() < 1e-9 && (c.y - 38.8977).abs() < 1e-9, "{c:?}");
    assert_eq!(
        b.features[0].attrs,
        [
            Value::Text("Washington ✓".into()),
            Value::Int(689_545),
            Value::Int(4_503_599_627_370_497),
            Value::Float(0.25),
            Value::Int(1),
            Value::Text("yes".into()),
            Value::Text("a".into()),
            Value::Int(7),
            Value::Text("dup".into()),
        ]
    );
    assert!(b.features[1].geometry.is_none());
    assert_eq!(b.features[2].attrs[3], Value::Null, "NaN is written as null");
    assert_eq!(b.features[2].attrs[4], Value::Int(0));

    let polys = open(&files, Some("parcels")).unwrap().read(&Request::all()).unwrap();
    let Some(Geometry::Polygon(p)) = &polys.features[0].geometry else { panic!() };
    // The hole stays in its polygon and the island is closed: the reader groups rings by winding,
    // so this holds only if the writer wound them as Esri does.
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].len(), 2);
    assert_eq!(p[1][0].len(), 4);
    assert!(signed_area_of(&p[0][0]) < 0.0 && signed_area_of(&p[0][1]) > 0.0);

    let lines = open(&files, Some("roads")).unwrap().read(&Request::all()).unwrap();
    let Some(Geometry::Line(l)) = &lines.features[0].geometry else { panic!() };
    assert_eq!(l[1], [Coord::new(5.0, 5.0), Coord::new(6.0, 7.0), Coord::new(8.0, 7.5)]);

    let mixed_points = open(&files, Some("mixed_points")).unwrap();
    let b = mixed_points.read(&Request::all()).unwrap();
    assert!(matches!(&b.features[0].geometry, Some(Geometry::MultiPoint(p)) if p.len() == 2));
    assert!(b.features[1].geometry.is_none(), "features without geometry go with the first kind");

    let lookup = open(&files, Some("lookup")).unwrap();
    assert_eq!(lookup.info().geometry_type, GeometryType::None);
    assert_eq!(lookup.read(&Request::all()).unwrap().features[1].attrs, [Value::Int(2), Value::Null]);
}

fn signed_area_of(r: &[Coord]) -> f64 {
    r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum()
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn writing_folders_and_zips() {
    let sample = writer_sample();
    let refs: Vec<&Dataset> = sample.iter().collect();
    let dir = std::env::temp_dir().join(format!("rustgis-filegdb-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.gdb");
    write::write(&refs, &path).unwrap();
    assert_eq!(layers(&path).unwrap().len(), 6);
    // An existing geodatabase (or anything else) is never replaced.
    let again = write::write(&refs, &path).unwrap_err().to_string();
    assert!(again.contains("already exists"), "{again}");
    // Through `io::write_path`, by extension.
    let one = dir.join("roads.gdb");
    assert_eq!(crate::io::write_path(&sample[1], &one).unwrap(), 0);
    assert_eq!(layers(&one).unwrap(), ["roads"]);
    // Nothing left behind beside them.
    let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["out.gdb", "roads.gdb"]);
    // Zipped for downloading, and read back from the zip.
    let (zip, _) = write::zip(&refs, "sample").unwrap();
    assert_eq!(zip_layers(&zip).unwrap().len(), 6);
    let (bytes, _) = crate::io::write_bytes(&sample[2], "gdb").unwrap();
    assert_eq!(zip_layers(&bytes).unwrap(), ["parcels"]);
    assert!(write::encode(&[]).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coordinates_outside_lonlat_are_null_shapes() {
    let mut ds = Dataset::new("far", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(-500.0, 0.0))), vec![]);
    ds.push(Some(Geometry::Point(Coord::new(f64::NAN, 0.0))), vec![]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]);
    let (files, report) = write::encode(&[&ds]).unwrap();
    assert_eq!(report.null_shapes, 2);
    let b = open(&files.into_iter().collect(), None).unwrap().read(&Request::all()).unwrap();
    assert_eq!(b.features.iter().filter(|f| f.geometry.is_some()).count(), 1);
}

/// Writes the writer's sample to `RUSTGIS_FILEGDB_WRITE` (a new `.gdb` path), for opening in
/// GDAL and ArcGIS by hand.
#[test]
#[ignore = "writes to a path given by hand"]
#[cfg(not(target_arch = "wasm32"))]
fn write_sample() {
    let sample = writer_sample();
    let refs: Vec<&Dataset> = sample.iter().collect();
    let report = write::write(&refs, Path::new(&std::env::var("RUSTGIS_FILEGDB_WRITE").unwrap())).unwrap();
    println!("{report:?}");
}
