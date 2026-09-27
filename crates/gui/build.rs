fn main() -> Result<(), slint_build::CompileError> {
    let config = slint_build::CompilerConfiguration::new()
        // Plain `fluent`, not `fluent-dark`: the suffix folds the platform's
        // color scheme to a constant, and the window writes the widget
        // palette's scheme itself from the one the user chose.
        .with_style("fluent".into())
        // Slint's element-query API, which the pointer-target tests use to
        // click a real button in a real window, can only find an element when
        // the compiler kept its debug info. Release builds do not need it and
        // should not carry it, so it is kept for every other profile.
        .with_debug_info(std::env::var("PROFILE").as_deref() != Ok("release"));
    slint_build::compile_with_config("ui/app.slint", config)
}
