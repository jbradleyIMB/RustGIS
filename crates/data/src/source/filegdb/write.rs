//! Writing File Geodatabases: each dataset a feature class (or a table, without geometry) in a new
//! `.gdb` folder that ArcGIS opens.
//!
//! Coordinates are written as WGS 84 longitude/latitude at a nanodegree, as Esri stores geographic
//! data. A feature class holds one kind of geometry, so a dataset mixing points, lines and
//! polygons becomes a class for each. Names are made valid for ArcGIS (letters, digits and
//! underscores, starting with a letter, not a reserved word), and said in the report when changed.
//! Each feature class gets a spatial index (see [`spx`]), as ArcGIS keeps one.

use super::encode::{self, FieldDef, GeomDef, Out, code};
use super::{spx, system};
use crate::{DataError, Dataset, Feature, FieldType, Geometry, Result, Value};
use rustgis_geom::Coord;

/// What writing a geodatabase did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Each feature class or table written: its name and number of rows.
    pub classes: Vec<(String, usize)>,
    /// Features left out: a geometry that couldn't be stored (its coordinates outside
    /// longitude/latitude) is written as a null shape and counted here.
    pub null_shapes: usize,
    /// Names changed, classes split, and the like.
    pub notes: Vec<String>,
}

/// Most characters in a table's and a field's name.
const MAX_TABLE_NAME: usize = 160;
const MAX_FIELD_NAME: usize = 64;

/// Words ArcGIS doesn't take as names.
const RESERVED: &[&str] = &[
    "ADD", "ALTER", "AND", "BETWEEN", "BY", "COLUMN", "CREATE", "DELETE", "DROP", "EXISTS", "FOR", "FROM", "GROUP", "IN", "INSERT", "INTO",
    "IS", "LIKE", "NOT", "NULL", "OR", "ORDER", "SELECT", "SET", "TABLE", "UPDATE", "VALUES", "WHERE",
];

/// A name made valid and unique among `taken` (compared without case), which it joins.
fn valid_name(name: &str, max: usize, fallback: &str, taken: &mut Vec<String>) -> String {
    let mut n: String = name.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if n.is_empty() {
        n = fallback.to_string();
    }
    if !n.starts_with(|c: char| c.is_ascii_alphabetic()) {
        n = format!("{}_{n}", fallback.chars().next().unwrap_or('F').to_ascii_uppercase());
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&n)) || n.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("GDB_")) {
        n.push('_');
    }
    n.truncate(max);
    let base = n.clone();
    let mut i = 1;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&n)) {
        let suffix = format!("_{i}");
        n = format!("{}{suffix}", base.get(..base.len().min(max - suffix.len())).unwrap_or(&base));
        i += 1;
    }
    taken.push(n.clone());
    n
}

/// One kind of geometry, for splitting a dataset that mixes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Points,
    Lines,
    Polygons,
}

impl Family {
    fn of(g: &Geometry) -> Family {
        match g {
            Geometry::Point(_) | Geometry::MultiPoint(_) => Family::Points,
            Geometry::Line(_) => Family::Lines,
            Geometry::Polygon(_) => Family::Polygons,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Family::Points => "points",
            Family::Lines => "lines",
            Family::Polygons => "polygons",
        }
    }
}

/// A column as written: its descriptor and how values become it.
struct Column {
    def: FieldDef,
    source: usize,
}

/// The columns for `ds`'s fields, typed for the values in `features`.
fn columns(ds: &Dataset, features: &[&Feature], taken: &mut Vec<String>, notes: &mut Vec<String>, class: &str) -> Vec<Column> {
    ds.fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let name = valid_name(&f.name, MAX_FIELD_NAME, "field", taken);
            if name != f.name {
                notes.push(format!("{class}: field {} written as {name}", f.name));
            }
            let values = || features.iter().filter_map(|x| x.attrs.get(i));
            let def = match f.ty {
                FieldType::Integer => {
                    let wide = values().any(|v| matches!(v, Value::Int(x) if i32::try_from(*x).is_err()));
                    if wide { FieldDef::new(name, code::INT64, 8, true) } else { FieldDef::new(name, code::INT32, 4, true) }
                }
                FieldType::Double => FieldDef::new(name, code::FLOAT64, 8, true),
                FieldType::Boolean => FieldDef::new(name, code::INT16, 2, true),
                FieldType::Text => {
                    // Room for every value, and at least ArcGIS's default.
                    let longest = values().map(|v| v.to_string().chars().count()).max().unwrap_or(0);
                    FieldDef::new(name, code::TEXT, longest.max(255).min(i32::MAX as usize) as u32, true)
                }
            };
            Column { def, source: i }
        })
        .collect()
}

/// A value as a column takes it.
fn out(v: &Value, col: &FieldDef) -> Out {
    match (v, col.code) {
        (Value::Null, _) => Out::Null,
        (Value::Text(s), code::TEXT) => Out::Text(s.clone()),
        (other, code::TEXT) => Out::Text(other.to_string()),
        (Value::Int(i), code::INT32) => i32::try_from(*i).map_or(Out::Null, Out::I32),
        (Value::Int(i), code::INT64) => Out::I64(*i),
        (Value::Bool(b), code::INT16) => Out::I16(i16::from(*b)),
        (Value::Bool(b), code::INT32) => Out::I32(i32::from(*b)),
        (Value::Bool(b), code::INT64) => Out::I64(i64::from(*b)),
        (v, code::FLOAT64) => v.as_f64().map_or(Out::Null, Out::F64),
        (Value::Float(f), code::INT32 | code::INT64 | code::INT16) if f.is_finite() && f.fract() == 0.0 => match col.code {
            code::INT64 => Out::I64(*f as i64),
            code::INT32 => i32::try_from(*f as i64).map_or(Out::Null, Out::I32),
            _ => i16::try_from(*f as i64).map_or(Out::Null, Out::I16),
        },
        (Value::Text(s), code::INT32 | code::INT64 | code::INT16) => match s.trim().parse::<i64>() {
            Ok(i) => match col.code {
                code::INT64 => Out::I64(i),
                code::INT32 => i32::try_from(i).map_or(Out::Null, Out::I32),
                _ => i16::try_from(i).map_or(Out::Null, Out::I16),
            },
            Err(_) => Out::Null,
        },
        _ => Out::Null,
    }
}

/// A ring closed, if it isn't; `None` when too short to be a ring.
fn closed(r: &[Coord]) -> Option<Vec<Coord>> {
    let mut r = r.to_vec();
    if r.first() != r.last() {
        r.push(*r.first()?);
    }
    (r.len() >= 4).then_some(r)
}

/// Twice a ring's signed area: positive when counter-clockwise.
fn signed_area(r: &[Coord]) -> f64 {
    r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum()
}

/// A shape blob for a class of `geometry_type`; `None` when it can't be stored.
fn shape(g: &Geometry, geometry_type: u8, storage: &GeomDef) -> Option<Vec<u8>> {
    match (g, geometry_type) {
        (Geometry::Point(c), 1) => encode::point(*c, storage),
        (Geometry::MultiPoint(p), 1) => encode::point(*p.first()?, storage),
        (Geometry::Point(c), _) => encode::multipoint(&[*c], storage),
        (Geometry::MultiPoint(p), _) if !p.is_empty() => encode::multipoint(p, storage),
        (Geometry::Line(parts), _) => {
            let parts: Vec<Vec<Coord>> = parts.iter().filter(|p| p.len() >= 2).cloned().collect();
            encode::parts(3, &parts, storage)
        }
        (Geometry::Polygon(polys), _) => {
            // Esri's winding: outer rings clockwise, holes counter-clockwise.
            let mut rings = Vec::new();
            for poly in polys {
                for (i, ring) in poly.iter().enumerate() {
                    let Some(mut r) = closed(ring) else { continue };
                    let ccw = signed_area(&r) > 0.0;
                    if (i == 0) == ccw {
                        r.reverse();
                    }
                    rings.push(r);
                }
            }
            encode::parts(5, &rings, storage)
        }
        _ => None,
    }
}

/// A feature class or table from some of a dataset's features.
fn class(ds: &Dataset, name: String, family: Option<Family>, features: &[&Feature], report: &mut Report) -> system::Dataset {
    let mut taken = vec!["OBJECTID".to_string()];
    let mut fields = vec![FieldDef::new("OBJECTID", code::OBJECT_ID, 4, false)];
    if family.is_some() {
        taken.push("SHAPE".into());
        fields.push(FieldDef::new("SHAPE", code::GEOMETRY, 0, true));
    }
    let cols = columns(ds, features, &mut taken, &mut report.notes, &name);
    fields.extend(cols.iter().map(|c| c.def.clone()));
    let geometry_type = match family {
        None => 0,
        Some(Family::Points) => {
            let multi = features.iter().any(|f| matches!(&f.geometry, Some(Geometry::MultiPoint(p)) if p.len() != 1));
            if multi { 2 } else { 1 }
        }
        Some(Family::Lines) => 3,
        Some(Family::Polygons) => 4,
    };
    let extent = features.iter().filter_map(|f| f.geometry.as_ref()).fold(rustgis_geom::Extent::EMPTY, |e, g| e.union(&g.extent()));
    // The spatial index's grid: what each feature (each point of a multipoint) is keyed by.
    let points = family == Some(Family::Points);
    let boxes: Vec<Option<Vec<rustgis_geom::Extent>>> = features
        .iter()
        .map(|f| {
            f.geometry.as_ref().map(|g| match g {
                Geometry::MultiPoint(p) => p.iter().map(|c| rustgis_geom::Extent::new(c.x, c.y, c.x, c.y)).collect(),
                other => vec![other.extent()],
            })
        })
        .collect();
    let all_boxes: Vec<rustgis_geom::Extent> = boxes.iter().flatten().flatten().copied().filter(|b| !b.is_empty()).collect();
    let grid = spx::grid_size(points, &extent, &all_boxes, all_boxes.len());
    let geom = family.map(|_| {
        let ext = if extent.is_empty() { [f64::NAN; 4] } else { [extent.min_x, extent.min_y, extent.max_x, extent.max_y] };
        system::lonlat_storage(ext, vec![grid])
    });
    let mut rows = Vec::with_capacity(features.len());
    let (mut index, mut cells) = (Vec::new(), Vec::new());
    for f in features {
        let mut row = Vec::with_capacity(cols.len() + 1);
        if let Some(storage) = &geom {
            let blob = f.geometry.as_ref().and_then(|g| {
                let b = shape(g, geometry_type, storage);
                if b.is_none() {
                    report.null_shapes += 1;
                }
                b
            });
            // Indexed under the cells its boxes meet (a shape that couldn't be stored isn't).
            if blob.is_some()
                && let Some(Some(bs)) = boxes.get(rows.len())
            {
                let oid = u32::try_from(rows.len() + 1).unwrap_or(u32::MAX);
                for b in bs {
                    cells.clear();
                    spx::keys(b, grid, &mut cells);
                    index.extend(cells.iter().map(|k| (*k, oid)));
                }
            }
            row.push(blob.map_or(Out::Null, Out::Shape));
        }
        row.extend(cols.iter().map(|c| f.attrs.get(c.source).map_or(Out::Null, |v| out(v, &c.def))));
        rows.push(row);
    }
    let (gdbtable, gdbtablx) = encode::table(geometry_type, &fields, geom.as_ref(), &rows);
    report.classes.push((name.clone(), rows.len()));
    let mut extra = vec![("gdbindexes", spx::gdbindexes(geom.is_some()))];
    if geom.is_some() {
        extra.push(("spx", spx::spx(index)));
    }
    system::Dataset { name, geometry_type, fields, geom, gdbtable, gdbtablx, extra }
}

/// Every file of a new geodatabase holding `datasets` (named by their names), by file name, with
/// what was done.
pub fn encode(datasets: &[&Dataset]) -> Result<(Vec<(String, Vec<u8>)>, Report)> {
    let mut report = Report::default();
    let mut names = Vec::new();
    let mut classes = Vec::new();
    for ds in datasets {
        let families: Vec<Family> = {
            let mut f: Vec<Family> = ds.features.iter().filter_map(|x| x.geometry.as_ref()).map(Family::of).collect();
            f.sort_by_key(|x| *x as u8);
            f.dedup();
            f
        };
        let all: Vec<&Feature> = ds.features.iter().collect();
        match families.as_slice() {
            [] if ds.geometry_type == crate::GeometryType::None => {
                let name = valid_name(&ds.name, MAX_TABLE_NAME, "table", &mut names);
                note_rename(&ds.name, &name, &mut report);
                classes.push(class(ds, name, None, &all, &mut report));
            }
            [] | [_] => {
                let family = families.first().copied().unwrap_or(match ds.geometry_type {
                    crate::GeometryType::Polyline => Family::Lines,
                    crate::GeometryType::Polygon => Family::Polygons,
                    _ => Family::Points,
                });
                let name = valid_name(&ds.name, MAX_TABLE_NAME, "layer", &mut names);
                note_rename(&ds.name, &name, &mut report);
                classes.push(class(ds, name, Some(family), &all, &mut report));
            }
            many => {
                report.notes.push(format!("{}: mixes geometry types, so each is a feature class of its own", ds.name));
                for (k, family) in many.iter().enumerate() {
                    // Features without geometry go with the first kind.
                    let some: Vec<&Feature> =
                        all.iter().copied().filter(|f| f.geometry.as_ref().map_or(k == 0, |g| Family::of(g) == *family)).collect();
                    let name = valid_name(&format!("{}_{}", ds.name, family.suffix()), MAX_TABLE_NAME, "layer", &mut names);
                    classes.push(class(ds, name, Some(*family), &some, &mut report));
                }
            }
        }
    }
    if classes.is_empty() {
        return Err(DataError::Format("File Geodatabase: nothing to write".into()));
    }
    if report.null_shapes > 0 {
        report.notes.push(format!("{} shapes outside longitude/latitude were written as null", report.null_shapes));
    }
    let mut guid = guids();
    Ok((system::files(classes, &mut guid), report))
}

fn note_rename(from: &str, to: &str, report: &mut Report) {
    if from != to {
        report.notes.push(format!("{from} written as {to}"));
    }
}

/// Random version-4 GUIDs, as stored.
fn guids() -> impl FnMut() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    // Randomly keyed for each process (and on the desktop, seeded with the time too).
    let state = std::collections::hash_map::RandomState::new();
    #[cfg(not(target_arch = "wasm32"))]
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    #[cfg(target_arch = "wasm32")]
    let seed = 0u64;
    let mut n = 0u64;
    move || {
        let mut b = [0u8; 16];
        for half in b.as_chunks_mut::<8>().0 {
            let mut h = state.build_hasher();
            h.write_u64(seed);
            h.write_u64(n);
            n += 1;
            half.copy_from_slice(&h.finish().to_le_bytes());
        }
        // Version 4 (in the third group, stored little-endian) and the RFC 4122 variant.
        b[7] = (b[7] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        b
    }
}

/// Writes a new geodatabase folder at `path` (which must not exist): built beside it and moved into
/// place when complete, so a failed write leaves nothing behind.
#[cfg(not(target_arch = "wasm32"))]
pub fn write(datasets: &[&Dataset], path: &std::path::Path) -> Result<Report> {
    let path = std::path::Path::new(path.to_str().map(|s| s.trim_end_matches(['/', '\\'])).unwrap_or_default());
    if path.exists() {
        return Err(DataError::Format(format!(
            "{} already exists; choose a new name (an existing geodatabase isn't changed)",
            path.display()
        )));
    }
    let (files, report) = encode(datasets)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("export.gdb");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir(&tmp)?;
    let res = (|| -> std::io::Result<()> {
        for (n, b) in &files {
            std::fs::write(tmp.join(n), b)?;
        }
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e.into());
    }
    Ok(report)
}

/// A new geodatabase as a zip of its folder (`<name>.gdb/…`), for downloading.
pub fn zip(datasets: &[&Dataset], name: &str) -> Result<(Vec<u8>, Report)> {
    let (files, report) = encode(datasets)?;
    let folder = if name.to_ascii_lowercase().ends_with(".gdb") { name.to_string() } else { format!("{name}.gdb") };
    let entries: Vec<(String, Vec<u8>)> = files.into_iter().map(|(n, b)| (format!("{folder}/{n}"), b)).collect();
    Ok((crate::zip::write(&entries).map_err(DataError::Format)?, report))
}
