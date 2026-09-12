use super::*;

/// A binary STL of `tris` facets, each a unit triangle in the z = i plane.
fn binary_stl(tris: u32, header: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 80];
    b[..header.len().min(80)].copy_from_slice(&header[..header.len().min(80)]);
    b.extend_from_slice(&tris.to_le_bytes());
    for i in 0..tris {
        let z = i as f32;
        // A deliberately wrong stored normal, to prove it is not used.
        for c in [0.0f32, 0.0, -7.0] {
            b.extend_from_slice(&c.to_le_bytes());
        }
        for p in [[0.0f32, 0.0, z], [1.0, 0.0, z], [0.0, 1.0, z]] {
            for c in p {
                b.extend_from_slice(&c.to_le_bytes());
            }
        }
        b.extend_from_slice(&0u16.to_le_bytes());
    }
    b
}

const ASCII_STL: &str = "solid cube
facet normal 0 0 1
  outer loop
    vertex 0 0 0
    vertex 1 0 0
    vertex 0 1 0
  endloop
endfacet
endsolid cube
";

#[test]
fn a_binary_stl_is_parsed_and_bounded() {
    let m = load(&binary_stl(3, b"exported by something"), "part.stl").unwrap();
    assert_eq!(m.tris.len(), 3);
    assert_eq!(m.format, "STL");
    assert_eq!(m.min, v3(0.0, 0.0, 0.0));
    assert_eq!(m.max, v3(1.0, 1.0, 2.0));
}

#[test]
fn a_binary_stl_whose_header_says_solid_is_still_read_as_binary() {
    // The classic trap: the length arithmetic must win over the leading word.
    let m = load(&binary_stl(2, b"solid produced by a lazy exporter"), "p.stl").unwrap();
    assert_eq!(m.tris.len(), 2, "parsed as binary, not as an empty ASCII file");
}

#[test]
fn an_ascii_stl_is_parsed() {
    let m = load(ASCII_STL.as_bytes(), "p.stl").unwrap();
    assert_eq!(m.tris.len(), 1);
    assert_eq!(m.max, v3(1.0, 1.0, 0.0));
}

#[test]
fn the_stored_facet_normal_is_ignored_in_favour_of_the_winding() {
    // Every facet in the fixture stores (0,0,-7); the winding gives +Z.
    let m = load(&binary_stl(1, b""), "p.stl").unwrap();
    let n = m.tris[0].n;
    assert!((n.z - 1.0).abs() < 1e-5, "recomputed from the winding, got {n:?}");
    assert!((n.len() - 1.0).abs() < 1e-5, "and normalised");
}

#[test]
fn a_truncated_binary_stl_yields_what_survived_rather_than_panicking() {
    let mut b = binary_stl(4, b"");
    b.truncate(b.len() - 30);
    // The count still says 4 but the table is short; the parser stops early.
    let tris = stl::parse(&b).unwrap_or_default();
    assert!(tris.len() < 4);
}

#[test]
fn an_empty_mesh_is_refused_so_the_viewer_falls_back() {
    assert!(load(&binary_stl(0, b""), "p.stl").is_none());
    assert!(load(b"", "p.stl").is_none());
    assert!(load(b"solid empty\nendsolid empty\n", "p.stl").is_none());
}

#[test]
fn degenerate_triangles_are_dropped() {
    let mut out = Vec::new();
    // Zero area: two coincident corners.
    push_tri(&mut out, v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(1.0, 0.0, 0.0));
    // Zero area: three collinear points.
    push_tri(&mut out, v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(2.0, 0.0, 0.0));
    push_tri(&mut out, v3(f32::NAN, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
    assert!(out.is_empty(), "none of these enclose an area");
}

#[test]
fn a_tiny_triangle_survives_because_the_area_test_is_relative() {
    // An absolute epsilon would discard a model authored in microns entirely.
    let mut out = Vec::new();
    let s = 1e-4;
    push_tri(&mut out, v3(0.0, 0.0, 0.0), v3(s, 0.0, 0.0), v3(0.0, s, 0.0));
    assert_eq!(out.len(), 1);
    assert!((out[0].n.z - 1.0).abs() < 1e-5);
}

#[test]
fn an_obj_with_a_quad_is_fan_triangulated() {
    let src = "# a square\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
    let m = load(src.as_bytes(), "p.obj").unwrap();
    assert_eq!(m.tris.len(), 2, "a quad becomes two triangles");
    assert_eq!(m.format, "OBJ");
    assert_eq!(m.max, v3(1.0, 1.0, 0.0));
}

#[test]
fn obj_index_forms_and_negative_indices_all_resolve() {
    let base = "v 0 0 0\nv 1 0 0\nv 0 1 0\n";
    for f in ["f 1 2 3", "f 1/1 2/2 3/3", "f 1//1 2//2 3//3", "f 1/1/1 2/2/2 3/3/3", "f -3 -2 -1"] {
        let src = format!("{base}{f}\n");
        let m = load(src.as_bytes(), "p.obj").unwrap_or_else(|| panic!("failed on {f:?}"));
        assert_eq!(m.tris.len(), 1, "{f:?}");
        assert!((m.tris[0].n.z - 1.0).abs() < 1e-5, "{f:?} winding");
    }
}

#[test]
fn obj_vt_and_vn_lines_are_not_mistaken_for_vertices() {
    let src = "v 0 0 0\nvt 9 9\nv 1 0 0\nvn 0 0 1\nv 0 1 0\nf 1 2 3\n";
    let m = load(src.as_bytes(), "p.obj").unwrap();
    assert_eq!(m.max, v3(1.0, 1.0, 0.0), "vt/vn must not enter the vertex list");
}

#[test]
fn an_obj_face_referencing_a_missing_vertex_is_refused() {
    let src = "v 0 0 0\nv 1 0 0\nf 1 2 9\n";
    assert!(load(src.as_bytes(), "p.obj").is_none());
}

#[test]
fn only_formats_with_a_parser_are_claimed() {
    assert!(is_model_name("part.stl") && is_model_name("PART.STL"));
    assert!(is_model_name("scene.obj"));
    // Listed by `util::filetype` for colour and shape, but not parseable yet.
    assert!(!is_model_name("mesh.ply") && !is_model_name("mesh.3mf"));
    assert!(!is_model_name("notes.txt") && !is_model_name("noext"));
}

#[test]
fn centre_and_radius_describe_the_bounding_sphere() {
    let m = load(&binary_stl(3, b""), "p.stl").unwrap();
    assert_eq!(m.centre(), v3(0.5, 0.5, 1.0));
    assert!(m.radius() > 0.0 && m.radius().is_finite());
}

#[test]
fn a_flat_mesh_still_has_a_usable_radius() {
    // Everything in one plane: the fit must not divide by zero.
    let m = load(ASCII_STL.as_bytes(), "p.stl").unwrap();
    assert!(m.radius() > 0.0 && m.radius().is_finite());
}

#[test]
fn an_unknown_extension_is_not_loaded() {
    assert!(load(&binary_stl(1, b""), "part.ply").is_none());
}
