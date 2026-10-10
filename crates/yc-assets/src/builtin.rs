use std::collections::BTreeMap;

use indexmap::IndexMap;

use crate::ir::{FaceAxis, FaceName};
use crate::model::{RawDisplayTransform, RawElement, RawElementRotation, RawModel, raw_face};

/// Builtin parentは独自の最小geometryとして定義する。Vanilla JSON本文を複製・同梱しない。
pub(crate) fn models() -> BTreeMap<String, Vec<u8>> {
    let mut definitions = BTreeMap::<String, RawModel>::new();
    definitions.insert("block/block".to_owned(), RawModel::default());

    definitions.insert(
        "block/cube".to_owned(),
        RawModel {
            parent: Some("block/block".to_owned()),
            elements: Some(vec![cube(
                0.0,
                0.0,
                0.0,
                16.0,
                16.0,
                16.0,
                ["#down", "#up", "#north", "#south", "#west", "#east"],
            )]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/cube_all".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#all"),
                ("down", "#all"),
                ("up", "#all"),
                ("north", "#all"),
                ("south", "#all"),
                ("west", "#all"),
                ("east", "#all"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/cube_column".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#end"),
                ("down", "#end"),
                ("up", "#end"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/cube_column_horizontal".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#side"),
                ("down", "#side"),
                ("up", "#side"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#end"),
                ("east", "#end"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/cube_bottom_top".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#side"),
                ("down", "#bottom"),
                ("up", "#top"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/cube_top".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#side"),
                ("down", "#side"),
                ("up", "#top"),
                ("north", "#side"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/orientable".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#side"),
                ("down", "#side"),
                ("up", "#top"),
                ("north", "#front"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "block/orientable_with_bottom".to_owned(),
        RawModel {
            parent: Some("block/cube".to_owned()),
            textures: aliases(&[
                ("particle", "#side"),
                ("down", "#bottom"),
                ("up", "#top"),
                ("north", "#front"),
                ("south", "#side"),
                ("west", "#side"),
                ("east", "#side"),
            ]),
            ..RawModel::default()
        },
    );
    for (name, from, to) in [
        ("block/slab", [0.0, 0.0, 0.0], [16.0, 8.0, 16.0]),
        ("block/slab_top", [0.0, 8.0, 0.0], [16.0, 16.0, 16.0]),
    ] {
        definitions.insert(
            name.to_owned(),
            RawModel {
                parent: Some("block/block".to_owned()),
                textures: aliases(&[
                    ("particle", "#side"),
                    ("down", "#bottom"),
                    ("up", "#top"),
                    ("north", "#side"),
                    ("south", "#side"),
                    ("west", "#side"),
                    ("east", "#side"),
                ]),
                elements: Some(vec![cube(
                    from[0],
                    from[1],
                    from[2],
                    to[0],
                    to[1],
                    to[2],
                    ["#down", "#up", "#north", "#south", "#west", "#east"],
                )]),
                ..RawModel::default()
            },
        );
    }

    // Fence / wall / pane blockstatesは小さなpostと隣接時のarmを別modelとして重ねる。
    // Blockstate JSONが用意できないModでも、親modelの役割だけで自然な形を描けるようにする。
    for (name, bounds, texture) in [
        (
            "block/fence_post",
            ([6.0, 0.0, 6.0], [10.0, 16.0, 10.0]),
            "texture",
        ),
        (
            "block/fence_inventory",
            ([6.0, 0.0, 6.0], [10.0, 16.0, 10.0]),
            "texture",
        ),
        (
            "block/fence_side",
            ([7.0, 6.0, 0.0], [9.0, 15.0, 8.0]),
            "texture",
        ),
        (
            "block/wall_post",
            ([4.0, 0.0, 4.0], [12.0, 16.0, 12.0]),
            "wall",
        ),
        (
            "block/wall_inventory",
            ([4.0, 0.0, 4.0], [12.0, 16.0, 12.0]),
            "wall",
        ),
        (
            "block/wall_side",
            ([5.0, 4.0, 0.0], [11.0, 14.0, 8.0]),
            "wall",
        ),
        (
            "block/wall_side_tall",
            ([5.0, 4.0, 0.0], [11.0, 16.0, 8.0]),
            "wall",
        ),
        (
            "block/pane_post",
            ([7.0, 0.0, 7.0], [9.0, 16.0, 9.0]),
            "pane",
        ),
        (
            "block/pane_side",
            ([7.0, 0.0, 0.0], [9.0, 16.0, 8.0]),
            "pane",
        ),
    ] {
        let texture_reference = format!("#{texture}");
        let model = textured_model(
            &[("particle", texture_reference.as_str())],
            vec![cube(
                bounds.0[0],
                bounds.0[1],
                bounds.0[2],
                bounds.1[0],
                bounds.1[1],
                bounds.1[2],
                [texture_reference.as_str(); 6],
            )],
        );
        definitions.insert(name.to_owned(), model);
    }

    // Doorは下半分・上半分を別modelにし、blockstate rotationで向きと開閉を表現する。
    for (name, from, to, texture) in [
        (
            "block/door_bottom",
            [0.0, 0.0, 0.0],
            [3.0, 16.0, 16.0],
            "#bottom",
        ),
        ("block/door_top", [0.0, 0.0, 0.0], [3.0, 16.0, 16.0], "#top"),
        (
            "block/door_bottom_left",
            [0.0, 0.0, 0.0],
            [3.0, 16.0, 16.0],
            "#bottom",
        ),
        (
            "block/door_bottom_right",
            [0.0, 0.0, 0.0],
            [3.0, 16.0, 16.0],
            "#bottom",
        ),
        (
            "block/door_top_left",
            [0.0, 0.0, 0.0],
            [3.0, 16.0, 16.0],
            "#top",
        ),
        (
            "block/door_top_right",
            [0.0, 0.0, 0.0],
            [3.0, 16.0, 16.0],
            "#top",
        ),
    ] {
        definitions.insert(
            name.to_owned(),
            textured_model(
                &[("particle", "#bottom")],
                vec![cube(
                    from[0],
                    from[1],
                    from[2],
                    to[0],
                    to[1],
                    to[2],
                    [texture; 6],
                )],
            ),
        );
    }

    // Trapdoorは閉じたときに床/天井へ沿い、open parentでは縦に立つ薄板になる。
    for (name, from, to) in [
        ("block/trapdoor_bottom", [0.0, 0.0, 0.0], [16.0, 3.0, 16.0]),
        ("block/trapdoor_top", [0.0, 13.0, 0.0], [16.0, 16.0, 16.0]),
        ("block/trapdoor_open", [0.0, 0.0, 0.0], [16.0, 16.0, 3.0]),
        (
            "block/orientable_trapdoor_bottom",
            [0.0, 0.0, 0.0],
            [16.0, 3.0, 16.0],
        ),
        (
            "block/orientable_trapdoor_top",
            [0.0, 13.0, 0.0],
            [16.0, 16.0, 16.0],
        ),
        (
            "block/orientable_trapdoor_open",
            [0.0, 0.0, 0.0],
            [16.0, 16.0, 3.0],
        ),
        // 1.19以降のvanillaは template_ 付きの名前をparentにする（例: oak_trapdoor_bottom）。
        // Minecraft本体JARが無い環境でも同じ形を出すため、同じgeometryを別名でも登録する。
        (
            "block/template_trapdoor_bottom",
            [0.0, 0.0, 0.0],
            [16.0, 3.0, 16.0],
        ),
        (
            "block/template_trapdoor_top",
            [0.0, 13.0, 0.0],
            [16.0, 16.0, 16.0],
        ),
        (
            "block/template_trapdoor_open",
            [0.0, 0.0, 0.0],
            [16.0, 16.0, 3.0],
        ),
        (
            "block/template_orientable_trapdoor_bottom",
            [0.0, 0.0, 0.0],
            [16.0, 3.0, 16.0],
        ),
        (
            "block/template_orientable_trapdoor_top",
            [0.0, 13.0, 0.0],
            [16.0, 16.0, 16.0],
        ),
        (
            "block/template_orientable_trapdoor_open",
            [0.0, 0.0, 0.0],
            [16.0, 16.0, 3.0],
        ),
    ] {
        definitions.insert(
            name.to_owned(),
            textured_model(
                &[("particle", "#texture")],
                vec![cube(
                    from[0],
                    from[1],
                    from[2],
                    to[0],
                    to[1],
                    to[2],
                    ["#texture"; 6],
                )],
            ),
        );
    }

    for (name, from, to) in [
        ("block/button", [5.0, 6.0, 5.0], [11.0, 10.0, 11.0]),
        ("block/button_pressed", [6.0, 7.0, 6.0], [10.0, 9.0, 10.0]),
        (
            "block/pressure_plate_up",
            [1.0, 0.0, 1.0],
            [15.0, 1.0, 15.0],
        ),
        (
            "block/pressure_plate_down",
            [1.0, 0.0, 1.0],
            [15.0, 1.0, 15.0],
        ),
    ] {
        definitions.insert(
            name.to_owned(),
            textured_model(
                &[("particle", "#texture")],
                vec![cube(
                    from[0],
                    from[1],
                    from[2],
                    to[0],
                    to[1],
                    to[2],
                    ["#texture"; 6],
                )],
            ),
        );
    }

    definitions.insert(
        "block/carpet".to_owned(),
        textured_model(
            &[("particle", "#wool")],
            vec![cube(0.0, 0.0, 0.0, 16.0, 1.0, 16.0, ["#wool"; 6])],
        ),
    );

    // 葉・crop・鉢植えの植物はcross planeがalpha付きtextureの形を保つ。
    definitions.insert(
        "block/leaves".to_owned(),
        RawModel {
            parent: Some("block/cube_all".to_owned()),
            ..RawModel::default()
        },
    );
    for name in ["block/flower_pot_cross", "block/flower_pot_plant"] {
        definitions.insert(
            name.to_owned(),
            RawModel {
                parent: Some("block/tinted_cross".to_owned()),
                textures: aliases(&[("cross", "#plant"), ("particle", "#plant")]),
                ..RawModel::default()
            },
        );
    }
    definitions.insert(
        "block/crop".to_owned(),
        RawModel {
            parent: Some("block/block".to_owned()),
            textures: aliases(&[("particle", "#crop")]),
            ambient_occlusion: Some(false),
            elements: Some(vec![
                crop_plane(4.0, true),
                crop_plane(12.0, true),
                crop_plane(4.0, false),
                crop_plane(12.0, false),
            ]),
            ..RawModel::default()
        },
    );

    // Torchとlanternの簡易geometryは、Minecraft JARなしでもtextureを立体に配置する。
    for (name, from, to, angle) in [
        ("block/torch", [7.0, 0.0, 7.0], [9.0, 10.0, 9.0], 0.0),
        ("block/wall_torch", [7.0, 3.0, 7.0], [9.0, 13.0, 9.0], 22.5),
    ] {
        let mut element = cube(
            from[0],
            from[1],
            from[2],
            to[0],
            to[1],
            to[2],
            ["#torch"; 6],
        );
        if angle != 0.0 {
            element.rotation = Some(RawElementRotation {
                origin: [8.0, 0.0, 8.0],
                axis: FaceAxis::X,
                angle,
                rescale: false,
            });
        }
        definitions.insert(
            name.to_owned(),
            textured_model(&[("particle", "#torch")], vec![element]),
        );
    }

    definitions.insert(
        "block/rail".to_owned(),
        textured_model(
            &[("particle", "#rail")],
            vec![cube(0.0, 0.0, 0.0, 16.0, 2.0, 16.0, ["#rail"; 6])],
        ),
    );

    definitions.insert(
        "block/template_lantern".to_owned(),
        textured_model(
            &[("particle", "#lantern")],
            vec![
                cube(5.0, 5.0, 5.0, 11.0, 11.0, 11.0, ["#lantern"; 6]),
                cube(4.0, 11.0, 4.0, 12.0, 13.0, 12.0, ["#lantern"; 6]),
                cube(5.0, 3.0, 5.0, 11.0, 5.0, 11.0, ["#lantern"; 6]),
            ],
        ),
    );

    let mut chain_x = cube(0.0, 7.0, 7.0, 16.0, 9.0, 9.0, ["#chain"; 6]);
    chain_x.rotation = Some(RawElementRotation {
        origin: [8.0, 8.0, 8.0],
        axis: FaceAxis::Y,
        angle: 45.0,
        rescale: false,
    });
    let mut chain_z = cube(0.0, 7.0, 7.0, 16.0, 9.0, 9.0, ["#chain"; 6]);
    chain_z.rotation = Some(RawElementRotation {
        origin: [8.0, 8.0, 8.0],
        axis: FaceAxis::Y,
        angle: -45.0,
        rescale: false,
    });
    definitions.insert(
        "block/chain".to_owned(),
        textured_model(&[("particle", "#chain")], vec![chain_x, chain_z]),
    );

    // Block entity renderer由来のJSONはvanilla内でもtexture宣言だけのことがある。
    // JARなしでも親として使えるよう、状態を必要としない代表形だけBuiltinへ置く。
    for name in synthetic_model_names() {
        definitions
            .entry(name.clone())
            .or_insert_with(|| synthetic_block_model(&name));
    }
    // inner/outerは上段が占める区画を変え、Blockstate回転後も角の形を区別できるようにする。
    for (name, upper_steps) in [
        ("block/stairs", vec![(0.0, 8.0, 8.0, 16.0, 16.0, 16.0)]),
        (
            "block/inner_stairs",
            vec![
                (0.0, 8.0, 8.0, 16.0, 16.0, 16.0),
                (0.0, 8.0, 0.0, 8.0, 16.0, 8.0),
            ],
        ),
        ("block/outer_stairs", vec![(0.0, 8.0, 8.0, 8.0, 16.0, 16.0)]),
    ] {
        let mut elements = vec![cube(
            0.0,
            0.0,
            0.0,
            16.0,
            8.0,
            16.0,
            ["#down", "#up", "#side", "#side", "#side", "#side"],
        )];
        elements.extend(upper_steps.into_iter().map(|(x1, y1, z1, x2, y2, z2)| {
            cube(
                x1,
                y1,
                z1,
                x2,
                y2,
                z2,
                ["#side", "#top", "#side", "#side", "#side", "#side"],
            )
        }));
        definitions.insert(
            name.to_owned(),
            RawModel {
                parent: Some("block/block".to_owned()),
                textures: aliases(&[
                    ("particle", "#side"),
                    ("down", "#bottom"),
                    ("up", "#top"),
                    ("north", "#side"),
                    ("south", "#side"),
                    ("west", "#side"),
                    ("east", "#side"),
                ]),
                elements: Some(elements),
                ..RawModel::default()
            },
        );
    }
    for name in ["block/cross", "block/tinted_cross"] {
        definitions.insert(
            name.to_owned(),
            RawModel {
                parent: Some("block/block".to_owned()),
                textures: aliases(&[("particle", "#cross")]),
                ambient_occlusion: Some(false),
                elements: Some(vec![
                    crossed_plane(45.0, name == "block/tinted_cross"),
                    crossed_plane(-45.0, name == "block/tinted_cross"),
                ]),
                ..RawModel::default()
            },
        );
    }

    // thin_blockは親modelとして子のelements/displayだけを受け継がせる用途があるため、空の基底として登録する。
    definitions.insert(
        "block/thin_block".to_owned(),
        RawModel {
            parent: Some("block/block".to_owned()),
            elements: Some(Vec::new()),
            ..RawModel::default()
        },
    );
    // Vanillaはmultipartのfence/wall/pane用にtemplate_* parentを使う。
    // Builtin版にも同じtexture変数名の形を用意し、Minecraft JARなしでもModの子modelを継承できるようにする。
    for (template, source) in [
        ("block/template_wall_post", "block/wall_post"),
        ("block/template_wall_side", "block/wall_side"),
        ("block/template_wall_side_tall", "block/wall_side_tall"),
        ("block/template_glass_pane_post", "block/pane_post"),
        ("block/template_glass_pane_side", "block/pane_side"),
        ("block/template_glass_pane_noside", "block/pane_post"),
        ("block/template_glass_pane_noside_alt", "block/pane_post"),
    ] {
        if let Some(model) = definitions.get(source).cloned() {
            definitions.insert(template.to_owned(), model);
        }
    }

    definitions.insert(
        "item/generated".to_owned(),
        RawModel {
            // Vanilla item/generatedはbuiltin/generatedを親に持つため、同じ向きでcycleを作らない。
            parent: Some("builtin/generated".to_owned()),
            ambient_occlusion: Some(false),
            elements: Some(Vec::new()),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "item/handheld".to_owned(),
        RawModel {
            parent: Some("item/generated".to_owned()),
            display: handheld_display(),
            ..RawModel::default()
        },
    );
    definitions.insert(
        "builtin/generated".to_owned(),
        RawModel {
            // builtin/generated自体がspecial parentなので、geometryを持たない基底modelとして終端にする。
            ambient_occlusion: Some(false),
            elements: Some(Vec::new()),
            ..RawModel::default()
        },
    );

    definitions
        .into_iter()
        .filter_map(|(id, model)| {
            let path = format!("assets/minecraft/models/{id}.json");
            serde_json::to_vec(&model).ok().map(|bytes| (path, bytes))
        })
        .collect()
}

fn aliases(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn textured_model(textures: &[(&str, &str)], elements: Vec<RawElement>) -> RawModel {
    RawModel {
        parent: Some("block/block".to_owned()),
        textures: aliases(textures),
        elements: Some(elements),
        ..RawModel::default()
    }
}

/// renderer依存modelや、Vanilla JSONのない環境で参照される複合形状に使う簡略形。
pub(crate) fn fallback_elements(path: &str) -> Option<Vec<RawElement>> {
    let particle_cube = |from: [f64; 3], to: [f64; 3]| {
        cube(
            from[0],
            from[1],
            from[2],
            to[0],
            to[1],
            to[2],
            ["#particle"; 6],
        )
    };
    match path {
        "block/banner" => Some(vec![
            particle_cube([7.0, 0.0, 7.0], [9.0, 16.0, 9.0]),
            particle_cube([0.0, 8.0, 7.0], [16.0, 16.0, 9.0]),
        ]),
        "block/bed" => Some(vec![
            particle_cube([0.0, 3.0, 0.0], [16.0, 9.0, 16.0]),
            particle_cube([1.0, 0.0, 1.0], [4.0, 3.0, 4.0]),
            particle_cube([12.0, 0.0, 1.0], [15.0, 3.0, 4.0]),
            particle_cube([1.0, 0.0, 12.0], [4.0, 3.0, 15.0]),
            particle_cube([12.0, 0.0, 12.0], [15.0, 3.0, 15.0]),
        ]),
        "block/skull" => Some(vec![
            particle_cube([3.0, 3.0, 3.0], [13.0, 13.0, 13.0]),
            particle_cube([5.0, 1.0, 4.0], [11.0, 4.0, 12.0]),
        ]),
        "block/chest" | "block/ender_chest" | "block/trapped_chest" => Some(vec![
            particle_cube([1.0, 0.0, 1.0], [15.0, 11.0, 15.0]),
            particle_cube([1.0, 11.0, 1.0], [15.0, 15.0, 15.0]),
            particle_cube([7.0, 6.0, 0.0], [9.0, 9.0, 1.0]),
        ]),
        path if path.ends_with("shulker_box") => Some(vec![
            particle_cube([1.0, 0.0, 1.0], [15.0, 13.0, 15.0]),
            particle_cube([1.0, 13.0, 1.0], [15.0, 15.0, 15.0]),
        ]),
        path if path.ends_with("hanging_sign") => Some(vec![
            particle_cube([2.0, 4.0, 6.0], [14.0, 12.0, 10.0]),
            particle_cube([3.0, 12.0, 7.0], [5.0, 16.0, 9.0]),
            particle_cube([11.0, 12.0, 7.0], [13.0, 16.0, 9.0]),
        ]),
        path if path.ends_with("wall_sign") || path.ends_with("_sign") => Some(vec![
            particle_cube([0.0, 5.0, 7.0], [16.0, 15.0, 9.0]),
            particle_cube([7.0, 0.0, 7.0], [9.0, 8.0, 9.0]),
        ]),
        "block/water" | "block/lava" => {
            Some(vec![particle_cube([0.0, 0.0, 0.0], [16.0, 16.0, 16.0])])
        }
        "block/end_portal" | "block/end_gateway" => {
            Some(vec![particle_cube([0.0, 0.0, 7.0], [16.0, 16.0, 9.0])])
        }
        "block/conduit" => Some(vec![
            particle_cube([4.0, 4.0, 4.0], [12.0, 12.0, 12.0]),
            particle_cube([6.0, 0.0, 6.0], [10.0, 16.0, 10.0]),
            particle_cube([0.0, 6.0, 6.0], [16.0, 10.0, 10.0]),
            particle_cube([6.0, 6.0, 0.0], [10.0, 10.0, 16.0]),
        ]),
        "block/decorated_pot" => Some(vec![
            particle_cube([3.0, 0.0, 3.0], [13.0, 2.0, 13.0]),
            particle_cube([4.0, 2.0, 4.0], [12.0, 12.0, 12.0]),
            particle_cube([3.0, 12.0, 3.0], [13.0, 14.0, 13.0]),
            particle_cube([2.0, 14.0, 2.0], [14.0, 16.0, 14.0]),
        ]),
        "block/moving_piston" => Some(vec![
            particle_cube([5.0, 0.0, 5.0], [11.0, 16.0, 11.0]),
            particle_cube([0.0, 12.0, 0.0], [16.0, 16.0, 16.0]),
        ]),
        "block/lectern" => {
            // Vanillaの読書面はX軸22.5度で傾くため、簡略geometryでもそのシルエットを保つ。
            let mut reading_surface = particle_cube([0.0, 12.0, 3.0], [16.0, 16.0, 16.0]);
            reading_surface.rotation = Some(RawElementRotation {
                origin: [8.0, 8.0, 8.0],
                axis: FaceAxis::X,
                angle: -22.5,
                rescale: false,
            });
            Some(vec![
                particle_cube([0.0, 0.0, 0.0], [16.0, 2.0, 16.0]),
                particle_cube([4.0, 2.0, 4.0], [12.0, 14.0, 12.0]),
                reading_surface,
            ])
        }
        _ => None,
    }
}

pub(crate) fn particle_cube() -> RawElement {
    cube(0.0, 0.0, 0.0, 16.0, 16.0, 16.0, ["#particle"; 6])
}

fn synthetic_model_names() -> Vec<String> {
    let mut names = [
        "block/banner",
        "block/bed",
        "block/skull",
        "block/chest",
        "block/ender_chest",
        "block/trapped_chest",
        "block/shulker_box",
        "block/water",
        "block/lava",
        "block/end_portal",
        "block/end_gateway",
        "block/conduit",
        "block/decorated_pot",
        "block/moving_piston",
        "block/lectern",
    ]
    .map(str::to_owned)
    .to_vec();
    for wood in [
        "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry", "bamboo",
        "crimson", "warped", "pale_oak",
    ] {
        for form in ["sign", "hanging_sign", "wall_sign"] {
            names.push(format!("block/{wood}_{form}"));
        }
    }
    names
}

fn synthetic_block_model(path: &str) -> RawModel {
    RawModel {
        textures: aliases(&[("particle", "minecraft:block/oak_planks")]),
        elements: fallback_elements(path),
        ..RawModel::default()
    }
}

fn cube(x1: f64, y1: f64, z1: f64, x2: f64, y2: f64, z2: f64, textures: [&str; 6]) -> RawElement {
    let face_names = [
        FaceName::Down,
        FaceName::Up,
        FaceName::North,
        FaceName::South,
        FaceName::West,
        FaceName::East,
    ];
    let faces: BTreeMap<_, _> = face_names
        .into_iter()
        .zip(textures)
        .map(|(name, texture)| (name, raw_face(texture)))
        .collect();
    RawElement {
        from: [x1, y1, z1],
        to: [x2, y2, z2],
        rotation: None,
        shade: true,
        faces,
    }
}

fn crossed_plane(angle: f64, tinted: bool) -> RawElement {
    let mut element = cube(0.0, 0.0, 7.0, 16.0, 16.0, 9.0, ["#cross"; 6]);
    element.rotation = Some(RawElementRotation {
        origin: [8.0, 8.0, 8.0],
        axis: FaceAxis::Y,
        angle,
        rescale: false,
    });
    if tinted {
        for face in element.faces.values_mut() {
            face.tintindex = Some(0);
        }
    }
    element
}

fn crop_plane(coordinate: f64, x_plane: bool) -> RawElement {
    let mut element = if x_plane {
        cube(coordinate, -1.0, 0.0, coordinate, 15.0, 16.0, ["#crop"; 6])
    } else {
        cube(0.0, -1.0, coordinate, 16.0, 15.0, coordinate, ["#crop"; 6])
    };
    element.faces.retain(|face, _| {
        if x_plane {
            matches!(face, FaceName::East | FaceName::West)
        } else {
            matches!(face, FaceName::North | FaceName::South)
        }
    });
    element.shade = false;
    element
}

fn handheld_display() -> IndexMap<String, RawDisplayTransform> {
    let defaults = [
        (
            "gui",
            RawDisplayTransform {
                rotation: [15.0, -25.0, -5.0],
                translation: [0.0, 2.0, 0.0],
                scale: [0.83; 3],
            },
        ),
        (
            "ground",
            RawDisplayTransform {
                rotation: [0.0; 3],
                translation: [0.0, 3.0, 0.0],
                scale: [0.25; 3],
            },
        ),
        (
            "fixed",
            RawDisplayTransform {
                rotation: [0.0, 180.0, 0.0],
                translation: [0.0; 3],
                scale: [0.5; 3],
            },
        ),
        (
            "thirdperson_righthand",
            RawDisplayTransform {
                rotation: [0.0, -90.0, 55.0],
                translation: [0.0, 4.0, 0.5],
                scale: [0.85; 3],
            },
        ),
        (
            "thirdperson_lefthand",
            RawDisplayTransform {
                rotation: [0.0, -90.0, 55.0],
                translation: [0.0, 4.0, 0.5],
                scale: [0.85; 3],
            },
        ),
        (
            "firstperson_righthand",
            RawDisplayTransform {
                rotation: [0.0, -90.0, 25.0],
                translation: [1.13, 3.2, 1.13],
                scale: [0.68; 3],
            },
        ),
        (
            "firstperson_lefthand",
            RawDisplayTransform {
                rotation: [0.0, -90.0, 25.0],
                translation: [1.13, 3.2, 1.13],
                scale: [0.68; 3],
            },
        ),
    ];
    defaults
        .into_iter()
        .map(|(context, transform)| (context.to_owned(), transform))
        .collect()
}
