use super::values::expected_type;
use super::*;
use clap::FromArgMatches;

fn parse(args: &[&str]) -> (Cli, ArgMatches) {
    let matches = crate::cli::command()
        .try_get_matches_from(std::iter::once("anytopdf").chain(args.iter().copied()))
        .unwrap();
    (Cli::from_arg_matches(&matches).unwrap(), matches)
}

fn env(vars: &[(&str, &str)], user_file: Option<PathBuf>) -> Environment {
    Environment {
        vars: vars.iter().map(|(k, v)| (k.into(), v.into())).collect(),
        user_file,
    }
}

fn resolve(args: &[&str], vars: &[(&str, &str)], file: Option<&str>) -> Result<Resolved> {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("config.toml");
    if let Some(text) = file {
        std::fs::write(&user, text).unwrap();
    }
    let (cli, matches) = parse(args);
    Resolved::load(&cli, &matches, &env(vars, Some(user)))
}

fn at(resolved: &Resolved, key: &str) -> Value {
    let pointer = format!("/{}", key.replace('.', "/"));
    Value::Object(resolved.document.clone())
        .pointer(&pointer)
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn later_layers_override_earlier_ones_and_record_their_origin() {
    let file = "plugin_timeout = 10\n[video]\ninterval = 2.0\nscene_threshold = 0.5\n";
    let resolved = resolve(
        &["convert", "--scene-threshold", "0.9", "a.mp4"],
        &[("ANYTOPDF_VIDEO_INTERVAL", "3")],
        Some(file),
    )
    .unwrap();
    assert_eq!(at(&resolved, "plugin_timeout"), json!(10));
    assert_eq!(at(&resolved, "video.interval"), json!(3));
    assert_eq!(at(&resolved, "video.scene_threshold"), json!(0.9));
    assert_eq!(at(&resolved, "video.dedupe_distance"), json!(4));
    assert!(matches!(
        resolved.origins["plugin_timeout"],
        Origin::File { .. }
    ));
    assert_eq!(
        resolved.origins["video.interval"],
        Origin::Env {
            name: "ANYTOPDF_VIDEO_INTERVAL".into()
        }
    );
    assert_eq!(
        resolved.origins["video.scene_threshold"],
        Origin::Flag {
            flag: "--video-scene-threshold".into()
        }
    );
    assert_eq!(resolved.origins["video.dedupe_distance"], Origin::Default);
}

#[test]
fn set_wins_over_flags_and_reaches_runtime_plugin_tables() {
    let resolved = resolve(
        &[
            "--set",
            "plugin_timeout=7",
            "--set",
            "json.max_records=100",
            "--set",
            "face-detect.labels=[\"a\", \"b\"]",
            "--plugin-timeout",
            "9",
            "doctor",
        ],
        &[],
        None,
    )
    .unwrap();
    assert_eq!(at(&resolved, "plugin_timeout"), json!(7));
    let tables = resolved.tables();
    assert_eq!(tables.table("json").unwrap()["max_records"], json!(100));
    assert_eq!(
        tables.table("face_detect").unwrap()["labels"],
        json!(["a", "b"])
    );
}

#[test]
fn unrelated_anytopdf_variables_are_not_settings() {
    let resolved = resolve(
        &["doctor"],
        &[
            ("ANYTOPDF_WEBHOOK_SECRET", "whsec_x"),
            ("ANYTOPDF_IMAP_HOST", "mail"),
            ("ANYTOPDF_PLUGIN_PATH", "/opt"),
            ("ANYTOPDF_PROFILE", "share"),
            ("ANYTOPDF_DENY_PLUGIN_KIND", "renderer, importer"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(at(&resolved, "profile"), json!("share"));
    assert_eq!(
        at(&resolved, "deny_plugin_kind"),
        json!(["renderer", "importer"])
    );
    let text = resolved.report().to_string();
    assert!(
        !text.contains("whsec_x") && !text.contains("mail"),
        "{text}"
    );
}

#[test]
fn invalid_values_name_the_key_and_where_they_came_from() {
    let err = resolve(&["doctor"], &[], Some("[video]\nintervall = 2\n")).unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("video.intervall") && text.contains("config.toml"),
        "{text}"
    );

    let err = resolve(&["doctor"], &[("ANYTOPDF_STRICT", "maybe")], None).unwrap_err();
    assert!(format!("{err:#}").contains("ANYTOPDF_STRICT"), "{err:#}");

    let err = resolve(&["doctor"], &[], Some("profile = \"public\"\n")).unwrap_err();
    assert!(
        format!("{err:#}")
            .contains("profile: value is not in enum; expected one of archive, share"),
        "{err:#}"
    );

    let err = resolve(&["doctor"], &[], Some("renderer = 3\n")).unwrap_err();
    assert!(format!("{err:#}").contains("renderer"), "{err:#}");

    assert!(resolve(&["doctor"], &[], Some("not toml = =")).is_err());
    assert!(resolve(&["--set", "novalue", "doctor"], &[], None).is_err());
}

#[test]
fn a_missing_user_file_is_fine_but_a_named_file_must_exist() {
    let resolved = resolve(&["doctor"], &[], None).unwrap();
    assert_eq!(resolved.files.len(), 1);
    assert!(!resolved.files[0].loaded);
    let err = resolve(
        &["--config", "/nonexistent/anytopdf.toml", "doctor"],
        &[],
        None,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("/nonexistent/anytopdf.toml"));
}

#[test]
fn named_files_replace_the_user_file_and_no_config_skips_all() {
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("project.toml");
    std::fs::write(&named, "strict = true\n").unwrap();
    let named = named.to_str().unwrap();
    let user_file = "fail_fast = true\n";

    let resolved = resolve(&["--config", named, "doctor"], &[], Some(user_file)).unwrap();
    assert_eq!(at(&resolved, "strict"), json!(true));
    assert_eq!(at(&resolved, "fail_fast"), json!(false));

    let resolved = resolve(&["doctor"], &[(CONFIG_ENV, named)], Some(user_file)).unwrap();
    assert_eq!(at(&resolved, "strict"), json!(true));

    let resolved = resolve(&["--no-config", "doctor"], &[], Some(user_file)).unwrap();
    assert!(resolved.files.is_empty());
    assert_eq!(at(&resolved, "fail_fast"), json!(false));

    let resolved = resolve(&["doctor"], &[(NO_CONFIG_ENV, "1")], Some(user_file)).unwrap();
    assert_eq!(at(&resolved, "fail_fast"), json!(false));
}

#[test]
fn effective_values_are_written_back_into_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("config.toml");
    std::fs::write(
        &user,
        "plugins = false\nprofile = \"share\"\nfilter = \"x\"\n\
         renderer = \"pdf\"\n[ocr]\nmode = \"off\"\n[video]\ninterval = 9.0\n",
    )
    .unwrap();
    let (mut cli, matches) = parse(&["convert", "--filter", "keep", "a.txt"]);
    let resolved = Resolved::load(&cli, &matches, &env(&[], Some(user))).unwrap();
    apply(&mut cli, &matches, &resolved).unwrap();
    assert!(cli.no_plugins);
    let Commands::Convert(args) = &cli.command else {
        panic!("convert")
    };
    assert_eq!(args.renderer, "pdf");
    assert_eq!(args.profile, anytopdf_core::Profile::Share);
    assert_eq!(args.filter.as_deref(), Some("keep"));
    let builtin = BuiltinOptions::from_tables(&resolved.tables()).unwrap();
    assert_eq!(builtin.ocr.mode, anytopdf_builtin::OcrMode::Off);
    assert_eq!(builtin.video.interval, 9.0);
}

#[test]
fn defaults_match_the_command_line_defaults_and_the_schema() {
    let (mut cli, matches) = parse(&["convert", "a.txt"]);
    let before = format!("{:?}", cli);
    let resolved = Resolved::load(&cli, &matches, &env(&[], None)).unwrap();
    apply(&mut cli, &matches, &resolved).unwrap();
    assert_eq!(format!("{:?}", cli), before);
    assert!(resolved.files.is_empty());
    assert!(
        resolved
            .origins
            .values()
            .all(|origin| *origin == Origin::Default)
    );
}

#[test]
fn every_bound_flag_has_a_typed_schema_key() {
    for binding in GLOBAL_FLAGS.iter().chain(CONVERT_FLAGS) {
        let path = key_path(binding.key).unwrap();
        assert!(
            expected_type(&path).is_some(),
            "{} has no schema type",
            binding.key
        );
    }
    let commands = crate::cli::command();
    let convert = commands.find_subcommand("convert").unwrap();
    for binding in GLOBAL_FLAGS {
        assert!(
            commands.get_arguments().any(|a| a.get_id() == binding.arg),
            "{}",
            binding.arg
        );
    }
    for binding in CONVERT_FLAGS {
        let arg = convert
            .get_arguments()
            .find(|a| a.get_id() == binding.arg)
            .unwrap_or_else(|| panic!("{}", binding.arg));
        assert_eq!(Some(&binding.flag[2..]), arg.get_long(), "{}", binding.arg);
    }
}

#[test]
fn rendered_toml_parses_back_to_the_same_values() {
    let resolved = resolve(
        &["--set", "json.max_records=1", "doctor"],
        &[],
        Some(
            "renderer = \"pdf\"\n[whisper]\nmodel = \"base\"\n\"odd key\" = 1\n\
             nested = { a = 1 }\n[\"two words\"]\nx = 1\n",
        ),
    )
    .unwrap();
    assert_eq!(at(&resolved, "renderer"), json!("pdf"));
    for annotate in [true, false] {
        let text = resolved.render(annotate);
        let parsed: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            Value::Object(resolved.document.clone()),
            "{text}"
        );
    }
}

#[test]
fn forwarded_flags_reproduce_the_configuration_sources() {
    let (cli, _) = parse(&[
        "--no-config",
        "--config",
        "a.toml",
        "--set",
        "strict=true",
        "doctor",
    ]);
    assert_eq!(
        forward_flags(&cli),
        ["--no-config", "--config=a.toml", "--set=strict=true"].map(OsString::from)
    );
}

#[test]
fn every_builtin_table_matches_its_schema_entry() {
    for table in anytopdf_builtin::option_tables() {
        let node = super::values::schema_node(&[table.name.to_string()]).unwrap();
        assert_eq!(
            node["additionalProperties"],
            json!(false),
            "[{}] needs a $defs entry in config-file.schema.json",
            table.name
        );
        let defaults = table.defaults.as_object().unwrap();
        let declared = node["properties"].as_object().unwrap();
        assert_eq!(
            defaults.keys().collect::<Vec<_>>(),
            declared.keys().collect::<Vec<_>>(),
            "[{}] keys differ between the struct and the schema",
            table.name
        );
        for (key, value) in defaults {
            assert_eq!(
                &declared[key]["default"], value,
                "[{}] {key}: schema default differs from the struct default",
                table.name
            );
            assert!(
                declared[key]["description"].is_string(),
                "[{}] {key} needs a description for its --help line",
                table.name
            );
        }
    }
}

/// A value other than the default, valid for `option`.
fn other_value(option: &super::flags::OptionDef) -> (Value, String) {
    match option.kind.as_str() {
        "boolean" => {
            let v = !option.default.as_bool().unwrap();
            (json!(v), v.to_string())
        }
        "integer" => {
            let v = option.default.as_i64().unwrap() + 3;
            (json!(v), v.to_string())
        }
        "number" => {
            let v = option.default.as_f64().unwrap() + 0.25;
            (json!(v), v.to_string())
        }
        _ if !option.values.is_empty() => {
            let v = option
                .values
                .iter()
                .find(|v| json!(v) != option.default)
                .unwrap()
                .clone();
            (json!(v), v)
        }
        _ => (json!("xyz"), "xyz".to_string()),
    }
}

#[test]
fn every_builtin_option_has_three_equivalent_spellings() {
    let options = super::flags::builtin_options();
    assert!(options.len() >= 8, "{options:?}");
    for option in options {
        let (expected, raw) = other_value(&option);
        let toml_value = super::render::toml_value(&expected);
        let file = format!("[{}]\n{} = {toml_value}\n", option.table, option.key);
        let from_file = resolve(&["doctor"], &[], Some(&file)).unwrap();
        let env_name = option.env();
        let from_env = resolve(&["doctor"], &[(env_name.as_str(), raw.as_str())], None).unwrap();
        let flag = format!("--{}={raw}", option.flag());
        let from_flag = resolve(&["convert", &flag, "a.txt"], &[], None).unwrap();
        for (label, resolved) in [
            ("file", &from_file),
            ("env", &from_env),
            ("flag", &from_flag),
        ] {
            assert_eq!(
                at(resolved, &option.dotted()),
                expected,
                "{} via {label}",
                option.dotted()
            );
        }
        assert_eq!(
            from_flag.origins[&option.dotted()],
            Origin::Flag {
                flag: format!("--{}", option.flag())
            }
        );
        assert_eq!(
            from_env.origins[&option.dotted()],
            Origin::Env { name: env_name }
        );
    }
}

#[test]
fn older_flag_names_still_work() {
    let resolved = resolve(
        &[
            "convert",
            "--ocr",
            "off",
            "--lang",
            "deu",
            "--scene-threshold",
            "0.5",
            "--dedupe-distance",
            "6",
            "--max-video-frames",
            "3",
            "--max-image-frames",
            "2",
            "--no-embedded-subtitles",
            "a.txt",
        ],
        &[],
        None,
    )
    .unwrap();
    for (key, value) in [
        ("ocr.mode", json!("off")),
        ("ocr.lang", json!("deu")),
        ("video.scene_threshold", json!(0.5)),
        ("video.dedupe_distance", json!(6)),
        ("video.max_frames", json!(3)),
        ("image.max_frames", json!(2)),
        ("captions.embedded_subtitles", json!(false)),
    ] {
        assert_eq!(at(&resolved, key), value, "{key}");
    }
}

#[test]
fn environment_names_map_to_tables_and_globals() {
    let tables = vec![
        "video".to_string(),
        "whisper".to_string(),
        "imap".to_string(),
    ];
    let path = |name: &str| super::env_path(name, &tables);
    let v = |parts: &[&str]| Some(parts.iter().map(|p| p.to_string()).collect::<Vec<_>>());
    assert_eq!(
        path("ANYTOPDF_VIDEO_MAX_FRAMES"),
        v(&["video", "max_frames"])
    );
    assert_eq!(path("ANYTOPDF_PLUGIN_TIMEOUT"), v(&["plugin_timeout"]));
    assert_eq!(path("ANYTOPDF_WHISPER_MODEL"), v(&["whisper", "model"]));
    assert_eq!(
        path("ANYTOPDF_FACE_DETECT__MIN_SIZE"),
        v(&["face_detect", "min_size"])
    );
    assert_eq!(path("ANYTOPDF_FACE_DETECT_MIN_SIZE"), None);
    assert_eq!(path("ANYTOPDF_IMAP_PASSWORD"), None);
    assert_eq!(path("ANYTOPDF_WEBHOOK_SECRET"), None);
    assert_eq!(path("ANYTOPDF_CONFIG"), None);
    assert_eq!(path("ANYTOPDF_VIDEO"), None);
}
