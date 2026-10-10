use std::{collections::BTreeMap, sync::Arc};

use serde_json::json;

use crate::{
    AssetPack, AssetResolver, AssetSource, LinkConfidence, PreviewManifest, PreviewOptions,
    ResolverError, SourceKind,
};

#[derive(Default)]
struct MemorySource(BTreeMap<String, Vec<u8>>);

impl AssetSource for MemorySource {
    fn paths(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.get(path).cloned())
    }
}

fn source(entries: &[(&str, &str)]) -> Arc<dyn AssetSource> {
    Arc::new(MemorySource(
        entries
            .iter()
            .map(|(path, value)| (path.to_string(), value.as_bytes().to_vec()))
            .collect(),
    ))
}

fn pack(id: &str, kind: SourceKind, entries: &[(&str, &str)]) -> AssetPack {
    AssetPack::new(id, kind, source(entries))
}

fn mod_resolver(entries: &[(&str, &str)]) -> AssetResolver {
    AssetResolver::new(vec![pack("mod", SourceKind::Mod, entries)])
}

fn face_model(texture: &str) -> String {
    format!(
        r##"{{"textures":{{"all":"{texture}"}},"elements":[{{"from":[1,2,3],"to":[10,11,12],"faces":{{"east":{{"texture":"#all","rotation":90,"cullface":"east","tintindex":2}}}}}}]}}"##
    )
}

#[test]
fn resolves_parent_textures_and_multi_level_texture_variables() {
    let resolver = mod_resolver(&[
        (
            "assets/example/models/block/base.json",
            r##"{"textures":{"root":"example:block/parent","side":"#root","face":"#side"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#face"}}}]}"##,
        ),
        (
            "assets/example/models/block/child.json",
            r##"{"parent":"example:block/base","textures":{"root":"example:block/child"}}"##,
        ),
    ]);
    let model = resolver.resolve_model("example:block/child").unwrap();
    assert_eq!(model.elements.len(), 1);
    assert_eq!(
        model.elements[0].faces[&crate::FaceName::North].texture,
        "example:block/child"
    );
    assert_eq!(model.textures["face"].location, "example:block/child");
}

#[test]
fn resolves_a_large_texture_variable_chain_without_recursive_stack_growth() {
    const VARIABLE_COUNT: usize = 16_384;
    let mut textures = serde_json::Map::new();
    for index in 0..VARIABLE_COUNT {
        let value = if index + 1 == VARIABLE_COUNT {
            "example:block/final".to_owned()
        } else {
            format!("#v{}", index + 1)
        };
        textures.insert(format!("v{index}"), json!(value));
    }
    let model_json = json!({"textures": textures}).to_string();
    assert!(model_json.len() > 190 * 1024);
    let mut files = BTreeMap::new();
    files.insert(
        "assets/example/models/block/deep.json".to_owned(),
        model_json.into_bytes(),
    );
    files.insert(
        "assets/example/textures/block/final.png".to_owned(),
        b"not-decoded".to_vec(),
    );
    let resolver = AssetResolver::new(vec![AssetPack::new(
        "mod",
        SourceKind::Mod,
        Arc::new(MemorySource(files)),
    )]);

    let model = resolver.resolve_model("example:block/deep").unwrap();

    assert_eq!(model.textures["v0"].location, "example:block/final");
    assert_eq!(
        model.textures[&format!("v{}", VARIABLE_COUNT - 1)].location,
        "example:block/final"
    );
    assert!(!model.textures["v0"].missing);
}

#[test]
fn cyclic_texture_variables_make_only_the_model_preview_unresolvable() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/cyclic.json",
        r##"{"textures":{"first":"#second","second":"#first"}}"##,
    )]);

    assert!(matches!(
        resolver.resolve_model("example:block/cyclic"),
        Err(ResolverError::TextureVariableCycle(_))
    ));
    assert!(matches!(
        resolver.resolve_preview("block.example.cyclic", None),
        PreviewManifest::NoPreview(_)
    ));
}

#[test]
fn resolves_textures_from_custom_directories() {
    let resolver = mod_resolver(&[
        (
            "assets/example/models/block/token.json",
            r##"{"textures":{"all":"example:custom/token"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#all"}}}]}"##,
        ),
        ("assets/example/textures/custom/token.png", "not-decoded"),
    ]);

    let model = resolver.resolve_model("example:block/token").unwrap();
    let face = &model.elements[0].faces[&crate::FaceName::North];

    assert_eq!(
        face.texture_path.as_ref().unwrap().archive_path,
        "assets/example/textures/custom/token.png"
    );
    assert!(!face.missing);
}

#[test]
fn detects_parent_cycles() {
    let resolver = mod_resolver(&[
        ("assets/example/models/a.json", r#"{"parent":"example:b"}"#),
        ("assets/example/models/b.json", r#"{"parent":"example:a"}"#),
    ]);
    assert!(matches!(
        resolver.resolve_model("example:a"),
        Err(ResolverError::ParentCycle(_))
    ));
}

#[test]
fn rejects_parent_chains_over_sixty_four_models() {
    let mut files = BTreeMap::new();
    for index in 0..65 {
        let body = if index < 64 {
            format!(r#"{{"parent":"example:model{}"}}"#, index + 1)
        } else {
            "{}".to_owned()
        };
        files.insert(
            format!("assets/example/models/model{index}.json"),
            body.into_bytes(),
        );
    }
    let resolver = AssetResolver::new(vec![AssetPack::new(
        "mod",
        SourceKind::Mod,
        Arc::new(MemorySource(files)),
    )]);
    assert!(matches!(
        resolver.resolve_model("example:model0"),
        Err(ResolverError::MaximumModelDepth)
    ));
}

#[test]
fn calculates_default_uv_and_preserves_face_rotation_and_element_rotation() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/test.json",
        r##"{"textures":{"all":"example:block/missing"},"elements":[{"from":[1,2,3],"to":[10,11,12],"rotation":{"origin":[8,8,8],"axis":"y","angle":22.5,"rescale":true},"shade":false,"faces":{"east":{"texture":"#all","rotation":90,"cullface":"east","tintindex":2}}}]}"##,
    )]);
    let model = resolver.resolve_model("example:block/test").unwrap();
    let element = &model.elements[0];
    assert_eq!(element.rotation.as_ref().unwrap().angle, 22.5);
    assert!(!element.shade);
    let face = &element.faces[&crate::FaceName::East];
    assert_eq!(face.uv, [4.0, 5.0, 13.0, 14.0]);
    assert_eq!(face.rotation, 90);
    assert_eq!(face.cullface, Some(crate::FaceName::East));
    assert_eq!(face.tintindex, Some(2));
}

#[test]
fn prefers_the_propertyless_variant_and_uses_highest_weight_with_path_tie_break() {
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/machine.json",
            r#"{"variants":{"facing=north":{"model":"example:block/side"},"": [{"model":"example:block/z","weight":4},{"model":"example:block/b","weight":8},{"model":"example:block/a","weight":8}]}}"#,
        ),
        ("assets/example/models/block/a.json", r#"{"elements":[]}"#),
        ("assets/example/models/block/b.json", r#"{"elements":[]}"#),
        (
            "assets/example/models/block/side.json",
            r#"{"elements":[]}"#,
        ),
        ("assets/example/models/block/z.json", r#"{"elements":[]}"#),
    ]);
    let PreviewManifest::Block(preview) = resolver.resolve_preview("block.example.machine", None)
    else {
        panic!("block preview expected");
    };
    assert_eq!(preview.selected_variant.as_deref(), Some("variant:"));
    assert_eq!(preview.models[0].model.model, "example:block/a");
    assert_eq!(preview.variants[0].models.len(), 3);
}

#[test]
fn resolves_multipart_cases_as_a_default_combination_and_exposes_selector_options() {
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/plant.json",
            r#"{"multipart":[{"when":{"north":"true"},"apply":{"model":"example:block/plant_a"}},{"apply":[{"model":"example:block/plant_c","weight":2},{"model":"example:block/plant_b","weight":5}]}]}"#,
        ),
        (
            "assets/example/models/block/plant_a.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/plant_b.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/plant_c.json",
            r#"{"elements":[]}"#,
        ),
    ]);
    let PreviewManifest::Block(preview) = resolver.resolve_preview("block.example.plant", None)
    else {
        panic!("block preview expected");
    };
    assert_eq!(preview.variants.len(), 2);
    assert_eq!(preview.models.len(), 2);
    assert_eq!(preview.models[1].model.model, "example:block/plant_b");
    assert_eq!(preview.variants[0].selector, "multipart[0]");
}

#[test]
fn evaluates_multipart_or_and_pipe_values_and_switches_blockstate_properties() {
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/fence.json",
            r#"{"multipart":[{"when":{"north":"true|false"},"apply":{"model":"example:block/part_0"}},{"when":{"OR":[{"north":"true"},{"east":"true"}]},"apply":{"model":"example:block/part_1"}},{"when":{"AND":[{"north":"false"},{"east":"false"}]},"apply":{"model":"example:block/part_2"}},{"when":{"connection":"none|up"},"apply":{"model":"example:block/part_3"}},{"when":{"connection":"up"},"apply":{"model":"example:block/part_4"}}]}"#,
        ),
        (
            "assets/example/models/block/part_0.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/part_1.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/part_2.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/part_3.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/part_4.json",
            r#"{"elements":[]}"#,
        ),
    ]);
    let PreviewManifest::Block(default) = resolver.resolve_preview("block.example.fence", None)
    else {
        panic!("block preview expected");
    };
    assert_eq!(default.selected_state["north"], "false");
    assert_eq!(default.selected_state["east"], "false");
    assert_eq!(default.selected_state["connection"], "none");
    assert_eq!(
        default
            .models
            .iter()
            .map(|model| model.model.model.as_str())
            .collect::<Vec<_>>(),
        [
            "example:block/part_0",
            "example:block/part_2",
            "example:block/part_3"
        ]
    );
    assert_eq!(
        default
            .properties
            .iter()
            .find(|property| property.name == "connection")
            .unwrap()
            .values,
        ["none", "up"]
    );

    let options = PreviewOptions {
        blockstate: BTreeMap::from([
            ("north".to_owned(), "true".to_owned()),
            ("east".to_owned(), "false".to_owned()),
            ("connection".to_owned(), "up".to_owned()),
        ]),
        ..PreviewOptions::default()
    };
    let PreviewManifest::Block(switched) =
        resolver.resolve_preview_with_options("block.example.fence", &options)
    else {
        panic!("block preview expected");
    };
    assert_eq!(switched.selected_state["north"], "true");
    assert_eq!(
        switched
            .models
            .iter()
            .map(|model| model.model.model.as_str())
            .collect::<Vec<_>>(),
        [
            "example:block/part_0",
            "example:block/part_1",
            "example:block/part_3",
            "example:block/part_4"
        ]
    );
}

#[test]
fn resolves_builtin_parent_geometry_and_legacy_generated_layers() {
    let resolver = mod_resolver(&[
        (
            "assets/example/models/block/machine.json",
            r##"{"parent":"minecraft:block/cube_all","textures":{"all":"example:block/machine"}}"##,
        ),
        (
            "assets/example/models/block/log.json",
            r##"{"parent":"minecraft:block/cube_column","textures":{"side":"example:block/log_side","end":"example:block/log_end"}}"##,
        ),
        (
            "assets/example/models/block/furnace.json",
            r##"{"parent":"minecraft:block/orientable","textures":{"side":"example:block/furnace_side","top":"example:block/furnace_top","front":"example:block/furnace_front"}}"##,
        ),
        (
            "assets/example/models/block/stairs.json",
            r##"{"parent":"minecraft:block/stairs","textures":{"side":"example:block/side","bottom":"example:block/bottom","top":"example:block/top"}}"##,
        ),
        (
            "assets/example/models/block/inner_stairs.json",
            r##"{"parent":"minecraft:block/inner_stairs","textures":{"side":"example:block/side","bottom":"example:block/bottom","top":"example:block/top"}}"##,
        ),
        (
            "assets/example/models/block/outer_stairs.json",
            r##"{"parent":"minecraft:block/outer_stairs","textures":{"side":"example:block/side","bottom":"example:block/bottom","top":"example:block/top"}}"##,
        ),
        (
            "assets/example/models/item/token.json",
            r##"{"parent":"minecraft:item/generated","textures":{"layer0":"example:item/token"}}"##,
        ),
        ("assets/example/textures/item/token.png", "not-decoded"),
    ]);
    for model in [
        "example:block/machine",
        "example:block/log",
        "example:block/furnace",
    ] {
        assert_eq!(resolver.resolve_model(model).unwrap().elements.len(), 1);
    }
    let stairs = resolver.resolve_model("example:block/stairs").unwrap();
    let inner = resolver
        .resolve_model("example:block/inner_stairs")
        .unwrap();
    let outer = resolver
        .resolve_model("example:block/outer_stairs")
        .unwrap();
    assert_eq!(stairs.elements.len(), 2);
    assert_eq!(inner.elements.len(), 3);
    assert_eq!(outer.elements[1].to[0], 8.0);
    let PreviewManifest::Item(item) = resolver.resolve_preview("item.example.token", None) else {
        panic!("item preview expected");
    };
    assert!(item.model.generated_item);
    assert_eq!(item.layers.len(), 1);
    assert!(!item.layers[0].texture.missing);
}

#[test]
fn serializes_builtin_parent_shapes_for_common_blocks() {
    let resolver = mod_resolver(&[
        (
            "assets/example/models/block/slab.json",
            r##"{"parent":"minecraft:block/slab","textures":{"side":"example:block/side","bottom":"example:block/bottom","top":"example:block/top"}}"##,
        ),
        (
            "assets/example/models/block/slab_top.json",
            r##"{"parent":"minecraft:block/slab_top","textures":{"side":"example:block/side","bottom":"example:block/bottom","top":"example:block/top"}}"##,
        ),
        (
            "assets/example/models/block/fence_post.json",
            r##"{"parent":"minecraft:block/fence_post","textures":{"texture":"example:block/fence"}}"##,
        ),
        (
            "assets/example/models/block/fence_side.json",
            r##"{"parent":"minecraft:block/fence_side","textures":{"texture":"example:block/fence"}}"##,
        ),
        (
            "assets/example/models/block/wall_post.json",
            r##"{"parent":"minecraft:block/wall_post","textures":{"wall":"example:block/wall"}}"##,
        ),
        (
            "assets/example/models/block/template_wall_post.json",
            r##"{"parent":"minecraft:block/template_wall_post","textures":{"wall":"example:block/wall"}}"##,
        ),
        (
            "assets/example/models/block/template_glass_pane_post.json",
            r##"{"parent":"minecraft:block/template_glass_pane_post","textures":{"pane":"example:block/glass"}}"##,
        ),
        (
            "assets/example/models/block/template_glass_pane_noside.json",
            r##"{"parent":"minecraft:block/template_glass_pane_noside","textures":{"pane":"example:block/glass"}}"##,
        ),
        (
            "assets/example/models/block/pane_side.json",
            r##"{"parent":"minecraft:block/pane_side","textures":{"pane":"example:block/pane"}}"##,
        ),
        (
            "assets/example/models/block/thin_child.json",
            r##"{"parent":"minecraft:block/thin_block","elements":[{"from":[0,0,0],"to":[16,2,16],"faces":{"up":{"texture":"example:block/plate"}}}]}"##,
        ),
        (
            "assets/example/models/block/door.json",
            r##"{"parent":"minecraft:block/door_bottom","textures":{"bottom":"example:block/door_bottom","top":"example:block/door_top"}}"##,
        ),
        (
            "assets/example/models/block/trapdoor.json",
            r##"{"parent":"minecraft:block/trapdoor_open","textures":{"texture":"example:block/trapdoor"}}"##,
        ),
        (
            "assets/example/models/block/crop.json",
            r##"{"parent":"minecraft:block/crop","textures":{"crop":"example:block/crop"}}"##,
        ),
        (
            "assets/example/models/block/cross.json",
            r##"{"parent":"minecraft:block/cross","textures":{"cross":"example:block/plant"}}"##,
        ),
        (
            "assets/example/models/block/carpet.json",
            r##"{"parent":"minecraft:block/carpet","textures":{"wool":"example:block/carpet"}}"##,
        ),
        (
            "assets/example/models/block/bed.json",
            r##"{"parent":"minecraft:block/bed"}"##,
        ),
    ]);

    let snapshots = [
        ("example:block/slab", 1, [0.0, 0.0, 0.0], [16.0, 8.0, 16.0]),
        (
            "example:block/slab_top",
            1,
            [0.0, 8.0, 0.0],
            [16.0, 16.0, 16.0],
        ),
        (
            "example:block/fence_post",
            1,
            [6.0, 0.0, 6.0],
            [10.0, 16.0, 10.0],
        ),
        (
            "example:block/fence_side",
            1,
            [7.0, 6.0, 0.0],
            [9.0, 15.0, 8.0],
        ),
        (
            "example:block/wall_post",
            1,
            [4.0, 0.0, 4.0],
            [12.0, 16.0, 12.0],
        ),
        (
            "example:block/template_wall_post",
            1,
            [4.0, 0.0, 4.0],
            [12.0, 16.0, 12.0],
        ),
        (
            "example:block/template_glass_pane_post",
            1,
            [7.0, 0.0, 7.0],
            [9.0, 16.0, 9.0],
        ),
        (
            "example:block/template_glass_pane_noside",
            1,
            [7.0, 0.0, 7.0],
            [9.0, 16.0, 9.0],
        ),
        (
            "example:block/pane_side",
            1,
            [7.0, 0.0, 0.0],
            [9.0, 16.0, 8.0],
        ),
        ("example:block/door", 1, [0.0, 0.0, 0.0], [3.0, 16.0, 16.0]),
        (
            "example:block/trapdoor",
            1,
            [0.0, 0.0, 0.0],
            [16.0, 16.0, 3.0],
        ),
        ("example:block/cross", 2, [0.0, 0.0, 7.0], [16.0, 16.0, 9.0]),
        (
            "example:block/carpet",
            1,
            [0.0, 0.0, 0.0],
            [16.0, 1.0, 16.0],
        ),
        ("example:block/bed", 5, [0.0, 3.0, 0.0], [16.0, 9.0, 16.0]),
    ];

    for (model_name, element_count, first_from, first_to) in snapshots {
        let model = resolver.resolve_model(model_name).unwrap();
        let snapshot = serde_json::to_value(&model.elements).unwrap();
        assert_eq!(model.elements.len(), element_count, "{model_name}");
        assert_eq!(snapshot[0]["from"], json!(first_from), "{model_name}");
        assert_eq!(snapshot[0]["to"], json!(first_to), "{model_name}");
        assert_eq!(
            snapshot[0]["faces"].as_object().unwrap().len(),
            6,
            "{model_name}"
        );
    }
    let thin_child = resolver.resolve_model("example:block/thin_child").unwrap();
    assert_eq!(thin_child.elements.len(), 1);
    assert_eq!(thin_child.elements[0].to, [16.0, 2.0, 16.0]);
    let crop = resolver.resolve_model("example:block/crop").unwrap();
    assert_eq!(crop.elements.len(), 4);
    assert_eq!(crop.elements[0].from, [4.0, -1.0, 0.0]);
    assert_eq!(crop.elements[0].to, [4.0, 15.0, 16.0]);
    assert_eq!(crop.elements[0].faces.len(), 2);
}

#[test]
fn chooses_wall_post_and_unconnected_multipart_default_state() {
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/wall.json",
            r#"{"multipart":[{"when":{"up":"true"},"apply":{"model":"example:block/post"}},{"when":{"up":"false"},"apply":{"model":"example:block/no_post"}},{"when":{"north":"false"},"apply":{"model":"example:block/north_disconnected"}},{"when":{"north":"true"},"apply":{"model":"example:block/north_connected"}}]}"#,
        ),
        (
            "assets/example/models/block/post.json",
            r#"{"elements":[{"from":[4,0,4],"to":[12,16,12],"faces":{"up":{"texture":"example:block/wall"}}}]}"#,
        ),
        (
            "assets/example/models/block/no_post.json",
            r#"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"example:block/wall"}}}]}"#,
        ),
        (
            "assets/example/models/block/north_disconnected.json",
            r#"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"example:block/wall"}}}]}"#,
        ),
        (
            "assets/example/models/block/north_connected.json",
            r#"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"example:block/wall"}}}]}"#,
        ),
    ]);
    let PreviewManifest::Block(preview) = resolver.resolve_preview("block.example.wall", None)
    else {
        panic!("wall preview expected");
    };

    assert_eq!(preview.selected_state["up"], "true");
    assert_eq!(preview.selected_state["north"], "false");
    assert_eq!(
        preview
            .models
            .iter()
            .map(|model| model.model.model.as_str())
            .collect::<Vec<_>>(),
        ["example:block/post", "example:block/north_disconnected"]
    );
}

#[test]
fn preserves_blockstate_quarter_turns_and_uvlock_in_preview_ir() {
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/rotated.json",
            r#"{"variants":{"":{"model":"example:block/rotated","x":90,"y":270,"uvlock":true}}}"#,
        ),
        (
            "assets/example/models/block/rotated.json",
            r#"{"elements":[{"from":[1,2,3],"to":[13,14,15],"rotation":{"origin":[8,8,8],"axis":"z","angle":22.5,"rescale":true},"faces":{"north":{"texture":"example:block/texture","rotation":90}}}]}"#,
        ),
    ]);
    let PreviewManifest::Block(preview) = resolver.resolve_preview("block.example.rotated", None)
    else {
        panic!("block preview expected");
    };
    let block_model = &preview.models[0];
    assert_eq!(block_model.x_rotation, 90);
    assert_eq!(block_model.y_rotation, 270);
    assert!(block_model.uvlock);
    let element = &block_model.model.elements[0];
    assert_eq!(element.rotation.as_ref().unwrap().angle, 22.5);
    assert!(element.rotation.as_ref().unwrap().rescale);
    assert_eq!(element.faces[&crate::FaceName::North].rotation, 90);
}

#[test]
fn keeps_custom_loader_reason_while_rendering_a_particle_placeholder() {
    let resolver = mod_resolver(&[(
        "assets/create/models/block/track.json",
        r##"{"loader":"neoforge:obj","model":"create:models/block/track.obj","textures":{"0":"create:block/track","particle":"create:block/track"}}"##,
    )]);
    let model = resolver.resolve_model("create:block/track").unwrap();

    assert_eq!(model.elements.len(), 1);
    assert_eq!(
        model.unsupported_features,
        ["unsupported model loader: neoforge:obj"]
    );
    assert_eq!(
        model.elements[0].faces[&crate::FaceName::North].texture,
        "create:block/track"
    );
}

#[test]
fn gives_texture_only_mod_models_a_noted_particle_cube_preview() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/sign.json",
        r#"{"textures":{"particle":"example:block/sign"}}"#,
    )]);
    let model = resolver.resolve_model("example:block/sign").unwrap();

    assert_eq!(model.elements.len(), 1);
    assert_eq!(
        model.unsupported_features,
        ["model has no elements; displaying its particle texture on a cube"]
    );
}

#[test]
fn resolves_every_modern_item_definition_node_and_exposes_branch_options() {
    let resolver = mod_resolver(&[
        (
            "assets/example/items/model.json",
            r#"{"model":{"type":"minecraft:model","model":"example:item/modern","tints":[{"type":"minecraft:constant","value":16711680}]}}"#,
        ),
        (
            "assets/example/items/composite.json",
            r#"{"model":{"type":"minecraft:composite","children":[{"type":"minecraft:model","model":"example:item/first"},{"type":"minecraft:model","model":"example:item/second"}]}}"#,
        ),
        (
            "assets/example/items/condition.json",
            r#"{"model":{"type":"minecraft:condition","property":"minecraft:using_item","on_true":{"type":"minecraft:model","model":"example:item/true"},"on_false":{"type":"minecraft:model","model":"example:item/false"}}}"#,
        ),
        (
            "assets/example/items/select.json",
            r#"{"model":{"type":"minecraft:select","property":"minecraft:display_context","cases":[{"when":"gui","model":{"type":"minecraft:model","model":"example:item/gui"}},{"when":"ground","model":{"type":"minecraft:model","model":"example:item/ground"}}],"fallback":{"type":"minecraft:model","model":"example:item/fallback"}}}"#,
        ),
        (
            "assets/example/items/select_first.json",
            r#"{"model":{"type":"minecraft:select","property":"minecraft:display_context","cases":[{"when":"gui","model":{"type":"minecraft:model","model":"example:item/gui"}},{"when":"ground","model":{"type":"minecraft:model","model":"example:item/ground"}}]}}"#,
        ),
        (
            "assets/example/items/range.json",
            r#"{"model":{"type":"minecraft:range_dispatch","property":"minecraft:damage","entries":[{"threshold":1.0,"model":{"type":"minecraft:model","model":"example:item/damaged"}},{"threshold":0.0,"model":{"type":"minecraft:model","model":"example:item/intact"}}],"fallback":{"type":"minecraft:model","model":"example:item/fallback"}}}"#,
        ),
        (
            "assets/example/items/empty.json",
            r#"{"model":{"type":"minecraft:empty"}}"#,
        ),
        (
            "assets/example/items/direct.json",
            r#"{"type":"minecraft:empty"}"#,
        ),
        (
            "assets/example/items/special.json",
            r#"{"model":{"type":"minecraft:special","base":"example:item/template","model":{"type":"minecraft:head","kind":"player"}}}"#,
        ),
        (
            "assets/example/items/static_special.json",
            r#"{"model":{"type":"minecraft:special","base":"example:item/static","model":{"type":"minecraft:head","kind":"player"}}}"#,
        ),
        (
            "assets/example/items/bundle.json",
            r#"{"model":{"type":"minecraft:bundle/selected_item"}}"#,
        ),
        (
            "assets/example/models/item/modern.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/first.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/second.json",
            r#"{"elements":[]}"#,
        ),
        ("assets/example/models/item/true.json", r#"{"elements":[]}"#),
        (
            "assets/example/models/item/false.json",
            r#"{"elements":[]}"#,
        ),
        ("assets/example/models/item/gui.json", r#"{"elements":[]}"#),
        (
            "assets/example/models/item/ground.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/fallback.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/damaged.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/intact.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/item/template.json",
            r#"{"textures":{"layer0":"example:item/template"},"elements":[]}"#,
        ),
        (
            "assets/example/models/item/static.json",
            r##"{"textures":{"particle":"example:item/static"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#particle"}}}]}"##,
        ),
        ("assets/example/textures/item/template.png", "not-decoded"),
        ("assets/example/textures/item/bundle.png", "not-decoded"),
    ]);

    let PreviewManifest::Item(model) = resolver.resolve_preview("item.example.model", None) else {
        panic!("modern model definition expected");
    };
    assert_eq!(model.model.model, "example:item/modern");
    assert_eq!(model.models.len(), 1);
    let Some(crate::ResolvedItemDefinition::Model { model_index, tints }) = model.definition else {
        panic!("resolved model node expected");
    };
    assert_eq!(model_index, 0);
    assert_eq!(
        tints,
        [json!({"type":"minecraft:constant","value":16711680})]
    );

    let PreviewManifest::Item(composite) = resolver.resolve_preview("item.example.composite", None)
    else {
        panic!("composite definition expected");
    };
    assert_eq!(composite.models.len(), 2);

    let PreviewManifest::Item(condition) = resolver.resolve_preview("item.example.condition", None)
    else {
        panic!("condition definition expected");
    };
    assert_eq!(condition.variants[0].selected, "false");
    assert_eq!(condition.model.model, "example:item/false");
    let options = PreviewOptions {
        item: BTreeMap::from([("root".to_owned(), "true".to_owned())]),
        ..PreviewOptions::default()
    };
    let PreviewManifest::Item(condition_true) =
        resolver.resolve_preview_with_options("item.example.condition", &options)
    else {
        panic!("condition definition expected");
    };
    assert_eq!(condition_true.variants[0].selected, "true");
    assert_eq!(condition_true.model.model, "example:item/true");

    let PreviewManifest::Item(select) = resolver.resolve_preview("item.example.select", None)
    else {
        panic!("select definition expected");
    };
    assert_eq!(select.variants[0].selected, "fallback");
    assert_eq!(select.model.model, "example:item/fallback");
    let options = PreviewOptions {
        item: BTreeMap::from([("root".to_owned(), "case-1".to_owned())]),
        ..PreviewOptions::default()
    };
    let PreviewManifest::Item(select_case) =
        resolver.resolve_preview_with_options("item.example.select", &options)
    else {
        panic!("select definition expected");
    };
    assert_eq!(select_case.model.model, "example:item/ground");
    let PreviewManifest::Item(select_first) =
        resolver.resolve_preview("item.example.select_first", None)
    else {
        panic!("select definition expected");
    };
    assert_eq!(select_first.model.model, "example:item/gui");

    let PreviewManifest::Item(range) = resolver.resolve_preview("item.example.range", None) else {
        panic!("range dispatch definition expected");
    };
    assert_eq!(range.variants[0].selected, "entry-1");
    assert_eq!(range.model.model, "example:item/intact");
    let options = PreviewOptions {
        item: BTreeMap::from([("root".to_owned(), "entry-0".to_owned())]),
        ..PreviewOptions::default()
    };
    let PreviewManifest::Item(range_high) =
        resolver.resolve_preview_with_options("item.example.range", &options)
    else {
        panic!("range dispatch definition expected");
    };
    assert_eq!(range_high.model.model, "example:item/damaged");

    let PreviewManifest::Item(empty) = resolver.resolve_preview("item.example.empty", None) else {
        panic!("empty definition expected");
    };
    assert!(empty.models.is_empty());
    assert_eq!(empty.definition, Some(crate::ResolvedItemDefinition::Empty));
    let PreviewManifest::Item(direct) = resolver.resolve_preview("item.example.direct", None)
    else {
        panic!("direct item definition node expected");
    };
    assert_eq!(
        direct.definition,
        Some(crate::ResolvedItemDefinition::Empty)
    );

    let PreviewManifest::Texture(special) = resolver.resolve_preview("item.example.special", None)
    else {
        panic!("special renderer should fall back to texture preview");
    };
    assert!(special.candidates.iter().any(
        |candidate| candidate.asset.archive_path == "assets/example/textures/item/template.png"
    ));
    let PreviewManifest::Item(static_special) =
        resolver.resolve_preview("item.example.static_special", None)
    else {
        panic!("special renderer with static base should use a 3D preview");
    };
    assert_eq!(static_special.models[0].elements.len(), 1);
    assert!(
        static_special.models[0]
            .unsupported_features
            .iter()
            .any(|feature| { feature.contains("special item renderer minecraft:head") })
    );
    let PreviewManifest::Texture(bundle) = resolver.resolve_preview("item.example.bundle", None)
    else {
        panic!("selected bundle item should fall back to texture preview");
    };
    assert!(
        bundle
            .candidates
            .iter()
            .any(|candidate| candidate.asset.archive_path
                == "assets/example/textures/item/bundle.png")
    );
}

#[test]
fn modern_item_definition_takes_precedence_over_legacy_item_model() {
    let resolver = mod_resolver(&[
        (
            "assets/example/items/token.json",
            r#"{"model":{"type":"minecraft:model","model":"example:item/modern"}}"#,
        ),
        (
            "assets/example/models/item/token.json",
            r#"{"parent":"minecraft:item/generated","textures":{"layer0":"example:item/legacy"}}"#,
        ),
        (
            "assets/example/models/item/modern.json",
            r#"{"elements":[]}"#,
        ),
    ]);
    let PreviewManifest::Item(item) = resolver.resolve_preview("item.example.token", None) else {
        panic!("item preview expected");
    };
    assert_eq!(item.model.model, "example:item/modern");
    assert!(item.definition.is_some());
    assert!(item.layers.is_empty());
}

#[test]
fn resolves_geckolib_geometry_and_infers_entity_texture_with_confidence() {
    let resolver = mod_resolver(&[
        (
            "assets/example/geo/machine.geo.json",
            r##"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.example.machine","texture_width":64,"texture_height":32},"bones":[{"name":"root","pivot":[0,0,0],"rotation":[0,10,0],"cubes":[{"origin":[-2,0,-2],"size":[4,8,4],"pivot":[0,4,0],"rotation":[0,0,5],"uv":[0,0],"inflate":0.25,"mirror":true}]},{"name":"head","parent":"root","pivot":[0,8,0],"rotation":[10,0,0],"cubes":[{"origin":[-1,8,-1],"size":[2,2,2],"uv":{"north":{"uv":[0,0],"uv_size":[2,2]},"up":{"uv":[2,0],"uv_size":[2,2]}}}]}]}]}"##,
        ),
        ("assets/example/textures/entity/machine.png", "not-decoded"),
    ]);
    let PreviewManifest::Entity(entity) = resolver.resolve_preview("entity.example.machine", None)
    else {
        panic!("GeckoLib entity preview expected");
    };
    assert_eq!(entity.geometry_source.resource_id, "machine");
    assert_eq!(
        entity.geometry_source.archive_path,
        "assets/example/geo/machine.geo.json"
    );
    assert_eq!(entity.confidence, LinkConfidence::High);
    assert_eq!(entity.geometry.identifier, "geometry.example.machine");
    assert_eq!(entity.geometry.format_version, "1.12.0");
    assert_eq!(entity.geometry.texture_width, Some(64));
    assert_eq!(entity.geometry.texture_height, Some(32));
    assert_eq!(entity.geometry.bones[1].parent.as_deref(), Some("root"));
    assert_eq!(entity.geometry.bones[0].rotation, Some([0.0, 10.0, 0.0]));
    let box_cube = &entity.geometry.bones[0].cubes[0];
    assert_eq!(box_cube.pivot, Some([0.0, 4.0, 0.0]));
    assert_eq!(box_cube.rotation, Some([0.0, 0.0, 5.0]));
    assert_eq!(box_cube.inflate, 0.25);
    assert!(box_cube.mirror);
    assert_eq!(box_cube.uv, Some(crate::EntityCubeUv::Box([0.0, 0.0])));
    let face_cube = &entity.geometry.bones[1].cubes[0];
    let Some(crate::EntityCubeUv::PerFace(faces)) = &face_cube.uv else {
        panic!("per-face cube UV expected");
    };
    assert_eq!(faces[&crate::FaceName::North].uv, [0.0, 0.0]);
    assert_eq!(faces[&crate::FaceName::North].uv_size, [2.0, 2.0]);
    assert_eq!(entity.texture_confidence, Some(LinkConfidence::High));
    assert_eq!(
        entity
            .texture
            .as_ref()
            .unwrap()
            .path
            .as_ref()
            .unwrap()
            .archive_path,
        "assets/example/textures/entity/machine.png"
    );
}

#[test]
fn resolves_legacy_geckolib_geometry_identifier_and_texture_dimensions() {
    let resolver = mod_resolver(&[(
        "assets/example/geo/legacy.geo.json",
        r##"{"format_version":"1.8.0","geometry.example.legacy":{"texturewidth":32,"textureheight":16,"bones":[{"name":"root","pivot":[1,2,3],"cubes":[{"origin":[0,0,0],"size":[4,4,4],"uv":[2,3]}]}]}}"##,
    )]);
    let PreviewManifest::Entity(entity) = resolver.resolve_preview("entity.example.legacy", None)
    else {
        panic!("legacy GeckoLib entity preview expected");
    };
    assert_eq!(entity.geometry_source.resource_id, "legacy");
    assert_eq!(entity.geometry.identifier, "geometry.example.legacy");
    assert_eq!(entity.geometry.format_version, "1.8.0");
    assert_eq!(entity.geometry.texture_width, Some(32));
    assert_eq!(entity.geometry.texture_height, Some(16));
    assert_eq!(
        entity.geometry.bones[0].cubes[0].uv,
        Some(crate::EntityCubeUv::Box([2.0, 3.0]))
    );
}

#[test]
fn entity_preview_falls_back_from_geometry_to_entity_textures_and_candidates() {
    let exact_texture = mod_resolver(&[("assets/example/textures/entity/cow.png", "not-decoded")]);
    let PreviewManifest::Texture(texture) =
        exact_texture.resolve_preview("entity.example.cow", None)
    else {
        panic!("entity texture fallback expected");
    };
    assert_eq!(texture.candidates.len(), 1);
    assert_eq!(texture.candidates[0].confidence, LinkConfidence::High);

    let inferred_texture = mod_resolver(&[(
        "assets/example/textures/entity/press_machine.png",
        "not-decoded",
    )]);
    let PreviewManifest::Texture(candidates) =
        inferred_texture.resolve_preview("entity.example.field_machine", None)
    else {
        panic!("low-confidence texture candidate preview expected");
    };
    assert_eq!(candidates.candidates.len(), 1);
    assert_eq!(candidates.candidates[0].confidence, LinkConfidence::Low);
    assert!(!candidates.candidates[0].confirmed);

    let no_assets = AssetResolver::new(Vec::new());
    assert!(matches!(
        no_assets.resolve_preview("entity.example.ghost", None),
        PreviewManifest::NoPreview(_)
    ));
}

#[test]
fn serializes_entity_preview_ir_as_a_stable_snapshot() {
    let resolver = mod_resolver(&[(
        "assets/example/geo/machine.geo.json",
        r##"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.machine"},"bones":[{"name":"root","pivot":[0,0,0],"cubes":[{"origin":[0,0,0],"size":[1,1,1],"uv":[0,0]}]}]}]}"##,
    )]);
    let preview = resolver.resolve_preview("entity.example.machine", None);
    assert_eq!(
        serde_json::to_value(preview).unwrap(),
        json!({
            "kind":"entity",
            "manifest":{
                "translationKey":"entity.example.machine",
                "geometry":{
                    "identifier":"geometry.machine",
                    "formatVersion":"1.12.0",
                    "textureWidth":null,
                    "textureHeight":null,
                    "bones":[{
                        "name":"root",
                        "pivot":[0.0,0.0,0.0],
                        "cubes":[{
                            "origin":[0.0,0.0,0.0],
                            "size":[1.0,1.0,1.0],
                            "uv":{"kind":"box","value":[0.0,0.0]},
                            "inflate":0.0,
                            "mirror":false
                        }]
                    }]
                },
                "geometrySource":{
                    "sourceId":"mod",
                    "sourceKind":"mod",
                    "archivePath":"assets/example/geo/machine.geo.json",
                    "namespace":"example",
                    "resourceId":"machine",
                    "assetType":"geo_model"
                },
                "confidence":"high",
                "textureCandidates":[]
            }
        })
    );
}

#[test]
fn mod_models_override_minecraft_jar_and_builtin_models() {
    let resolver = AssetResolver::new(vec![
        pack(
            "minecraft-jar",
            SourceKind::Minecraft,
            &[(
                "assets/minecraft/models/block/cube_all.json",
                r#"{"elements":[]}"#,
            )],
        ),
        pack(
            "mod",
            SourceKind::Mod,
            &[(
                "assets/minecraft/models/block/cube_all.json",
                r#"{"elements":[{"from":[2,2,2],"to":[14,14,14],"faces":{}}]}"#,
            )],
        ),
    ]);
    let model = resolver.resolve_model("minecraft:block/cube_all").unwrap();
    assert_eq!(model.source.as_ref().unwrap().source_id, "mod");
    assert_eq!(model.elements[0].from, [2.0, 2.0, 2.0]);
}

#[test]
fn marks_missing_textures_and_reports_every_searched_source() {
    let resolver = AssetResolver::new(vec![
        pack(
            "minecraft-jar",
            SourceKind::Minecraft,
            &[(
                "assets/minecraft/models/block/test.json",
                face_model("minecraft:block/absent").as_str(),
            )],
        ),
        pack(
            "mod",
            SourceKind::Mod,
            &[(
                "assets/minecraft/models/block/test.json",
                face_model("minecraft:block/absent").as_str(),
            )],
        ),
    ]);
    let model = resolver.resolve_model("minecraft:block/test").unwrap();
    let face = &model.elements[0].faces[&crate::FaceName::East];
    assert!(face.missing);
    assert_eq!(
        face.searched_sources,
        ["mod", "minecraft-jar", "yc-assets:builtin"]
    );
}

#[test]
fn assigns_high_medium_and_low_link_confidence() {
    let resolver = mod_resolver(&[
        ("assets/example/blockstates/gear_box.json", "{}"),
        ("assets/example/models/block/sub/gear_box.json", "{}"),
        ("assets/example/models/block/gear.json", "{}"),
    ]);
    let links = resolver.links_for_translation_key("block.example.gear_box");
    assert_eq!(links[0].confidence, LinkConfidence::High);
    assert!(links[0].confirmed);
    assert_eq!(links[1].confidence, LinkConfidence::Medium);
    assert_eq!(links[2].confidence, LinkConfidence::Low);
    assert!(!links[2].confirmed);
}

#[test]
fn serializes_resolved_model_ir_as_a_stable_snapshot() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/cube.json",
        r#"{"elements":[]}"#,
    )]);
    let model = resolver.resolve_model("example:block/cube").unwrap();
    assert_eq!(
        serde_json::to_string(&model).unwrap(),
        r#"{"model":"example:block/cube","source":{"sourceId":"mod","sourceKind":"mod","archivePath":"assets/example/models/block/cube.json","namespace":"example","resourceId":"block/cube","assetType":"block_model"},"sourceChain":[{"sourceId":"mod","sourceKind":"mod","archivePath":"assets/example/models/block/cube.json","namespace":"example","resourceId":"block/cube","assetType":"block_model"}],"textures":{},"elements":[],"ambientOcclusion":true,"display":{},"guiLight":null,"generatedItem":false}"#
    );
}

#[test]
fn texture_fallback_keeps_candidate_paths_and_raw_model_json() {
    let resolver = mod_resolver(&[
        (
            "assets/example/models/block/machine.json",
            r##"{"parent":"minecraft:builtin/entity","textures":{"layer0":"example:block/machine"}}"##,
        ),
        ("assets/example/textures/block/machine.png", "not-decoded"),
    ]);
    let PreviewManifest::Texture(preview) = resolver.resolve_preview("block.example.machine", None)
    else {
        panic!("texture fallback expected");
    };
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(
        preview.reason.as_deref(),
        Some("model requires an unsupported Java renderer: minecraft:builtin/entity")
    );
    assert_eq!(
        preview.candidates[0].asset.archive_path,
        "assets/example/textures/block/machine.png"
    );
    assert_eq!(
        preview.raw_model_info,
        Some(
            json!({"parent":"minecraft:builtin/entity","textures":{"layer0":"example:block/machine"}})
        )
    );
}

#[test]
fn unresolved_model_preview_exposes_the_raw_model_file_path() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/machine.json",
        r#"{"parent":"example:missing","custom":true}"#,
    )]);
    let PreviewManifest::NoPreview(preview) =
        resolver.resolve_preview("block.example.machine", None)
    else {
        panic!("unresolved model should use the no-preview manifest");
    };

    assert_eq!(
        preview.raw_model_path.as_deref(),
        Some("assets/example/models/block/machine.json")
    );
    assert_eq!(
        preview.raw_model_info,
        Some(json!({"parent":"example:missing","custom":true}))
    );
}

#[test]
fn builtin_entity_remains_unresolved_even_if_a_model_json_is_present() {
    let resolver = AssetResolver::new(vec![
        pack(
            "minecraft-jar",
            SourceKind::Minecraft,
            &[(
                "assets/minecraft/models/builtin/entity.json",
                r#"{"elements":[]}"#,
            )],
        ),
        pack(
            "mod",
            SourceKind::Mod,
            &[
                (
                    "assets/example/models/block/machine.json",
                    r##"{"parent":"minecraft:builtin/entity","textures":{"layer0":"example:block/machine"}}"##,
                ),
                ("assets/example/textures/block/machine.png", "not-decoded"),
            ],
        ),
    ]);
    let PreviewManifest::Texture(preview) = resolver.resolve_preview("block.example.machine", None)
    else {
        panic!("renderer fallback expected");
    };
    assert_eq!(preview.candidates.len(), 1);
    assert_eq!(
        preview.raw_model_info,
        Some(
            json!({"parent":"minecraft:builtin/entity","textures":{"layer0":"example:block/machine"}})
        )
    );
}

#[test]
fn default_blockstate_variant_prefers_the_natural_standalone_shape() {
    // 名前順では inner_left が先頭になるが、プレビューの既定は単独設置の straight / bottom にする。
    let resolver = mod_resolver(&[
        (
            "assets/example/blockstates/steps.json",
            r#"{"variants":{
                "facing=east,half=bottom,shape=inner_left":{"model":"example:block/steps_inner"},
                "facing=east,half=bottom,shape=straight":{"model":"example:block/steps"},
                "facing=east,half=top,shape=straight":{"model":"example:block/steps_top","x":180},
                "facing=north,half=bottom,shape=straight":{"model":"example:block/steps","y":270},
                "facing=north,half=bottom,shape=outer_left":{"model":"example:block/steps_outer","y":270}
            }}"#,
        ),
        (
            "assets/example/models/block/steps.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/steps_inner.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/steps_outer.json",
            r#"{"elements":[]}"#,
        ),
        (
            "assets/example/models/block/steps_top.json",
            r#"{"elements":[]}"#,
        ),
    ]);
    let PreviewManifest::Block(preview) = resolver.resolve_preview("block.example.steps", None)
    else {
        panic!("block preview expected");
    };
    assert_eq!(preview.models[0].model.model, "example:block/steps");
    assert_eq!(preview.selected_state["shape"], "straight");
    assert_eq!(preview.selected_state["half"], "bottom");
    // 回転なしの variant（モデル本来の向き）を facing の名前より優先する。
    assert_eq!(preview.selected_state["facing"], "east");
    assert_eq!(preview.models[0].y_rotation, 0);
}

#[test]
fn resolves_template_trapdoor_parents_without_a_minecraft_jar() {
    let resolver = mod_resolver(&[(
        "assets/example/models/block/hatch_bottom.json",
        r#"{"parent":"minecraft:block/template_trapdoor_bottom","textures":{"texture":"example:block/hatch"}}"#,
    )]);
    let model = resolver
        .resolve_model("example:block/hatch_bottom")
        .unwrap();
    assert_eq!(model.elements.len(), 1);
    assert_eq!(model.elements[0].to[1], 3.0);
}
