#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use places::pmtiles::{self, Archive, Compression, MapFile, TileType, tile_id};

#[test]
fn tile_ids_follow_the_spec() {
    // the values in the PMTiles reference implementations' tests
    assert_eq!(tile_id(0, 0, 0), Some(0));
    assert_eq!(tile_id(1, 0, 0), Some(1));
    assert_eq!(tile_id(1, 0, 1), Some(2));
    assert_eq!(tile_id(1, 1, 1), Some(3));
    assert_eq!(tile_id(1, 1, 0), Some(4));
    assert_eq!(tile_id(2, 0, 0), Some(5));
    assert_eq!(tile_id(12, 3423, 1763), Some(19_078_479));
    assert_eq!(tile_id(1, 2, 0), None, "outside the zoom");
    // every tile of a zoom has its own id, right after the zoom before
    let mut ids: Vec<u64> = (0..8).flat_map(|x| (0..8).map(move |y| tile_id(3, x, y).unwrap())).collect();
    ids.sort_unstable();
    assert_eq!(ids, (21..85).collect::<Vec<u64>>());
}

#[test]
fn reads_tiles_from_plain_and_gzip_directories() {
    let dir = tempfile::tempdir().unwrap();
    for gzip in [false, true] {
        let tiles =
            vec![(0, 0, 0, b"world".to_vec()), (13, 4093, 2724, b"city".to_vec()), (16, 32748, 21794, vec![7u8; 3000])];
        let path = dir.path().join(format!("t{gzip}.pmtiles"));
        std::fs::write(&path, pmtiles::build(&tiles, 2, gzip)).unwrap();
        let a = Archive::open(&path).unwrap();
        assert_eq!(a.header.tile_type, TileType::Png);
        assert_eq!(a.header.tile_type.kind(), "raster");
        assert_eq!(a.header.tile_compression, Compression::None);
        assert_eq!((a.header.min_zoom, a.header.max_zoom), (0, 16));
        for (z, x, y, b) in &tiles {
            assert_eq!(a.tile(*z, *x, *y).unwrap().as_ref(), Some(b), "{z}/{x}/{y} gzip {gzip}");
        }
        assert_eq!(a.tile(13, 4092, 2724).unwrap(), None, "a tile the file does not have");
        assert_eq!(a.tile(17, 0, 0).unwrap(), None, "deeper than the file");
        assert_eq!(a.tile(2, 9, 0).unwrap(), None, "outside the zoom");
    }
}

#[test]
fn refuses_files_that_are_not_pmtiles() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.pmtiles");
    std::fs::write(&p, vec![0u8; 400]).unwrap();
    assert!(Archive::open(&p).is_err());
    let mut v2 = pmtiles::build(&[(0, 0, 0, b"a".to_vec())], 1, false);
    v2[7] = 2;
    std::fs::write(&p, &v2).unwrap();
    assert!(Archive::open(&p).unwrap_err().to_string().contains("version 3"));
    // a directory that claims more entries than it has bytes
    let mut broken = pmtiles::build(&[(0, 0, 0, b"a".to_vec())], 1, false);
    broken[127] = 0x7f;
    std::fs::write(&p, &broken).unwrap();
    assert!(Archive::open(&p).is_err());
}

#[test]
fn lists_and_picks_map_files() {
    let dir = tempfile::tempdir().unwrap();
    assert!(pmtiles::maps_in(dir.path()).is_empty(), "no maps folder");
    let maps = dir.path().join("maps");
    std::fs::create_dir_all(&maps).unwrap();
    let small = pmtiles::build(&[(0, 0, 0, b"a".to_vec())], 1, false);
    let big = pmtiles::build(&[(0, 0, 0, vec![1u8; 5000])], 1, false);
    std::fs::write(maps.join("home.pmtiles"), &small).unwrap();
    std::fs::write(maps.join("region.pmtiles"), &big).unwrap();
    std::fs::write(maps.join("notes.txt"), b"x").unwrap();
    std::fs::write(maps.join(".hidden.pmtiles"), &big).unwrap();
    std::fs::write(maps.join("tiny.pmtiles"), b"x").unwrap();
    std::fs::create_dir_all(maps.join("dir.pmtiles")).unwrap();
    let files = pmtiles::maps_in(dir.path());
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["region.pmtiles", "home.pmtiles"], "largest first, only real map files");
    assert_eq!(pmtiles::pick(&files, None).map(|f| f.name.as_str()), Some("region.pmtiles"));
    assert_eq!(pmtiles::pick(&files, Some("home.pmtiles")).map(|f| f.name.as_str()), Some("home.pmtiles"));
    assert_eq!(pmtiles::pick(&files, Some("gone.pmtiles")).map(|f| f.name.as_str()), Some("region.pmtiles"));
    assert_eq!(pmtiles::pick(&[] as &[MapFile], None), None);
    for bad in ["../x.pmtiles", "a/b.pmtiles", ".pmtiles", "x.pmtiles.tmp", "a b.pmtiles", ".x.pmtiles"] {
        assert!(!pmtiles::name_ok(bad), "{bad}");
    }
    assert!(pmtiles::name_ok("europe-2026_11.pmtiles"));
}
