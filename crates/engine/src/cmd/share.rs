//! Publish tab: export the map as PNG and layers as GeoJSON, CSV, FlatGeobuf, GeoPackage,
//! GeoParquet, Shapefile, KML/KMZ, GPX or an Esri File Geodatabase.

use std::path::Path;

use rustgis_render::View;
use serde_json::{Value, json};

use super::{f, need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("share.exportMap", "Export Map", "Publish › Output", export_map)
            .view()
            .params(r#"{"path": "*.png", "width"?: px, "height"?: px, "dpi"?: 96..600}"#),
        C::new("share.exportLayer", "Export Features", "Publish › Output", export_layer)
            .view()
            .when(need_layer)
            .params(r#"{"layer"?, "path": "*.geojson|*.csv|*.fgb|*.gpkg|*.gdb|*.parquet|*.shp|*.zip|*.kml|*.kmz|*.gpx"}"#),
        C::new("share.exportLayers", "Export Layers to Geodatabase", "Publish › Output", export_layers)
            .view()
            .params(r#"{"layers"?: [name|id] (default: every vector layer), "path": "*.gdb" (a new folder)}"#),
        C::new("share.exportTable", "Export Table", "Publish › Output", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let t = s.project.tables.get(s.table_index(p)?).ok_or("no such table")?;
            rustgis_data::io::write_path(&t.dataset, Path::new(path)).map_err(|e| e.to_string())?;
            Ok(json!({"path": path}))
        })
        .params(
            r#"{"table": name|id (a standalone table), "path": "*.geojson|*.csv|*.fgb|*.gpkg|*.parquet|*.shp|*.zip|*.kml|*.kmz|*.gpx"}"#,
        )
        .view(),
    ]
}

/// Renders the active map at the current view (or a requested size) to PNG bytes.
pub fn map_png(s: &mut Session, width: Option<f64>, height: Option<f64>, dpi: Option<f64>) -> Result<Vec<u8>, String> {
    let (vw, vh) = s.view_size;
    // NaN would pass through `clamp`; non-finite values fall back to the defaults.
    let fin = |v: Option<f64>| v.filter(|x| x.is_finite());
    let (width, height, dpi) = (fin(width), fin(height), fin(dpi));
    let w = width.unwrap_or(vw).clamp(16.0, 8000.0);
    let h = height.unwrap_or(vh).clamp(16.0, 8000.0);
    let ppp = (dpi.unwrap_or(96.0) / 96.0).clamp(0.5, 6.0);
    let mut camera = s.map().camera;
    // Keep the same ground extent when the size differs from the view.
    if width.is_some() || height.is_some() {
        camera.res *= (vw / w).max(vh / h);
        camera.clamp();
    }
    let view = View { width: w, height: h, pixels_per_point: ppp, camera };
    rustgis_render::render_png(s.map_mut(), &view)
}

/// The map view on one PDF page (for printing): the page is the view's shape at 96 points per
/// inch of screen, rendered at `dpi`.
pub fn map_pdf(s: &mut Session, dpi: f64) -> Result<Vec<u8>, String> {
    let (vw, vh) = s.view_size;
    let ppp = (dpi / 96.0).clamp(0.5, 6.0);
    let view = View { width: vw.clamp(16.0, 8000.0), height: vh.clamp(16.0, 8000.0), pixels_per_point: ppp, camera: s.map().camera };
    let png = rustgis_render::render_png(s.map_mut(), &view)?;
    let pm = rustgis_render::decode_png(&png).ok_or("could not draw the map")?;
    let mm = |pt: f64| pt * 25.4 / 96.0;
    Ok(rustgis_render::pdf::write(&[rustgis_render::pdf::Page::new(&pm, mm(view.width), mm(view.height))], &s.map().name))
}

fn export_map(s: &mut Session, p: &Value) -> CmdResult {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let png = map_png(s, f(p, "width"), f(p, "height"), f(p, "dpi"))?;
    rustgis_data::io::write_atomic(std::path::Path::new(&path), &png).map_err(|e| format!("{path}: {e}"))?;
    s.notify(format!("Exported {path}"), false);
    Ok(json!({"path": path, "bytes": png.len()}))
}

fn export_layer(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    // A geodatabase says what it renamed and split.
    if rustgis_data::source::filegdb::is_gdb_name(&path) {
        return export_gdb(s, &[id], &path);
    }
    let l = s.map().layer(id).ok_or("no layer")?;
    // A streamed layer exports its whole source, not the features read for the view.
    let whole = if l.is_streamed() { Some(l.load_all()?) } else { None };
    let ds = whole.as_ref().unwrap_or(&l.dataset);
    let left_out = rustgis_data::io::write_path(ds, Path::new(&path)).map_err(|e| e.to_string())?;
    let written = ds.features.len().saturating_sub(left_out);
    // FlatGeobuf, KML, GPX and Shapefile hold features with geometry only (a Shapefile, only
    // geometry it can draw).
    if left_out > 0 {
        s.notify(format!("{path}: {left_out} features without drawable geometry were left out"), false);
    }
    s.notify(format!("Exported {path}"), false);
    Ok(json!({"path": path, "features": written, "leftOut": left_out}))
}

/// Writes vector layers into a new File Geodatabase: the ones `"layers"` names, else every one.
fn export_layers(s: &mut Session, p: &Value) -> CmdResult {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    if !rustgis_data::source::filegdb::is_gdb_name(&path) {
        return Err(format!("{path}: give a path ending in .gdb"));
    }
    let ids: Vec<u32> = match p.get("layers") {
        Some(Value::Array(names)) => names.iter().map(|n| s.layer_id(&json!({"layer": n}))).collect::<Result<_, _>>()?,
        Some(_) => return Err("layers is a list of layer names or ids".into()),
        None => s.map().layers.iter().filter(|l| !l.is_raster()).map(|l| l.id).collect(),
    };
    export_gdb(s, &ids, &path)
}

fn export_gdb(s: &mut Session, ids: &[u32], path: &str) -> CmdResult {
    if ids.is_empty() {
        return Err("no vector layers to export".into());
    }
    let mut owned = Vec::with_capacity(ids.len());
    for id in ids {
        let l = s.map().layer(*id).ok_or("no layer")?;
        if l.is_raster() {
            return Err(format!("{} is a raster; a geodatabase holds vector layers here", l.name));
        }
        // A streamed layer exports its whole source, not the features read for the view.
        let mut ds = if l.is_streamed() { l.load_all()? } else { l.dataset.clone() };
        // Classes are named after the layers as the Layers pane shows them.
        ds.name = l.name.clone();
        owned.push(ds);
    }
    let refs: Vec<&rustgis_data::Dataset> = owned.iter().collect();
    #[cfg(not(target_arch = "wasm32"))]
    let report = rustgis_data::source::filegdb::write::write(&refs, Path::new(path)).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    let report: rustgis_data::source::filegdb::write::Report = {
        let _ = refs;
        return Err("in the web app, Export Features downloads a geodatabase as a zip".into());
    };
    for n in &report.notes {
        s.notify(format!("{path}: {n}"), false);
    }
    s.notify(format!("Exported {path}"), false);
    let classes: Vec<Value> = report.classes.iter().map(|(name, rows)| json!({"name": name, "features": rows})).collect();
    Ok(json!({"path": path, "classes": classes, "nullShapes": report.null_shapes, "notes": report.notes}))
}
