//! Esri File Geodatabases read natively: a `.gdb` folder's feature classes and tables, without
//! GDAL.
//!
//! The geodatabase's catalog (`a00000001.gdbtable`) names its tables; each is a `.gdbtable` of
//! rows and a `.gdbtablx` of where each row is. Opening a feature class walks its rows once,
//! keeping each one's place and bounding box (from the start of its shape); a read picks the rows
//! whose boxes meet the request and reads just those, like a large Shapefile.
//!
//! A geodatabase is a folder on the desktop; in the browser (and anywhere else) it can come as a
//! zip of that folder, read from memory. Its files are hostile input: counts and lengths are
//! checked against the bytes there, and a row that doesn't parse is skipped and counted.
//!
//! Read: points, multipoints, lines and polygons (true curves turned into short straight
//! segments), standalone tables, and every field type but rasters (binary fields are left out).
//! Z and M values, domains, subtypes, relationship classes and attachments aren't.
//!
//! [`write`] writes new geodatabases.

mod encode;
mod shape;
mod system;
mod table;
#[cfg(test)]
mod tests;
pub mod write;

use std::path::Path;
use std::sync::Arc;

use super::{Batch, FeatureSource, Reproject, Request, SourceInfo, clamp_limit, stride};
use crate::{DataError, Feature, Field, GeometryType, Result, Value};
use rustgis_geom::Extent;
use table::{Cell, Layout};

fn bad(msg: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("File Geodatabase: {msg}"))
}

/// Whether a name is a File Geodatabase: a folder ending in `.gdb` (with or without a trailing
/// separator).
pub fn is_gdb_name(name: &str) -> bool {
    let n = name.trim_end_matches(['/', '\\']);
    Path::new(n).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gdb"))
}

/// A file of the geodatabase read at offsets.
trait ReadAt {
    fn len(&self) -> u64;
    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

/// Where a geodatabase's files are.
trait Store: Send + Sync {
    /// Opens one of its files, by name (`a00000001.gdbtable`).
    fn open(&self, file: &str) -> Result<Box<dyn ReadAt + '_>>;
    fn exists(&self, file: &str) -> bool;
}

/// A geodatabase folder on disk.
#[cfg(not(target_arch = "wasm32"))]
struct Dir(std::path::PathBuf);

/// Bytes read ahead from a file: rows near each other in a read share a fill.
#[cfg(not(target_arch = "wasm32"))]
const BUFFER: usize = 1 << 20;

#[cfg(not(target_arch = "wasm32"))]
impl Store for Dir {
    fn open(&self, file: &str) -> Result<Box<dyn ReadAt + '_>> {
        let f = std::fs::File::open(self.0.join(file))?;
        let len = f.metadata()?.len();
        Ok(Box::new(FileAt { f: std::io::BufReader::with_capacity(BUFFER, f), pos: 0, len }))
    }

    fn exists(&self, file: &str) -> bool {
        self.0.join(file).is_file()
    }
}

/// A file read at offsets, through a buffer that a nearby offset reuses.
#[cfg(not(target_arch = "wasm32"))]
struct FileAt {
    f: std::io::BufReader<std::fs::File>,
    pos: u64,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl ReadAt for FileAt {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        if at != self.pos {
            // Near, either way: within the buffer (reading a row again from its start keeps
            // it), or worth reading through.
            let d = i128::from(at) - i128::from(self.pos);
            match i64::try_from(d) {
                Ok(d) if self.pos != u64::MAX && d.unsigned_abs() < BUFFER as u64 => self.f.seek_relative(d)?,
                _ => {
                    self.f.seek(SeekFrom::Start(at))?;
                }
            }
        }
        let r = self.f.read_exact(buf);
        // After a failure, where the file is is unknown: seek next time.
        self.pos = if r.is_ok() { at + buf.len() as u64 } else { u64::MAX };
        r
    }
}

/// A geodatabase's files held in memory (from a zip), by lower-case name.
struct Mem(std::collections::HashMap<String, Vec<u8>>);

impl Store for Mem {
    fn open(&self, file: &str) -> Result<Box<dyn ReadAt + '_>> {
        let b = self.0.get(&file.to_ascii_lowercase()).ok_or_else(|| bad(format!("{file} is missing")))?;
        Ok(Box::new(SliceAt(b)))
    }

    fn exists(&self, file: &str) -> bool {
        self.0.contains_key(&file.to_ascii_lowercase())
    }
}

struct SliceAt<'a>(&'a [u8]);

impl ReadAt for SliceAt<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let src = usize::try_from(at)
            .ok()
            .and_then(|a| self.0.get(a..a.checked_add(buf.len())?))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// The geodatabase in a zip: the files of the first `.gdb` folder in it (at any depth), or
/// `None` when it holds none.
fn zip_store(bytes: &[u8]) -> Result<Option<Mem>> {
    let budget = usize::try_from(crate::io::MAX_BYTES).unwrap_or(usize::MAX);
    let entries = crate::zip::read_matching_within(bytes, |n| gdb_folder(n).is_some(), budget).map_err(bad)?;
    let Some(folder) = entries.iter().find_map(|(n, _)| gdb_folder(n)) else { return Ok(None) };
    let files = entries
        .into_iter()
        .filter(|(n, _)| gdb_folder(n).as_deref() == Some(folder.as_str()))
        .filter_map(|(n, b)| Some((n.rsplit(['/', '\\']).next()?.to_ascii_lowercase(), b)))
        .collect();
    Ok(Some(Mem(files)))
}

fn zip_store_or_err(bytes: &[u8]) -> Result<Mem> {
    zip_store(bytes)?.ok_or_else(|| bad("the zip holds no .gdb folder"))
}

/// Reads every feature class and table (up to `cap`) of the geodatabase in a zip; `None` when the
/// zip holds none. One that can't be read is said in the first layer's report.
pub fn read_zip(bytes: &[u8], cap: usize) -> Result<Option<Vec<(crate::Dataset, crate::ReadReport)>>> {
    let Some(store) = zip_store(bytes)? else { return Ok(None) };
    let store: Arc<dyn Store> = Arc::new(store);
    let entries = catalog(store.as_ref())?;
    let mut out = Vec::new();
    let mut left_out = Vec::new();
    for e in &entries {
        if out.len() >= cap {
            left_out.push(format!("{} (past the first {cap} layers)", e.name));
            continue;
        }
        match FileGdbSource::open(store.clone(), Some(&e.name)).and_then(|src| super::materialize(&src)) {
            Ok(read) => out.push(read),
            Err(err) => left_out.push(err.to_string()),
        }
    }
    if let (Some((_, report)), false) = (out.first_mut(), left_out.is_empty()) {
        report.messages.push(format!("left out: {}", left_out.join("; ")));
    }
    if out.is_empty() {
        return Err(bad(if left_out.is_empty() { "it holds no feature classes or tables".to_string() } else { left_out.join("; ") }));
    }
    Ok(Some(out))
}

/// The `.gdb` folder a zip entry is directly in, if any.
fn gdb_folder(entry: &str) -> Option<String> {
    let (folder, _) = entry.rsplit_once(['/', '\\'])?;
    is_gdb_name(folder).then(|| folder.to_string())
}

/// A table the catalog names, and the file it's in.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    stem: String,
}

/// The feature classes and tables of a geodatabase, in catalog order (system tables and
/// attachments left out).
fn catalog(store: &dyn Store) -> Result<Vec<Entry>> {
    let stem = "a00000001";
    let mut t =
        store.open(&format!("{stem}.gdbtable")).map_err(|_| bad("not a File Geodatabase: its catalog (a00000001.gdbtable) is missing"))?;
    let layout = table::read_layout(t.as_mut())?;
    let index = table::read_index(store.open(&format!("{stem}.gdbtablx"))?.as_mut())?;
    let col = |name: &str| layout.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name));
    let (Some(name_col), Some(format_col)) = (col("Name"), col("FileFormat")) else {
        return Err(bad("the catalog's fields aren't as expected"));
    };
    let mut out = Vec::new();
    for (oid, offset) in index {
        let Some(row) = read_row(t.as_mut(), offset, None) else { continue };
        let Some(cells) = table::parse_row(&layout, &row, false) else { continue };
        let text = |i: usize| match cells.get(i) {
            Some(Cell::Value(Value::Text(s))) => Some(s.clone()),
            _ => None,
        };
        let int = |i: usize| match cells.get(i) {
            Some(Cell::Value(Value::Int(v))) => Some(*v),
            _ => None,
        };
        let Some(name) = text(name_col) else { continue };
        // System tables, and catalog entries without a table of rows (the replica log).
        if name.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("GDB_")) || int(format_col) != Some(0) {
            continue;
        }
        let stem = format!("a{oid:08x}");
        if !store.exists(&format!("{stem}.gdbtable")) {
            continue;
        }
        // Attachments: binary files tied to another table's rows.
        if name.to_ascii_uppercase().ends_with("__ATTACH") {
            continue;
        }
        out.push(Entry { name, stem });
    }
    Ok(out)
}

/// Most bytes one row may take.
const MAX_ROW: u64 = 1 << 30;

/// The row at `offset` (after its 4-byte size); `None` for a deleted or damaged one. With
/// `prefix`, at most that many bytes of it.
fn read_row(f: &mut dyn ReadAt, offset: u64, prefix: Option<usize>) -> Option<Vec<u8>> {
    let mut size = [0u8; 4];
    f.read_at(offset, &mut size).ok()?;
    // A negative size marks a deleted row.
    let size = u64::try_from(i32::from_le_bytes(size)).ok()?;
    if size > MAX_ROW || offset.saturating_add(4).saturating_add(size) > f.len() {
        return None;
    }
    let want = prefix.map_or(size, |p| size.min(p as u64));
    let mut buf = vec![0u8; usize::try_from(want).ok()?];
    f.read_at(offset + 4, &mut buf).ok()?;
    Some(buf)
}

/// Where a row is.
#[derive(Clone, Copy)]
struct Place {
    oid: u64,
    offset: u64,
}

/// A feature class or table read in place.
pub struct FileGdbSource {
    info: SourceInfo,
    store: Arc<dyn Store>,
    stem: String,
    layout: Layout,
    places: Vec<Place>,
    /// Each row's box in the table's coordinates, rounded outward (NaN for a null shape); empty
    /// for a table without geometry.
    boxes: Vec<[f32; 4]>,
    /// For each output field, its column.
    columns: Vec<usize>,
    reproject: Reproject,
}

/// How much of a row is read for its shape's box: rows up to this size are read whole; of a larger
/// one, this much first, which is enough when the shape comes before any long text (it's usually
/// the first or second field).
const BOX_PREFIX: usize = 64 << 10;

impl FileGdbSource {
    /// Opens a folder ending in `.gdb`; `layer` names the feature class or table (default: the
    /// first).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(path: &Path, layer: Option<&str>) -> Result<FileGdbSource> {
        let dir = Path::new(path.to_str().map(|s| s.trim_end_matches(['/', '\\'])).unwrap_or_default());
        if !dir.is_dir() {
            return Err(bad(format!("{} is not a folder", path.display())));
        }
        FileGdbSource::open(Arc::new(Dir(dir.to_path_buf())), layer)
    }

    /// Opens the geodatabase in a zip held in memory.
    pub fn open_zip(bytes: &[u8], layer: Option<&str>) -> Result<FileGdbSource> {
        FileGdbSource::open(Arc::new(zip_store_or_err(bytes)?), layer)
    }

    fn open(store: Arc<dyn Store>, layer: Option<&str>) -> Result<FileGdbSource> {
        let mut notes = Vec::new();
        let entries = catalog(store.as_ref())?;
        let entry = match layer {
            Some(l) => entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(l))
                .ok_or_else(|| bad(format!("no feature class or table named {l}")))?,
            None => entries.first().ok_or_else(|| bad("it holds no feature classes or tables"))?,
        };
        let mut t = store.open(&format!("{}.gdbtable", entry.stem))?;
        let layout = table::read_layout(t.as_mut()).map_err(|e| bad(format!("{}: {e}", entry.name)))?;
        let index = table::read_index(store.open(&format!("{}.gdbtablx", entry.stem))?.as_mut())?;
        let geometry_type = match layout.geometry_type {
            0 => GeometryType::None,
            1 | 2 => GeometryType::Point,
            3 => GeometryType::Polyline,
            4 => GeometryType::Polygon,
            9 => return Err(bad(format!("{}: multipatch (3D) feature classes aren't read", entry.name))),
            other => return Err(bad(format!("{}: geometry type {other} isn't supported", entry.name))),
        };
        let columns: Vec<usize> = layout.columns.iter().enumerate().filter(|(_, c)| c.kind.out().is_some()).map(|(i, _)| i).collect();
        let fields =
            columns.iter().filter_map(|i| layout.columns.get(*i)).filter_map(|c| Some(Field::new(c.name.clone(), c.kind.out()?))).collect();
        let mut places: Vec<Place> = index.into_iter().map(|(oid, offset)| Place { oid, offset }).collect();
        // Walk the rows in file order: each one's box from the start of its shape.
        places.sort_by_key(|p| p.offset);
        let mut boxes = Vec::new();
        let (mut skipped, mut curves) = (0usize, false);
        if let Some(g) = &layout.geom {
            let mut kept = Vec::with_capacity(places.len());
            boxes.reserve(places.len());
            for p in &places {
                let mut parsed = |row: &[u8]| -> Option<Option<[f64; 4]>> {
                    let cells = table::parse_row(&layout, row, true)?;
                    Some(match cells.last() {
                        Some(Cell::Geometry(blob)) => {
                            curves |= shape::has_curves(blob);
                            shape::bbox(blob, g)
                        }
                        _ => None,
                    })
                };
                let b = match read_row(t.as_mut(), p.offset, Some(BOX_PREFIX)) {
                    // Cut short by the prefix: read the whole row.
                    Some(row) => match parsed(&row) {
                        Some(b) => Some(b),
                        None if row.len() == BOX_PREFIX => read_row(t.as_mut(), p.offset, None).and_then(|r| parsed(&r)),
                        None => None,
                    },
                    None => None,
                };
                match b {
                    Some(b) => {
                        kept.push(*p);
                        boxes.push(b.map_or([f32::NAN; 4], outward));
                    }
                    None => skipped += 1,
                }
            }
            places = kept;
        }
        if skipped > 0 {
            notes.push(format!("{skipped} rows couldn't be read and are left out"));
        }
        if curves {
            notes.push("curves are drawn as short straight segments (elliptic arcs as straight lines)".into());
        }
        let reproject = gdb_reproject(layout.geom.as_ref().map(|g| g.wkt.as_str()), &mut notes);
        let native = match &layout.geom {
            Some(g) if g.extent.iter().all(|v| v.is_finite()) => Extent::new(g.extent[0], g.extent[1], g.extent[2], g.extent[3]),
            _ => boxes
                .iter()
                .filter(|b| b[0].is_finite())
                .fold(Extent::EMPTY, |e, b| e.union(&Extent::new(f64::from(b[0]), f64::from(b[1]), f64::from(b[2]), f64::from(b[3])))),
        };
        let extent = if native.is_empty() { Extent::EMPTY } else { reproject.extent_to_lonlat(&native) };
        #[cfg(not(target_arch = "wasm32"))]
        if reproject.is_lonlat()
            && !extent.is_empty()
            && (extent.min_x < -180.5 || extent.max_x > 180.5 || extent.min_y < -90.5 || extent.max_y > 90.5)
        {
            notes.push("coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place".into());
        }
        let info = SourceInfo {
            name: entry.name.clone(),
            format: "File Geodatabase",
            geometry_type,
            fields,
            extent,
            count: Some(places.len() as u64),
            crs: reproject.label(),
            indexed: false,
            notes,
        };
        let stem = entry.stem.clone();
        drop(t);
        Ok(FileGdbSource { info, store, stem, layout, places, boxes, columns, reproject })
    }

    /// The rows a request matches, in file order: every one without an extent (or for a table);
    /// otherwise those whose boxes meet it (null shapes never do).
    fn matches(&self, req: &Request) -> impl Iterator<Item = usize> + '_ {
        let e = req
            .extent
            .filter(|_| !self.boxes.is_empty())
            .map(|e| self.reproject.extent_from_lonlat(&e))
            .map(|e| (e.is_empty(), [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]));
        (0..self.places.len()).filter(move |i| match e {
            None => true,
            Some((true, _)) => false,
            Some((false, [x0, y0, x1, y1])) => self.boxes.get(*i).is_some_and(|b| b[0] <= x1 && b[2] >= x0 && b[1] <= y1 && b[3] >= y0),
        })
    }
}

impl FeatureSource for FileGdbSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let matched = self.matches(req).count();
        let step = stride(matched as u64, limit).max(1) as usize;
        let mut t = self.store.open(&format!("{}.gdbtable", self.stem))?;
        let mut features = Vec::with_capacity(matched.min(limit));
        let mut skipped = 0;
        for i in self.matches(req).step_by(step).take(limit) {
            let Some(p) = self.places.get(i) else { continue };
            let Some(row) = read_row(t.as_mut(), p.offset, None) else {
                skipped += 1;
                continue;
            };
            let Some(cells) = table::parse_row(&self.layout, &row, false) else {
                skipped += 1;
                continue;
            };
            let mut geometry = None;
            if let (Some(g), Some(Cell::Geometry(blob))) = (&self.layout.geom, cells.iter().find(|c| matches!(c, Cell::Geometry(_)))) {
                match shape::decode(blob, g) {
                    Ok(Some(shape)) => match self.reproject.geometry(shape) {
                        Some(s) => geometry = Some(s),
                        None => {
                            skipped += 1;
                            continue;
                        }
                    },
                    Ok(None) => {}
                    Err(()) => {
                        skipped += 1;
                        continue;
                    }
                }
            }
            let attrs = self
                .columns
                .iter()
                .map(|c| match cells.get(*c) {
                    Some(Cell::Value(v)) => v.clone(),
                    _ => Value::Null,
                })
                .collect();
            features.push(Feature { id: p.oid, geometry, attrs });
        }
        Ok(Batch { features, matched: matched as u64, complete: step == 1, skipped })
    }
}

/// The feature classes and tables of a geodatabase folder, by name.
#[cfg(not(target_arch = "wasm32"))]
pub fn layers(path: &Path) -> Result<Vec<String>> {
    let dir = Path::new(path.to_str().map(|s| s.trim_end_matches(['/', '\\'])).unwrap_or_default());
    Ok(catalog(&Dir(dir.to_path_buf()))?.into_iter().map(|e| e.name).collect())
}

/// The feature classes and tables of the geodatabase in a zip, by name.
pub fn zip_layers(bytes: &[u8]) -> Result<Vec<String>> {
    Ok(catalog(&zip_store_or_err(bytes)?)?.into_iter().map(|e| e.name).collect())
}

/// A box rounded outward to `f32`.
fn outward(b: [f64; 4]) -> [f32; 4] {
    if b.iter().any(|v| !v.is_finite()) {
        return [f32::NAN; 4];
    }
    let down = |v: f64| {
        let f = v as f32;
        if f64::from(f) > v { f.next_down() } else { f }
    };
    let up = |v: f64| {
        let f = v as f32;
        if f64::from(f) < v { f.next_up() } else { f }
    };
    [down(b[0]), down(b[1]), up(b[2]), up(b[3])]
}

/// The coordinate system in the geometry field's WKT, with what couldn't be understood said in the
/// notes. A geodatabase writes a placeholder for an unknown system.
fn gdb_reproject(wkt: Option<&str>, notes: &mut Vec<String>) -> Reproject {
    let crs = match wkt.map(str::trim).filter(|w| !w.is_empty() && !w.starts_with('{')) {
        None => crate::raster::Crs::LonLat,
        Some(w) => match crate::raster::wkt::read_wkt(w) {
            Ok((c, note)) => {
                if let Some(n) = note {
                    notes.push(n);
                }
                c
            }
            Err(e) => {
                notes.push(format!("{e}; coordinates are read as longitude/latitude"));
                crate::raster::Crs::LonLat
            }
        },
    };
    Reproject::from_crs(crs, notes)
}
