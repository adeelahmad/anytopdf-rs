use super::values::expected_type;
use super::*;
use clap::{CommandFactory, FromArgMatches};

fn parse(args: &[&str]) -> (Cli, ArgMatches) {
    let matches = Cli::command()
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
    let file = "plugin_timeout = 10\n[importer.video]\ninterval = 2.0\nscene_threshold = 0.5\n";
    let resolved = resolve(
        &["convert", "--scene-threshold", "0.9", "a.mp4"],
        &[("ANYTOPDF_IMPORTER__VIDEO__INTERVAL", "3")],
        Some(file),
    )
    .unwrap();
    assert_eq!(at(&resolved, "plugin_timeout"), json!(10));
    assert_eq!(at(&resolved, "importer.video.interval"), json!(3));
    assert_eq!(at(&resolved, "importer.video.scene_threshold"), json!(0.9));
    assert_eq!(at(&resolved, "importer.video.dedupe_distance"), json!(4));
    assert!(matches!(
        resolved.origins["plugin_timeout"],
        Origin::File { .. }
    ));
    assert_eq!(
        resolved.origins["importer.video.interval"],
        Origin::Env {
            name: "ANYTOPDF_IMPORTER__VIDEO__INTERVAL".into()
        }
    );
    assert_eq!(
        resolved.origins["importer.video.scene_threshold"],
        Origin::Flag {
            flag: "--scene-threshold".into()
        }
    );
    assert_eq!(
        resolved.origins["importer.video.dedupe_distance"],
        Origin::Default
    );
}

#[test]
fn set_wins_over_flags_and_reaches_runtime_plugin_tables() {
    let resolved = resolve(
        &[
            "--set",
            "plugin_timeout=7",
            "--set",
            "importer.json.max_records=100",
            "--set",
            "enricher.face-detect.labels=[\"a\", \"b\"]",
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
    assert_eq!(
        tables.table("importer", "json").unwrap()["max_records"],
        json!(100)
    );
    assert_eq!(
        tables.for_kind("unit-enricher", "face-detect").unwrap()["labels"],
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
    let err = resolve(&["doctor"], &[], Some("[importer.video]\nintervall = 2\n")).unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("importer.video.intervall") && text.contains("config.toml"),
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

    let err = resolve(&["doctor"], &[], Some("[renderer]\nuse = 3\n")).unwrap_err();
    assert!(format!("{err:#}").contains("renderer.use"), "{err:#}");

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
         [renderer]\nuse = \"pdf\"\n[enricher.ocr]\nmode = \"off\"\n[importer.video]\ninterval = 9.0\n",
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
    assert_eq!(args.ocr, anytopdf_builtin::OcrMode::Off);
    assert_eq!(args.video_interval, 9.0);
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
    let commands = Cli::command();
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
        &["--set", "importer.json.max_records=1", "doctor"],
        &[],
        Some(
            "renderer = \"pdf\"\n[enricher.whisper]\nmodel = \"base\"\n\"odd key\" = 1\n\
             nested = { a = 1 }\n[enricher.\"two words\"]\nx = 1\n",
        ),
    )
    .unwrap();
    assert_eq!(at(&resolved, "renderer.use"), json!("pdf"));
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
