//! The system tables a new geodatabase needs (its catalog, configuration keywords, coordinate
//! systems, items and their types and relationships), and the XML definitions ArcGIS reads its
//! feature classes and tables from.
//!
//! Their layout and constant rows are as ArcGIS (and GDAL's OpenFileGDB driver) write them.

use super::encode::{FieldDef, GeomDef, Out, code, table};

/// GUIDs as stored (the first three groups little-endian), in hex.
mod ids {
    pub const FOLDER: &str = "6f3e78f3ca6514458315ce3985dad3b1";
    pub const WORKSPACE: &str = "0ffe73c680724f40853220755dd8fc06";
    pub const TABLE: &str = "3bbc06cd9d78514caafaa467912b8965";
    pub const FEATURE_CLASS: &str = "097873702c85034a9e222cecea5b9bfa";
    pub const DATASET_IN_FOLDER: &str = "abf178dce434ac43ba471c4eabd0e7c7";
}

fn hex16(h: &str) -> [u8; 16] {
    let mut b = [0u8; 16];
    for (i, slot) in b.iter_mut().enumerate() {
        *slot = h.get(i * 2..i * 2 + 2).and_then(|p| u8::from_str_radix(p, 16).ok()).unwrap_or(0);
    }
    b
}

/// Configuration keywords (`GDB_DBTune`): keyword, parameter, value.
const DBTUNE: [(&str, &str, &str); 35] = [
    ("DEFAULTS", "UI_TEXT", "The default datafile configuration."),
    ("DEFAULTS", "CHARACTER_FORMAT", "UTF8"),
    ("DEFAULTS", "GEOMETRY_FORMAT", "Compressed"),
    ("DEFAULTS", "GEOMETRY_STORAGE", "InLine"),
    ("DEFAULTS", "BLOB_STORAGE", "InLine"),
    ("DEFAULTS", "MAX_FILE_SIZE", "1TB"),
    ("DEFAULTS", "RASTER_STORAGE", "InLine"),
    ("TEXT_UTF16", "UI_TEXT", "The UTF16 text format configuration."),
    ("TEXT_UTF16", "CHARACTER_FORMAT", "UTF16"),
    ("MAX_FILE_SIZE_4GB", "UI_TEXT", "The 4GB maximum datafile size configuration."),
    ("MAX_FILE_SIZE_4GB", "MAX_FILE_SIZE", "4GB"),
    ("MAX_FILE_SIZE_256TB", "UI_TEXT", "The 256TB maximum datafile size configuration."),
    ("MAX_FILE_SIZE_256TB", "MAX_FILE_SIZE", "256TB"),
    ("GEOMETRY_UNCOMPRESSED", "UI_TEXT", "The Uncompressed Geometry configuration."),
    ("GEOMETRY_UNCOMPRESSED", "GEOMETRY_FORMAT", "Uncompressed"),
    ("GEOMETRY_OUTOFLINE", "UI_TEXT", "The Outofline Geometry configuration."),
    ("GEOMETRY_OUTOFLINE", "GEOMETRY_STORAGE", "OutOfLine"),
    ("BLOB_OUTOFLINE", "UI_TEXT", "The Outofline Blob configuration."),
    ("BLOB_OUTOFLINE", "BLOB_STORAGE", "OutOfLine"),
    ("GEOMETRY_AND_BLOB_OUTOFLINE", "UI_TEXT", "The Outofline Geometry and Blob configuration."),
    ("GEOMETRY_AND_BLOB_OUTOFLINE", "GEOMETRY_STORAGE", "OutOfLine"),
    ("GEOMETRY_AND_BLOB_OUTOFLINE", "BLOB_STORAGE", "OutOfLine"),
    ("TERRAIN_DEFAULTS", "UI_TERRAIN_TEXT", "The terrains default configuration."),
    ("TERRAIN_DEFAULTS", "GEOMETRY_STORAGE", "OutOfLine"),
    ("TERRAIN_DEFAULTS", "BLOB_STORAGE", "OutOfLine"),
    ("MOSAICDATASET_DEFAULTS", "UI_MOSAIC_TEXT", "The Outofline Raster and Blob configuration."),
    ("MOSAICDATASET_DEFAULTS", "RASTER_STORAGE", "OutOfLine"),
    ("MOSAICDATASET_DEFAULTS", "BLOB_STORAGE", "OutOfLine"),
    ("MOSAICDATASET_INLINE", "UI_MOSAIC_TEXT", "The mosaic dataset inline configuration."),
    ("MOSAICDATASET_INLINE", "CHARACTER_FORMAT", "UTF8"),
    ("MOSAICDATASET_INLINE", "GEOMETRY_FORMAT", "Compressed"),
    ("MOSAICDATASET_INLINE", "GEOMETRY_STORAGE", "InLine"),
    ("MOSAICDATASET_INLINE", "BLOB_STORAGE", "InLine"),
    ("MOSAICDATASET_INLINE", "MAX_FILE_SIZE", "1TB"),
    ("MOSAICDATASET_INLINE", "RASTER_STORAGE", "InLine"),
];

/// Item types (`GDB_ItemTypes`): UUID, parent type, name.
const ITEM_TYPES: [(&str, &str, &str); 32] = [
    ("d5ad0584f88d27428fac3fcade073386", "00000000000000000000000000000000", "Item"),
    ("6f3e78f3ca6514458315ce3985dad3b1", "d5ad0584f88d27428fac3fcade073386", "Folder"),
    ("289cd0ff70fe254e907caf8e8a5ec5f3", "d5ad0584f88d27428fac3fcade073386", "Resource"),
    ("899eda2880ff6d4d89264ee2b161677d", "289cd0ff70fe254e907caf8e8a5ec5f3", "Dataset"),
    ("d67dddfb254ab7409a1aecc3d1172447", "899eda2880ff6d4d89264ee2b161677d", "Tin"),
    ("622191d413346e479da42aefbbc16939", "899eda2880ff6d4d89264ee2b161677d", "AbstractTable"),
    ("e1a706b65bfa9c43849c6e9c2481537b", "899eda2880ff6d4d89264ee2b161677d", "Relationship Class"),
    ("49717374b5dc57428904b9724e32a530", "899eda2880ff6d4d89264ee2b161677d", "Feature Dataset"),
    ("668a7173b9af884ba551cffa0ae12620", "899eda2880ff6d4d89264ee2b161677d", "Geometric Network"),
    ("d352717666ed25438774420d46674e07", "899eda2880ff6d4d89264ee2b161677d", "Topology"),
    ("652630e66b41fa44be334e15916ba101", "899eda2880ff6d4d89264ee2b161677d", "Survey Dataset"),
    ("8802a4d59e0266478c81de3f61129371", "899eda2880ff6d4d89264ee2b161677d", "Schematic Dataset"),
    ("7a691bdbb63b6a4298a26ee7a4c6aed3", "899eda2880ff6d4d89264ee2b161677d", "Toolbox"),
    ("0ffe73c680724f40853220755dd8fc06", "899eda2880ff6d4d89264ee2b161677d", "Workspace"),
    ("77f69edca31aa7458acd303a5202d0dc", "899eda2880ff6d4d89264ee2b161677d", "Workspace Extension"),
    ("032629770f935d47ae4fb8970f42f394", "899eda2880ff6d4d89264ee2b161677d", "Extension Dataset"),
    ("edf13786048c6648a44a1cb8288b3c63", "899eda2880ff6d4d89264ee2b161677d", "Domain"),
    ("8ea5d44e1f62434095ed850fba45fcbc", "899eda2880ff6d4d89264ee2b161677d", "Replica"),
    ("eb2184d982d51347948443304d0810f6", "899eda2880ff6d4d89264ee2b161677d", "Replica Dataset"),
    ("e4b664dc0fdcbd43b4f5f22385dcf055", "899eda2880ff6d4d89264ee2b161677d", "Historical Marker"),
    ("3bbc06cd9d78514caafaa467912b8965", "622191d413346e479da42aefbbc16939", "Table"),
    ("097873702c85034a9e222cecea5b9bfa", "622191d413346e479da42aefbbc16939", "Feature Class"),
    ("a367d65ea99ca2448029d95bf23704b9", "622191d413346e479da42aefbbc16939", "Raster Dataset"),
    ("f701b635ce45ff4aadb77702d3839b12", "622191d413346e479da42aefbbc16939", "Raster Catalog"),
    ("7dfc71778ba3d34f8225639d17e9a131", "032629770f935d47ae4fb8970f42f394", "Network Dataset"),
    ("377535766433af48a4be783c7c28b5cb", "032629770f935d47ae4fb8970f42f394", "Terrain"),
    ("693380a3c25f6349bae013effc09dd73", "032629770f935d47ae4fb8970f42f394", "Parcel Fabric"),
    ("8d0000a3ea0c6a4f9dfa46af829a3df2", "032629770f935d47ae4fb8970f42f394", "Representation Class"),
    ("35ea7b78864a4f49bb48500b96145b58", "032629770f935d47ae4fb8970f42f394", "Catalog Dataset"),
    ("cb3d41f848223549bfe9315f397e5110", "032629770f935d47ae4fb8970f42f394", "Mosaic Dataset"),
    ("88a99dc23e8cf7458b5c18e51ee7beb4", "edf13786048c6648a44a1cb8288b3c63", "Range Domain"),
    ("128b368c2ea17e4c9638c9c64e69e98f", "edf13786048c6648a44a1cb8288b3c63", "Coded Value Domain"),
];

/// Relationship types (`GDB_ItemRelationshipTypes`): UUID, origin and destination item types,
/// name, labels, whether it's containment.
const RELATIONSHIP_TYPES: [(&str, &str, &str, &str, &str, &str, i16); 16] = [
    (
        "a7b3100d642fe645b7ac2fc27bf2133c",
        "6f3e78f3ca6514458315ce3985dad3b1",
        "6f3e78f3ca6514458315ce3985dad3b1",
        "FolderInFolder",
        "Parent Folder Of",
        "Child Folder Of",
        1,
    ),
    (
        "afc1d05d3dcbea4f8c51cb3ba8d77cdb",
        "6f3e78f3ca6514458315ce3985dad3b1",
        "d5ad0584f88d27428fac3fcade073386",
        "ItemInFolder",
        "Contains Item",
        "Contained In Folder",
        1,
    ),
    (
        "593a63a1ba4648448706d8abe2b2b02e",
        "49717374b5dc57428904b9724e32a530",
        "899eda2880ff6d4d89264ee2b161677d",
        "DatasetInFeatureDataset",
        "Contains Dataset",
        "Contained In FeatureDataset",
        1,
    ),
    (
        "abf178dce434ac43ba471c4eabd0e7c7",
        "6f3e78f3ca6514458315ce3985dad3b1",
        "899eda2880ff6d4d89264ee2b161677d",
        "DatasetInFolder",
        "Contains Dataset",
        "Contained in Dataset",
        1,
    ),
    (
        "db8ae017312bcd4d8fdddf529e88f843",
        "899eda2880ff6d4d89264ee2b161677d",
        "edf13786048c6648a44a1cb8288b3c63",
        "DomainInDataset",
        "Contains Domain",
        "Contained in Dataset",
        0,
    ),
    (
        "abad5b7252341b49a79555f32d67229c",
        "899eda2880ff6d4d89264ee2b161677d",
        "899eda2880ff6d4d89264ee2b161677d",
        "DatasetsRelatedThrough",
        "Origin Of",
        "Destination Of",
        0,
    ),
    (
        "10b188d00b192942bdf789fddd14d1ea",
        "d352717666ed25438774420d46674e07",
        "097873702c85034a9e222cecea5b9bfa",
        "FeatureClassInTopology",
        "Spatially Manages Feature Class",
        "Participates In Topology",
        0,
    ),
    (
        "709a73dc719be841868c008cf46f16d7",
        "668a7173b9af884ba551cffa0ae12620",
        "097873702c85034a9e222cecea5b9bfa",
        "FeatureClassInGeometricNetwork",
        "Spatially Manages Feature Class",
        "Participates In Geometric Network",
        0,
    ),
    (
        "63852bb3960b324d92c4086423ae9962",
        "7dfc71778ba3d34f8225639d17e9a131",
        "097873702c85034a9e222cecea5b9bfa",
        "FeatureClassInNetworkDataset",
        "Spatially Manages Feature Class",
        "Participates In Network Dataset",
        0,
    ),
    (
        "70468a901111c6488269134fdd3fe617",
        "7dfc71778ba3d34f8225639d17e9a131",
        "3bbc06cd9d78514caafaa467912b8965",
        "TableInNetworkDataset",
        "Manages Table",
        "Participates In Network Dataset",
        0,
    ),
    (
        "dcf4d25517cb324ea8c747591e8c71de",
        "377535766433af48a4be783c7c28b5cb",
        "097873702c85034a9e222cecea5b9bfa",
        "FeatureClassInTerrain",
        "Spatially Manages Feature Class",
        "Participates In Terrain",
        0,
    ),
    (
        "aa5b3a585135ae418aa81185719f3889",
        "693380a3c25f6349bae013effc09dd73",
        "097873702c85034a9e222cecea5b9bfa",
        "FeatureClassInParcelFabric",
        "Spatially Manages Feature Class",
        "Participates In Parcel Fabric",
        0,
    ),
    (
        "e085905f8f785443ae3c34c83a7ea784",
        "693380a3c25f6349bae013effc09dd73",
        "3bbc06cd9d78514caafaa467912b8965",
        "TableInParcelFabric",
        "Manages Table",
        "Participates In Parcel Fabric",
        0,
    ),
    (
        "e3449be733f8124b90a1364ec4ddc43e",
        "097873702c85034a9e222cecea5b9bfa",
        "8d0000a3ea0c6a4f9dfa46af829a3df2",
        "RepresentationOfFeatureClass",
        "Feature Class Representation",
        "Represented Feature Class",
        0,
    ),
    (
        "f11ab38d7cdf3246aa103cc44b0c6914",
        "8ea5d44e1f62434095ed850fba45fcbc",
        "eb2184d982d51347948443304d0810f6",
        "ReplicaDatasetInReplica",
        "Replicated Dataset",
        "Participates In Replica",
        1,
    ),
    (
        "33de22d0bd454c4288bf5b1b6b957bd3",
        "eb2184d982d51347948443304d0810f6",
        "899eda2880ff6d4d89264ee2b161677d",
        "DatasetOfReplicaDataset",
        "Replicated Dataset",
        "Dataset of Replicated Dataset",
        0,
    ),
];

/// The system tables, in catalog order (`GDB_ReplicaLog` has no table of its own).
const SYSTEM: [(&str, i32); 8] = [
    ("GDB_SystemCatalog", 0),
    ("GDB_DBTune", 0),
    ("GDB_SpatialRefs", 0),
    ("GDB_Items", 0),
    ("GDB_ItemTypes", 0),
    ("GDB_ItemRelationships", 0),
    ("GDB_ItemRelationshipTypes", 0),
    ("GDB_ReplicaLog", 2),
];

/// WGS 84 as Esri writes it.
pub(super) const WGS84: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

/// The storage precision Esri gives longitude/latitude: a nanodegree, from -400.
pub(super) fn lonlat_storage(extent: [f64; 4], grids: Vec<f64>) -> GeomDef {
    GeomDef { wkt: WGS84.into(), x_origin: -400.0, y_origin: -400.0, xy_scale: 1e9, xy_tolerance: 8.983_153e-9, extent, grids }
}

/// A dataset to write, already encoded.
pub(super) struct Dataset {
    pub name: String,
    /// 0 for a table, else the geometry type (1 point, 2 multipoint, 3 polyline, 4 polygon).
    pub geometry_type: u8,
    pub fields: Vec<FieldDef>,
    pub geom: Option<GeomDef>,
    pub gdbtable: Vec<u8>,
    pub gdbtablx: Vec<u8>,
}

/// Every file of a new geodatabase holding `datasets`, by name; `guid` makes the random GUIDs
/// items are identified by.
pub(super) fn files(datasets: Vec<Dataset>, guid: &mut dyn FnMut() -> [u8; 16]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut put = |stem: &str, (t, x): (Vec<u8>, Vec<u8>)| {
        out.push((format!("{stem}.gdbtable"), t));
        out.push((format!("{stem}.gdbtablx"), x));
    };
    let text = |s: &str| Out::Text(s.to_string());
    // The catalog: the system tables, then the datasets from object ID 9.
    let mut catalog: Vec<Vec<Out>> = SYSTEM.iter().map(|(n, f)| vec![text(n), Out::I32(*f)]).collect();
    catalog.extend(datasets.iter().map(|d| vec![text(&d.name), Out::I32(0)]));
    put(
        "a00000001",
        table(
            0,
            &[
                FieldDef::new("ID", code::OBJECT_ID, 4, false),
                FieldDef::new("Name", code::TEXT, 160, false),
                FieldDef::new("FileFormat", code::INT32, 4, false),
            ],
            None,
            &catalog,
        ),
    );
    put(
        "a00000002",
        table(
            0,
            &[
                FieldDef::new("Keyword", code::TEXT, 32, false),
                FieldDef::new("ParameterName", code::TEXT, 32, false),
                FieldDef::new("ConfigString", code::TEXT, 2048, true),
            ],
            None,
            &DBTUNE.iter().map(|(k, p, c)| vec![text(k), text(p), text(c)]).collect::<Vec<_>>(),
        ),
    );
    // Coordinate systems: the one the items' shapes use, then the datasets'.
    let items_geom = GeomDef {
        wkt: WGS84.into(),
        x_origin: -180.0,
        y_origin: -90.0,
        xy_scale: 1e6,
        xy_tolerance: 2e-6,
        extent: [f64::NAN; 4],
        grids: vec![0.012, 0.4, 12.0],
    };
    let mut refs: Vec<&GeomDef> = vec![&items_geom];
    for g in datasets.iter().filter_map(|d| d.geom.as_ref()) {
        if !refs.iter().any(|r| r.wkt == g.wkt && r.x_origin == g.x_origin && r.y_origin == g.y_origin && r.xy_scale == g.xy_scale) {
            refs.push(g);
        }
    }
    let mut ref_fields = vec![FieldDef::new("ID", code::OBJECT_ID, 4, false), FieldDef::new("SRTEXT", code::TEXT, 2048, false)];
    for n in ["FalseX", "FalseY", "XYUnits", "FalseZ", "ZUnits", "FalseM", "MUnits", "XYTolerance", "ZTolerance", "MTolerance"] {
        ref_fields.push(FieldDef::new(n, code::FLOAT64, 8, true));
    }
    let ref_rows: Vec<Vec<Out>> = refs
        .iter()
        .map(|g| {
            let mut r = vec![text(&g.wkt)];
            for v in [g.x_origin, g.y_origin, g.xy_scale, -100_000.0, 10_000.0, -100_000.0, 10_000.0, g.xy_tolerance, 0.001, 0.001] {
                r.push(Out::F64(v));
            }
            r
        })
        .collect();
    put("a00000003", table(0, &ref_fields, None, &ref_rows));
    // Items: the root folder, the workspace, then each dataset.
    let root = guid();
    let mut items: Vec<Vec<Out>> = Vec::new();
    let item = |uuid: [u8; 16],
                ty: &str,
                name: &str,
                physical: &str,
                path: &str,
                sub: Option<(i32, i32, &str)>,
                def: Option<String>,
                props: i32| {
        vec![
            Out::Guid(uuid),
            Out::Guid(hex16(ty)),
            text(name),
            text(physical),
            text(path),
            sub.map_or(Out::Null, |s| Out::I32(s.0)),
            sub.map_or(Out::Null, |s| Out::I32(s.1)),
            sub.map_or(Out::Null, |s| text(s.2)),
            Out::Null,
            text(""),
            def.map_or(Out::Null, Out::Text),
            Out::Null,
            Out::Null,
            Out::I32(props),
            Out::Null,
            Out::Null,
        ]
    };
    items.push(item(root, ids::FOLDER, "", "", "\\", None, None, 1));
    items.push(item(guid(), ids::WORKSPACE, "Workspace", "WORKSPACE", "", None, Some(workspace_xml()), 0));
    let mut relationships: Vec<Vec<Out>> = Vec::new();
    for d in &datasets {
        let id = guid();
        // The item's object ID, which its definition repeats.
        let dsid = items.len() + 1;
        let (ty, sub) =
            if d.geom.is_some() { (ids::FEATURE_CLASS, Some((1, i32::from(d.geometry_type), "SHAPE"))) } else { (ids::TABLE, None) };
        items.push(item(id, ty, &d.name, &d.name.to_uppercase(), &format!("\\{}", d.name), sub, Some(dataset_xml(d, dsid)), 1));
        relationships.push(vec![
            Out::Guid(guid()),
            Out::Guid(root),
            Out::Guid(id),
            Out::Guid(hex16(ids::DATASET_IN_FOLDER)),
            Out::Null,
            Out::I32(1),
        ]);
    }
    let item_fields = vec![
        FieldDef::new("ObjectID", code::OBJECT_ID, 4, false),
        FieldDef::new("UUID", code::GLOBAL_ID, 38, false),
        FieldDef::new("Type", code::GUID, 38, false),
        FieldDef::new("Name", code::TEXT, 160, true),
        FieldDef::new("PhysicalName", code::TEXT, 160, true),
        FieldDef::new("Path", code::TEXT, 260, true),
        FieldDef::new("DatasetSubtype1", code::INT32, 4, true),
        FieldDef::new("DatasetSubtype2", code::INT32, 4, true),
        FieldDef::new("DatasetInfo1", code::TEXT, 255, true),
        FieldDef::new("DatasetInfo2", code::TEXT, 255, true),
        FieldDef::new("URL", code::TEXT, 255, true),
        FieldDef::new("Definition", code::XML, 0, true),
        FieldDef::new("Documentation", code::XML, 0, true),
        FieldDef::new("ItemInfo", code::XML, 0, true),
        FieldDef::new("Properties", code::INT32, 4, true),
        FieldDef::new("Defaults", code::BINARY, 0, true),
        FieldDef::new("Shape", code::GEOMETRY, 0, true),
    ];
    put("a00000004", table(4, &item_fields, Some(&items_geom), &items));
    put(
        "a00000005",
        table(
            0,
            &[
                FieldDef::new("ObjectID", code::OBJECT_ID, 4, false),
                FieldDef::new("UUID", code::GUID, 38, false),
                FieldDef::new("ParentTypeID", code::GUID, 38, false),
                FieldDef::new("Name", code::TEXT, 160, false),
            ],
            None,
            &ITEM_TYPES.iter().map(|(u, p, n)| vec![Out::Guid(hex16(u)), Out::Guid(hex16(p)), text(n)]).collect::<Vec<_>>(),
        ),
    );
    put(
        "a00000006",
        table(
            0,
            &[
                FieldDef::new("ObjectID", code::OBJECT_ID, 4, false),
                FieldDef::new("UUID", code::GLOBAL_ID, 38, false),
                FieldDef::new("OriginID", code::GUID, 38, false),
                FieldDef::new("DestID", code::GUID, 38, false),
                FieldDef::new("Type", code::GUID, 38, false),
                FieldDef::new("Attributes", code::XML, 0, true),
                FieldDef::new("Properties", code::INT32, 4, true),
            ],
            None,
            &relationships,
        ),
    );
    put(
        "a00000007",
        table(
            0,
            &[
                FieldDef::new("ObjectID", code::OBJECT_ID, 4, false),
                FieldDef::new("UUID", code::GUID, 38, false),
                FieldDef::new("OrigItemTypeID", code::GUID, 38, false),
                FieldDef::new("DestItemTypeID", code::GUID, 38, false),
                FieldDef::new("Name", code::TEXT, 160, true),
                FieldDef::new("ForwardLabel", code::TEXT, 255, true),
                FieldDef::new("BackwardLabel", code::TEXT, 255, true),
                FieldDef::new("IsContainment", code::INT16, 2, true),
            ],
            None,
            &RELATIONSHIP_TYPES
                .iter()
                .map(|(u, o, d, n, f, b, c)| {
                    vec![Out::Guid(hex16(u)), Out::Guid(hex16(o)), Out::Guid(hex16(d)), text(n), text(f), text(b), Out::I16(*c)]
                })
                .collect::<Vec<_>>(),
        ),
    );
    for (i, d) in datasets.into_iter().enumerate() {
        let stem = format!("a{:08x}", SYSTEM.len() + 1 + i);
        out.push((format!("{stem}.gdbtable"), d.gdbtable));
        out.push((format!("{stem}.gdbtablx"), d.gdbtablx));
    }
    // The geodatabase's marker file.
    out.push(("gdb".into(), vec![5, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef]));
    out
}

fn workspace_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<DEWorkspace xmlns:typens="http://www.esri.com/schemas/ArcGIS/10.3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xs="http://www.w3.org/2001/XMLSchema" xsi:type="typens:DEWorkspace">
  <CatalogPath>\</CatalogPath>
  <Name/>
  <ChildrenExpanded>false</ChildrenExpanded>
  <WorkspaceType>esriLocalDatabaseWorkspace</WorkspaceType>
  <WorkspaceFactoryProgID/>
  <ConnectionString/>
  <ConnectionInfo xsi:nil="true"/>
  <Domains xsi:type="typens:ArrayOfDomain"/>
  <MajorVersion>3</MajorVersion>
  <MinorVersion>0</MinorVersion>
  <BugfixVersion>0</BugfixVersion>
</DEWorkspace>"#
        .to_string()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A feature class's (`DEFeatureClassInfo`) or table's (`DETableInfo`) definition.
fn dataset_xml(d: &Dataset, dsid: usize) -> String {
    let (kind, dataset_type, clsid) = if d.geom.is_some() {
        ("DEFeatureClassInfo", "esriDTFeatureClass", "{52353152-891A-11D0-BEC6-00805F7C4268}")
    } else {
        ("DETableInfo", "esriDTTable", "{7A566981-C114-11D2-8A28-006097AFF44E}")
    };
    let name = escape(&d.name);
    let mut x = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<typens:{kind} xmlns:typens="http://www.esri.com/schemas/ArcGIS/10.3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xs="http://www.w3.org/2001/XMLSchema" xsi:type="typens:{kind}">
  <CatalogPath>\{name}</CatalogPath>
  <Name>{name}</Name>
  <ChildrenExpanded>false</ChildrenExpanded>
  <DatasetType>{dataset_type}</DatasetType>
  <DSID>{dsid}</DSID>
  <Versioned>false</Versioned>
  <CanVersion>false</CanVersion>
  <HasOID>true</HasOID>
  <OIDFieldName>OBJECTID</OIDFieldName>
  <GPFieldInfoExs xsi:type="typens:ArrayOfGPFieldInfoEx">
"#
    );
    for f in &d.fields {
        let (ty, length, required) = match f.code {
            code::GEOMETRY => ("esriFieldTypeGeometry", 0, true),
            code::OBJECT_ID => ("esriFieldTypeOID", 4, true),
            code::INT16 => ("esriFieldTypeSmallInteger", 2, false),
            code::INT32 => ("esriFieldTypeInteger", 4, false),
            code::INT64 => ("esriFieldTypeBigInteger", 8, false),
            code::FLOAT64 => ("esriFieldTypeDouble", 8, false),
            _ => ("esriFieldTypeString", f.width, false),
        };
        let nullable = f.flag & 1 != 0 && f.code != code::OBJECT_ID;
        x.push_str(&format!(
            "    <GPFieldInfoEx xsi:type=\"typens:GPFieldInfoEx\">\n      <Name>{}</Name>\n      <FieldType>{ty}</FieldType>\n      <IsNullable>{nullable}</IsNullable>\n      <Length>{length}</Length>\n      <Precision>0</Precision>\n      <Scale>0</Scale>\n{}    </GPFieldInfoEx>\n",
            escape(&f.name),
            if required { "      <Required>true</Required>\n" } else { "" }
        ));
    }
    x.push_str("  </GPFieldInfoExs>\n");
    x.push_str(&format!("  <CLSID>{clsid}</CLSID>\n  <EXTCLSID></EXTCLSID>\n  <IsTimeInUTC>true</IsTimeInUTC>\n"));
    if let Some(g) = &d.geom {
        let shape = match d.geometry_type {
            1 => "esriGeometryPoint",
            2 => "esriGeometryMultipoint",
            3 => "esriGeometryPolyline",
            _ => "esriGeometryPolygon",
        };
        x.push_str(&format!(
            r#"  <FeatureType>esriFTSimple</FeatureType>
  <ShapeType>{shape}</ShapeType>
  <ShapeFieldName>SHAPE</ShapeFieldName>
  <HasM>false</HasM>
  <HasZ>false</HasZ>
  <HasSpatialIndex>false</HasSpatialIndex>
  <AreaFieldName></AreaFieldName>
  <LengthFieldName></LengthFieldName>
  <Extent xsi:nil="true"></Extent>
  <SpatialReference xsi:type="typens:GeographicCoordinateSystem">
    <WKT>{}</WKT>
    <XOrigin>{}</XOrigin>
    <YOrigin>{}</YOrigin>
    <XYScale>{}</XYScale>
    <ZOrigin>-100000</ZOrigin>
    <ZScale>10000</ZScale>
    <MOrigin>-100000</MOrigin>
    <MScale>10000</MScale>
    <XYTolerance>{:e}</XYTolerance>
    <ZTolerance>0.001</ZTolerance>
    <MTolerance>0.001</MTolerance>
    <HighPrecision>true</HighPrecision>
    <WKID>4326</WKID>
    <LatestWKID>4326</LatestWKID>
  </SpatialReference>
"#,
            escape(&g.wkt),
            g.x_origin,
            g.y_origin,
            g.xy_scale,
            g.xy_tolerance
        ));
    }
    x.push_str(&format!("</typens:{kind}>\n"));
    x
}
