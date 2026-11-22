use std::process::Command;

#[test]
fn error_color_respects_force_color_in_pipes() {
    for (value, expected) in [
        ("", true),
        ("1", true),
        ("true", true),
        ("2", true),
        ("3", true),
        ("0", false),
        ("false", false),
        ("other", false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_microtypo"))
            .arg("--unknown-option")
            .env_remove("NO_COLOR")
            .env_remove("NODE_DISABLE_COLORS")
            .env("FORCE_COLOR", value)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stderr.starts_with(b"\x1b[31m"), expected, "FORCE_COLOR={value:?}");
    }
}

#[test]
fn color_reopens_after_a_closing_color_code_in_an_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_microtypo"))
        .arg("--bad\x1b[39moption")
        .env_remove("NO_COLOR")
        .env_remove("NODE_DISABLE_COLORS")
        .env("FORCE_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, b"\x1b[31merror: Unknown option '--bad\x1b[31moption'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--bad\\u001b[39moption\"\n\x1b[39mrun `microtypo --help` for usage\n");
}
